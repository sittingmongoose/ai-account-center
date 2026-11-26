//! Private Z.ai response normalization and claim rules. No provider I/O.
use crate::reset::{Grant, Identity, Result, Snapshot};
use crate::reset_inventory::provider_words;
use crate::reset_rules::Rules;
use aac_num::ecma::parse_instant;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

const UNAVAILABLE: &str = "inventory-unavailable";
/// The saved Z.ai key's write authorization awaits the supervised first claim.
pub const CLAIM_PROVEN: bool = false;
pub const METHOD: &str = "zai.customer-package-reset.use";

fn code(data: &Value) -> bool {
    matches!(&data["code"], Value::Null) || [json!(0), json!(200), json!("0"), json!("200")].contains(&data["code"])
}

fn stamp(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_i64().filter(|n| *n > 0) {
        return Some(if n < 100_000_000_000 { n * 1000 } else { n });
    }
    let s = value.as_str()?;
    if s.len() == 19 && s.is_ascii() && s.as_bytes()[10] == b' ' {
        return parse_instant(&format!("{}T{}+08:00", &s[..10], &s[11..]));
    }
    parse_instant(s)
}

fn identifier(value: &Value) -> Option<String> {
    if let Some(n) = value.as_u64() {
        return Some(n.to_string());
    }
    value
        .as_str()
        .filter(|s| {
            !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
        .map(str::to_owned)
}

fn variant(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| {
        !s.is_empty()
            && s.len() <= 48
            && s.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    })
}

/// Complete pack list and the identity headers the provider supplied for it.
pub struct PackList {
    customer: String,
    pub organization: String,
    pub project: String,
    grants: Vec<Grant>,
}

impl PackList {
    pub fn parse(response: &Value) -> Result<Self> {
        if response["success"] != true || !code(response) {
            return Err(UNAVAILABLE);
        }
        let data = &response["data"];
        if data["targetType"] != "PERSONAL" {
            return Err(UNAVAILABLE);
        }
        let mut ids = HashSet::new();
        let mut grants = Vec::new();
        for (field, weekly) in [("weekResets", true), ("fiveHourResets", false)] {
            let rows = data[field]
                .as_array()
                .filter(|rows| rows.len() <= 100)
                .ok_or(UNAVAILABLE)?;
            for row in rows {
                let id = row["recordId"]
                    .as_u64()
                    .filter(|id| ids.insert(*id))
                    .ok_or(UNAVAILABLE)?;
                let available = row["available"].as_bool().ok_or(UNAVAILABLE)?;
                let expiry = &row["expireTime"];
                let expires_at_ms = if expiry.is_null() {
                    None
                } else {
                    Some(stamp(expiry).ok_or(UNAVAILABLE)?)
                };
                let ty = row["resetType"].as_str();
                let known = weekly && (row["resetType"].is_null() || ty == Some("WEEK"));
                grants.push(Grant {
                    id: Some(id.to_string()),
                    kind: if known { "weekly" } else { "unknown" }.into(),
                    clears: if known {
                        vec!["seven_day".into(), "five_hour".into()]
                    } else {
                        vec![]
                    },
                    left: u32::from(available),
                    expires_at_ms,
                    usable: available,
                    words: (!known).then(|| {
                        ty.and_then(provider_words).unwrap_or_else(|| {
                            if weekly {
                                "Weekly reset pack"
                            } else {
                                "5-hour reset pack"
                            }
                            .into()
                        })
                    }),
                    variant: variant(&row["grantType"]).map(str::to_owned),
                });
            }
        }
        Ok(Self {
            customer: identifier(&data["customerId"]).ok_or(UNAVAILABLE)?,
            organization: identifier(&data["organizationId"]).ok_or(UNAVAILABLE)?,
            project: identifier(&data["projectId"]).ok_or(UNAVAILABLE)?,
            grants,
        })
    }

    /// A dashboard-owned key's collection. The caller attaches its Store-issued ticket.
    pub fn snapshot(self, key_id: &str, host: &str, source_binding: String, started_at_ms: i64) -> Snapshot {
        let mut grants = self.grants;
        for g in &mut grants {
            if g.expires_at_ms.is_some_and(|expiry| expiry <= started_at_ms) {
                g.left = 0;
                g.usable = false;
            }
        }
        Snapshot {
            identity: Identity {
                provider: "zai".into(),
                account: self.customer,
                organization: self.organization,
                profile: key_id.into(),
                host: host.into(),
            },
            read_ticket: None,
            observed_at_ms: started_at_ms,
            grants,
            windows: BTreeMap::new(),
            executor_ready: true,
            source_binding,
            account_label: "Z.ai".into(),
            weekly_reset_at_ms: None,
        }
    }
}

/// Preserve the known limits, including the monthly window that a weekly pack cannot clear.
pub fn apply_quota(snapshot: &mut Snapshot, response: &Value) -> Result<()> {
    if !matches!(&response["success"], Value::Bool(true) | Value::Null) || !code(response) {
        return Err(UNAVAILABLE);
    }
    let items = response["data"]["limits"].as_array().ok_or(UNAVAILABLE)?;
    for item in items {
        let key = match (item["type"].as_str(), item["unit"].as_u64(), item["number"].as_u64()) {
            (Some("TOKENS_LIMIT"), Some(3), Some(5)) => "five_hour",
            (Some("TOKENS_LIMIT"), Some(6), Some(1)) => "seven_day",
            (Some("TIME_LIMIT"), _, _) => "monthly_time",
            _ => continue,
        };
        let usage = item["currentValue"]
            .as_f64()
            .zip(item["usage"].as_f64().filter(|n| *n > 0.0))
            .map(|(used, limit)| 100.0 * used / limit)
            .or_else(|| item["percentage"].as_f64())
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or(UNAVAILABLE)?;
        if snapshot.windows.insert(key.into(), usage.min(100.0)).is_some() {
            return Err(UNAVAILABLE);
        }
        if key == "seven_day" {
            snapshot.weekly_reset_at_ms = stamp(&item["nextResetTime"]);
        }
    }
    Ok(())
}

pub(super) struct Zai;

impl Rules for Zai {
    fn kind(&self, g: &Grant) -> Option<(&'static str, &'static str)> {
        (g.kind == "weekly").then_some((
            "Weekly reset pack",
            "Starts a new 7-day week and clears the 5-hour window. The monthly limit stays as it is.",
        ))
    }

    fn blocked(&self, s: &Snapshot, g: &Grant) -> Option<&'static str> {
        match s.windows.get("seven_day") {
            None => return Some("refill-windows-unconfirmed"),
            Some(&0.0) => return Some("no-weekly-usage"),
            _ => {}
        }
        if g.variant.is_none() || g.id.as_deref().and_then(|id| id.parse::<u64>().ok()).is_none() {
            return Some("exact-grant-unavailable");
        }
        (!CLAIM_PROVEN).then_some("claim-unproven")
    }

    fn write(&self, _s: &Snapshot, g: &Grant, attempt: &str) -> Result<(&'static str, Value)> {
        if g.kind != "weekly" {
            return Err("unknown-grant-kind");
        }
        let id =
            g.id.as_deref()
                .and_then(|id| id.parse::<u64>().ok())
                .ok_or("exact-grant-unavailable")?;
        let grant_type = g.variant.as_deref().ok_or("exact-grant-unavailable")?;
        if attempt.len() != 64 || !attempt.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("entropy");
        }
        // Use 128 bits of the attempt's random entropy and set the UUID v4 version and variant bits.
        let rest = u8::from_str_radix(&attempt[16..17], 16).map_err(|_| "entropy")?;
        let request = format!(
            "{}-{}-4{}-{:x}{}-{}",
            &attempt[..8],
            &attempt[8..12],
            &attempt[13..16],
            (rest & 3) | 8,
            &attempt[17..20],
            &attempt[20..32]
        );
        Ok((
            METHOD,
            json!({"targetType":"PERSONAL","resetType":"WEEK","recordId":id,
            "grantType":grant_type,"requestId":request}),
        ))
    }

    fn verify(&self, before: &Snapshot, g: &Grant, after: &Snapshot, _provider_said_spent: bool) -> bool {
        before.source_binding == after.source_binding
            && g.kind == "weekly"
            && g.expires_at_ms.is_none_or(|at| at > after.observed_at_ms)
            && after
                .grants
                .iter()
                .find(|n| n.id == g.id)
                .is_none_or(|n| n.kind == g.kind && n.variant == g.variant && !n.usable && n.left == 0)
            && before
                .windows
                .get("seven_day")
                .zip(after.windows.get("seven_day"))
                .is_some_and(|(b, a)| a < b)
    }

    fn unspent(&self, before: &Snapshot, g: &Grant, after: &Snapshot, since_send_ms: i64) -> bool {
        since_send_ms >= 120_000
            && before.source_binding == after.source_binding
            && g.kind == "weekly"
            && before.weekly_reset_at_ms.is_some()
            && before.weekly_reset_at_ms == after.weekly_reset_at_ms
            && after.grants.iter().any(|n| n == g && n.usable && n.left > 0)
    }
}
