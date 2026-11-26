use super::Host;
use serde_json::{Map, Value, json};
use std::sync::LazyLock;

static CONTRACT: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../../config/tables/app-updates.json"))
        .expect("app update contract")
});
static VERSION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A[0-9]+(?:\.[0-9A-Za-z-]+){1,5}(?:[+-][0-9A-Za-z.-]+)?\z")
        .expect("app release pattern")
});
static WRAPPER_VERSION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9._+-]{0,63}\z").expect("wrapper version pattern")
});
static WORD: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A[a-z0-9][a-z0-9_.:-]{0,63}\z").expect("diagnostic word pattern")
});
static TMUX: LazyLock<[regex::Regex; 2]> = LazyLock::new(|| {
    [
        r"\Accs-updates-[a-f0-9]{12}\z",
        r"\Accs-updated-(?:antigravity-cli|muse-code|omp|codex-cli|claude-code)-[1-9][0-9]{0,3}\z",
    ]
    .map(|pattern| regex::Regex::new(pattern).expect("tmux target pattern"))
});

pub(super) fn app_ids() -> impl Iterator<Item = &'static str> {
    CONTRACT["labels"]
        .as_object()
        .into_iter()
        .flat_map(|labels| labels.keys().map(String::as_str))
}
pub(super) fn app_known(app: &str) -> bool {
    CONTRACT["labels"].get(app).is_some()
}
pub(super) fn is_acp_deferred(row: &Value) -> bool {
    row["message"] == CONTRACT["messages"]["t3_restart_pending"]
}
pub(super) fn legacy(app: &str) -> bool {
    listed(&CONTRACT["legacyApps"], app)
}
fn listed(list: &Value, word: &str) -> bool {
    array(list).iter().any(|value| value == word)
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}
fn word(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| WORD.is_match(value))
}
fn version(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|value| value.len() <= 64 && VERSION.is_match(value))
}
fn count(value: &Value, limit: u64) -> Option<u64> {
    value.as_u64().filter(|n| *n <= limit)
}

pub(super) fn blank(host: Host, app: &str, status: &str, code: &str) -> Value {
    json!({
        "appId": app, "appLabel": CONTRACT["labels"][app], "platform": host.id,
        "status": status, "previousVersion": null, "version": null, "manager": null,
        "message": CONTRACT["messages"][code], "updateAttempted": false,
        "restartedProcesses": 0, "forcedStops":0, "restartTargets": []
    })
}

/// A row for a computer whose helpers could not be synced: nothing ran there, and the
/// diagnostics name why. It restores through `normalize` like a helper's own row.
pub(super) fn sync_failure(host: Host, app: &str, reason: &str, cause: &str) -> Value {
    normalize(
        host,
        app,
        &json!({
            "appId": app, "platform": host.os, "status": "failed",
            "messageCode": "helper_sync_failed", "updateAttempted": false,
            "diagnostics": {"cause": cause, "reason": reason},
        }),
    )
    .unwrap_or_else(|| blank(host, app, "failed", "helper_sync_failed"))
}

