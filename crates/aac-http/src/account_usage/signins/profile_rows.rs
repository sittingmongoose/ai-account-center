//! Sign-in publication rules shared by the native host fanout.
use serde_json::{Map, Value, json};
use std::collections::HashSet;

const ERRORS: &[&str] = &[
    "profile directory resolves outside the allowed roots",
    "credentials file missing or unreadable",
    "expiry times unreadable",
    "keychain item unreadable",
    "keychain item holds no readable credentials",
    "keychain locked or access not allowed",
    "keychain state could not be checked",
    "auth file missing or unreadable",
    "profile discovery was incomplete; this machine's list may be missing homes",
    "sign-in state changed during collection; recheck pending",
];
const CODEX_FIELDS: &[&str] = &[
    "OPENAI_API_KEY",
    "auth_mode",
    "last_refresh",
    "tokens",
    "access_token",
    "refresh_token",
    "id_token",
    "account_id",
    "agent_identity",
    "personal_access_token",
    "bedrock_api_key",
    "bedrock_access_keys",
];

fn control(c: char) -> bool {
    matches!(c,
        '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{61c}' | '\u{200e}' | '\u{200f}' |
        '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'
    )
}

fn clean_name(value: &Value) -> String {
    let raw = value.as_str().unwrap_or("");
    if (1..=64).contains(&raw.len())
        && raw.as_bytes()[0].is_ascii_alphanumeric()
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.@+ -".contains(&b))
    {
        return raw.to_owned();
    }
    let name: String = raw
        .encode_utf16()
        .map(|unit| {
            u8::try_from(unit)
                .ok()
                .filter(|b| b.is_ascii_alphanumeric() || b"_.@+ -".contains(b))
                .map_or('_', char::from)
        })
        .collect();
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "unnamed".into()
    } else {
        trimmed.chars().take(64).collect()
    }
}

fn cut_dir(value: &Value) -> Option<String> {
    let path = value.as_str().filter(|path| !path.chars().any(control))?;
    let mut length = 0;
    Some(
        path.chars()
            .take_while(|c| {
                length += c.len_utf16();
                length <= 200
            })
            .collect(),
    )
}

fn stamp(value: &Value) -> Option<&str> {
    let input = value.as_str()?;
    let bytes = input.as_bytes();
    if !input.is_ascii()
        || !(20..=40).contains(&bytes.len())
        || bytes.get(10) != Some(&b'T')
        || bytes.get(16) != Some(&b':')
        || !(input.ends_with('Z')
            || (matches!(bytes[bytes.len() - 6], b'+' | b'-') && bytes[bytes.len() - 3] == b':'))
    {
        return None;
    }
    // A wall date is checked independently of the supplied offset.
    let wall = format!("{}Z", &input[..19]);
    let instant = aac_views::js::parse_date(&wall).ok().flatten()?;
    if aac_num::ecma::to_iso_string(instant)?.get(..19)? != &input[..19] {
        return None;
    }
    aac_views::js::parse_date(input).ok().flatten()?;
    Some(input)
}

fn row(value: &Value, machine: &str) -> Option<Map<String, Value>> {
    let state = value["state"].as_str().filter(|s| {
        [
            "ok",
            "warn",
            "critical",
            "logged_out",
            "unavailable",
            "unsupported",
        ]
        .contains(s)
    })?;
    let fix = value["fixCommand"]
        .as_str()
        .filter(|text| {
            text.encode_utf16().count() <= 1000
                && !text.chars().any(|c| control(c) || c == '\u{60}')
        })
        .unwrap_or("");
    let provider = if value["provider"] == "codex" {
        "codex"
    } else {
        "claude"
    };
    let mut row = Map::new();
    for (key, value) in [
        ("name", json!(clean_name(&value["name"]))),
        ("provider", json!(provider)),
        ("platform", json!(machine)),
        ("machine", json!(machine)),
    ] {
        row.insert(key.into(), value);
    }
    if let Some(path) = cut_dir(&value["configDir"]) {
        row.insert("configDir".into(), json!(path));
    }
    for key in super::STAMPS {
        row.insert(key.into(), json!(stamp(&value[key])));
    }
    row.insert("state".into(), json!(state));
    row.insert("fixCommand".into(), json!(fix));
    if state == "unavailable"
        && let Some(error) = value["error"].as_str().filter(|s| !s.is_empty())
    {
        row.insert(
            "error".into(),
            json!(if ERRORS.contains(&error) {
                error
            } else {
                "unavailable"
            }),
        );
    }
    if state == "unsupported"
        && let Some(fields) = value["fieldsAvailable"].as_array()
    {
        let fields: Vec<_> = fields
            .iter()
            .filter_map(Value::as_str)
            .filter(|key| CODEX_FIELDS.contains(key))
            .take(32)
            .collect();
        row.insert("fieldsAvailable".into(), json!(fields));
    }
    Some(row)
}

pub(super) fn profiles(reply: &Value, machine: &str) -> Option<Vec<Value>> {
    if reply["schemaVersion"] != 1 {
        return None;
    }
    let rows = reply["profiles"].as_array()?;
    let mut profiles: Vec<_> = rows
        .iter()
        .filter_map(|value| row(value, machine))
        .collect();
    let reserved: HashSet<_> = profiles
        .iter()
        .filter_map(|r| r["name"].as_str())
        .map(str::to_owned)
        .collect();
    let mut used = HashSet::new();
    for row in &mut profiles {
        let name = row["name"].as_str()?;
        if used.insert(name.to_owned()) {
            continue;
        }
        for n in 2..=rows.len().saturating_add(1) {
            let suffix = format!("-{n}");
            let prefix = &name[..name.len().min(64 - suffix.len())];
            let candidate = format!("{prefix}{suffix}");
            if !reserved.contains(&candidate) && used.insert(candidate.clone()) {
                row.insert("name".to_owned(), json!(candidate));
                break;
            }
        }
    }
    Some(profiles.into_iter().map(Value::Object).collect())
}
