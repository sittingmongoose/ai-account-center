//! The dashboard sampler's observation projection, shared by the native collector and store route.
use aac_core::{AccountKey, ProviderId};
use aac_store::usage::{AccountObs, Sampled, WindowObs};
use aac_views::decline::Decline;
use serde_json::{Value, json};

const ACCOUNTS: usize = 64;
const WINDOWS: usize = 256;

fn string(value: &Value, max: usize) -> Option<&str> {
    let word = value.as_str()?;
    (!word.is_empty()
        && word.encode_utf16().count() <= max
        && !word.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}'))
    .then_some(word)
}

fn timestamp(value: &Value) -> Result<Option<i64>, Decline> {
    let Some(text) = value.as_str() else {
        return Ok(None);
    };
    let prefix = text.get(..11).unwrap_or("").as_bytes();
    if prefix.len() != 11
        || prefix[4] != b'-'
        || prefix[7] != b'-'
        || prefix[10] != b'T'
        || ![&prefix[..4], &prefix[5..7], &prefix[8..10]]
            .iter()
            .all(|part| part.iter().all(u8::is_ascii_digit))
    {
        return Ok(None);
    }
    aac_views::js::parse_date(text).map_err(|_| Decline::Date)
}

fn number(value: &Value, percent: bool) -> Option<f64> {
    value
        .as_f64()
        .filter(|val| val.is_finite() && (!percent || *val >= 0.0))
}

fn public_key(value: &Value, limit: usize) -> Option<String> {
    let word = string(value, limit).filter(|s| s.is_ascii())?;
    let mut chars = word.bytes();
    if !chars.next().is_some_and(|b| b.is_ascii_alphanumeric())
        || !chars.all(|b| b.is_ascii_alphanumeric() || b"_.:/@-".contains(&b))
        || aac_views::antigravity::display_text(Some(value), limit).is_none()
    {
        return None;
    }
    Some(word.into())
}

fn window(value: &Value, now: i64, cutoff: i64) -> Result<Option<WindowObs>, Decline> {
    let (Some(key), Some(label)) = (string(&value["key"], 64), string(&value["label"], 80)) else {
        return Ok(None);
    };
    if let Some(clears) = value.get("clears") {
        let Some(clears) = clears.as_array().filter(|rows| rows.len() <= 8) else {
            return Ok(None);
        };
        let mut slugs = std::collections::BTreeSet::new();
        for clear in clears {
            let Some(clear) = clear.as_str().filter(|s| {
                (1..=40).contains(&s.len())
                    && s.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            }) else {
                return Ok(None);
            };
            if !slugs.insert(clear) {
                return Ok(None);
            }
        }
    }
    let cached = if value["status"] == "cached" {
        let Some(at) = timestamp(&value["sampledAt"])?.filter(|at| (cutoff..=now).contains(at))
        else {
            return Ok(None);
        };
        Some(at)
    } else {
        None
    };
    let kind = value["kind"]
        .as_str()
        .filter(|kind| ["rate_limit", "balance", "spend", "extra_usage"].contains(kind));
    let unit = string(&value["unit"], 40);
    let pool = public_key(&value["poolId"], 128);
    let mut identity = vec![json!(key), json!(kind), json!(unit)];
    if let Some(pool) = &pool {
        identity.push(json!(pool));
    }
    let pool_label = aac_views::antigravity::display_text(value.get("poolLabel"), 80);
    Ok(Some(WindowObs {
        identity: Value::Array(identity).to_string(),
        key: key.into(),
        label: label.into(),
        kind: kind.map(str::to_owned),
        unit: unit.map(str::to_owned),
        used_pct: number(&value["usedPercent"], true),
        remaining_pct: number(&value["remainingPercent"], true),
        used: number(&value["used"], false),
        limit: number(&value["limit"], false),
        remaining: number(&value["remaining"], false),
        reset_at_ms: timestamp(&value["resetAt"])?,
        expires_at_ms: timestamp(&value["expiresAt"])?,
        window_minutes: number(&value["windowMinutes"], false),
        unlimited: value["unlimited"].as_bool(),
        enabled: value["enabled"].as_bool(),
        cached_sampled_at_ms: cached,
        pool_id: pool,
        pool_id_source: value["poolIdSource"]
            .as_str()
            .filter(|source| ["provider-id", "provider-bucket-membership"].contains(source))
            .map(str::to_owned),
        pool_label,
        model_ids: value["modelIds"].as_array().map(|rows| {
            rows.iter()
                .take(64)
                .filter_map(|row| public_key(row, 160))
                .collect()
        }),
    }))
}

