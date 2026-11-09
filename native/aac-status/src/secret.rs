//! The secret check every agent report passes before it is read, ported pattern for pattern
//! from `src/web-server/status/schema-helpers.ts` (where the invariants are documented in
//! full): the same patterns in the same order, the same decoded walk over every key and
//! string, the same embedded-JSON budget, the same unescape passes. A file that matches one
//! is refused whole; the matched text is never shown, only the kind.
//!
//! Every word boundary is written `(?-u:\b)`, JavaScript's ASCII `\b` the patterns were
//! ported from: a non-ASCII letter (say `密`) is not a word character to it, so a token
//! directly after one still has its leading boundary and is refused the same. `\s` stays
//! Unicode, as it is on the TypeScript side.

use regex::Regex;
use serde_json::Value as Json;
use std::sync::LazyLock;

enum Check {
    Pattern(&'static LazyLock<Regex>),
    Jwt,
    Email,
}

macro_rules! static_regex {
    ($re:expr) => {{
        static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new($re).expect("secret pattern"));
        &RE
    }};
}

fn patterns() -> Vec<(&'static str, Check)> {
    vec![
        (
            "a private key",
            Check::Pattern(static_regex!(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")),
        ),
        (
            "an API key",
            Check::Pattern(static_regex!(r"(?-u:\b)sk-(?:ant-|proj-)?[A-Za-z0-9_-]{20,}")),
        ),
        (
            "a GitHub token",
            Check::Pattern(static_regex!(
                r"(?-u:\b)(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,})"
            )),
        ),
        (
            "a Slack token",
            Check::Pattern(static_regex!(r"(?-u:\b)xox[abposr]-[A-Za-z0-9-]{10,}")),
        ),
        (
            "an AWS key",
            Check::Pattern(static_regex!(r"(?-u:\b)AKIA[0-9A-Z]{16}(?-u:\b)")),
        ),
        (
            "a GitLab token",
            Check::Pattern(static_regex!(r"(?-u:\b)glpat-[A-Za-z0-9_-]{20,}")),
        ),
        (
            "an npm token",
            Check::Pattern(static_regex!(r"(?-u:\b)npm_[A-Za-z0-9]{36}(?-u:\b)")),
        ),
        (
            "a Hugging Face token",
            Check::Pattern(static_regex!(r"(?-u:\b)hf_[A-Za-z0-9]{30,}(?-u:\b)")),
        ),
        (
            "a Google API key",
            Check::Pattern(static_regex!(r"(?-u:\b)AIza[0-9A-Za-z_-]{35}")),
        ),
        ("a JSON web token", Check::Jwt),
        (
            "a bearer token",
            Check::Pattern(static_regex!(r"(?i)(?-u:\b)Bearer\s+[A-Za-z0-9._~+/\-]{20,}")),
        ),
        (
            "a credential field",
            Check::Pattern(static_regex!(
                r#"(?i)"(?:password|passwd|secret|client_?secret|api_?key|access_?token|refresh_?token|session_?token|cookie)"\s*:\s*"[^"]{6,}""#
            )),
        ),
        (
            "a credential in a URL",
            Check::Pattern(static_regex!(
                r#"(?i)[?&](?:access_token|token|api_key|apikey|key|sig|signature|password|code)=[^&\s"]{8,}"#
            )),
        ),
        ("an email address", Check::Email),
    ]
}

/// The kind of secret a text contains, or none.
pub fn find_secret(text: &str) -> Option<&'static str> {
    for (kind, check) in patterns() {
        let found = match check {
            Check::Pattern(re) => re.is_match(text),
            Check::Jwt => has_json_web_token(text),
            Check::Email => has_email_address(text),
        };
        if found {
            return Some(kind);
        }
    }
    None
}

fn is_email_local(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
}

/// `local@domain.tld`: one local character before an `@` and a domain after it.
fn has_email_address(text: &str) -> bool {
    static DOMAIN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}(?-u:\b)").expect("domain"));
    let chars: Vec<char> = text.chars().collect();
    for (at, c) in chars.iter().enumerate() {
        if *c != '@' || at == 0 || !is_email_local(chars[at - 1]) {
            continue;
        }
        // A domain is at most 253 characters (RFC 1035); the window is 256.
        let window: String = chars[at + 1..usize::min(chars.len(), at + 1 + 256)].iter().collect();
        if DOMAIN.is_match(&window) {
            return true;
        }
    }
    false
}