pub(super) fn normalize(host: Host, app: &str, row: &Value) -> Option<Value> {
    if row["appId"] != app || !app_known(app) || row["platform"] != host.os {
        return None;
    }
    let status = row["status"].as_str()?;
    let code = row["messageCode"].as_str()?;
    let label = CONTRACT["labels"][app].as_str()?;
    let base_message = CONTRACT["messages"][code].as_str()?;
    let quota_reset = match row.get("quotaReset") {
        None => None,
        Some(value)
            if app == "muse-code"
                && matches!(code, "muse_quota_exhausted" | "muse_quota_current") =>
        {
            let reset = value.as_str()?;
            if reset.is_empty()
                || reset.encode_utf16().count() > 64
                || reset.chars().any(|character| character.is_ascii_control())
            {
                return None;
            }
            Some(reset)
        }
        Some(_) => return None,
    };
    if ![
        "updated",
        "current",
        "not_installed",
        "failed",
        "restart_failed",
        "skipped",
        "unknown",
        "action_required",
        "held",
    ]
    .contains(&status)
    {
        return None;
    }
    let current = version(&row["version"]);
    let previous = version(&row["previousVersion"]);
    let manager = row["manager"].as_str().filter(|manager| {
        [
            "native",
            "npm",
            "brew",
            "winget",
            "msix",
            "apt",
            "official-download",
        ]
        .contains(manager)
    });
    let restarted = count(&row["restartedProcesses"], 10000).unwrap_or(0);
    let attempted = row["updateAttempted"] == true;
    let held = status == "held";
    let is_cli = [
        "antigravity-cli",
        "muse-code",
        "omp",
        "codex-cli",
        "claude-code",
    ]
    .contains(&app);
    let supports_strays = ["codex-cli", "claude-code"].contains(&app);
    let windows = host.os == "windows";
    let reopens = windows && ["codex-desktop", "claude-desktop", "zcode"].contains(&app);
    let targets: Vec<_> = array(&row["restartTargets"])
        .iter()
        .take(100)
        .filter_map(|target| {
            Some(match target["kind"].as_str()? {
                kind @ ("terminal" | "windows-terminal") => json!({"kind": kind}),
                "desktop" if (app == "t3-code" && host.os != "ubuntu") || reopens => {
                    json!({"kind":"desktop"})
                }
                "systemd"
                    if app == "t3-code"
                        && host.os == "ubuntu"
                        && target["service"] == "t3code.service"
                        && target["delaySeconds"] == 30 =>
                {
                    json!({"kind":"systemd","service":"t3code.service","delaySeconds":30})
                }
                "tmux"
                    if target["server"]
                        .as_str()
                        .is_some_and(|s| TMUX[0].is_match(s))
                        && target["session"]
                            .as_str()
                            .is_some_and(|s| TMUX[1].is_match(s)) =>
                {
                    json!({"kind":"tmux","server":target["server"],"session":target["session"]})
                }
                _ => return None,
            })
        })
        .collect();
    let parts = (app == "t3-acp-adapters").then(|| adapter_parts(&row["parts"]));
    let versioned = parts.as_ref().map_or(current.is_some(), |parts| {
        parts
            .iter()
            .any(|part| !part["version"].is_null() && part["version"] != part["previousVersion"])
    });
    let good = match code {
        "t3_updated" => app == "t3-code" && status == "updated",
        "t3_restart_scheduled" => {
            app == "t3-code"
                && status == "updated"
                && targets.iter().any(|target| target["kind"] == "systemd")
        }
        "source_disabled" | "in_use" => status == "action_required",
        "desktop_reopened" => reopens && status == "updated",
        "store_no_newer" => app == "codex-desktop" && windows && status == "current",
        "store_update_in_app" => app == "codex-desktop" && windows && status == "action_required",
        "package_mismatch" => app == "codex-desktop" && status == "failed",
        "package_in_use" => {
            host.id == "windows"
                && ["codex-desktop", "claude-desktop"].contains(&app)
                && status == "action_required"
                && attempted
        }
        "held_for_review" | "held_unchecked" => held && app == "antigravity-cli",
        "updated_unreviewed" => app == "antigravity-cli" && status == "updated",
        "t3_sessions_kept" | "background_sessions_kept" => is_cli && status == "updated",
        "model_catalog_unrefreshed" => {
            app == "muse-code" && matches!(status, "failed" | "restart_failed")
        }
        "cursor_updated" => app == "cursor-cli" && status == "updated",
        "cursor_update_staged" => app == "cursor-cli" && windows && status == "action_required",
        "antigravity_acp_updated" => app == "antigravity-acp" && status == "updated",
        "antigravity_acp_newer_kept" => app == "antigravity-acp" && status == "current",
        "antigravity_acp_activation_unsettled" => app == "antigravity-acp" && status == "failed",
        "antigravity_acp_staged" => app == "antigravity-acp" && status == "action_required",
        "t3_install_timeout"
        | "t3_install_state_unknown"
        | "t3_session_revoke_failed"
        | "t3_session_issue_unknown"
        | "t3_issuer_protection_failed" => app == "antigravity-acp" && status == "failed",
        code if code.starts_with("t3_install_failed:") => {
            app == "antigravity-acp" && status == "failed"
        }
        "mac_keychain_locked" | "mac_gui_unavailable" | "mac_gui_cleanup_failed" => {
            host.os == "mac"
                && ["muse-code", "cursor-cli"].contains(&app)
                && matches!(status, "failed" | "restart_failed")
        }
        "codex_sessions_stopped" => {
            app == "codex-cli" && host.id == "windows" && matches!(status, "updated" | "current")
        }
        "codex_force_failed" => {
            app == "codex-cli"
                && host.id == "windows"
                && matches!(status, "failed" | "restart_failed")
        }
        "older_copy_first" => supports_strays && status == "action_required",
        "zcode_release_unavailable" => app == "zcode" && status == "failed" && !attempted,
        _ => true,
    };
    let rule = &CONTRACT["causeCodeRules"][code];
    let allowed = rule.is_null()
        || (rule["app"] == app && rule["status"] == status && (rule["windows"] != true || windows));
    if !good
        || !allowed
        || held != matches!(code, "held_for_review" | "held_unchecked")
        || (status == "updated" && (!versioned || manager.is_none()))
    {
        return None;
    }
    let strays = if supports_strays {
        stray_copies(&row["strays"])
    } else {
        Vec::new()
    };
    let mut words = match code {
        "quit_first" => format!("Quit {label} to finish its update, then run Update apps again."),
        "in_use"
            if parts
                .as_ref()
                .is_some_and(|parts| parts.iter().any(|part| part["inUse"] == true)) =>
        {
            adapter_message(parts.as_deref().unwrap_or_default())
        }
        "in_use" => format!(
            "{label} is in use (open, or running in a T3 session), so nothing changed; run Update apps again once it is closed."
        ),
        "updated" if app == "antigravity-cli" && restarted == 0 => {
            "Updated Antigravity. Copies already open keep the old version until restarted.".into()
        }
        "muse_quota_exhausted" | "muse_quota_current" if quota_reset.is_some() => {
            let lead = if code == "muse_quota_current" {
                "Muse is current."
            } else {
                "Updated Muse."
            };
            format!(
                "{lead} Its quota is used up until {}, so the new models appear after its next session.",
                quota_reset?
            )
        }
        _ => base_message.to_owned(),
    };
    if let Some(note) = older_copy(label, &strays, current.or(previous)) {
        if !words.ends_with(['.', '!', '?']) {
            words.push('.');
        }
        words.push(' ');
        words.push_str(&note);
    }
    let mut public = json!({
        "appId":app, "appLabel":label, "platform":host.id, "status":status,
        "previousVersion":previous, "version":current, "manager":manager, "message":words,
        "updateAttempted":attempted, "restartedProcesses":restarted, "forcedStops":count(&row["forcedStops"],10000).unwrap_or(0)
    });
    if matches!(code, "held_for_review" | "antigravity_acp_signin_hold")
        && let Some(held) = version(&row["heldVersion"])
    {
        public["heldVersion"] = json!(held);
    }
    if let Some(reset) = quota_reset {
        public["quotaReset"] = json!(reset);
    }
    if let Some(parts) = parts {
        public["parts"] = json!(parts);
    }
    if !strays.is_empty() {
        public["strays"] = json!(strays);
    }
    if let Some(diagnostics) = diagnostics(&row["diagnostics"]) {
        public["diagnostics"] = diagnostics;
    }
    public["restartTargets"] = json!(targets);
    Some(public)
}

