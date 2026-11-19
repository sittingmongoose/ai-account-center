//! The sign-in output parser, ported from `signin-output.ts` (CONTRACT-registry-lifecycle 6.6).
//!
//! Only two facts survive: the allowlisted verification URL and the one-time user code.
//! Everything else is discarded unread; nothing here logs or persists output. At most 64 KB
//! is examined; terminal control sequences are removed first.

use regex::Regex;
use std::sync::LazyLock;

/// The examined-output budget (`MAX_SIGNIN_OUTPUT_BYTES`).
pub const MAX_SIGNIN_OUTPUT_BYTES: usize = 64 * 1024;
/// The partial-line cap in UTF-16 code units (`MAX_LINE_CHARS`; JS `.length`).
const MAX_LINE_CHARS: usize = 8 * 1024;

/// The one-time user code: a whole trimmed line (`USER_CODE_PATTERN`).
static USER_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z0-9]{4,6}(?:-[A-Z0-9]{4,6})?$").expect("fixed pattern"));
/// OSC sequences: `ESC ]` payload, optionally terminated by BEL or `ESC \`.
static OSC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("\x1b\\][^\x07\x1b]*(?:\x07|\x1b\\\\)?").expect("fixed pattern"));
/// CSI sequences: `ESC [` parameters, intermediates, one final byte.
static CSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("\x1b\\[[0-?]*[ -/]*[@-~]").expect("fixed pattern"));
/// Two-character escapes: `ESC` followed by `@-Z` or `\`-`_`.
static ESC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("\x1b[@-Z\\\\-_]").expect("fixed pattern"));
/// Remaining control characters except HT, LF and CR.
static CONTROL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("[\x00-\x08\x0b-\x1f\x7f]").expect("fixed pattern"));

/// Remove OSC, CSI and two-character escapes, then any other control character
/// (`stripTerminalControls`). The four passes run in the reference's order.
pub fn strip_terminal_controls(text: &str) -> String {
    let step = OSC.replace_all(text, "");
    let step = CSI.replace_all(&step, "");
    let step = ESC.replace_all(&step, "");
    CONTROL.replace_all(&step, "").into_owned()
}

/// ECMAScript WhiteSpace and LineTerminator, the set `String.prototype.trim` and `\s` use.
fn is_ecma_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{000b}' | '\u{000c}' | '\r' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// JavaScript `String.prototype.trim` on the parsed line.
fn trim_js(text: &str) -> &str {
    text.trim_matches(is_ecma_space)
}

/// ASCII word character: `\w` without the `u` flag.
fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The URL stop set: `\s` plus `"`, `'`, `<`, `>` and backtick.
fn is_url_stop(c: char) -> bool {
    is_ecma_space(c) || matches!(c, '"' | '\'' | '<' | '>' | '`')
}

/// Trailing punctuation the reference strips from a URL match (`/[.,;:)\]}>]+$/`).
fn trim_url_tail(text: &str) -> &str {
    text.trim_end_matches(['.', ',', ';', ':', ')', ']', '}', '>'])
}

/// Every `/\bhttps?:\/\/[^\s"'<>`]+/gi` match, left to right. Without the `u` flag `\b`
/// is ASCII: the scheme must start the text or follow a non-word character.
fn url_matches(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out = Vec::new();
    let mut search = 0usize;
    while let Some(pos) = lower[search..].find("http").map(|p| search + p) {
        search = pos + 4;
        let rest = &bytes[pos..];
        let scheme_len = if rest.starts_with(b"https://") {
            8
        } else if rest.starts_with(b"http://") {
            7
        } else {
            continue;
        };
        // `\b` before the scheme: start of text or an ASCII non-word character.
        if pos > 0 {
            let prev = text[..pos].chars().next_back();
            if prev.is_some_and(is_word) {
                continue;
            }
        }
        let start = pos + scheme_len;
        let mut end = start;
        for (i, c) in text[start..].char_indices() {
            if is_url_stop(c) {
                break;
            }
            end = start + i + c.len_utf8();
        }
        // The match needs at least one character after the scheme (`+`).
        if end > start {
            out.push(text[pos..end].to_string());
        }
    }
    out
}

/// The parser's options (`SignInOutputOptions`).
#[derive(Clone)]
pub struct Options {
    /// Exact origins, e.g. `https://auth.openai.com`.
    pub allowed_origins: Vec<String>,
    /// Device-code flows need a user code; supervised flows only a URL.
    pub expects_user_code: bool,
    /// Keep the query string (supervised flows); device-code URLs never keep it.
    pub keep_query: bool,
}

/// The two kept facts (`SignInVerification`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verification {
    /// The allowlisted URL, query kept only for supervised flows.
    pub url: String,
    /// The one-time user code, when the flow expects one.
    pub user_code: Option<String>,
}

