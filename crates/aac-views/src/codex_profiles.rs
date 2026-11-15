//! GET /api/codex/profiles and /api/codex/profiles/quotas.
//!
//! [`summary`] ports `composeCodexAuthProfilesSummary` with `resolveActive`
//! (`src/codex-auth/codex-auth-dashboard-service.ts`). TS read the registry and every saved and
//! live login, keeping display-safe fields only; no credential reaches this module. The one
//! case TS logs (a `CCS_CODEX_PROFILE` that names no saved profile) is a [`Refuse`].
//!
//! [`quotas`] ports `composeCodexProfileQuotas` with `buildQuota` and `readingPredatesReset`
//! (`src/web-server/services/codex-profile-quota-service.ts`, `account-window-reset.ts`) over
//! the collector rows and renewal notes TS read, in the ISO-only presentation envelope.
use crate::js::{ascii_word, date_parse, trim};
use crate::spread::{Admit, Refuse, req, truthy};
use aac_status::read_context::{Context, Envelope};
use serde_json::{Map, Value, json};

type Object = Map<String, Value>;

/// `getCodexProfileNameError(name) === null` for the names the composer joins.
fn profile_name(name: &str) -> bool {
    ascii_word(
        name,
        |b| b.is_ascii_lowercase() || b.is_ascii_digit(),
        |b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-',
        63,
    )
}

/// `path.join(dir, name)` for a normalized absolute POSIX `dir` and a profile name; any other
/// folder spelling refuses rather than reimplementing `path.normalize`.
fn join(dir: &str, name: &str) -> Admit<String> {
    let normalized = dir.strip_prefix('/').is_some_and(|rest| {
        rest.split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
    });
    if !normalized || !profile_name(name) {
        return Err(Refuse);
    }
    Ok(format!("{dir}/{name}"))
}

fn env_text<'a>(env: &'a Object, key: &str) -> Admit<&'a str> {
    match env.get(key) {
        Some(Value::Null) => Ok(""),
        Some(Value::String(text)) => Ok(trim(text)),
        _ => Err(Refuse),
    }
}

/// `resolveActive`.
fn active(dir: &str, env: &Object, names: &[&str], default: Option<&str>) -> Admit<Value> {
    let home = env_text(env, "codexHome")?;
    if !home.is_empty() {
        let mut matched = Value::Null;
        for name in names {
            if join(dir, name)? == home {
                matched = json!(name);
                break;
            }
        }
        return Ok(json!({"name": matched, "source": "explicit-codex-home", "codexHome": home}));
    }
    let profile = env_text(env, "profile")?;
    if !profile.is_empty() {
        // TS warns and answers null; the warning stays with TS.
        if !names.contains(&profile) {
            return Err(Refuse);
        }
        return Ok(json!({"name": profile, "source": "env", "codexHome": join(dir, profile)?}));
    }
    Ok(match default {
        Some(name) if !name.is_empty() => {
            json!({"name": name, "source": "default", "codexHome": join(dir, name)?})
        }
        _ => Value::Null,
    })
}

/// The summary from `{instancesDir, env, registry: {default, names}, profiles, activated}`.
/// The profile rows and the live login's saved profile were composed by TS from the logins it
/// read; they pass through unchanged.
pub fn summary(input: &Value) -> Admit<Value> {
    let input = req(input.as_object())?;
    let dir = req(input.get("instancesDir").and_then(Value::as_str))?;
    let env = req(input.get("env").and_then(Value::as_object))?;
    let registry = req(input.get("registry").and_then(Value::as_object))?;
    let names = req(registry.get("names").and_then(Value::as_array))?
        .iter()
        .map(|name| req(name.as_str()))
        .collect::<Admit<Vec<&str>>>()?;
    let default = match registry.get("default") {
        Some(Value::Null) => None,
        Some(Value::String(name)) => Some(name.as_str()),
        _ => return Err(Refuse),
    };
    let profiles = req(input.get("profiles").filter(|p| p.is_array()))?;
    let activated = req(input
        .get("activated")
        .filter(|a| a.is_null() || a.is_object()))?;
    Ok(json!({
        "active": active(dir, env, &names, default)?,
        "activated": activated,
        "default": default,
        "profiles": profiles,
    }))
}