fn adapter_parts(value: &Value) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();
    for part in array(value) {
        if let Some(name @ ("muse-acp" | "zcode-acp-server")) = part["name"].as_str()
            && !parts.iter().any(|part| part["name"] == name)
        {
            let mut output = json!({"name":name,"previousVersion":version(&part["previousVersion"]),"version":version(&part["version"])});
            if part["inUse"] == true {
                output["inUse"] = Value::Bool(true);
            }
            parts.push(output);
        }
    }
    parts
}

fn adapter_message(parts: &[Value]) -> String {
    let held: Vec<_> = parts
        .iter()
        .filter(|part| part["inUse"] == true)
        .filter_map(|part| part["name"].as_str())
        .collect();
    let names = held.join(" and ");
    let (state, close) = if held.len() == 1 {
        (
            format!(
                "{names} is in use (open, or running in a T3 session), so it stayed at its installed version"
            ),
            "once it is closed",
        )
    } else {
        (
            format!(
                "{names} are in use (open, or running in T3 sessions), so they stayed at their installed versions"
            ),
            "once they are closed",
        )
    };
    let mut words = vec![state];
    for part in parts.iter().filter(|part| {
        part["inUse"] != true
            && !part["version"].is_null()
            && part["version"] != part["previousVersion"]
    }) {
        if let Some(name) = part["name"].as_str() {
            words.push(format!("{name} updated"));
        }
    }
    words.push(format!("run Update apps again {close}."));
    words.join("; ")
}

fn stray_copies(value: &Value) -> Vec<Value> {
    array(value).iter().take(4).filter_map(|row| {
        let place @ ("usr-local" | "homebrew" | "bun" | "user-npm" | "other") = row["location"].as_str()? else { return None };
        Some(json!({"location":place,"version":version(&row["version"]),"shadows":row["shadows"].as_bool()?}))
    }).collect()
}