/// The parser state (`SignInOutputState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Still looking.
    Pending,
    /// Both required facts were found.
    Ready,
    /// A foreign URL or an over-budget input failed the job.
    Rejected,
}

/// The in-memory output parser (`SignInOutputParser`). Output after `ready` is ignored.
pub struct Parser {
    options: Options,
    examined: usize,
    partial: String,
    url: Option<String>,
    user_code: Option<String>,
    state: State,
}

impl Parser {
    /// A parser for one flow's options.
    pub fn new(options: Options) -> Self {
        Self {
            options,
            examined: 0,
            partial: String::new(),
            url: None,
            user_code: None,
            state: State::Pending,
        }
    }

    /// The kept facts once ready.
    pub fn verification(&self) -> Option<Verification> {
        if self.state == State::Ready {
            self.url.as_ref().map(|url| Verification {
                url: url.clone(),
                user_code: self.user_code.clone(),
            })
        } else {
            None
        }
    }

    /// Feed more output; returns the state after it (`push`). The chunk arrives already
    /// decoded as UTF-8, the way Node's `StringDecoder` delivers it.
    pub fn push(&mut self, chunk: &str) -> State {
        if self.state != State::Pending {
            return self.state;
        }
        let budget = MAX_SIGNIN_OUTPUT_BYTES - self.examined;
        let bytes = chunk.len();
        let accepted = if bytes > budget {
            // Buffer.subarray(0, budget).toString(): a cut code point becomes U+FFFD.
            decode_prefix(&chunk.as_bytes()[..budget])
        } else {
            chunk.to_string()
        };
        self.examined += bytes.min(budget);
        let joined = format!("{}{}", self.partial, accepted);
        let mut lines = split_lines(&joined);
        self.partial = lines.pop().unwrap_or_default();
        for line in lines {
            self.line(&line);
            if self.state != State::Pending {
                return self.state;
            }
        }
        if utf16_len(&self.partial) > MAX_LINE_CHARS {
            return self.reject();
        }
        if self.examined >= MAX_SIGNIN_OUTPUT_BYTES {
            return self.reject();
        }
        self.state
    }

    /// The process ended: read the unterminated last line too (`end`).
    pub fn end(&mut self) -> State {
        if self.state == State::Pending && !self.partial.is_empty() {
            let last = std::mem::take(&mut self.partial);
            self.line(&last);
        }
        self.state
    }

    fn reject(&mut self) -> State {
        self.state = State::Rejected;
        self.partial.clear();
        self.state
    }

    fn line(&mut self, raw: &str) {
        let stripped = strip_terminal_controls(raw);
        let text = trim_js(&stripped);
        if text.is_empty() {
            return;
        }
        for candidate in url_matches(text) {
            let trimmed = trim_url_tail(&candidate);
            match self.allowed_url(trimmed) {
                None => {
                    self.reject();
                    return;
                }
                Some(url) => {
                    if self.url.is_none() {
                        self.url = Some(url);
                    }
                }
            }
        }
        if self.user_code.is_none() && USER_CODE.is_match(text) {
            self.user_code = Some(text.to_string());
        }
        if self.url.is_some() && (self.user_code.is_some() || !self.options.expects_user_code) {
            self.state = State::Ready;
        }
    }

    /// `new URL(value)` then the origin allowlist; the kept form is origin + pathname
    /// (+ search for supervised flows).
    fn allowed_url(&self, value: &str) -> Option<String> {
        let parsed = url::Url::parse(value).ok()?;
        if parsed.scheme() != "https"
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return None;
        }
        let origin = parsed.origin().ascii_serialization();
        if !self.options.allowed_origins.iter().any(|o| o == &origin) {
            return None;
        }
        let search = match parsed.query() {
            Some(query) if self.options.keep_query && !query.is_empty() => format!("?{query}"),
            _ => String::new(),
        };
        Some(format!("{origin}{}{search}", parsed.path()))
    }
}

/// Split on `\r\n`, `\n` or `\r` (`/\r\n|\n|\r/`), keeping the trailing partial.
fn split_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                out.push(text[start..i].to_string());
                i += if bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = i;
            }
            b'\n' => {
                out.push(text[start..i].to_string());
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    out.push(text[start..].to_string());
    out
}

