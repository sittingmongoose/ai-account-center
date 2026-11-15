//! GET /api/claude/desktop-profiles and /api/claude/desktop-profiles/usage.
//!
//! [`profiles`] ports `listClaudeDesktopProfiles` with `listClaudeDesktopProfileMetadata`
//! (`src/web-server/services/claude-desktop-profile-service.ts`) over the manifest bytes the
//! service read itself; TS adds each row's in-memory Open progress. [`usage`] ports
//! `composeClaudeDesktopUsage` with `readCachedSample`
//! (`src/web-server/services/claude-desktop-usage-service.ts`) over the manifest identities and
//! remote history text TS read. Every manifest TS refuses (a 500) and every history text whose
//! `JSON.parse` answer serde_json cannot prove is a [`Refuse`].
use crate::js::{ascii_word, email_shape, has_control, iso, trim};
use crate::spread::{Admit, Refuse, req, utf16_len};
use serde_json::{Map, Value, json};

type Object = Map<String, Value>;

const MAX_PROFILES: usize = 1000;
const MAX_HISTORY_BYTES: usize = 1024 * 1024;
/// `validTimestamp`: `new Date(t)` is valid up to 8.64e15.
const MAX_TIME: f64 = 8.64e15;
const USAGE_FIELDS: [(&str, &str); 5] = [
    ("fh", "fiveHour"),
    ("sd", "weekly"),
    ("so", "weeklyOpus"),
    ("sn", "weeklySonnet"),
    ("xu", "extra"),
];

/// `readText(value, max)`: a non-blank string of at most `max` UTF-16 units without controls.
fn text(value: Option<&Value>, max: usize) -> Admit<&str> {
    let text = req(value.and_then(Value::as_str))?;
    if trim(text).is_empty() || utf16_len(text) > max || has_control(text) {
        return Err(Refuse);
    }
    Ok(text)
}

fn optional<'a>(object: &'a Object, key: &str, max: usize) -> Admit<Option<&'a str>> {
    object
        .get(key)
        .map(|value| text(Some(value), max))
        .transpose()
}

fn alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

struct Launcher<'a> {
    name: &'a str,
    path: Option<&'a str>,
    profile: Option<&'a str>,
    start_menu: Option<&'a str>,
    default: Option<bool>,
    ssh: Option<&'a str>,
}

/// `readLauncher`.
fn launcher(value: &Value, windows: bool) -> Admit<Launcher<'_>> {
    let object = req(value.as_object())?;
    let ssh = optional(object, "sshHost", 128)?;
    if ssh.is_some_and(|host| {
        !ascii_word(
            host,
            alnum,
            |b| b.is_ascii_alphanumeric() || b"._-".contains(&b),
            usize::MAX,
        )
    }) {
        return Err(Refuse);
    }
    Ok(Launcher {
        name: text(object.get("launcherName"), 255)?,
        path: optional(object, "launcherPath", 4096)?,
        profile: optional(object, "profilePath", 4096)?,
        start_menu: if windows {
            optional(object, "startMenuPath", 4096)?
        } else {
            None
        },
        default: match object.get("isDefault") {
            None => None,
            Some(Value::Bool(value)) => Some(*value),
            Some(_) => return Err(Refuse),
        },
        ssh,
    })
}

/// `publicLauncher`: the launcher without its private SSH alias.
fn public_launcher(launcher: &Launcher<'_>) -> Object {
    let mut out = Object::new();
    out.insert("launcherName".into(), json!(launcher.name));
    if let Some(path) = launcher.path {
        out.insert("launcherPath".into(), json!(path));
    }
    if let Some(profile) = launcher.profile {
        out.insert("profilePath".into(), json!(profile));
    }
    if let Some(default) = launcher.default {
        out.insert("isDefault".into(), json!(default));
    }
    out
}