fn older_copy(label: &str, strays: &[Value], current: Option<&str>) -> Option<String> {
    fn numbers(s: &str) -> Option<Vec<f64>> {
        let mut parts = s
            .split(['-', '+'])
            .next()?
            .split('.')
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        parts.resize(6, 0.0);
        Some(parts)
    }
    let current = numbers(current?)?;
    for stray in strays {
        let Some(version) = stray["version"]
            .as_str()
            .filter(|_| stray["shadows"] == true)
        else {
            continue;
        };
        if numbers(version).is_some_and(|version| version < current) {
            let place = match stray["location"].as_str()? {
                "usr-local" => "/usr/local/bin",
                "homebrew" => "the Homebrew bin folder",
                "bun" => "the Bun bin folder",
                "user-npm" => "a user npm folder",
                _ => "another PATH folder",
            };
            return Some(format!(
                "An older {label} copy ({version}) in {place} comes first on some PATHs; remove it so it never runs instead."
            ));
        }
    }
    None
}

fn diagnostics(value: &Value) -> Option<Value> {
    let cause = value["cause"]
        .as_str()
        .filter(|value| listed(&CONTRACT["diagnosticCauses"], value))?;
    let mut row = json!({"cause":cause});
    if let Some(reason) = word(&value["reason"]) {
        row["reason"] = json!(reason);
    }
    if let Some(tool) = value["tool"]
        .as_str()
        .filter(|value| listed(&CONTRACT["diagnosticTools"], value))
    {
        row["tool"] = json!(tool);
    }
    if let Some(code) = value["exitCode"]
        .as_i64()
        .filter(|code| (-2147483648..4294967296).contains(code))
    {
        row["exitCode"] = json!(code);
    }
    if let Some(phases) = value["phases"].as_object() {
        let kept: Map<_, _> = phases
            .iter()
            .take(8)
            .filter_map(|(name, v)| {
                WORD.is_match(name)
                    .then_some((name.clone(), json!(count(v, 86_400_000)?)))
            })
            .collect();
        if !kept.is_empty() {
            row["phases"] = Value::Object(kept);
        }
    }
    if let Some(owner) = owner(&value["owner"]) {
        row["owner"] = owner;
    }
    let notes: Vec<_> = array(&value["notes"])
        .iter()
        .take(6)
        .filter_map(word)
        .collect();
    if !notes.is_empty() {
        row["notes"] = json!(notes);
    }
    Some(row)
}

fn display_path(value: &Value) -> Option<String> {
    let text = value
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 1024)?;
    // Adapter paths come from the helper's U13 scrubber. Apply the shared scrubber
    // again at the service boundary and drop credential flags before shortening.
    static FLAG: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)(?:--?(?:token|api-key|secret|password|credential|authorization|bearer|session-key|device-code)|-p[ =])").expect("diagnostic credential flag")
    });
    if FLAG.is_match(text) {
        return Some("<rest of output removed: may contain a credential>".into());
    }
    let scrubbed = aac_core::scrub(text);
    let mut chars: Vec<_> = scrubbed.as_str().chars().rev().take(160).collect();
    chars.reverse();
    (!chars.is_empty()).then(|| chars.into_iter().collect())
}

fn owner(value: &Value) -> Option<Value> {
    let mut owner = Map::new();
    for key in ["manager", "source"] {
        if let Some(value) = word(&value[key]) {
            owner.insert(key.into(), json!(value));
        }
    }
    for (key, allowed) in [
        ("runtime", ["native", "harness"]),
        ("target", ["t3-offer", "cli-manifest"]),
    ] {
        if let Some(value) = value[key].as_str().filter(|value| allowed.contains(value)) {
            owner.insert(key.into(), json!(value));
        }
    }
    for field in ["executable", "firstOnPath"] {
        if let Some(path) = display_path(&value[field]) {
            owner.insert(field.into(), json!(path));
        }
    }
    if let Some(version) = version(&value["firstOnPathVersion"]) {
        owner.insert("firstOnPathVersion".into(), json!(version));
    }
    if let Some(flag) = value["onPath"].as_bool() {
        owner.insert("onPath".into(), json!(flag));
    }
    if let Some(count) = count(&value["copies"], 32) {
        owner.insert("copies".into(), json!(count));
    }
    for (field, max) in [("packages", 4), ("elsewhere", 8)] {
        let entries: Vec<_> = array(&value[field]).iter().take(max).filter_map(|row| {
            let name @ ("muse-acp" | "zcode-acp-server") = row["package"].as_str()? else { return None };
            let prefix = display_path(&row["prefix"])?;
            Some(if field == "packages" {
                json!({"package":name,"manager":word(&row["manager"]).unwrap_or("unknown"),"prefix":prefix,
                    "source":word(&row["source"]).unwrap_or("unknown"),"version":version(&row["version"])})
            } else { json!({"package":name,"prefix":prefix,"version":version(&row["version"])}) })
        }).collect();
        if !entries.is_empty() {
            owner.insert(field.into(), json!(entries));
        }
    }
    (!owner.is_empty()).then_some(Value::Object(owner))
}