/// `readingPredatesReset(resetAt, sampledAt, now)`.
fn predates_reset(
    ctx: &Context,
    reset_at: Option<&Value>,
    sampled_at: Option<&Value>,
    now: f64,
) -> Admit<bool> {
    let Some(Value::String(reset_at)) = reset_at else {
        return Ok(false);
    };
    let Some(reset) = req(date_parse(ctx, reset_at))? else {
        return Ok(false);
    };
    #[allow(clippy::cast_precision_loss)]
    if reset as f64 > now {
        return Ok(false);
    }
    let sampled = match sampled_at {
        Some(Value::String(text)) => req(date_parse(ctx, text))?,
        _ => None,
    };
    Ok(crate::js::reading_predates_reset(Some(reset), sampled, now))
}

/// `a ?? b` over JSON members: a defined, non-null `a` is kept.
fn coalesce<'a>(a: Option<&'a Value>, b: Option<&'a Value>) -> Option<&'a Value> {
    a.filter(|value| !value.is_null()).or(b)
}

fn insert(out: &mut Object, key: &str, value: Option<&Value>) {
    if let Some(value) = value {
        out.insert(key.into(), value.clone());
    }
}

/// `buildQuota(profile, now, row, renewal)`.
fn quota(
    ctx: &Context,
    profile: &Object,
    now: f64,
    row: Option<&Object>,
    renewal: Option<&Value>,
) -> Admit<Value> {
    let name = req(profile.get("name").filter(|n| n.is_string()))?;
    let mut out = Object::new();
    out.insert("profileName".into(), name.clone());
    if !truthy(profile.get("authValid")) {
        out.insert("status".into(), json!("not_connected"));
        out.insert("windows".into(), json!([]));
        out.insert(
            "message".into(),
            json!("No valid saved Codex login is available for this profile."),
        );
        return Ok(Value::Object(out));
    }
    let sampled_at = row.and_then(|row| coalesce(row.get("staleAsOf"), row.get("fetchedAt")));
    let mut windows = Vec::new();
    let listed = match row.and_then(|row| row.get("quotaWindows")) {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(list)) => list.as_slice(),
        Some(_) => return Err(Refuse),
    };
    for window in listed {
        let window = req(window.as_object())?;
        let key = window.get("key");
        let mut item = Object::new();
        insert(&mut item, "key", key);
        match key.and_then(Value::as_str) {
            Some("five_hour") => {
                item.insert("label".into(), json!("5-hour limit"));
            }
            Some("seven_day") => {
                item.insert("label".into(), json!("Weekly limit"));
            }
            _ => insert(&mut item, "label", window.get("label")),
        }
        insert(&mut item, "usedPercent", window.get("usedPercent"));
        let reset_at = window.get("resetAt");
        if truthy(reset_at) {
            insert(&mut item, "resetsAt", reset_at);
        }
        if predates_reset(ctx, reset_at, sampled_at, now)? {
            item.insert("resetPassed".into(), json!(true));
        }
        windows.push(Value::Object(item));
    }
    let status = row
        .and_then(|row| row.get("quotaStatus"))
        .and_then(Value::as_str);
    let fetched_at = row.and_then(|row| row.get("fetchedAt"));
    let renewal = renewal.and_then(Value::as_object);
    if renewal.and_then(|r| r.get("state")).and_then(Value::as_str) == Some("failed") {
        out.insert("status".into(), json!("reauth_required"));
        let shown = if status == Some("ok") {
            windows
        } else {
            Vec::new()
        };
        out.insert("windows".into(), Value::Array(shown));
        if row.is_some() {
            insert(&mut out, "fetchedAt", fetched_at);
        }
        insert(&mut out, "message", renewal.and_then(|r| r.get("message")));
        return Ok(Value::Object(out));
    }
    if status == Some("ok") && !windows.is_empty() {
        out.insert("status".into(), json!("available"));
        out.insert("windows".into(), Value::Array(windows));
        insert(&mut out, "fetchedAt", fetched_at);
        return Ok(Value::Object(out));
    }
    if status == Some("error") && truthy(row.and_then(|row| row.get("needsReauth"))) {
        out.insert("status".into(), json!("reauth_required"));
        out.insert("windows".into(), json!([]));
        insert(&mut out, "fetchedAt", fetched_at);
        out.insert(
            "message".into(),
            json!("The saved Codex login needs to be renewed."),
        );
        return Ok(Value::Object(out));
    }
    out.insert("status".into(), json!("unavailable"));
    out.insert("windows".into(), json!([]));
    if row.is_some() {
        insert(&mut out, "fetchedAt", fetched_at);
    }
    out.insert(
        "message".into(),
        json!(if row.is_some() {
            "Codex usage is temporarily unavailable."
        } else {
            "Codex usage is being retrieved."
        }),
    );
    Ok(Value::Object(out))
}

