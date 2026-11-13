//! Small helpers: digests, hour labels, base64url, canonical JSON.

use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// "YYYY-MM-DD HH:00" in UTC for an epoch-millisecond time, as the scanner labels hours.
pub fn hour_label(epoch_ms: i64) -> Option<String> {
    if epoch_ms < 0 {
        return None;
    }
    let secs = epoch_ms / 1000;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Some(format!("{year:04}-{month:02}-{day:02} {:02}:00", rem / 3600))
}

/// The epoch milliseconds an hour label starts at.
pub fn hour_epoch(label: &str) -> Option<i64> {
    let bytes = label.as_bytes();
    if bytes.len() != 16 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b' ' || &label[13..] != ":00" {
        return None;
    }
    let number = |range: std::ops::Range<usize>| label.get(range)?.parse::<i64>().ok();
    let (year, month, day, hour) = (number(0..4)?, number(5..7)?, number(8..10)?, number(11..13)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || !(0..24).contains(&hour) {
        return None;
    }
    // Days from civil (the inverse of hour_label's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + hour * 3600) * 1000)
}

/// Unpadded base64url, as a JWT's segments are written; None on any other character.
pub fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        } as u32)
    };
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        buffer = (buffer << 6) | value(byte)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// JSON with object keys sorted at every level: equal events print equal (page-boundary copies).
pub fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|key| format!("{}:{}", Value::String(key.clone()), canonical(&map[key])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => format!("[{}]", items.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

/// A Cursor model name compared without its `cursor-` prefix and effort, `-thinking` or `-fast`
/// suffix (T3's `cursorRateModel`), lower case. Only for matching: rows keep their own spelling.
pub fn match_model(model: &str) -> String {
    let mut name = model.trim().to_ascii_lowercase();
    if let Some(rest) = name.strip_prefix("cursor-") {
        name = rest.to_string();
    }
    if let Some(rest) = name.strip_suffix("-fast") {
        name = rest.to_string();
    }
    for effort in ["-none", "-minimal", "-low", "-medium", "-high", "-xhigh", "-max"] {
        if let Some(rest) = name.strip_suffix(effort) {
            name = rest.to_string();
            break;
        }
    }
    if let Some(rest) = name.strip_suffix("-thinking") {
        name = rest.to_string();
    }
    name
}