fn legacy_isolation_message(code: &str, value: &Value) -> Option<String> {
    let current = version(&value["version"]);
    let wrapper = value.get("wrapperVersion");
    let reason = value["qualifyReason"].as_str();
    if wrapper.is_some_and(|value| {
        !value.is_null()
            && !value
                .as_str()
                .is_some_and(|text| WRAPPER_VERSION.is_match(text))
    }) || reason.is_some_and(|text| {
        text.is_empty()
            || text.encode_utf16().count() > 160
            || text.chars().any(|c| c.is_ascii_control())
    }) {
        return None;
    }
    Some(match code {
        "antigravity_acp_qualified" => format!(
            "Updated Antigravity ACP to {}; account isolation verified",
            current?
        ),
        "antigravity_acp_isolation_unverified" => {
            let runs = match wrapper {
                None => "T3 keeps using the release it ran before".to_owned(),
                Some(Value::Null) => {
                    "no qualified Antigravity ACP runtime is available to T3".to_owned()
                }
                Some(value) => format!("T3 keeps using {}", value.as_str()?),
            };
            let mut message = format!(
                "Updated Antigravity ACP to {}; account isolation is not verified yet, so {runs}",
                current?
            );
            if let Some(reason) = reason {
                message.push_str(". The check could not test it: ");
                message.push_str(reason);
            }
            message
        }
        _ => return None,
    })
}

pub(super) fn restore(host: Host, app: &str, value: &Value) -> Value {
    let mut input = value.clone();
    input["platform"] = json!(host.os);
    for code in CONTRACT["messages"]
        .as_object()
        .into_iter()
        .flat_map(Map::keys)
    {
        input["messageCode"] = json!(code);
        if let Some(row) = normalize(host, app, &input)
            && (row["message"] == value["message"]
                || legacy_isolation_message(code, value)
                    .is_some_and(|word| value["message"] == word)
                || array(&CONTRACT["legacyMessages"][code]).contains(&value["message"])
                || (app == "antigravity-cli"
                    && code == "updated"
                    && CONTRACT["messages"]["updated"] == value["message"]))
        {
            return row;
        }
    }
    blank(host, app, "failed", "helper_invalid")
}

pub(super) fn legacy_row(row: &Value) -> Value {
    let mut copy = row.clone();
    if let Some(record) = copy.as_object_mut() {
        record.remove("diagnostics");
    }
    let message = &row["message"];
    if row["appId"] == "muse-code"
        && [
            "model_catalog_unrefreshed",
            "mac_keychain_locked",
            "mac_gui_unavailable",
            "mac_gui_cleanup_failed",
        ]
        .iter()
        .any(|code| message == &CONTRACT["messages"][code])
        && let Some(old) = array(&CONTRACT["legacyMessages"]["model_catalog_unrefreshed"]).first()
    {
        copy["message"] = old.clone();
    }
    if [
        "version_went_down",
        "shared_updater_unsafe",
        "mixed_ownership",
        "pnpm_store_copy",
    ]
    .iter()
    .any(|code| message == &CONTRACT["messages"][code])
    {
        copy["message"] = CONTRACT["messages"][if row["status"] == "skipped" {
            "unsupported"
        } else {
            "update_failed"
        }]
        .clone();
    }
    if row["appId"] == "t3-acp-adapters" {
        if message == &CONTRACT["messages"]["wrong_prefix"] {
            copy["message"] = CONTRACT["messages"]["update_failed"].clone();
        }
        if message == &CONTRACT["messages"]["prefix_not_writable"] {
            copy["message"] = CONTRACT["messages"]["unsupported"].clone();
        }
    }
    copy
}