/// One `readProfile` row as `listClaudeDesktopProfileMetadata` publishes it; also its id.
fn profile(value: &Value) -> Admit<(Option<&str>, Value)> {
    let object = req(value.as_object())?;
    let email = text(object.get("email"), 254)?;
    if !email_shape(email) {
        return Err(Refuse);
    }
    let id = optional(object, "id", 64)?;
    if id.is_some_and(|id| !ascii_word(id, alnum, |b| alnum(b) || b"_-".contains(&b), 63)) {
        return Err(Refuse);
    }
    let mac = object.get("mac").map(|v| launcher(v, false)).transpose()?;
    let windows = object
        .get("windows")
        .map(|v| launcher(v, true))
        .transpose()?;
    if mac.is_none() && windows.is_none() {
        return Err(Refuse);
    }
    let mut out = Object::new();
    if let Some(id) = id {
        out.insert("id".into(), json!(id));
    }
    out.insert("email".into(), json!(email));
    if let Some(mac) = &mac {
        let mut row = public_launcher(mac);
        if id.is_some() {
            // canOpenClaudeMacProfile
            let path = mac.path.unwrap_or("");
            let open = mac.ssh.is_some() && path.starts_with('/') && path.ends_with(".app");
            row.insert("canOpen".into(), json!(open));
        }
        out.insert("mac".into(), Value::Object(row));
    }
    if let Some(windows) = &windows {
        let mut row = public_launcher(windows);
        if id.is_some() {
            // canOpenClaudeWindowsProfile: the id already has CLAUDE_PROFILE_ID_PATTERN's shape.
            row.insert("canOpen".into(), json!(windows.ssh.is_some()));
        }
        if let Some(start_menu) = windows.start_menu {
            row.insert("startMenuPath".into(), json!(start_menu));
        }
        if let Some(id) = id {
            row.insert(
                "launchUri".into(),
                json!(format!("ccs-claude://launch/{id}")),
            );
        }
        out.insert("windows".into(), Value::Object(row));
    }
    Ok((id, Value::Object(out)))
}

/// The published manifest rows, or [`Refuse`] for every manifest TS refuses with its 500.
/// The bytes are what TS's UTF-8 read would decode; invalid UTF-8 and text serde_json cannot
/// parse exactly as `JSON.parse` would refuse here and stay with the TS reader.
pub fn profiles(manifest: &[u8]) -> Admit<Value> {
    let manifest: Value = serde_json::from_slice(manifest).map_err(|_| Refuse)?;
    let manifest = req(manifest.as_object())?;
    if manifest.get("version").and_then(Value::as_f64) != Some(1.0) {
        return Err(Refuse);
    }
    let rows = req(manifest.get("profiles").and_then(Value::as_array))?;
    if rows.len() > MAX_PROFILES {
        return Err(Refuse);
    }
    let mut ids: Vec<&str> = Vec::new();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let (id, value) = profile(row)?;
        if let Some(id) = id {
            if ids.contains(&id) {
                return Err(Refuse);
            }
            ids.push(id);
        }
        out.push(value);
    }
    Ok(json!({ "profiles": out }))
}