fn platform<'a>(value: &'a Value, source: &str) -> Option<&'a str> {
    // Source labels are only a fallback when the retained host is absent.
    match value["host"].as_str() {
        Some("mac") => Some("mac"),
        Some("windows" | "alexis") => Some("windows"),
        Some("ubuntu" | "nas1") => Some("ubuntu"),
        _ if source.contains("on Windows") => Some("windows"),
        _ if source.contains("on Mac") => Some("mac"),
        _ => value["platform"].as_str(),
    }
}

pub(crate) fn identity(value: &Value) -> Result<AccountKey, Decline> {
    let provider = value["provider"].as_str().ok_or(Decline::Shape)?;
    let provider_id = ProviderId::new(provider).map_err(|_| Decline::Shape)?;
    let id = value["id"].as_str().ok_or(Decline::Shape)?;
    let email = match &value["email"] {
        Value::Null => None,
        Value::String(email) if email.is_ascii() => Some(email.as_str()),
        _ => return Err(Decline::Shape),
    };
    AccountKey::derive(&provider_id, id, email).map_err(|_| Decline::Shape)
}

fn observation(value: &Value, now: i64, cutoff: i64) -> Result<Option<Sampled>, Decline> {
    let identity = identity(value)?;
    let provider = value["provider"].as_str().ok_or(Decline::Shape)?;
    let id = value["id"].as_str().ok_or(Decline::Shape)?;
    let status = value["status"].as_str().ok_or(Decline::Shape)?;
    let source = value["source"].as_str().ok_or(Decline::Shape)?;
    let platform = platform(value, source).ok_or(Decline::Shape)?;
    let healthy = status == "ok" || status == "cached";
    let sampled = if healthy {
        match timestamp(&value["sampledAt"])? {
            Some(at) => Some(at),
            None => timestamp(&value["fetchedAt"])?,
        }
    } else {
        Some(now)
    };
    let Some(sampled_at_ms) = sampled.filter(|at| (cutoff..=now).contains(at)) else {
        return Ok(None);
    };
    let mut windows = Vec::new();
    if healthy {
        for row in value["windows"]
            .as_array()
            .ok_or(Decline::Shape)?
            .iter()
            .take(WINDOWS)
        {
            if let Some(window) = window(row, now, cutoff)? {
                windows.push(window);
            }
        }
    }
    // The store's UTF-8 admission must also hold after JS's UTF-16 truncation.
    fn truncate(s: &str, max: usize) -> Result<String, Decline> {
        if !s.is_ascii() && s.encode_utf16().count() > max {
            return Err(Decline::Shape);
        }
        Ok(s.chars().take(max).collect())
    }
    let item = Sampled {
        account: AccountObs {
            identity: identity.as_str().into(),
            provider: provider.into(),
            account_id: truncate(id, 128)?,
            plan: value["plan"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| truncate(s, 80))
                .transpose()?,
            platform: platform.into(),
            source: truncate(source, 120)?,
            status: status.into(),
            is_active: value["isActive"].as_bool().ok_or(Decline::Shape)?,
        },
        sampled_at_ms,
        observed_at_ms: now,
        windows,
        import_ref: None,
    };
    item.validate().map_err(|_| Decline::Shape)?;
    Ok(Some(item))
}

/// Store-facing observations from normalized dashboard accounts, with the sampler's bounds.
/// A caller can delegate if a date requires the TypeScript engine's legacy parser.
pub fn observations(rows: &[Value], now: i64, cutoff: i64) -> Result<Vec<Sampled>, Decline> {
    let mut observations = Vec::new();
    for row in rows.iter().take(ACCOUNTS) {
        if let Some(item) = observation(row, now, cutoff)? {
            observations.push(item);
        }
    }
    Ok(observations)
}
