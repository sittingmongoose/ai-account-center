//! antigravity-data.mjs: the saved Ubuntu profiles bound to their dashboard rows, the runtime's warnings and the
//! plan-dependent words of the Antigravity section.
use super::Acc;
use super::windows::{Win, antigravity_canonical, antigravity_service, visible};
use crate::js::*;
use aac_providers::{ANTIGRAVITY_THIRD_PARTY, ANTIGRAVITY_WEEKLY_ONLY};
use serde_json::Value;

/// One bound profile (`antigravityView(...).antigravityAccounts[]`): only what Home reads.
pub struct Bound {
    /// Account id.
    pub id: String,
    /// Profile id.
    pub profile: String,
    /// Status words.
    pub status: String,
    /// Note words.
    pub note: String,
}

/// `antigravityView(data, inventory)` for Home.
pub struct Native {
    /// Bound profiles.
    pub accounts: Vec<Bound>,
}

fn profile_id(v: Option<&Value>) -> Option<&str> {
    let s = match v {
        Some(Value::String(s)) => s.as_str(),
        _ => return None,
    };
    let b = s.as_bytes();
    (!b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-'))
    .then_some(s)
}

/// accounts-data.mjs `timeLabel(value, 'Sampled')`.
fn sampled_label(cx: &super::Ctx, v: Option<&Value>) -> R<String> {
    let Some(Value::String(s)) = v else {
        return Ok(String::new());
    };
    Ok(match parse_date(s)? {
        Some(t) => format!("Sampled {}", cx.fmt.date_time(t)?),
        None => String::new(),
    })
}

/// antigravity-data.mjs `antigravityView`.
pub fn native(cx: &super::Ctx, accounts: &[Value], inventory: Option<&Value>) -> R<Native> {
    let mut bound = Vec::new();
    let profiles = match get(inventory, "profiles") {
        Some(Value::Array(list))
            if matches!(get(inventory, "schemaVersion"), Some(Value::Number(n)) if n.as_f64() == Some(1.0))
                && is_str(get(inventory, "hostId"), "ubuntu")
                && list.len() <= 16 =>
        {
            list.as_slice()
        }
        _ => &[],
    };
    for profile in profiles {
        let p = Some(profile);
        let Some(id) = profile_id(get(p, "id")) else {
            continue;
        };
        let count = profiles
            .iter()
            .filter(|q| strict_eq(get(Some(*q), "id"), get(p, "id")))
            .count();
        let flag = |k: &str| matches!(get(p, k), Some(Value::Bool(_)));
        let Some(Value::String(email)) = get(p, "email") else {
            continue;
        };
        if count != 1
            || !is_str(get(p, "hostId"), "ubuntu")
            || !flag("available")
            || !flag("selected")
        {
            continue;
        }
        let want = format!("antigravity:profile:{id}");
        let matches: Vec<&serde_json::Map<String, Value>> = accounts
            .iter()
            .filter_map(|row| match row {
                Value::Object(m) => Some(m),
                _ => None,
            })
            .filter(|row| {
                is_str(row.get("provider"), "antigravity")
                    && is_str(row.get("id"), &want)
                    && strict_eq(
                        get(row.get("capabilities"), "antigravityProfileId"),
                        get(p, "id"),
                    )
                    && is_str(row.get("platform"), "ubuntu")
                    && is_str(row.get("email"), email)
            })
            .collect();
        let [account] = matches.as_slice() else {
            continue;
        };
        let selected = is_true(get(p, "selected"));
        let status_field = str_field(account.get("status"))?;
        let lead = if selected {
            "Selected"
        } else if status_field == Some("needs_sign_in") {
            "Sign-in needed"
        } else if is_true(get(p, "available")) {
            "Saved login"
        } else {
            "Login unavailable"
        };
        let tail = match status_field {
            Some(s) => lookup(
                &[
                    ("cached", " · Cached"),
                    ("needs_sign_in", " · Sign-in needed"),
                    ("unavailable", " · Usage unavailable"),
                    ("error", " · Refresh failed"),
                ],
                s,
            )?
            .unwrap_or(""),
            None => "",
        };
        let note = [
            str_field(account.get("source"))?.unwrap_or("").to_string(),
            status_field.unwrap_or("").to_string(),
            sampled_label(cx, account.get("sampledAt"))?,
            str_field(account.get("message"))?.unwrap_or("").to_string(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        bound.push(Bound {
            id: text(account.get("id")).into(),
            profile: id.into(),
            status: format!("{lead}{tail}"),
            note,
        });
    }
    Ok(Native { accounts: bound })
}

/// antigravity-data.mjs `antigravityMissingWindow`.
pub fn missing_window(account: Acc, key: &str) -> R<&'static str> {
    let plan = account.get("antigravityPlan");
    if ANTIGRAVITY_THIRD_PARTY.contains(&key) && is_false(get(plan, "thirdPartyModels")) {
        return Ok("Not on plan");
    }
    if ANTIGRAVITY_WEEKLY_ONLY.contains(&key) && is_str(get(plan, "quotaPolicy"), "weekly") {
        return Ok("Weekly only");
    }
    if !antigravity_canonical(key) && has_live_reading(account, antigravity_service(Some(key)))? {
        return Ok("Not reported");
    }
    Ok("Unavailable")
}

/// A live (not cached) reading of the same quota service (T3's CLI or the app).
///
/// The Mac tray's isMeter rule, as `antigravity-data.mjs` `antigravityHasLiveReading` states it:
/// amounts (balance, spend, extra_usage) are not meters, and a reading whose reset has passed
/// no longer describes the window. Neither proves Google reported no such quota.
fn has_live_reading(account: Acc, service: &str) -> R<bool> {
    if !is_str(account.get("status"), "ok") {
        return Ok(false);
    }
    Ok(visible(Some("antigravity"), account)?
        .iter()
        .any(|w: &Win| {
            !is_str(w.get("kind"), "balance")
                && !is_str(w.get("kind"), "spend")
                && !is_str(w.get("kind"), "extra_usage")
                && !is_true(w.get("resetPassed"))
                && !is_str(w.get("status"), "cached")
                && !is_true(w.get("cached"))
                && !is_str(w.get("status"), "unavailable")
                && antigravity_service(w.key()) == service
        }))
}

/// antigravity-data.mjs `antigravityNoQuota`.
pub fn no_quota(account: Acc) -> bool {
    is_str(account.get("provider"), "antigravity")
        && is_str(get(account.get("antigravityPlan"), "quotaPolicy"), "none")
        && array(account.get("windows")).is_empty()
}

/// antigravity-data.mjs `antigravityCreditsHint`.
pub fn credits_hint(account: Acc) -> &'static str {
    if is_false(get(account.get("antigravityPlan"), "creditsOverage")) {
        "Not usable for Antigravity on this plan."
    } else {
        "Used only after the plan quota runs out, when AI Credit Overages is on."
    }
}
