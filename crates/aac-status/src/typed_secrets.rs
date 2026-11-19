//! The typed-secret check for the owner's notes: secrets as a person types them, by name and
//! value. Ported rule for rule from `typed-secrets.ts` (its doc comment is the specification), so
//! the one secret filter is Rust: TypeScript calls this through the native module until the cut.
//! Verdicts are byte-equal to the last TypeScript implementation on `tests/spec/secret-filter.json`.
//!
//! JavaScript semantics kept: the text is read as UTF-16 code units (every non-ASCII character
//! that is not JS whitespace counts as one opaque unit per code unit, exactly as the shared
//! policy's `regex_text` does), so lengths and classes agree. The JS patterns' lookbehinds are a
//! check of the preceding character, and their lookaheads are matched in place while the scan
//! resumes where JavaScript's `lastIndex` would (the end of the name and separator), so a value
//! is searched for names in its turn. Every pattern is bounded; the scan stays linear.

use regex::Regex;
use std::sync::OnceLock;

const FOUND: &str = "a password or key";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Password,
    Bare,
    Key,
}

/// A JS-style global scan: the pattern's match must start where `before_ok` accepts the
/// preceding character; `main_end` is the capture group whose start is where JavaScript's
/// `lastIndex` lands (the end of the part outside the lookahead).
struct Scan {
    re: Regex,
    before: fn(char) -> bool,
}

impl Scan {
    fn each<'t>(&self, text: &'t str, mut visit: impl FnMut(&regex::Captures<'t>) -> bool) -> bool {
        let mut pos = 0;
        while pos <= text.len() {
            let Some(caps) = self.re.captures_at(text, pos) else { break };
            let whole = caps.get(0).expect("match");
            let start = whole.start();
            let rejected = text[..start].chars().next_back().is_some_and(self.before);
            if !rejected {
                if visit(&caps) {
                    return true;
                }
                // `lastIndex`: the end of the part outside the lookahead (group `end`, empty).
                let end = caps.name("end").map_or(whole.end(), |m| m.start());
                pos = if end > start { end } else { start + next_len(text, start) };
            } else {
                pos = start + next_len(text, start);
            }
        }
        false
    }
}

fn next_len(text: &str, at: usize) -> usize {
    text[at..].chars().next().map_or(1, char::len_utf8)
}

fn alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}
fn alnum_dash(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}
fn alnum_underscore_dash(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}
fn url_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')
}

