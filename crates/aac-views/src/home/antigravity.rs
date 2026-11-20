//! antigravity-data.mjs: the saved Ubuntu profiles bound to their dashboard rows, the runtime's warnings and the
//! plan-dependent words of the Antigravity section.
use super::Acc;
use super::windows::{Win, antigravity_canonical, antigravity_service, visible};
use crate::js::*;
use serde_json::Value;

/// One bound profile (`antigravityView(...).antigravityAccounts[]`): only what Home reads.
pub struct Bound {
    /// Account id.
    pub id: String,
    /// Profile id.
    pub profile: String,
    /// The live login.
    pub selected: bool,
    /// The runtime confirmed it.
    pub runtime_verified: bool,
    /// Status words.
    pub status: String,
    /// Note words.
    pub note: String,
}

/// `antigravityView(data, inventory)` for Home.
pub struct Native {
    /// Bound profiles.
    pub accounts: Vec<Bound>,
    /// Why the runtime service is not running.
    pub service_problem: Option<String>,
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

fn module_name(v: Option<&Value>) -> Option<&str> {
    let s = match v {
        Some(Value::String(s)) => s.as_str(),
        _ => return None,
    };
    let b = s.as_bytes();
    (!b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'.'))
    .then_some(s)
}

fn minor(v: Option<&Value>) -> Option<&str> {
    let s = match v {
        Some(Value::String(s)) => s.as_str(),
        _ => return None,
    };
    let (a, b) = s.split_once('.')?;
    let digits = |p: &str, max: usize| {
        !p.is_empty() && p.len() <= max && p.bytes().all(|c| c.is_ascii_digit())
    };
    (digits(a, 2) && digits(b, 3)).then_some(s)
}

/// antigravity-data.mjs `antigravityServiceProblem`.
fn service_problem(inventory: Option<&Value>) -> Option<String> {
    let problem = get(inventory, "runtimeServiceProblem");
    if !truthy(problem) || !matches!(problem, Some(Value::Object(_) | Value::Array(_))) {
        return None;
    }
    let name = module_name(get(problem, "module")).unwrap_or("unknown");
    let python = minor(get(problem, "python"));
    let built = minor(get(problem, "builtFor"));
    let moved = match (python, built) {
        (Some(p), Some(b)) if p != b => format!(" (Python {p}; runtime built for {b})"),
        _ => String::new(),
    };
    let rebuild = "; managed updates wait until the runtime bundle is rebuilt";
    let reason = get(problem, "reason");
    if is_str(reason, "missing-python-module") {
        return Some(format!(
            "Runtime service failed: missing Python module {name}{moved}{rebuild}"
        ));
    }
    if is_str(reason, "parser-mismatch") {
        return Some(format!(
            "Runtime service failed: Python module {name} is not the pinned version{moved}{rebuild}"
        ));
    }
    if is_str(reason, "parser-import-failed") {
        return Some(format!(
            "Runtime service failed: Python module {name} does not load{moved}{rebuild}"
        ));
    }
    if is_str(reason, "service-failed") {
        let exit = finite(get(problem, "exitStatus"))
            .filter(|x| x.fract() == 0.0 && (1.0..=255.0).contains(x));
        return Some(format!(
            "Runtime service failed{}; managed updates wait until its cause is fixed",
            exit.map(|x| format!(" (exit status {x})"))
                .unwrap_or_default()
        ));
    }
    None
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
            || !flag("runtimeVerified")
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
        let verified = is_true(get(p, "runtimeVerified"));
        let status_field = str_field(account.get("status"))?;
        let lead = if selected {
            if verified {
                "Selected · running verified"
            } else {
                "Selected · runtime unverified"
            }
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
            selected,
            runtime_verified: verified,
            status: format!("{lead}{tail}"),
            note,
        });
    }
    Ok(Native {
        accounts: bound,
        service_problem: service_problem(inventory),
    })
}

/// antigravity-data.mjs `antigravityMissingWindow`.
pub fn missing_window(account: Acc, key: &str) -> R<&'static str> {
    let plan = account.get("antigravityPlan");
    if matches!(key, "3p-5h" | "3p-weekly" | "cli-3p-5h" | "cli-3p-weekly")
        && is_false(get(plan, "thirdPartyModels"))
    {
        return Ok("Not on plan");
    }
    if matches!(key, "gemini-5h" | "3p-5h" | "cli-gemini-5h" | "cli-3p-5h")
        && is_str(get(plan, "quotaPolicy"), "weekly")
    {
        return Ok("Weekly only");
    }
    if !antigravity_canonical(key) && has_live_reading(account, antigravity_service(Some(key)))? {
        return Ok("Not reported");
    }
    Ok("Unavailable")
}

/// A live (not cached) reading of the same quota service (T3's CLI or the app).
fn has_live_reading(account: Acc, service: &str) -> R<bool> {
    if !is_str(account.get("status"), "ok") {
        return Ok(false);
    }
    Ok(visible(Some("antigravity"), account)?
        .iter()
        .any(|w: &Win| {
            !is_str(w.get("kind"), "balance")
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
