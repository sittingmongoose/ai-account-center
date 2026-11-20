//! Port of `src/services/logging/log-redaction.ts` and the URL scrub of
//! `src/web-server/middleware/request-logging-middleware.ts`. The JavaScript `\s` class is
//! spelled out (ECMAScript WhiteSpace and LineTerminator) so the patterns match exactly.
use regex::{Captures, Regex};
use serde_json::{Map, Value};
use std::sync::LazyLock;

/// ECMAScript `\s`.
const WS: &str = r"\t\n\x0B\x0C\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}";

fn regex(pattern: &str) -> Regex {
    // The patterns are fixed source text checked by the unit tests; an invalid one is a bug.
    #[allow(clippy::unwrap_used)]
    Regex::new(pattern).unwrap()
}

static SENSITIVE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)^(authorization|proxy[_-]?authorization|cookie|set-cookie|password|password_hash|secret|client[_-]?secret|token|auth[_-]?token|access[_-]?token|refresh[_-]?token|id[_-]?token|bearer|assertion|api[_-]?key|x[_-]?api[_-]?key|x[_-]?goog[_-]?api[_-]?key|management[_-]?key|copilot[_-]?token|cursor[_-]?session[_-]?key|oauth[_-]?code|auth[_-]?code|current[_-]?password|new[_-]?password|device[_-]?token|setup[_-]?code|session[_-]?id|sid|token[_-]?sha256|prev[_-]?token[_-]?sha256)$",
    )
});
static SENSITIVE_KEY_CONTAINS: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)(password|passwd|secret|setup[_-]?code)"));
static ARGV_FLAG: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)^--(token|api[_-]?key|auth|auth[_-]?token|secret|bearer|password|client[_-]?secret|refresh[_-]?token|access[_-]?token|id[_-]?token|prompt)$",
    )
});
static ARGV_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)^--(token|api[_-]?key|auth|auth[_-]?token|secret|bearer|password|client[_-]?secret|refresh[_-]?token|access[_-]?token|id[_-]?token|prompt)=",
    )
});
static AUTH_SCHEME_VALUE: LazyLock<Regex> =
    LazyLock::new(|| regex(&format!(r"^(Bearer|Basic|Token)[{WS}]+[^{WS}]+")));
static SECRET_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"(?:Bearer|Basic|Token)[{WS}]+[^{WS}]{{8,}}|aacd_[A-Za-z0-9_-]{{16,}}|sk-ant-[A-Za-z0-9_-]{{16,}}|sk-[A-Za-z0-9_-]{{32,}}|xox[bpoa]-[A-Za-z0-9-]{{10,}}|gh[opsu]_[A-Za-z0-9]{{36,}}|glpat-[A-Za-z0-9_-]{{18,}}|AIza[0-9A-Za-z_-]{{35}}|eyJ[A-Za-z0-9_-]{{8,}}\.eyJ[A-Za-z0-9_-]{{8,}}\.[A-Za-z0-9_-]*|(?:api[_-]?key|access[_-]?token|refresh[_-]?token|secret)(?:=|%3D)[A-Za-z0-9._~+/=-]{{8,}}"
    ))
});
static SCHEME_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| regex(&format!(r"^(Bearer|Basic|Token)[{WS}]+")));
static URL_USERINFO: LazyLock<Regex> =
    LazyLock::new(|| regex(&format!(r"(?i)([a-z][a-z0-9+.-]*://)([^{WS}/@]+)@")));
static TOKEN_IN_URL: LazyLock<Regex> = LazyLock::new(|| regex(r"(aac[du])_[A-Za-z0-9_%-]{8,}"));
static RESET_URL: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?is)^/api/accounts/.*/reset-claims(?:[/?]|$)"));
static ASCII_ESCAPE: LazyLock<Regex> = LazyLock::new(|| regex(r"%([0-7][0-9A-Fa-f])"));

const MAX_STRING_LENGTH: usize = 2000;
const MAX_DEPTH: usize = 5;

fn sensitive_key(key: &str) -> bool {
    SENSITIVE_KEY.is_match(key) || SENSITIVE_KEY_CONTAINS.is_match(key)
}

/// `value.slice(0, 2000) + '...[truncated]'` past 2000 UTF-16 code units.
fn truncate(value: String) -> String {
    if value.encode_utf16().count() <= MAX_STRING_LENGTH {
        return value;
    }
    let mut units = 0;
    let mut end = 0;
    for (index, c) in value.char_indices() {
        if units + c.len_utf16() > MAX_STRING_LENGTH {
            break;
        }
        units += c.len_utf16();
        end = index + c.len_utf8();
    }
    format!("{}...[truncated]", &value[..end])
}