struct Rules {
    named: Scan,
    setx: Scan,
    flag: Scan,
    auth: Scan,
    url_password: Scan,
    glued: Scan,
    login: Scan,
    user: Scan,
    camel: Regex,
    word_split: Regex,
    password_word: Regex,
    glued_phrase: Regex,
    glued_word: Regex,
    placeholder: Regex,
    leading: Regex,
    trailing: Regex,
    url: Regex,
    date: Regex,
    version: Regex,
    fingerprint: Regex,
    settings: Regex,
    path_like: Regex,
    run_split: Regex,
    path_run_split: Regex,
    letter: Regex,
    mixed: Regex,
    digit: Regex,
    upper: Regex,
    symbol: Regex,
    all_digits: Regex,
    letters_only: Regex,
    part_split: Regex,
    test_name: Regex,
    ends_letter: Regex,
    slash: Regex,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        // JavaScript's `\s` (WhiteSpace and LineTerminator), spelled out: the workspace builds
        // `regex` without Unicode Perl classes, and this set is exactly JS's, not Unicode's.
        const WS: &str = "\t\n\x0B\x0C\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";
        const WS_NO_NEWLINE: &str = "\t\x0B\x0C\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";
        let re = |source: &str| {
            let source = source
                .replace(r"[^\S\n]", &format!("[{WS_NO_NEWLINE}]"))
                .replace(r"[^\s", &format!("[^{WS}"))
                .replace(r"\s", &format!("[{WS}]"));
            Regex::new(&source).expect("typed-secret pattern")
        };
        let scan = |source: &str, before: fn(char) -> bool| Scan { re: re(source), before };
        Rules {
            named: scan(
                r#"((?:[A-Za-z][A-Za-z0-9]{0,23} )?\$?[A-Za-z_][A-Za-z0-9_-]{0,63})(['"]?\]?\s{0,4}(?:=>|->|:=|[:=])\s{0,4}|['"]\]?\s{0,4},\s{0,4}|\s{1,4}(?:is|was)\s{1,4})(?P<end>)['"`]?([^\s'"`]{1,256})['"`]?([^\S\n]{1,4}[A-Za-z])?"#,
                alnum_underscore_dash,
            ),
            setx: scan(
                r#"(?i:setx)\s{1,4}([A-Za-z_][A-Za-z0-9_]{0,63})\s{1,4}(?P<end>)['"]?([^\s'"]{1,256})"#,
                alnum,
            ),
            flag: scan(
                r#"--([A-Za-z][A-Za-z0-9-]{0,47})(?:=|\s{1,4})(?P<end>)['"`]?([^\s'"`]{1,256})"#,
                alnum_dash,
            ),
            auth: scan(r#"(?i:bearer|basic)\s{1,4}(?P<end>)([^\s'"`]{1,256})"#, alnum),
            url_password: scan(
                r#"[A-Za-z][A-Za-z0-9+.-]{0,31}://[^\s/:@]{0,64}:(?P<end>)([^\s/@]{1,256})@"#,
                url_scheme_char,
            ),
            glued: scan(r#"-p(?P<end>)([^\s'"=-][^\s'"]{0,255})"#, alnum_dash),
            login: scan(
                r#"(?:sshpass|(?:docker|podman|nerdctl|helm|oras|buildah|skopeo|oc|crane|regctl|az|cf)(?:[^\S\n]{1,4}[a-z]{1,16}){0,2}[^\S\n]{1,4}login)(?P<end>)(?:[^\S\n]{1,4}[^\s;|&]{1,128}){0,8}?[^\S\n]{1,4}-p[^\S\n]{1,4}['"]?([^\s;|&'"]{1,256})"#,
                alnum_dash,
            ),
            user: scan(
                r#"(?:-u|--user)(?:\s{1,4}|=)['"]?[^\s:'"]{1,64}:(?P<end>)([^\s'"@]{1,256})"#,
                alnum_dash,
            ),
            camel: re(r"([a-z0-9])([A-Z])"),
            word_split: re(r"[\s_-]+"),
            password_word: re(r"(?:password|passwd|passphrase)$"),
            glued_phrase: re(r"^(?:secret|token|credentials?)$"),
            glued_word: re(r"(?:secret|token|credentials?|apikey|accesskey|privatekey|signingkey|secretkey)$"),
            placeholder: re(r"^(?:\$\{?[A-Za-z_][A-Za-z0-9_]*\}?|%[A-Za-z_][A-Za-z0-9_]*%|<[^>]*>?|\{\{.*)$"),
            leading: re(r"^[(\[]+"),
            trailing: re(r"[)\]}>.,;:!?]+$"),
            url: re(r"^[A-Za-z][A-Za-z0-9+.-]*://"),
            date: re(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}"),
            version: re(r"^v?[0-9]+(?:\.[0-9]+)+(?:[-+][A-Za-z0-9_.]+)?$"),
            fingerprint: re(r"^(?i:SHA256|SHA1|MD5):"),
            settings: re(r"^[A-Za-z_][A-Za-z0-9_-]*=[^,]*(?:,[A-Za-z_][A-Za-z0-9_-]*=[^,]*)+$"),
            path_like: re(concat!(
                r"^(?:~[\\/]|\.{1,2}[\\/]|[A-Za-z]:[\\/]|",
                r"[\\/](?:home|Users|etc|var|tmp|usr|opt|mnt|run|srv|root|Volumes|private|proc|dev|sys|bin|sbin|lib|lib64|",
                r"snap|media|nix|workspaces?|app|data|cygdrive)(?:[\\/]|$)|",
                r"[A-Za-z0-9_.-]+(?:[\\/][A-Za-z0-9_.-]+)+\.[A-Za-z][A-Za-z0-9]{0,5}$)"
            )),
            run_split: re(r"[-._:]+"),
            path_run_split: re(r"[-._:\\/]+"),
            letter: re(r"[A-Za-z]"),
            mixed: re(r"[0-9!@#$%^&*+=?~|]"),
            digit: re(r"[0-9]"),
            upper: re(r"[A-Z]"),
            symbol: re(r"[^A-Za-z0-9_.-]"),
            all_digits: re(r"^[0-9]{8,}$"),
            letters_only: re(r"^[A-Za-z]+$"),
            part_split: re(r"[_-]+"),
            test_name: re(r"^(?i:test|step|case|stage|phase)"),
            ends_letter: re(r"[A-Za-z]$"),
            slash: re(r"[\\/]"),
        }
    })
}