fn is_jwt_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn find_subsequence(chars: &[char], needle: &[char], from: usize) -> Option<usize> {
    (from..chars.len()).find(|&i| chars.len() - i >= needle.len() && chars[i..i + needle.len()] == *needle)
}

/// `eyJ...` `.eyJ...` `.sig`, found from its `.eyJ`. Each scan stops at the next dot, so it is linear.
fn has_json_web_token(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let dot_eyj = ['.', 'e', 'y', 'J'];
    let eyj = ['e', 'y', 'J'];
    let mut search_from = 0;
    while let Some(dot) = find_subsequence(&chars, &dot_eyj, search_from) {
        search_from = dot + 1;
        // `\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}` anchored at `dot`.
        let mut at = dot + 4;
        let tail_start = at;
        while chars.get(at).is_some_and(|&c| is_jwt_char(c)) {
            at += 1;
        }
        if at - tail_start < 8 || chars.get(at) != Some(&'.') {
            continue;
        }
        let sig_start = at + 1;
        let mut sig_end = sig_start;
        while chars.get(sig_end).is_some_and(|&c| is_jwt_char(c)) {
            sig_end += 1;
        }
        if sig_end - sig_start < 8 {
            continue;
        }
        let mut start = dot;
        while start > 0 && is_jwt_char(chars[start - 1]) {
            start -= 1;
        }
        // The earliest `eyJ` that starts a word leaves the longest head; a later one cannot do better.
        let mut head = find_subsequence(&chars, &eyj, start);
        while let Some(at) = head {
            if at + 3 + 8 > dot {
                break;
            }
            if at == 0 || !is_word_char(chars[at - 1]) {
                return true;
            }
            head = find_subsequence(&chars, &eyj, at + 1);
        }
    }
    false
}

/// A key that names a credential, whose string value is refused (decoded, so `"pass\u0077ord"` counts).
fn is_credential_key(key: &str) -> bool {
    static KEY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:password|passwd|secret|client_?secret|api_?key|access_?token|refresh_?token|session_?token|cookie)$")
            .expect("credential key")
    });
    KEY.is_match(key)
}

fn credential_key_in_text(text: &str) -> bool {
    static KEY_TEXT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)"(?:password|passwd|secret|client_?secret|api_?key|access_?token|refresh_?token|session_?token|cookie)""#)
            .expect("credential key text")
    });
    KEY_TEXT.is_match(text)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The secret check for a parsed JSON file: over the text as written, then over every decoded
/// key and string of the parsed value, so an escape (`"\u0073k-..."`), a newline or tab before
/// a token or JSON text inside a string is seen as the reader would see it. The walk is
/// iterative over an owned copy; any hit refuses the file.
pub fn find_secret_in(source: &str, value: Json) -> Option<String> {
    if let Some(kind) = find_secret(source) {
        return Some(kind.to_string());
    }
    // Each entry: a value, and whether it sits under a credential key.
    let mut stack: Vec<(Json, bool)> = vec![(value, false)];
    let mut work = EmbeddedWork::default();
    while let Some((node, credential)) = stack.pop() {
        match node {
            Json::String(text) => {
                if credential && utf16_len(&text) >= 6 {
                    return Some("a credential field".to_string());
                }
                let found = find_secret(&text).map(str::to_string).or_else(|| {
                    if text.contains('\\') {
                        find_secret(&unescape_text(&text)).map(str::to_string)
                    } else {
                        None
                    }
                });
                if let Some(found) = found {
                    return Some(found);
                }
                match embedded_json(&text, &mut work) {
                    Embedded::Unchecked => return Some("JSON text that could not be checked".to_string()),
                    Embedded::Parsed(inner) => stack.push((inner, credential)),
                    Embedded::NotJson => {}
                }
            }
            Json::Number(number) => {
                if credential && number_digit_count(&number) >= 6 {
                    return Some("a credential field".to_string());
                }
            }
            Json::Array(items) => {
                for item in items {
                    stack.push((item, credential));
                }
            }
            Json::Object(map) => {
                for (key, item) in map {
                    if let Some(kind) = find_secret(&key) {
                        return Some(kind.to_string());
                    }
                    let credential = credential || is_credential_key(&key);
                    stack.push((item, credential));
                }
            }
            Json::Bool(_) | Json::Null => {}
        }
    }
    None
}

