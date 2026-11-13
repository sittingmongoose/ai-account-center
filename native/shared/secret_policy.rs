//! The report reader's refusal policy at the native persistence boundary. Pattern definitions,
//! credential spellings and work budgets are shared with schema-helpers.ts; usage text retains
//! its older, separately scoped policy. Nothing here returns matched credential text.
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::Value as Json;
use std::borrow::Cow;
use std::sync::OnceLock;

/// Stable packaged resource, shared by every TS and Rust consumer.
pub const SECRET_POLICY_PATH: &str = "src/shared/secret-policy.json";

/// Immutable process-wide policy data. This module can be included by any native crate
/// through `#[path = "../../shared/secret_policy.rs"]`; it has no store/Status dependencies.
pub fn load_secret_policy() -> &'static Json {
    static POLICY: OnceLock<Json> = OnceLock::new();
    POLICY.get_or_init(|| {
        serde_json::from_str(include_str!("../../src/shared/secret-policy.json"))
            .expect("checked packaged secret policy")
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Policy {
    patterns: Vec<Pattern>,
    credential_key: String,
    refused_key: String,
    authorization: Authorization,
    credential_min: usize,
    embedded_budget: usize,
    email: Email,
    jwt: Jwt,
    whitespace: String,
}
#[derive(Deserialize)]
struct Pattern {
    kind: String,
    pattern: Option<String>,
    flags: Option<String>,
    scan: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Email {
    local_char: String,
    domain: String,
    domain_window: usize,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Jwt {
    prefix: String,
    min: usize,
    char: String,
    signature_min: usize,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Authorization {
    pattern: String,
    legacy_min: usize,
    opaque_min: usize,
    basic_min: usize,
    digit: String,
    upper: String,
    lower: String,
    base64: String,
}
struct Rules {
    policy: Policy,
    patterns: Vec<Regex>,
    credential_key: Regex,
    refused_key: Regex,
    authorization: Regex,
    auth_digit: Regex,
    auth_upper: Regex,
    auth_lower: Regex,
    auth_base64: Regex,
    email_local: Regex,
    email_domain: Regex,
    trim: Regex,
}

fn regex(source: &str, insensitive: bool, whitespace: &str) -> Regex {
    // JS word boundaries are ASCII. Its whitespace class includes these Unicode spaces.
    let source = source.replace(r"\b", r"(?-u:\b)").replace(r"\s", whitespace);
    RegexBuilder::new(&source)
        .case_insensitive(insensitive)
        .build()
        .expect("checked report policy")
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let policy: Policy =
            serde_json::from_value(load_secret_policy().clone()).expect("checked shared secret policy");
        let patterns = policy
            .patterns
            .iter()
            .map(|pattern| {
                let source = if pattern.scan.as_deref() == Some("jwt") {
                    format!(
                        r"\b{p}{c}{{{n},}}\.{p}{c}{{{n},}}\.{c}{{{sig},}}",
                        p = regex::escape(&policy.jwt.prefix),
                        c = policy.jwt.char,
                        n = policy.jwt.min,
                        sig = policy.jwt.signature_min
                    )
                } else {
                    pattern.pattern.clone().unwrap_or_default()
                };
                regex(&source, pattern.flags.as_deref() == Some("i"), &policy.whitespace)
            })
            .collect();
        Rules {
            refused_key: regex(&format!("^{}$", policy.refused_key), true, &policy.whitespace),
            authorization: regex(&policy.authorization.pattern, true, &policy.whitespace),
            auth_digit: regex(&policy.authorization.digit, false, &policy.whitespace),
            auth_upper: regex(&policy.authorization.upper, false, &policy.whitespace),
            auth_lower: regex(&policy.authorization.lower, false, &policy.whitespace),
            auth_base64: regex(&policy.authorization.base64, false, &policy.whitespace),
            credential_key: regex(&format!("^{}$", policy.credential_key), true, &policy.whitespace),
            email_local: regex(&policy.email.local_char, false, &policy.whitespace),
            email_domain: regex(&policy.email.domain, false, &policy.whitespace),
            trim: regex(
                &format!("^{}+|{}+$", policy.whitespace, policy.whitespace),
                false,
                &policy.whitespace,
            ),
            policy,
            patterns,
        }
    })
}

/// The shared patterns have ASCII literals/classes, JS whitespace and negated classes.
/// JS regexes without `u` count UTF-16 units and never fold non-ASCII letters into ASCII.
/// Preserve whitespace and represent other non-ASCII units by a non-folding placeholder.
fn regex_text(text: &str) -> Cow<'_, str> {
    if text.is_ascii() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii()
            || matches!(
                c,
                '\u{a0}' | '\u{1680}' | '\u{2000}'
                    ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
            )
        {
            out.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                out.push('\u{fffd}');
            }
        }
    }
    Cow::Owned(out)
}

pub fn find_secret(text: &str) -> Option<&'static str> {
    let rules = rules();
    let text = regex_text(text);
    for (pattern, matcher) in rules.policy.patterns.iter().zip(&rules.patterns) {
        let found = if pattern.scan.as_deref() == Some("email") {
            text.match_indices('@').any(|(at, _)| {
                if at == 0 || !text.as_bytes()[at - 1].is_ascii() || !rules.email_local.is_match(&text[at - 1..at]) {
                    return false;
                }
                let domain = &text[at + 1..];
                let end = domain
                    .char_indices()
                    .nth(rules.policy.email.domain_window)
                    .map_or(domain.len(), |(index, _)| index);
                rules.email_domain.is_match(&domain[..end])
            })
        } else if pattern.scan.as_deref() == Some("authorization") {
            has_authorization(&text, rules)
        } else {
            matcher.is_match(&text)
        };
        if found {
            return Some(&pattern.kind);
        }
    }
    None
}