/// UTF-16 code unit count, the unit of JS `.length`.
fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The first `budget` bytes as UTF-8, a cut code point replaced (`Buffer.toString`).
fn decode_prefix(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(error) => {
            let valid = error.valid_up_to();
            let mut out = String::from_utf8_lossy(&bytes[..valid]).into_owned();
            // The remainder is one incomplete sequence at the cut: one replacement char.
            if valid < bytes.len() {
                out.push('\u{fffd}');
            }
            out
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn codex() -> Options {
        Options {
            allowed_origins: vec!["https://auth.openai.com".into()],
            expects_user_code: true,
            keep_query: false,
        }
    }

    const DEVICE_OUTPUT: &str = "\x1b[1mWelcome to Codex\x1b[0m\r\nFollow these steps to sign in with ChatGPT using device code authorization:\r\n\r\n1. Open this link in your browser and sign in to your account\r\n   \x1b[94mhttps://auth.openai.com/codex/device\x1b[0m\r\n\r\n2. Enter this one-time code \x1b[2m(expires in 15 minutes)\x1b[0m\r\n   \x1b[94mABCD-12345\x1b[0m\r\n\r\n";

    #[test]
    fn strips_colors_osc8_and_cursor_moves() {
        let text = "\x1b[31mred\x1b[0m \x1b]8;;https://x.example\x07link\x1b]8;;\x07 \x1b[2Jhome\x1bH\x1bM";
        assert_eq!(strip_terminal_controls(text), "red link home");
    }

    #[test]
    fn finds_device_url_and_code_in_any_chunking() {
        for width in [1usize, 2, 3, 7, 64, DEVICE_OUTPUT.len()] {
            let mut parser = Parser::new(codex());
            let mut state = State::Pending;
            for chunk in DEVICE_OUTPUT.as_bytes().chunks(width) {
                state = parser.push(&String::from_utf8_lossy(chunk));
                if state != State::Pending {
                    break;
                }
            }
            assert_eq!(state, State::Ready, "width {width}");
            assert_eq!(
                parser.verification(),
                Some(Verification {
                    url: "https://auth.openai.com/codex/device".into(),
                    user_code: Some("ABCD-12345".into()),
                })
            );
        }
    }

    #[test]
    fn drops_query_for_device_and_keeps_for_supervised() {
        let mut device = Parser::new(codex());
        device.push("https://auth.openai.com/codex/device?state=PRIVATE\nABCD-1234\n");
        assert_eq!(
            device.verification().map(|v| v.url),
            Some("https://auth.openai.com/codex/device".into())
        );
        let mut supervised = Parser::new(Options {
            allowed_origins: vec!["https://accounts.google.com".into()],
            expects_user_code: false,
            keep_query: true,
        });
        supervised.push("https://accounts.google.com/o/oauth2/auth?code=4/0Abc\n");
        assert_eq!(
            supervised.verification().map(|v| v.url),
            Some("https://accounts.google.com/o/oauth2/auth?code=4/0Abc".into())
        );
    }

    #[test]
    fn rejects_foreign_http_and_embedded_credentials() {
        for text in [
            "https://evil.example/steal\n",
            "http://auth.openai.com/codex/device\n",
            "https://user:pass@auth.openai.com/x\n",
        ] {
            let mut parser = Parser::new(codex());
            assert_eq!(parser.push(text), State::Rejected, "{text}");
        }
    }

    #[test]
    fn waits_for_both_facts_and_reads_unterminated_last_line() {
        let mut parser = Parser::new(codex());
        assert_eq!(
            parser.push("https://auth.openai.com/codex/device\n"),
            State::Pending
        );
        assert_eq!(parser.push("ABCD-1234"), State::Pending);
        assert_eq!(parser.end(), State::Ready);
        assert_eq!(
            parser.verification().and_then(|v| v.user_code),
            Some("ABCD-1234".into())
        );
    }

    #[test]
    fn stops_after_64kb() {
        let mut parser = Parser::new(codex());
        let filler = "x".repeat(MAX_SIGNIN_OUTPUT_BYTES);
        assert_eq!(parser.push(&filler), State::Rejected);
        assert_eq!(
            parser.push("https://auth.openai.com/x\nABCD-1234\n"),
            State::Rejected
        );
        assert_eq!(parser.verification(), None);
    }

    #[test]
    fn overlong_partial_line_rejects() {
        let mut parser = Parser::new(codex());
        assert_eq!(
            parser.push(&"y".repeat(MAX_LINE_CHARS + 1)),
            State::Rejected
        );
    }

    #[test]
    fn url_boundary_and_tail_punctuation() {
        let mut parser = Parser::new(Options {
            allowed_origins: vec!["https://auth.openai.com".into()],
            expects_user_code: false,
            keep_query: false,
        });
        // A scheme inside a word is not a match; the trailing ')' is stripped.
        assert_eq!(
            parser.push("xhttps://auth.openai.com/a (https://auth.openai.com/b).\n"),
            State::Ready
        );
        assert_eq!(
            parser.verification().map(|v| v.url),
            Some("https://auth.openai.com/b".into())
        );
    }
}