/// JavaScript `.length`: the text is already one char per UTF-16 unit.
fn js_len(text: &str) -> usize {
    text.chars().count()
}

fn secret_kind(name: &str, separator: &str, flag: bool) -> Option<Kind> {
    let r = rules();
    let phrase = name.contains(' ');
    let stripped = name.strip_prefix('$').unwrap_or(name);
    let spaced = r.camel.replace_all(stripped, "$1 $2").to_lowercase();
    let words: Vec<&str> = r.word_split.split(&spaced).filter(|w| !w.is_empty()).collect();
    const NOT_A_SECRET_NAME: &[&str] = &[
        "file", "files", "path", "dir", "url", "uri", "name", "names", "count", "limit", "length", "len", "size",
        "ttl", "type", "kind", "mode", "policy", "budget", "usage", "id", "ids", "at", "expires", "expiry", "env",
        "hash", "hashes", "manager", "scanning", "scanner", "fingerprint", "min", "max", "ref", "refs", "endpoint",
        "helper",
    ];
    const KEY_PAIR: &[&str] = &["api", "access", "private", "secret", "signing", "encryption", "master", "deploy"];
    const LOOKUP_KEY: &[&str] = &[
        "cache", "sort", "partition", "primary", "foreign", "idempotency", "dedupe", "dedup", "hot", "row", "shard",
        "routing", "lookup", "map", "group", "index", "object",
    ];
    let last = *words.last()?;
    if NOT_A_SECRET_NAME.contains(&last) {
        return None;
    }
    if words.iter().any(|w| r.password_word.is_match(w)) {
        return Some(Kind::Password);
    }
    if words.iter().any(|w| matches!(*w, "pass" | "pw" | "pwd")) {
        if flag || (!phrase && words.len() > 1) {
            return Some(Kind::Password);
        }
        // `PASS: TestLogin2FA` is a test result
        if !(name == "PASS" && separator == ":") {
            return Some(Kind::Bare);
        }
    }
    let glued = if phrase { &r.glued_phrase } else { &r.glued_word };
    if words.iter().any(|w| glued.is_match(w)) {
        return Some(Kind::Key);
    }
    for at in 1..words.len() {
        if words[at] == "key" && !LOOKUP_KEY.contains(&words[at - 1]) && (!phrase || KEY_PAIR.contains(&words[at - 1]))
        {
            return Some(Kind::Key);
        }
    }
    None
}

/// The longest run, between `separators`, that mixes letters with digits or symbols.
fn longest_mixed_run(value: &str, separators: &Regex) -> usize {
    let r = rules();
    separators
        .split(value)
        .filter(|run| r.letter.is_match(run) && r.mixed.is_match(run))
        .map(js_len)
        .max()
        .unwrap_or(0)
}

