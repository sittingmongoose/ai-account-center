//! The report reader's refusal policy at the native persistence boundary. Pattern definitions,
//! credential spellings and work budgets are shared with schema-helpers.ts; usage text retains
//! its older, separately scoped policy. Nothing here returns matched credential text.
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::Value as Json;
use std::borrow::Cow;
use std::sync::OnceLock;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Policy {
    patterns: Vec<Pattern>,
    credential_key: String,
    credential_min: usize,
    embedded_budget: usize,
    embedded_failures: usize,
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
}
struct Rules {
    policy: Policy,
    patterns: Vec<Regex>,
    credential_key: Regex,
    credential_key_text: Regex,
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
            serde_json::from_str(include_str!("../../../src/web-server/status/report-secret-policy.json"))
                .expect("checked shared report policy");
        let patterns = policy
            .patterns
            .iter()
            .map(|pattern| {
                let source = if pattern.scan.as_deref() == Some("jwt") {
                    format!(
                        r"\b{p}{c}{{{n},}}\.{p}{c}{{{n},}}\.{c}{{{n},}}",
                        p = regex::escape(&policy.jwt.prefix),
                        c = policy.jwt.char,
                        n = policy.jwt.min
                    )
                } else {
                    pattern.pattern.clone().unwrap_or_default()
                };
                regex(&source, pattern.flags.as_deref() == Some("i"), &policy.whitespace)
            })
            .collect();
        Rules {
            credential_key: regex(&format!("^{}$", policy.credential_key), true, &policy.whitespace),
            credential_key_text: regex(&format!("\"{}\"", policy.credential_key), true, &policy.whitespace),
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
        } else {
            matcher.is_match(&text)
        };
        if found {
            return Some(&pattern.kind);
        }
    }
    None
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
    let mut failed = 0;
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
                let shaped = trimmed.len() >= 2
                    && matches!(
                        (trimmed.as_bytes().first(), trimmed.as_bytes().last()),
                        (Some(b'{'), Some(b'}')) | (Some(b'['), Some(b']')) | (Some(b'"'), Some(b'"'))
                    );
                if !shaped {
                    continue;
                }
                if failed >= rules.policy.embedded_failures || used + length > rules.policy.embedded_budget {
                    if used + length > rules.policy.embedded_budget
                        || rules.credential_key_text.is_match(&regex_text(&unescape_text(&text)))
                    {
                        return Some("JSON text that could not be checked");
                    }
                    continue;
                }
                match serde_json::from_str(&trimmed) {
                    Ok(inner) => {
                        used += length;
                        stack.push((inner, credential));
                    }
                    Err(error) => {
                        // serde's recursion guard is stricter than JSON.parse. Never let that
                        // implementation limit turn valid embedded JSON into unchecked content.
                        if error.to_string().contains("recursion limit") {
                            return Some("JSON text that could not be checked");
                        }
                        failed += 1;
                    }
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