/// Test canonical Base64 without retaining or logging decoded credential material.
fn basic_has_colon(value: &str) -> bool {
    let value = value.trim_end_matches('=');
    if value.len() % 4 == 1 {
        return false;
    }
    let mut bits = 0u16;
    let mut pending = 0;
    let mut colon = false;
    for c in value.bytes() {
        let digit = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return false,
        };
        bits = (bits << 6) | u16::from(digit);
        pending += 6;
        if pending >= 8 {
            pending -= 8;
            colon |= ((bits >> pending) & 255) == 0x3a;
        }
        bits &= (1 << pending) - 1;
    }
    colon && bits == 0
}

fn has_authorization(text: &str, rules: &Rules) -> bool {
    let policy = &rules.policy.authorization;
    rules.authorization.captures_iter(text).any(|captures| {
        let value = &captures[2];
        value.len() >= policy.opaque_min
            || (value.len() >= policy.legacy_min
                && (rules.auth_digit.is_match(value)
                    || (rules.auth_upper.is_match(value) && rules.auth_lower.is_match(value))))
            || (captures[1].eq_ignore_ascii_case("basic")
                && value.len() >= policy.basic_min
                && rules.auth_base64.is_match(value)
                && basic_has_colon(value))
    })
}

/// Same three-pass unescape of text as the reader, including repeated backslashes and Unicode
/// escapes that produce another escape. Work is linear in the bounded input size.
fn unescape_text(text: &str) -> String {
    let mut out = text.to_owned();
    for _ in 0..3 {
        if !out.contains('\\') {
            break;
        }
        let mut next = String::new();
        let mut at = 0;
        while let Some(relative) = out[at..].find('\\') {
            let slash = at + relative;
            next.push_str(&out[at..slash]);
            let mut end = slash;
            while out.as_bytes().get(end) == Some(&b'\\') {
                end += 1;
            }
            match out.as_bytes().get(end).copied() {
                Some(b'n' | b'r' | b't' | b'b' | b'f') => {
                    next.push(' ');
                    at = end + 1;
                }
                Some(b'"' | b'/') => {
                    next.push(out.as_bytes()[end] as char);
                    at = end + 1;
                }
                Some(b'u')
                    if out
                        .as_bytes()
                        .get(end + 1..end + 5)
                        .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit)) =>
                {
                    let code = u32::from_str_radix(&out[end + 1..end + 5], 16).unwrap();
                    next.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    at = end + 5;
                }
                _ => {
                    next.push_str(&out[slash..end]);
                    at = end;
                }
            }
        }
        next.push_str(&out[at..]);
        if next == out {
            break;
        }
        out = next;
    }
    out
}

fn numeric_credential(number: &serde_json::Number, min: usize) -> bool {
    let value = number.as_f64().unwrap_or(0.0).abs().trunc();
    // JSON.stringify's number representation is scientific at 1e21. Match the reader's
    // String(Math.abs(Math.trunc(node))) length rather than the original numeric spelling.
    let text = if value >= 1e21 {
        serde_json::to_string(&value).unwrap_or_default().replace(".0e", "e")
    } else {
        format!("{value:.0}")
    };
    text.len() >= min
}

/// Check decoded keys and strings as well as raw source, then any embedded JSON strings. The
/// iterative walk also propagates credential-key refusal through objects and arrays.
pub fn find_secret_in(source: &str, value: &Json) -> Option<&'static str> {
    if let Some(found) = find_secret(source) {
        return Some(found);
    }
    let rules = rules();
    let mut stack = vec![(value.clone(), false)];
    let mut used = 0;
    while let Some((node, credential)) = stack.pop() {
        match node {
            Json::String(text) => {
                let length = text.encode_utf16().count();
                if credential && length >= rules.policy.credential_min {
                    return Some("a credential field");
                }
                if let Some(found) = find_secret(&text).or_else(|| {
                    text.contains('\\')
                        .then(|| find_secret(&unescape_text(&text)))
                        .flatten()
                }) {
                    return Some(found);
                }
                let trimmed = rules.trim.replace_all(&text, "");
                let shaped = matches!(trimmed.as_bytes().first(), Some(b'{' | b'['))
                    || (trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"'));
                if !shaped {
                    continue;
                }
                if used + length > rules.policy.embedded_budget {
                    return Some("JSON text that could not be checked");
                }
                match serde_json::from_str(&trimmed) {
                    Ok(inner) => {
                        used += length;
                        stack.push((inner, credential));
                    }
                    Err(_) => return Some("unparseable embedded JSON"),
                }
            }
            Json::Number(number) => {
                if credential && numeric_credential(&number, rules.policy.credential_min) {
                    return Some("a credential field");
                }
            }
            Json::Array(values) => stack.extend(values.into_iter().map(|value| (value, credential))),
            Json::Object(values) => {
                for (key, value) in values {
                    if rules.refused_key.is_match(&regex_text(&key)) {
                        return Some("a credential field");
                    }
                    if let Some(found) = find_secret(&key) {
                        return Some(found);
                    }
                    let credential = credential || rules.credential_key.is_match(&regex_text(&key));
                    stack.push((value, credential));
                }
            }
            _ => {}
        }
    }
    None
}