fn mask_auth_scheme(value: &str) -> String {
    match AUTH_SCHEME_VALUE.captures(value) {
        Some(found) => format!("{} [redacted]", &found[1]),
        None => value.to_owned(),
    }
}

/// `maskSecretTokens`: credential shapes anywhere in a string; the auth scheme stays readable.
pub fn mask_secret_tokens(value: &str) -> String {
    let userinfo = URL_USERINFO.replace_all(value, "${1}[redacted]@");
    SECRET_TOKEN
        .replace_all(&userinfo, |found: &Captures<'_>| {
            match SCHEME_PREFIX.captures(&found[0]) {
                Some(scheme) => format!("{} [redacted]", &scheme[1]),
                None => "[redacted]".to_owned(),
            }
        })
        .into_owned()
}

fn sanitize(value: &Value, depth: usize) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    if depth >= MAX_DEPTH {
        return Value::String("[max-depth]".to_owned());
    }
    match value {
        Value::String(text) => Value::String(truncate(mask_secret_tokens(&mask_auth_scheme(text)))),
        Value::Number(_) | Value::Bool(_) => value.clone(),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| sanitize(item, depth + 1)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, nested)| {
                    let clean = if sensitive_key(key) {
                        Value::String("[redacted]".to_owned())
                    } else {
                        sanitize(nested, depth + 1)
                    };
                    (key.clone(), clean)
                })
                .collect::<Map<String, Value>>(),
        ),
        Value::Null => Value::Null,
    }
}

/// `redactContext`: sensitive keys replaced, strings scrubbed and bounded, depth capped at 5.
pub fn redact_context(context: &Map<String, Value>) -> Map<String, Value> {
    match sanitize(&Value::Object(context.clone()), 0) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// `redactErrorInfo`.
pub fn redact_value(value: &Value) -> Value {
    sanitize(value, 0)
}

/// `redactArgv`: the argument after a sensitive flag, and the value of `--flag=value`.
pub fn redact_argv(argv: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        if ARGV_ASSIGNMENT.is_match(arg) {
            // The pattern ends in `=`, so there is one; it is ASCII, so the slice is a boundary.
            let separator = arg.find('=').unwrap_or(0);
            out.push(format!("{}[redacted]", &arg[..=separator]));
            index += 1;
            continue;
        }
        out.push(arg.clone());
        if (ARGV_FLAG.is_match(arg) || arg == "-p") && index + 1 < argv.len() {
            out.push("[redacted]".to_owned());
            index += 1;
        }
        index += 1;
    }
    out
}

/// Percent-decodes ASCII escapes only; never fails on a malformed sequence.
fn decode_ascii_escapes(value: &str) -> String {
    ASCII_ESCAPE
        .replace_all(value, |found: &Captures<'_>| {
            u8::from_str_radix(&found[1], 16)
                .map(|byte| char::from(byte).to_string())
                .unwrap_or_else(|_| found[0].to_owned())
        })
        .into_owned()
}

/// `scrubLoggedUrl`: reset-claims URLs collapse to one path; device tokens (`aacd_`) and usage
/// hub keys (`aacu_`) keep their prefix, also when percent-encoded (up to three times).
pub fn scrub_logged_url(url: &str) -> String {
    let mut reset = url.to_owned();
    for _ in 0..3 {
        reset = decode_ascii_escapes(&reset);
    }
    if RESET_URL.is_match(&reset) {
        return "/api/accounts/[account]/reset-claims".to_owned();
    }
    let scrubbed = TOKEN_IN_URL
        .replace_all(url, "${1}_[redacted]")
        .into_owned();
    let mut decoded = scrubbed.clone();
    for _ in 0..3 {
        let next = decode_ascii_escapes(&decoded);
        if next == decoded {
            break;
        }
        decoded = next;
    }
    if decoded == scrubbed {
        return scrubbed;
    }
    let redacted = TOKEN_IN_URL
        .replace_all(&decoded, "${1}_[redacted]")
        .into_owned();
    if redacted == decoded {
        scrubbed
    } else {
        redacted
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn every_pattern_compiles() {
        for pattern in [
            &SENSITIVE_KEY,
            &SENSITIVE_KEY_CONTAINS,
            &ARGV_FLAG,
            &ARGV_ASSIGNMENT,
            &AUTH_SCHEME_VALUE,
            &SECRET_TOKEN,
            &SCHEME_PREFIX,
            &URL_USERINFO,
            &TOKEN_IN_URL,
            &RESET_URL,
            &ASCII_ESCAPE,
        ] {
            assert!(!pattern.as_str().is_empty());
        }
    }
}