/// The quota answer from `{profiles, rows, renewals, now}` in the presentation envelope.
pub fn quotas(envelope: Envelope) -> Admit<Value> {
    let ctx = Context::new(envelope.presentation);
    let input = req(envelope.data.as_object())?;
    let now = req(input.get("now").and_then(Value::as_f64))?;
    let rows = req(input.get("rows").and_then(Value::as_array))?
        .iter()
        .map(|row| req(row.as_object()))
        .collect::<Admit<Vec<&Object>>>()?;
    let renewals = req(input.get("renewals").and_then(Value::as_array))?
        .iter()
        .map(|pair| match pair.as_array().map(Vec::as_slice) {
            Some([name, entry]) => Ok((name, entry)),
            _ => Err(Refuse),
        })
        .collect::<Admit<Vec<(&Value, &Value)>>>()?;
    let mut profiles = Vec::new();
    for profile in req(input.get("profiles").and_then(Value::as_array))? {
        let profile = req(profile.as_object())?;
        let name = req(profile.get("name").and_then(Value::as_str))?;
        let named = |key: Option<&Value>| key.and_then(Value::as_str) == Some(name);
        // `new Map(rows.map(...))`: a later row for the same profile wins.
        let row = rows
            .iter()
            .rev()
            .find(|row| named(row.get("profile")))
            .copied();
        let renewal = renewals
            .iter()
            .find(|(key, _)| named(Some(key)))
            .map(|(_, entry)| *entry);
        profiles.push(quota(&ctx, profile, now, row, renewal)?);
    }
    if ctx.valid() {
        Ok(json!({ "profiles": profiles }))
    } else {
        Err(Refuse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_profile_precedence_matches_resolve_active() {
        let base = |env: Value, default: Value| {
            json!({"instancesDir":"/h/.ccs/codex-instances","env":env,
                "registry":{"default":default,"names":["work","home"]},"profiles":[],"activated":null})
        };
        let active_of = |input: Value| summary(&input).map(|out| out["active"].clone());
        assert_eq!(
            active_of(base(
                json!({"codexHome":" /h/.ccs/codex-instances/home ","profile":"work"}),
                json!(null)
            )),
            Ok(
                json!({"name":"home","source":"explicit-codex-home","codexHome":"/h/.ccs/codex-instances/home"})
            )
        );
        assert_eq!(
            active_of(base(
                json!({"codexHome":"/h/.codex","profile":null}),
                json!(null)
            )),
            Ok(json!({"name":null,"source":"explicit-codex-home","codexHome":"/h/.codex"}))
        );
        assert_eq!(
            active_of(base(
                json!({"codexHome":"\u{feff}","profile":"work"}),
                json!("home")
            )),
            Ok(json!({"name":"work","source":"env","codexHome":"/h/.ccs/codex-instances/work"}))
        );
        assert_eq!(
            active_of(base(
                json!({"codexHome":null,"profile":"gone"}),
                json!(null)
            )),
            Err(Refuse)
        );
        assert_eq!(
            active_of(base(
                json!({"codexHome":null,"profile":null}),
                json!("home")
            )),
            Ok(
                json!({"name":"home","source":"default","codexHome":"/h/.ccs/codex-instances/home"})
            )
        );
        assert_eq!(
            active_of(base(json!({"codexHome":null,"profile":null}), json!(null))),
            Ok(Value::Null)
        );
        let mut odd = base(json!({"codexHome":null,"profile":null}), json!("home"));
        odd["instancesDir"] = json!("/h/./codex-instances");
        assert_eq!(summary(&odd), Err(Refuse));
    }
}