fn sample(status: &str, sampled_at: Value, utilization: Object) -> [(&'static str, Value); 3] {
    [
        ("status", json!(status)),
        ("sampledAt", sampled_at),
        ("utilization", Value::Object(utilization)),
    ]
}

/// `readCachedSample(contents)`.
fn cached_sample(history: Option<&str>) -> Admit<[(&'static str, Value); 3]> {
    let missing = || sample("needs-sign-in", Value::Null, Object::new());
    let unavailable = || sample("unavailable", Value::Null, Object::new());
    let Some(history) = history else {
        return Ok(missing());
    };
    // Rust strings are well formed, so the UTF-8 length is Buffer.byteLength's.
    if history.len() > MAX_HISTORY_BYTES {
        return Ok(unavailable());
    }
    // A text JSON.parse may accept (deep nesting, numbers past f64) is decided by TS.
    let history: Value = serde_json::from_str(history).map_err(|_| Refuse)?;
    let Some(history) = history.as_object() else {
        return Ok(unavailable());
    };
    let version = history.get("version").and_then(Value::as_f64);
    let samples = match history.get("samples") {
        Some(Value::Array(samples)) if version == Some(2.0) => samples,
        _ => return Ok(unavailable()),
    };
    if samples.is_empty() {
        return Ok(missing());
    }
    let mut latest: Option<(&Object, f64)> = None;
    for entry in samples {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some(t) = entry.get("t").and_then(Value::as_f64) else {
            continue;
        };
        let valid = t.fract() == 0.0 && (0.0..=MAX_TIME).contains(&t);
        if valid && latest.is_none_or(|(_, at)| t >= at) {
            latest = Some((entry, t));
        }
    }
    let Some((latest, at)) = latest else {
        return Ok(unavailable());
    };
    let Some(usage) = latest.get("u").and_then(Value::as_object) else {
        return Ok(unavailable());
    };
    let mut utilization = Object::new();
    for (source, target) in USAGE_FIELDS {
        if let Some(value) = usage.get(source)
            && value.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)
        {
            utilization.insert(target.into(), value.clone());
        }
    }
    if utilization.is_empty() {
        return Ok(unavailable());
    }
    // An integral time value of at most 8.64e15 is exact in i64.
    #[allow(clippy::cast_possible_truncation)]
    let word = req(iso(at as i64))?;
    Ok(sample("cached", json!(word), utilization))
}

/// The usage answer from `{platform, fetchedAt, profiles: [{id?, email, read, history?}]}`.
pub fn usage(input: &Value) -> Admit<Value> {
    let input = req(input.as_object())?;
    let platform = req(input.get("platform").filter(|p| p.is_string()))?;
    let fetched_at = req(input.get("fetchedAt").filter(|f| f.is_string()))?;
    let mut profiles = Vec::new();
    for row in req(input.get("profiles").and_then(Value::as_array))? {
        let row = req(row.as_object())?;
        let mut out = Object::new();
        if let Some(id) = row.get("id") {
            out.insert(
                "id".into(),
                req(Some(id).filter(|id| id.is_string()))?.clone(),
            );
        }
        out.insert(
            "email".into(),
            req(row.get("email").filter(|e| e.is_string()))?.clone(),
        );
        out.insert("cached".into(), json!(true));
        out.insert("fetchedAt".into(), fetched_at.clone());
        let fields = match (row.get("read").and_then(Value::as_str), row.get("history")) {
            (Some("unconfigured" | "failed"), None) => {
                sample("unavailable", Value::Null, Object::new())
            }
            (Some("read"), Some(Value::Null)) => cached_sample(None)?,
            (Some("read"), Some(Value::String(history))) => cached_sample(Some(history))?,
            _ => return Err(Refuse),
        };
        for (key, value) in fields {
            out.insert(key.into(), value);
        }
        profiles.push(Value::Object(out));
    }
    Ok(json!({ "platform": platform, "fetchedAt": fetched_at, "profiles": profiles }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_rows_publish_like_the_ts_metadata() {
        let manifest = br#"{"version":1,"profiles":[
            {"id":"work","email":"w@example.com","mac":{"launcherName":"Claude W\u00e9rk","launcherPath":"/Applications/Claude Work.app","sshHost":"mac","isDefault":true},
             "windows":{"launcherName":"W","sshHost":"pc","startMenuPath":"C:\\S","profilePath":"C:\\P"}},
            {"email":"x@example.com","windows":{"launcherName":"X"}}]}"#;
        let out = profiles(manifest).expect("published");
        assert_eq!(
            serde_json::to_string(&out).expect("json"),
            concat!(
                r#"{"profiles":[{"id":"work","email":"w@example.com","mac":{"launcherName":"Claude Wérk","#,
                r#""launcherPath":"/Applications/Claude Work.app","isDefault":true,"canOpen":true},"#,
                r#""windows":{"launcherName":"W","profilePath":"C:\\P","canOpen":true,"startMenuPath":"C:\\S","#,
                r#""launchUri":"ccs-claude://launch/work"}},{"email":"x@example.com","windows":{"launcherName":"X"}}]}"#
            )
        );
        for bad in [
            &br#"{"version":2,"profiles":[]}"#[..],
            br#"{"version":1,"profiles":[{"email":"x@example.com"}]}"#,
            br#"{"version":1,"profiles":[{"email":"x@y","mac":{"launcherName":"M"}}]}"#,
            br#"{"version":1,"profiles":[{"email":"x@example.com","mac":{"launcherName":"  "}}]}"#,
            br#"{"version":1,"profiles":[{"email":"x@example.com","mac":{"launcherName":"M","sshHost":"-x"}}]}"#,
            br#"{"version":1,"profiles":[{"id":"a","email":"x@example.com","mac":{"launcherName":"M"}},{"id":"a","email":"y@example.com","mac":{"launcherName":"M"}}]}"#,
            b"\xef\xbb\xbf{}",
        ] {
            assert_eq!(profiles(bad), Err(Refuse), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn the_newest_valid_sample_is_projected() {
        let history = r#"{"version":2,"samples":[{"t":5,"u":{"fh":1}},{"t":1793577600000,"u":{"fh":12.5,"sd":-1,"xu":0}},{"t":1.5,"u":{"fh":9}}]}"#;
        let input = json!({"platform":"mac","fetchedAt":"F","profiles":[
            {"id":"a","email":"a@example.com","read":"read","history":history},
            {"email":"b@example.com","read":"read","history":null},
            {"email":"c@example.com","read":"failed"}]});
        assert_eq!(
            serde_json::to_string(&usage(&input).expect("usage")).expect("json"),
            concat!(
                r#"{"platform":"mac","fetchedAt":"F","profiles":[{"id":"a","email":"a@example.com","cached":true,"fetchedAt":"F","#,
                r#""status":"cached","sampledAt":"2026-11-02T00:00:00.000Z","utilization":{"fiveHour":12.5,"extra":0}}"#,
                r#",{"email":"b@example.com","cached":true,"fetchedAt":"F","status":"needs-sign-in","sampledAt":null,"utilization":{}}"#,
                r#",{"email":"c@example.com","cached":true,"fetchedAt":"F","status":"unavailable","sampledAt":null,"utilization":{}}]}"#
            )
        );
        let corrupt = json!({"platform":"mac","fetchedAt":"F","profiles":[{"email":"a@example.com","read":"read","history":"{"}]});
        assert_eq!(usage(&corrupt), Err(Refuse));
    }
}