/// Whether a value after a secret's name looks like one (`secretValue` in typed-secrets.ts).
fn secret_value(kind: Kind, raw: &str, separator: &str, more: bool) -> bool {
    let r = rules();
    let after_is = separator == "is" || separator == "was";
    if r.placeholder.is_match(raw) {
        return false;
    }
    let leading = r.leading.replace(raw, "");
    let value = r.trailing.replace(&leading, "").into_owned();
    let value = value.as_str();
    if r.url.is_match(value)
        || r.date.is_match(value)
        || r.version.is_match(value)
        || r.fingerprint.is_match(value)
        || (after_is && (value.contains('=') || (js_len(value) < 8 && more)))
    {
        return false;
    }
    // a list of settings (`id=x,src=y`): each setting's value is judged on its own
    if r.settings.is_match(value) {
        return value.split(',').any(|setting| {
            let at = setting.find('=').map_or(0, |i| i + 1);
            secret_value(kind, &setting[at..], separator, false)
        });
    }
    if r.path_like.is_match(value) {
        return kind != Kind::Bare
            && longest_mixed_run(value, &r.path_run_split) >= if kind == Kind::Key { 8 } else { 6 };
    }
    if kind == Kind::Key {
        return longest_mixed_run(value, &r.run_split) >= 8;
    }
    let len = js_len(value);
    if kind == Kind::Bare {
        if r.slash.is_match(value) {
            return false;
        }
        if longest_mixed_run(value, &r.run_split) >= 6 {
            return true;
        }
        let human = len >= 8 && r.letter.is_match(value) && r.digit.is_match(value);
        // a test or step name (`TestAuth_2`, `Step_2_Done`, `round_16_ok`) is no password; a password ends in a
        // digit or a symbol (`Summer_Winter_2026`)
        let words = r.part_split.split(value).filter(|part| r.letters_only.is_match(part)).count();
        let test_name = r.test_name.is_match(value) || (words >= 2 && r.ends_letter.is_match(value));
        if !human || test_name {
            return false;
        }
        return separator == "=" || r.upper.is_match(value) || r.symbol.is_match(value);
    }
    // a password: human ones are a word and a number (`Summer_2026`), so the whole value counts
    if r.all_digits.is_match(value) {
        return true;
    }
    if len >= 8 && r.letter.is_match(value) && r.digit.is_match(value) {
        return true;
    }
    longest_mixed_run(value, &r.run_split) >= 6
}

fn found(kind: Option<Kind>, value: Option<&str>, separator: &str, more: bool) -> bool {
    match (kind, value) {
        (Some(kind), Some(value)) => secret_value(kind, value, separator, more),
        _ => false,
    }
}

/// The kind of typed secret a note contains (`"a password or key"`), or None.
pub fn find_typed_secret(text: &str) -> Option<&'static str> {
    let r = rules();
    let text = crate::secret_policy::regex_text(text);
    let text = text.as_ref();
    let named = r.named.each(text, |m| {
        let separator: String = m[2].chars().filter(|c| !matches!(c, '\'' | '"' | ']') && !c.is_whitespace()).collect();
        found(
            secret_kind(&m[1], &separator, false),
            m.get(4).map(|v| v.as_str()),
            &separator,
            m.get(5).is_some(),
        )
    });
    if named {
        return Some(FOUND);
    }
    if r.setx.each(text, |m| found(secret_kind(&m[1], "=", false), m.get(3).map(|v| v.as_str()), "=", false)) {
        return Some(FOUND);
    }
    if r.flag.each(text, |m| found(secret_kind(&m[1], "=", true), m.get(3).map(|v| v.as_str()), "=", false)) {
        return Some(FOUND);
    }
    for scan in [&r.auth, &r.url_password, &r.glued, &r.login, &r.user] {
        if scan.each(text, |m| {
            // The value is the last capture (after the empty `end` marker).
            let value = m.iter().skip(1).flatten().filter(|g| !g.is_empty()).last().map(|g| g.as_str());
            found(Some(Kind::Password), value, "=", false)
        }) {
            return Some(FOUND);
        }
    }
    None
}