/// The digit count of `Math.abs(Math.trunc(node))` as JavaScript would print it, near enough
/// that a six-digit credential value is caught either way.
fn number_digit_count(number: &serde_json::Number) -> usize {
    if let Some(int) = number.as_u64() {
        return int.to_string().len();
    }
    if let Some(int) = number.as_i64() {
        return int.unsigned_abs().to_string().len();
    }
    match number.as_f64() {
        Some(float) => format!("{:.0}", float.trunc().abs()).len(),
        None => 0,
    }
}

/// How many characters of JSON text inside strings are parsed per file, and how many such parses may fail.
const EMBEDDED_BUDGET: usize = 4 * 1024 * 1024;
const EMBEDDED_FAILURES: usize = 256;

#[derive(Default)]
struct EmbeddedWork {
    used: usize,
    failed: usize,
}

enum Embedded {
    NotJson,
    /// JSON text that can't be checked within the budget: the file is refused, not read unchecked.
    Unchecked,
    Parsed(Json),
}

/// A string that is itself JSON (an object, an array or a string, as double-encoded text is),
/// parsed; only text whose last character closes its first is parsed, and after
/// EMBEDDED_FAILURES failed parses no more are tried (each failure costs, in bulk).
fn embedded_json(text: &str, work: &mut EmbeddedWork) -> Embedded {
    let trimmed = text.trim();
    let mut chars = trimmed.chars();
    let (Some(open), Some(close)) = (chars.next(), trimmed.chars().last()) else {
        return Embedded::NotJson;
    };
    let shaped = trimmed.chars().count() >= 2
        && ((open == '{' && close == '}') || (open == '[' && close == ']') || (open == '"' && close == '"'));
    if !shaped {
        return Embedded::NotJson;
    }
    let len = utf16_len(text);
    // Past the failure cap or the size budget, text that names a credential key can't be read unchecked.
    if work.failed >= EMBEDDED_FAILURES || work.used + len > EMBEDDED_BUDGET {
        return if credential_key_in_text(&unescape_text(text)) || work.used + len > EMBEDDED_BUDGET {
            Embedded::Unchecked
        } else {
            Embedded::NotJson
        };
    }
    match serde_json::from_str::<Json>(trimmed) {
        Ok(parsed) => {
            work.used += len;
            Embedded::Parsed(parsed)
        }
        Err(_) => {
            work.failed += 1;
            Embedded::NotJson
        }
    }
}

/// JSON text inside a string that doesn't parse, read without parsing: escapes of any depth of
/// backslashes become what they stand for, so a token after an escaped newline or a credential
/// key inside it is seen. Linear: one scan per pass and at most three passes.
fn unescape_text(text: &str) -> String {
    let mut out = text.to_string();
    for _ in 0..3 {
        if !out.contains('\\') {
            break;
        }
        let next = unescape_once(&out);
        if next == out {
            break;
        }
        out = next;
    }
    out
}

fn unescape_once(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut parts = String::new();
    let mut i = 0;
    while i < chars.len() {
        let Some(slash) = (i..chars.len()).find(|&at| chars[at] == '\\') else {
            parts.extend(&chars[i..]);
            break;
        };
        parts.extend(&chars[i..slash]);
        let mut j = slash;
        while chars.get(j) == Some(&'\\') {
            j += 1;
        }
        let next = chars.get(j).copied();
        let hex: Option<String> = (j + 4 < chars.len()).then(|| chars[j + 1..j + 5].iter().collect());
        match next {
            Some(c) if "nrtbf".contains(c) => {
                parts.push(' ');
                i = j + 1;
            }
            Some(c @ ('"' | '/')) => {
                parts.push(c);
                i = j + 1;
            }
            Some('u') if hex.as_deref().is_some_and(|h| h.chars().all(|c| c.is_ascii_hexdigit())) => {
                let code = u32::from_str_radix(hex.as_deref().unwrap_or(""), 16).unwrap_or(0x20);
                // A lone surrogate in JavaScript's read of the escape cannot be a Rust char;
                // it cannot take part in any (ASCII) token either, so a space stands for it.
                parts.push(char::from_u32(code).unwrap_or(' '));
                i = j + 5;
            }
            _ => {
                parts.extend(&chars[slash..j]);
                i = j;
            }
        }
    }
    parts
}
