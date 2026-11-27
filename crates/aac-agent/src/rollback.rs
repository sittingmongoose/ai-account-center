//! Central Logins step 3 on one computer: `aac-agent --logins-quarantine`, `--logins-restore` and
//! `--logins-status` (the owner or the lead runs them over ssh; the dashboard never does).
//!
//! ```text
//! aac-agent --logins-quarantine [--codex] [--claude <profile[=account],...>] [--claude-renewal-host <name>] [--dry-run]
//! aac-agent --logins-restore
//! aac-agent --logins-status
//! ```
//!
//! - `--codex` moves every AAC saved Codex copy (`<ccs>/codex-instances/<name>/auth.json`) and
//!   switches the dashboard on this computer to read each Codex account only through a T3
//!   instance's own login with the identity the saved copy proved (pinned before the move).
//! - `--claude a,b` moves those Claude sign-in files (`default` is `~/.claude`, others are
//!   `~/.claude-t3/<profile>`). Each profile is refused unless this computer already holds a
//!   delivered Claude token for its account that reads cleanly (and, where the static logins
//!   registry lives, the registry shows it delivered here with one clean read). The account is
//!   `claude:<profile>`, the Claude account whose email the profile is signed in with, or the one
//!   named as `profile=<account>`.
//! - `--claude-renewal-host <name>` (on the dashboard computer) cuts Claude renewal to that
//!   computer's sign-ins, one per account. The name must be a computer in the Claude renewal
//!   settings. A second profile of an account there is not renewed: it must already run on a
//!   static token, or be listed with `--claude` in the same run (only on that computer). Its
//!   renewed profiles are never moved.
//!
//! Nothing is deleted. The quarantine prints the one command that puts everything back.
use crate::failover;
use aac_files::login_rollback::{
    self as rollback, CodexPin, Kind, Manifest, Places, Planned, Refused, Switch,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The parsed quarantine request.
#[derive(Debug, Default, PartialEq, Eq)]
struct Request {
    codex: bool,
    claude: Vec<String>,
    /// `profile=<account>`: the Claude account (`claude:<account>`) whose token that profile uses.
    claude_accounts: BTreeMap<String, String>,
    renewal_host: Option<String>,
    dry_run: bool,
}

fn parse(args: &[OsString]) -> Result<Request, String> {
    let mut request = Request::default();
    let mut words = args
        .iter()
        .map(|a| a.to_str().ok_or("arguments must be ASCII"));
    while let Some(word) = words.next() {
        match word? {
            "--codex" if !request.codex => request.codex = true,
            "--dry-run" if !request.dry_run => request.dry_run = true,
            "--claude" if request.claude.is_empty() => {
                let list = words.next().ok_or("--claude needs profile names")??;
                for item in list.split(',') {
                    let (profile, account) = match item.split_once('=') {
                        Some((profile, account)) => (profile, Some(account)),
                        None => (item, None),
                    };
                    if !rollback::valid_name(profile)
                        || account.is_some_and(|a| !rollback::valid_name(a))
                        || request.claude.iter().any(|p| p == profile)
                    {
                        return Err(
                            "--claude takes profile names like default=jmtashjian,party".into()
                        );
                    }
                    request.claude.push(profile.to_owned());
                    if let Some(account) = account {
                        request
                            .claude_accounts
                            .insert(profile.to_owned(), account.to_owned());
                    }
                }
            }
            "--claude-renewal-host" if request.renewal_host.is_none() => {
                let host = words.next().ok_or("--claude-renewal-host needs a name")??;
                if !rollback::valid_name(host) {
                    return Err("--claude-renewal-host takes a computer name".into());
                }
                request.renewal_host = Some(host.to_owned());
            }
            other => return Err(format!("unknown or repeated argument: {other}")),
        }
    }
    if !request.codex && request.claude.is_empty() && request.renewal_host.is_none() {
        return Err(
            "say what to quarantine: --codex, --claude <profiles>, --claude-renewal-host <name>"
                .into(),
        );
    }
    Ok(request)
}

// ---------------------------------------------------------------------------------------------
// The Claude checks run before anything moves (dry run included). They read the renewal
// settings and state, the static logins and their registry; they write nothing.

/// A small JSON file this command only reads: `Ok(None)` when it is not there, `Err` when it is
/// not a plain file of at most `cap` bytes holding JSON.
fn small_json(path: &Path, cap: u64) -> Result<Option<Value>, ()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if meta.is_file() && meta.len() <= cap => {}
        _ => return Err(()),
    }
    let bytes = std::fs::read(path).map_err(|_| ())?;
    serde_json::from_slice(&bytes).map(Some).map_err(|_| ())
}

/// The renewal computers (name, whether it is this computer) and the profiles left alone.
type RenewalSettings = (Vec<(String, bool)>, Vec<String>);

/// The computers in the Claude renewal settings (`<ccs>/claude-renew/config.json`): each name and
/// whether it is this computer (no `ssh`), plus the profile names the renewal leaves alone.
fn renewal_settings(places: &Places) -> Result<RenewalSettings, String> {
    let value = match small_json(
        &places.ccs.join("claude-renew").join("config.json"),
        1 << 20,
    ) {
        Ok(Some(value)) => value,
        Ok(None) => {
            return Err(
                "the Claude renewal settings are not on this computer, so the renewal computer cannot be checked; run this on the dashboard computer"
                    .into(),
            );
        }
        Err(()) => return Err("the Claude renewal settings cannot be read".into()),
    };
    let hosts: Vec<(String, bool)> = value["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|host| {
            let name = host["name"].as_str().filter(|n| rollback::valid_name(n))?;
            Some((name.to_owned(), host["ssh"].is_null()))
        })
        .collect();
    if hosts.is_empty() {
        return Err("the Claude renewal settings name no computer".into());
    }
    let ignore = value["ignoreProfiles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    Ok((hosts, ignore))
}

/// The sign-ins the Claude renewal last read on `host` (`<ccs>/claude-renew/state.json`), by
/// profile name, each with its account email when one was read.
fn renewal_signins(
    places: &Places,
    host: &str,
    ignore: &[String],
) -> Result<Vec<(String, Option<String>)>, String> {
    let state = match small_json(&places.ccs.join("claude-renew").join("state.json"), 8 << 20) {
        Ok(Some(state)) => state,
        Ok(None) => Value::Null,
        Err(()) => return Err("the Claude renewal state cannot be read".into()),
    };
    let mut found: Vec<(String, Option<String>)> = state["profiles"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, saved)| {
            let (h, profile) = key.split_once('/')?;
            (h == host && rollback::valid_name(profile) && !ignore.iter().any(|i| i == profile))
                .then(|| {
                    (
                        profile.to_owned(),
                        saved["email"].as_str().map(str::to_owned),
                    )
                })
        })
        .collect();
    if found.is_empty() {
        return Err(format!(
            "the Claude renewal has not read any sign-in on {host} yet, so its renewed and cut profiles cannot be named; let it run once, then try again"
        ));
    }
    found.sort();
    Ok(found)
}

/// The renewal runner's rule (`aac_claude_renew::step3::renewed`): by profile name, the first
/// sign-in of each account is renewed (one with no account read yet stays renewed); every later
/// one is cut, paired with the profile that renews its account.
fn kept_and_cut(signins: &[(String, Option<String>)]) -> (Vec<String>, Vec<(String, String)>) {
    let mut first: BTreeMap<&str, &str> = BTreeMap::new();
    let (mut kept, mut cut) = (Vec::new(), Vec::new());
    for (profile, email) in signins {
        match email.as_deref().map(|e| (e, first.get(e).copied())) {
            Some((_, Some(renewed))) => cut.push((profile.clone(), renewed.to_owned())),
            Some((e, None)) => {
                first.insert(e, profile);
                kept.push(profile.clone());
            }
            None => kept.push(profile.clone()),
        }
    }
    (kept, cut)
}

/// The profile's config folder on this computer (`~/.claude` or `~/.claude-t3/<profile>`).
fn claude_folder(places: &Places, profile: &str) -> PathBuf {
    if profile == "default" {
        places.home.join(".claude")
    } else {
        places.home.join(".claude-t3").join(profile)
    }
}

/// The account email this profile is signed in with here, from Claude Code's own `.claude.json`.
fn claude_email(places: &Places, profile: &str) -> Option<String> {
    let path = if profile == "default" {
        places.home.join(".claude.json")
    } else {
        claude_folder(places, profile).join(".claude.json")
    };
    small_json(&path, 8 << 20).ok()??["oauthAccount"]["emailAddress"]
        .as_str()
        .map(str::to_owned)
}

/// The Claude accounts (`claude:<id>`) whose token a profile may use: the one named for it; else
/// `claude:<profile>` and every account in the Claude profiles list here signed in with the same
/// email. `claude:<profile>` is left out when that list gives it another email.
fn claude_accounts(places: &Places, profile: &str, named: Option<&str>) -> Vec<String> {
    if let Some(account) = named {
        return vec![format!("claude:{account}")];
    }
    let email = claude_email(places, profile);
    let listed: Vec<(String, String)> =
        match small_json(&places.ccs.join("claude-desktop-profiles.json"), 1 << 20) {
            Ok(Some(list)) if list["version"] == 1 => list["profiles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    Some((
                        p["id"].as_str()?.to_owned(),
                        p["email"].as_str()?.to_owned(),
                    ))
                })
                .collect(),
            _ => vec![],
        };
    let mut accounts = Vec::new();
    let other_email = listed
        .iter()
        .any(|(id, e)| id == profile && email.as_ref().is_some_and(|mine| mine != e));
    if !other_email {
        accounts.push(format!("claude:{profile}"));
    }
    if let Some(email) = &email {
        for (id, e) in &listed {
            let account = format!("claude:{id}");
            if e == email && !accounts.contains(&account) {
                accounts.push(account);
            }
        }
    }
    accounts
}

/// Where the static logins registry lives (the dashboard computer), it must show the login
/// delivered to this computer with one clean read of the same value.
fn registry_shows(places: &Places, login_id: &str, fingerprint: &str) -> Result<(), String> {
    let registry = match small_json(&places.ccs.join("logins-registry.json"), 262_144) {
        Ok(Some(registry)) => registry,
        Ok(None) => return Ok(()),
        Err(()) => return Err("the static logins registry on this computer cannot be read".into()),
    };
    let check = "run Check computers under Static logins, then try again";
    let key = small_json(&places.ccs.join("login-computer.json"), 4096)
        .ok()
        .flatten()
        .and_then(|k| k["key"].as_str().map(str::to_owned))
        .ok_or_else(|| {
            format!("this computer has no login key, so the registry cannot name it; {check}")
        })?;
    let computer = registry["computers"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|pin| pin["key"] == key.as_str())
        .and_then(|pin| pin["computer"].as_str())
        .ok_or_else(|| {
            format!("the static logins registry does not list this computer; {check}")
        })?;
    let entry = registry["logins"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|e| e["loginId"] == login_id);
    let clean = entry.is_some_and(|e| {
        e["fingerprint"] == fingerprint
            && e["holders"].as_array().into_iter().flatten().any(|h| {
                h["computer"] == computer && h["fingerprint"] == fingerprint && h["seen"] == "seen"
            })
    });
    if clean {
        Ok(())
    } else {
        Err(format!(
            "the static logins registry does not show login {login_id} delivered to {computer} with one clean read; {check}"
        ))
    }
}

/// The delivered static Claude token this profile runs on here (its login id), or why not.
fn static_token(places: &Places, profile: &str, named: Option<&str>) -> Result<String, String> {
    let accounts = claude_accounts(places, profile, named);
    let listed = aac_files::credential::list(&places.ccs)
        .map_err(|_| format!("{profile}: the static logins on this computer cannot be listed"))?;
    let tokens: Vec<_> = listed
        .iter()
        .filter_map(|(_, read)| read.as_ref().ok())
        .filter(|s| s.kind == aac_files::credential::Kind::ClaudeToken)
        .collect();
    let Some(token) = tokens.iter().find(|s| accounts.contains(&s.account)) else {
        let held: Vec<&str> = tokens.iter().map(|s| s.account.as_str()).collect();
        return Err(format!(
            "{profile}: no delivered Claude token for {} reads cleanly on this computer (Claude tokens here: {}); deliver one under Static logins and use it on that instance first, or name its account as {profile}=<account>",
            if accounts.is_empty() {
                "its account".to_owned()
            } else {
                accounts.join(" or ")
            },
            if held.is_empty() {
                "none".to_owned()
            } else {
                held.join(", ")
            },
        ));
    };
    registry_shows(places, &token.login_id, &token.fingerprint)
        .map_err(|why| format!("{profile}: {why}"))?;
    Ok(token.login_id.clone())
}

/// The Claude checks of one request: the lines that name what was found, and why it is refused.
fn check_claude(places: &Places, request: &Request) -> (Vec<String>, Result<(), String>) {
    let mut lines = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    let named = |p: &str| request.claude_accounts.get(p).map(String::as_str);
    let mut renewed: Vec<String> = Vec::new();
    let mut cut_here: Vec<String> = Vec::new();
    let mut renewal_here = false;
    if let Some(host) = &request.renewal_host {
        let (hosts, ignore) = match renewal_settings(places) {
            Ok(found) => found,
            Err(why) => return (lines, Err(why)),
        };
        let Some((_, here)) = hosts.iter().find(|(name, _)| name == host) else {
            let names: Vec<&str> = hosts.iter().map(|(n, _)| n.as_str()).collect();
            return (
                lines,
                Err(format!(
                    "{host} is not a computer in the Claude renewal settings (they name {}); switching on with it would pause Claude renewal everywhere",
                    names.join(", ")
                )),
            );
        };
        renewal_here = *here;
        let signins = match renewal_signins(places, host, &ignore) {
            Ok(found) => found,
            Err(why) => return (lines, Err(why)),
        };
        let (kept, cut) = kept_and_cut(&signins);
        lines.push(format!(
            "Claude renewal on {host} keeps: {}",
            kept.join(", ")
        ));
        for (profile, renews) in &cut {
            let what = format!("{host}/{profile} (same account as {host}/{renews}, not renewed)");
            if !renewal_here {
                problems.push(format!(
                    "{what} can be checked only on {host}, which is not this computer"
                ));
                lines.push(format!("  cut        {what}: refused, not this computer"));
            } else if request.claude.contains(profile) {
                lines.push(format!("  cut        {what}: listed for quarantine"));
                cut_here.push(profile.clone());
            } else if !claude_folder(places, profile).exists() {
                lines.push(format!(
                    "  cut        {what}: no profile folder here, nothing signs out"
                ));
            } else {
                match static_token(places, profile, None) {
                    Ok(id) => lines.push(format!("  cut        {what}: on static token {id}")),
                    Err(why) => {
                        lines.push(format!("  cut        {what}: refused"));
                        problems.push(format!(
                            "{why}; or list {profile} with --claude to quarantine its sign-in"
                        ));
                    }
                }
            }
        }
        renewed = kept;
    }
    for profile in &request.claude {
        if request.renewal_host.is_some() {
            if !renewal_here {
                problems.push(
                    "--claude goes with --claude-renewal-host only on the renewal computer itself"
                        .into(),
                );
                break;
            }
            if renewed.contains(profile) {
                problems.push(format!(
                    "{profile} is renewed on the renewal computer, which keeps that sign-in"
                ));
                continue;
            }
            if !cut_here.contains(profile) {
                problems.push(format!(
                    "{profile} is not a cut profile of the renewal computer; run --claude on the other computers"
                ));
                continue;
            }
        }
        match static_token(places, profile, named(profile)) {
            Ok(id) => lines.push(format!("  token      {profile}: on static token {id}")),
            Err(why) => problems.push(why),
        }
    }
    let verdict = if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    };
    (lines, verdict)
}

fn places() -> Result<Places, String> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("no home folder")?;
    let ccs = std::env::var_os("CCS_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".ccs"));
    if !home.is_absolute() || !ccs.is_absolute() {
        return Err("the home and AAC folders must be absolute".into());
    }
    Ok(Places { home, ccs })
}

fn restore_command() -> String {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "aac-agent".into());
    if cfg!(windows) {
        return format!("\"{exe}\" --logins-restore");
    }
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    match std::env::var_os("CCS_HOME") {
        Some(ccs) => format!(
            "CCS_HOME={} {} --logins-restore",
            quote(&ccs.to_string_lossy()),
            quote(&exe)
        ),
        None => format!("{} --logins-restore", quote(&exe)),
    }
}

fn now_stamp() -> Result<String, String> {
    let millis = aac_core::UtcMs::now()
        .map_err(|_| "no clock".to_owned())?
        .get();
    let millis = i64::try_from(millis).map_err(|_| "no clock".to_owned())?;
    chrono::DateTime::from_timestamp_millis(millis - millis.rem_euclid(1000))
        .map(|at| at.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .ok_or_else(|| "no clock".to_owned())
}

/// Every saved Codex profile name on this computer: the folders under `codex-instances` and the
/// names in `codex-profiles.yaml` (when it reads).
fn codex_names(places: &Places) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(places.ccs.join("codex-instances"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| rollback::valid_name(name))
        .collect();
    if let Some(registry) = aac_files::codex_registry::read(&places.ccs.join("codex-profiles.yaml"))
    {
        names.extend(registry.list().into_iter().map(str::to_owned));
    }
    names.sort();
    names.dedup();
    names
}

/// The identity each saved Codex profile proves, from the same native inventory the dashboard
/// reads. A saved copy's own candidate wins; `default` without one is pinned to the computer's
/// own `~/.codex` login (a tool-owned sign-in, not an AAC copy). `None`: the inventory could not
/// be read natively here, so nothing is pinned and nothing moves.
pub(crate) fn codex_pins(
    inventory: &Value,
    names: &[String],
) -> Option<BTreeMap<String, CodexPin>> {
    let candidates = inventory["candidates"].as_array()?;
    if inventory["version"] != 1
        || inventory["action"] != "inventory"
        || inventory["failed"] == true
        || inventory["incompleteSelectors"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    {
        return None;
    }
    let selectors = |row: &Value| -> Vec<String> {
        row["selectors"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    let pin = |row: &Value| -> Option<CodexPin> {
        (row["identity"]["kind"] == "codex-principal-v1").then_some(())?;
        let pin = CodexPin {
            identity: row["identity"]["key"].as_str()?.to_owned(),
            email: row["email"].as_str()?.to_owned(),
        };
        let probe = Switch {
            version: 1,
            since: "2026-01-01T00:00:00Z".into(),
            codex: Some(BTreeMap::from([("x".to_owned(), pin.clone())])),
            claude_renewal_host: None,
        };
        probe.valid().then_some(pin)
    };
    let mut pins = BTreeMap::new();
    for name in names {
        let wanted = format!("codex-profile:{name}");
        let saved = candidates.iter().find(|row| {
            let s = selectors(row);
            s.contains(&wanted) && !s.iter().any(|w| w == "discover" || w == "t3")
        });
        let own = (name == "default")
            .then(|| {
                candidates
                    .iter()
                    .find(|row| selectors(row).iter().any(|w| w == "discover"))
            })
            .flatten();
        if let Some(found) = saved.or(own).and_then(pin) {
            pins.insert(name.clone(), found);
        }
    }
    Some(pins)
}

fn inventory(places: &Places) -> Value {
    failover::run(&json!({
        "version": 1,
        "action": "inventory",
        "provider": "codex",
        "homeDir": places.home.to_string_lossy(),
        "ccsDir": places.ccs.to_string_lossy(),
    }))
    .unwrap_or(Value::Null)
}

fn label(kind: Kind) -> &'static str {
    match kind {
        Kind::CodexSaved => "codex saved copy  ",
        Kind::ClaudeSignin => "claude sign-in    ",
    }
}

fn say(out: &mut impl std::io::Write, line: &str) {
    let _ = writeln!(out, "{line}");
}

/// What one quarantine run will move and switch, ready for [`rollback::quarantine`].
pub(crate) struct Prepared {
    planned: Vec<Planned>,
    switch: Option<Switch>,
    unpinned: Vec<String>,
}

pub(crate) fn prepare(
    places: &Places,
    codex: bool,
    claude: &[String],
    renewal_host: Option<&str>,
    inventory: &Value,
    since: &str,
) -> Result<Prepared, String> {
    let mut planned = Vec::new();
    let mut pins = None;
    let mut unpinned = Vec::new();
    if codex {
        let names = codex_names(places);
        let found = codex_pins(inventory, &names).ok_or(
            "the Codex logins on this computer could not be read here, so no account can be pinned; nothing was moved",
        )?;
        for name in &names {
            if !found.contains_key(name) {
                unpinned.push(name.clone());
            }
            planned.push(Planned {
                kind: Kind::CodexSaved,
                profile: name.clone(),
            });
        }
        pins = Some(found);
    }
    planned.extend(claude.iter().map(|profile| Planned {
        kind: Kind::ClaudeSignin,
        profile: profile.clone(),
    }));
    let switch = (pins.is_some() || renewal_host.is_some()).then(|| Switch {
        version: 1,
        since: since.to_owned(),
        codex: pins,
        claude_renewal_host: renewal_host.map(str::to_owned),
    });
    Ok(Prepared {
        planned,
        switch,
        unpinned,
    })
}

fn quarantine(args: &[OsString], out: &mut impl std::io::Write) -> i32 {
    let request = match parse(args) {
        Ok(request) => request,
        Err(why) => {
            say(out, &format!("Refused: {why}. Nothing was moved."));
            return 2;
        }
    };
    let (places, since) = match places().and_then(|p| now_stamp().map(|at| (p, at))) {
        Ok(found) => found,
        Err(why) => {
            say(out, &format!("Refused: {why}. Nothing was moved."));
            return 1;
        }
    };
    let (lines, verdict) = check_claude(&places, &request);
    for line in &lines {
        say(out, line);
    }
    if let Err(why) = verdict {
        say(out, &format!("Refused: {why}. Nothing was moved."));
        return 1;
    }
    let inventory = if request.codex {
        inventory(&places)
    } else {
        Value::Null
    };
    let prepared = match prepare(
        &places,
        request.codex,
        &request.claude,
        request.renewal_host.as_deref(),
        &inventory,
        &since,
    ) {
        Ok(prepared) => prepared,
        Err(why) => {
            say(out, &format!("Refused: {why}."));
            return 1;
        }
    };
    if request.dry_run {
        say(out, "Dry run: nothing is moved.");
        for item in &prepared.planned {
            let there = match item.kind {
                Kind::CodexSaved => places.codex_copy(&item.profile),
                Kind::ClaudeSignin => places.claude_signin(&item.profile),
            }
            .is_some_and(|p| std::fs::symlink_metadata(p).is_ok());
            say(
                out,
                &format!(
                    "  {} {} {}",
                    if there { "would move" } else { "absent    " },
                    label(item.kind),
                    item.profile
                ),
            );
        }
        report_switch(out, prepared.switch.as_ref(), &prepared.unpinned);
        return 0;
    }
    let command = restore_command();
    match rollback::quarantine(
        &places,
        &prepared.planned,
        prepared.switch.as_ref(),
        &since,
        &command,
    ) {
        Ok(done) => {
            say(
                out,
                &format!(
                    "Quarantined {} file(s) into {} (0700). Nothing was deleted.",
                    done.moved.len(),
                    places.folder().display()
                ),
            );
            for m in &done.moved {
                say(
                    out,
                    &format!("  moved      {} {}", label(m.kind), m.profile),
                );
            }
            for (kind, profile) in &done.absent {
                say(out, &format!("  absent     {} {}", label(*kind), profile));
            }
            if done.switched {
                report_switch(out, prepared.switch.as_ref(), &prepared.unpinned);
            }
            if done.moved.is_empty() && !done.switched {
                say(out, "Nothing to quarantine on this computer.");
                return 0;
            }
            say(out, "To put everything back on this computer, run:");
            say(out, &format!("  {command}"));
            0
        }
        Err(refused) => {
            let why = match refused {
                Refused::AlreadyInPlace => {
                    "a quarantine is already in place on this computer; restore it first (aac-agent --logins-status shows it)".to_owned()
                }
                Refused::BadName(name) => format!("{name} is not a profile name"),
                Refused::Unsafe(kind, profile) => format!(
                    "the {}for {profile} is a link, has a second hard link or is not this user's file",
                    label(kind)
                ),
                Refused::MoveFailed(kind, profile) => format!(
                    "the {}for {profile} could not be moved (another disk?); every earlier move was put back",
                    label(kind)
                ),
                Refused::Failure(failure) => failure.detail.as_str().to_owned(),
            };
            say(out, &format!("Refused: {why}. Nothing was moved."));
            1
        }
    }
}

fn report_switch(out: &mut impl std::io::Write, switch: Option<&Switch>, unpinned: &[String]) {
    let Some(switch) = switch else { return };
    if let Some(pins) = &switch.codex {
        say(
            out,
            &format!(
                "Switch: Codex accounts are read only through a T3 instance's own login ({} account(s) pinned by identity).",
                pins.len()
            ),
        );
        for name in unpinned {
            say(
                out,
                &format!(
                    "  {name}: its login could not be read, so no account is pinned; it will show as unavailable"
                ),
            );
        }
    }
    if let Some(host) = &switch.claude_renewal_host {
        say(
            out,
            &format!("Switch: Claude renewal only on {host}, one sign-in per account."),
        );
    }
}

fn restore(out: &mut impl std::io::Write) -> i32 {
    let places = match places() {
        Ok(places) => places,
        Err(why) => {
            say(out, &format!("Refused: {why}. Nothing changed."));
            return 1;
        }
    };
    match rollback::restore(&places) {
        Ok(done) => {
            for m in &done.back {
                say(
                    out,
                    &format!("  back       {} {}", label(m.kind), m.profile),
                );
            }
            for (m, kept) in &done.kept {
                say(
                    out,
                    &format!(
                        "  kept       {} {}: {}",
                        label(m.kind),
                        m.profile,
                        kept.reason()
                    ),
                );
            }
            if done.switch_removed {
                say(
                    out,
                    "Switch off: Codex reads and Claude renewal are as before step 3.",
                );
            }
            if done.back.is_empty() && done.kept.is_empty() && !done.switch_removed {
                say(out, "Nothing to restore on this computer.");
            }
            if done.kept.is_empty() {
                0
            } else {
                say(
                    out,
                    &format!(
                        "{} file(s) stay in {}; the record lists them.",
                        done.kept.len(),
                        places.folder().display()
                    ),
                );
                1
            }
        }
        Err(failure) => {
            say(
                out,
                &format!("Refused: {}. Nothing changed.", failure.detail.as_str()),
            );
            1
        }
    }
}

fn status(out: &mut impl std::io::Write) -> i32 {
    let places = match places() {
        Ok(places) => places,
        Err(why) => {
            say(out, &format!("Refused: {why}."));
            return 1;
        }
    };
    let manifest: Result<Option<Manifest>, _> = rollback::read_manifest(&places.ccs);
    let switch = rollback::read_switch(&places.ccs);
    if matches!((&manifest, &switch), (Ok(None), Ok(None))) {
        say(out, "No quarantine and no switch on this computer.");
    }
    match manifest {
        Ok(Some(m)) => {
            say(
                out,
                &format!("Quarantine since {}: {} file(s).", m.at, m.moved.len()),
            );
            for moved in &m.moved {
                say(
                    out,
                    &format!("  held       {} {}", label(moved.kind), moved.profile),
                );
            }
            say(out, &format!("Restore: {}", m.restore));
        }
        Ok(None) => {}
        Err(_) => say(
            out,
            "The quarantine record cannot be read (nothing was changed).",
        ),
    }
    match switch {
        Ok(Some(s)) => report_switch(out, Some(&s), &[]),
        Ok(None) => {}
        Err(_) => say(
            out,
            "The step 3 switch cannot be read: Codex rows show unavailable and Claude renewal is paused.",
        ),
    }
    0
}

/// `--logins-quarantine`, `--logins-restore`, `--logins-status`; `None` for any other flag.
pub fn subcommand(flag: &str, args: &[OsString]) -> Option<i32> {
    let mut out = std::io::stdout().lock();
    Some(match flag {
        "--logins-quarantine" => quarantine(args, &mut out),
        "--logins-restore" if args.is_empty() => restore(&mut out),
        "--logins-status" if args.is_empty() => status(&mut out),
        "--logins-restore" | "--logins-status" => {
            say(&mut out, "Refused: this command takes no arguments.");
            2
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn words(text: &str) -> Vec<OsString> {
        text.split_whitespace().map(OsString::from).collect()
    }

    #[test]
    fn arguments_are_explicit() {
        assert_eq!(
            parse(&words("--codex --claude-renewal-host ubuntu")).unwrap(),
            Request {
                codex: true,
                claude: vec![],
                renewal_host: Some("ubuntu".into()),
                dry_run: false,
                ..Request::default()
            }
        );
        assert_eq!(
            parse(&words("--claude default,party --dry-run"))
                .unwrap()
                .claude,
            ["default", "party"]
        );
        for bad in [
            "",
            "--dry-run",
            "--claude",
            "--claude ../x",
            "--codex --codex",
            "--claude a,a",
            "--claude a=../x",
            "--claude =x",
            "--purge",
        ] {
            assert!(parse(&words(bad)).is_err(), "{bad}");
        }
        let both = parse(&words(
            "--claude default=jmtashjian,tokentest --claude-renewal-host ubuntu",
        ))
        .unwrap();
        assert_eq!(both.claude, ["default", "tokentest"]);
        assert_eq!(both.claude_accounts["default"], "jmtashjian");
        assert!(!both.claude_accounts.contains_key("tokentest"));
    }

    // ---- The three review lows: fixtures for the renewal settings, state and static logins.

    const TOKEN: &str = "sk-ant-oat01-TESTONLY-rollback-token-000000000000";

    fn fixture() -> (tempfile::TempDir, Places) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let places = Places {
            ccs: home.join(".ccs"),
            home,
        };
        let renew = places.ccs.join("claude-renew");
        std::fs::create_dir_all(&renew).unwrap();
        #[cfg(unix)]
        for private in [&places.home, &places.ccs] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(private, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(
            renew.join("config.json"),
            r#"{"hosts":[{"name":"ubuntu","os":"linux"},{"name":"nas1","os":"linux","ssh":"test-nas1"},{"name":"mac","os":"mac","ssh":"test-mac"}],"ignoreProfiles":["retired"]}"#,
        )
        .unwrap();
        std::fs::write(
            renew.join("state.json"),
            r#"{"version":1,"profiles":{
                "ubuntu/default":{"email":"own@example.test"},
                "ubuntu/gmail":{"email":"gmail@example.test"},
                "ubuntu/tokentest":{"email":"own@example.test"},
                "ubuntu/retired":{"email":"own@example.test"},
                "nas1/party":{"email":"party@example.test"},
                "nas1/second":{"email":"party@example.test"}}}"#,
        )
        .unwrap();
        std::fs::write(
            places.ccs.join("claude-desktop-profiles.json"),
            r#"{"version":1,"profiles":[{"id":"jmtashjian","email":"own@example.test"},{"id":"party","email":"party@example.test"}]}"#,
        )
        .unwrap();
        for (profile, email) in [
            ("default", "own@example.test"),
            ("gmail", "gmail@example.test"),
            ("tokentest", "own@example.test"),
            ("party", "party@example.test"),
        ] {
            let folder = claude_folder(&places, profile);
            std::fs::create_dir_all(&folder).unwrap();
            let state = if profile == "default" {
                places.home.join(".claude.json")
            } else {
                folder.join(".claude.json")
            };
            std::fs::write(
                state,
                format!(r#"{{"oauthAccount":{{"emailAddress":"{email}"}}}}"#),
            )
            .unwrap();
        }
        (dir, places)
    }

    /// Deliver a static Claude token for `account` through the agent's own `login-put`.
    fn deliver(places: &Places, login_id: &str, account: &str) {
        let key = aac_files::credential::computer_key(&places.ccs).unwrap();
        crate::logins::run(
            "login-put",
            &json!({"loginId":login_id,"kind":"claude-token","provider":"claude",
                "account":account,"secret":TOKEN,"expiresAt":"2027-11-27T00:00:00Z",
                "expect":"absent","home":places.home.to_str().unwrap(),
                "nowMs":1_795_000_000_000i64,"computer":key}),
        )
        .unwrap();
    }

    fn request(claude: &[&str], host: Option<&str>) -> Request {
        let mut request = Request {
            renewal_host: host.map(str::to_owned),
            ..Request::default()
        };
        for item in claude {
            match item.split_once('=') {
                Some((p, a)) => {
                    request.claude.push(p.to_owned());
                    request.claude_accounts.insert(p.to_owned(), a.to_owned());
                }
                None => request.claude.push((*item).to_owned()),
            }
        }
        request
    }

    /// Review low 2: the renewal computer must be a computer in the Claude renewal settings; a
    /// typo is refused with the configured names (dry run too: the check runs before anything).
    #[test]
    fn the_renewal_computer_must_be_in_the_renewal_settings() {
        let (_dir, places) = fixture();
        let (_, verdict) = check_claude(&places, &request(&[], Some("ubunto")));
        let why = verdict.unwrap_err();
        assert!(
            why.contains("ubunto is not a computer in the Claude renewal settings (they name ubuntu, nas1, mac)"),
            "{why}"
        );
        assert!(why.contains("pause Claude renewal everywhere"), "{why}");
        std::fs::remove_file(places.ccs.join("claude-renew/config.json")).unwrap();
        let (_, verdict) = check_claude(&places, &request(&[], Some("ubuntu")));
        assert!(
            verdict
                .unwrap_err()
                .contains("renewal settings are not on this computer")
        );
        std::fs::write(places.ccs.join("claude-renew/config.json"), "{").unwrap();
        let (_, verdict) = check_claude(&places, &request(&[], Some("ubuntu")));
        assert!(verdict.unwrap_err().contains("cannot be read"));
    }

    /// Review low 1: a second profile of an account on the renewal computer is named in the dry
    /// run and blocks switch-on until it is on a static token or listed for quarantine; the
    /// renewed profiles are listed and can never be quarantined there.
    #[test]
    fn a_second_profile_on_the_renewal_computer_is_named_and_refused() {
        let (_dir, places) = fixture();
        let (lines, verdict) = check_claude(&places, &request(&[], Some("ubuntu")));
        let text = lines.join("\n");
        assert!(
            text.contains("Claude renewal on ubuntu keeps: default, gmail"),
            "{text}"
        );
        assert!(
            text.contains(
                "cut        ubuntu/tokentest (same account as ubuntu/default, not renewed): refused"
            ),
            "{text}"
        );
        assert!(
            !text.contains("retired"),
            "ignored profiles are left alone: {text}"
        );
        let why = verdict.unwrap_err();
        assert!(
            why.contains("tokentest: no delivered Claude token"),
            "{why}"
        );
        assert!(why.contains("or list tokentest with --claude"), "{why}");
        assert!(!why.contains("@"), "no email in the output: {why}");

        // A renewed profile is never quarantined on the renewal computer.
        let (_, verdict) = check_claude(&places, &request(&["gmail"], Some("ubuntu")));
        assert!(
            verdict
                .unwrap_err()
                .contains("gmail is renewed on the renewal computer")
        );

        // Listed for quarantine: still needs its static token (low 3).
        let (lines, verdict) = check_claude(&places, &request(&["tokentest"], Some("ubuntu")));
        assert!(lines.join("\n").contains(
            "tokentest (same account as ubuntu/default, not renewed): listed for quarantine"
        ));
        assert!(verdict.is_err());

        // On a static token of its account (found through the Claude profiles list): switch-on.
        deliver(&places, "0a1b2c3d", "claude:jmtashjian");
        let (lines, verdict) = check_claude(&places, &request(&[], Some("ubuntu")));
        assert_eq!(verdict, Ok(()));
        assert!(lines.join("\n").contains(
            "tokentest (same account as ubuntu/default, not renewed): on static token 0a1b2c3d"
        ));
        let (_, verdict) = check_claude(&places, &request(&["tokentest"], Some("ubuntu")));
        assert_eq!(verdict, Ok(()));

        // The renewal computer is another computer with a second profile: refused, named.
        let (lines, verdict) = check_claude(&places, &request(&[], Some("nas1")));
        assert!(
            lines
                .join("\n")
                .contains("nas1/second (same account as nas1/party")
        );
        assert!(
            verdict
                .unwrap_err()
                .contains("can be checked only on nas1, which is not this computer")
        );
        // ... and --claude goes with the renewal host only on that computer.
        let (_, verdict) = check_claude(&places, &request(&["party"], Some("mac")));
        let why = verdict.unwrap_err();
        assert!(
            why.contains("not read any sign-in on mac")
                || why.contains("only on the renewal computer"),
            "{why}"
        );
    }

    /// Review low 3: `--claude <profile>` is refused unless this computer holds a delivered
    /// Claude token for that profile's account that reads cleanly, and (where the static logins
    /// registry lives) the registry shows it delivered here with one clean read.
    #[test]
    fn claude_quarantine_needs_a_static_token_in_use_here() {
        let (_dir, places) = fixture();
        let (_, verdict) = check_claude(&places, &request(&["party"], None));
        let why = verdict.unwrap_err();
        assert!(
            why.contains("party: no delivered Claude token for claude:party reads cleanly on this computer (Claude tokens here: none)"),
            "{why}"
        );
        deliver(&places, "1a1b2c3d", "claude:gmail");
        let why = check_claude(&places, &request(&["party"], None))
            .1
            .unwrap_err();
        assert!(why.contains("(Claude tokens here: claude:gmail)"), "{why}");
        deliver(&places, "2a1b2c3d", "claude:party");
        let (lines, verdict) = check_claude(&places, &request(&["party"], None));
        assert_eq!(verdict, Ok(()));
        assert!(
            lines
                .join("\n")
                .contains("token      party: on static token 2a1b2c3d")
        );

        // `default` is not an account name: matched through its email, or named explicitly.
        let why = check_claude(&places, &request(&["default"], None))
            .1
            .unwrap_err();
        assert!(why.contains("claude:default or claude:jmtashjian"), "{why}");
        assert_eq!(
            check_claude(&places, &request(&["default=gmail"], None)).1,
            Ok(())
        );
        // A name whose listed email is another account's is never taken on its name alone.
        std::fs::write(
            claude_folder(&places, "party").join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"gmail@example.test"}}"#,
        )
        .unwrap();
        let why = check_claude(&places, &request(&["party"], None))
            .1
            .unwrap_err();
        assert!(!why.contains("claude:party "), "{why}");
        std::fs::write(
            claude_folder(&places, "party").join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"party@example.test"}}"#,
        )
        .unwrap();

        // The dashboard computer: the registry must show it delivered here with a clean read.
        let key = aac_files::credential::computer_key(&places.ccs).unwrap();
        let fp = aac_files::credential::fingerprint(TOKEN);
        let registry = |pin: &str, seen: &str| {
            json!({"version":1,"checkedAt":null,"computers":[{"computer":"ubuntu","key":pin,"host":"vm","enrolledAt":"2026-11-27T00:00:00Z","seen":null}],
                "notes":[],"logins":[{"loginId":"2a1b2c3d","kind":"claude-token","provider":"claude","account":"claude:party",
                "fingerprint":fp,"expiresAt":null,"createdAt":"2026-11-27T00:00:00Z",
                "holders":[{"computer":"ubuntu","fingerprint":fp,"deliveredAt":"2026-11-27T00:00:00Z","seen":seen}]}]})
            .to_string()
        };
        let path = places.ccs.join("logins-registry.json");
        std::fs::write(&path, registry(&"f".repeat(32), "seen")).unwrap();
        let why = check_claude(&places, &request(&["party"], None))
            .1
            .unwrap_err();
        assert!(why.contains("does not list this computer"), "{why}");
        std::fs::write(&path, registry(&key, "unreadable")).unwrap();
        let why = check_claude(&places, &request(&["party"], None))
            .1
            .unwrap_err();
        assert!(
            why.contains("does not show login 2a1b2c3d delivered to ubuntu with one clean read"),
            "{why}"
        );
        std::fs::write(&path, registry(&key, "seen")).unwrap();
        assert_eq!(check_claude(&places, &request(&["party"], None)).1, Ok(()));
        std::fs::write(&path, "{").unwrap();
        let why = check_claude(&places, &request(&["party"], None))
            .1
            .unwrap_err();
        assert!(
            why.contains("registry on this computer cannot be read"),
            "{why}"
        );
    }

    fn row(selectors: &[&str], key: char, email: &str) -> Value {
        json!({"locator":"a".repeat(64),"selectors":selectors,
            "identity":{"kind":"codex-principal-v1","key":key.to_string().repeat(64)},
            "revision":"c".repeat(64),"email":email,"plan":"plus"})
    }

    /// A saved copy pins its own identity; `default` without a saved copy pins the computer's own
    /// `~/.codex` login; a T3 home never pins a saved profile; an unreadable inventory pins nothing.
    #[test]
    fn pins_come_from_the_saved_copy_identity() {
        let inventory = json!({"version":1,"action":"inventory","candidates":[
            row(&["codex-profile:work"], 'a', "work@example.test"),
            row(&["discover","codex-profile:default"], 'b', "own@example.test"),
            row(&["t3"], 'c', "party@example.test"),
        ]});
        let names = ["default", "party", "work"].map(String::from);
        let pins = codex_pins(&inventory, &names).unwrap();
        assert_eq!(pins["work"].identity, "a".repeat(64));
        assert_eq!(pins["default"].identity, "b".repeat(64));
        assert!(
            !pins.contains_key("party"),
            "a T3 login is never a saved copy"
        );
        assert!(codex_pins(&json!({"fallback":"python"}), &names).is_none());
        let incomplete = json!({"version":1,"action":"inventory","candidates":[],"incompleteSelectors":["codex-profile:*"]});
        assert!(codex_pins(&incomplete, &names).is_none());
    }

    /// The real flow on fixture homes: the saved Codex copy and two Claude sign-in files leave
    /// every reader's reach (the native inventories no longer find them), the T3 instance's own
    /// Codex login stays, the switch pins the saved copy's identity, and one restore brings the
    /// inventories back to exactly what they were.
    #[cfg(target_os = "linux")]
    #[test]
    fn quarantined_logins_are_out_of_every_readers_reach_until_restored() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let places = Places {
            home: home.clone(),
            ccs: home.join(".ccs"),
        };
        let encode = |value: &Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(value).unwrap())
        };
        let claims = json!({"email":"work@example.test","https://api.openai.com/auth":{"chatgpt_account_id":"fixture-account","chatgpt_user_id":"fixture-user","chatgpt_plan_type":"plus"},"sub":"fixture-subject","iss":"fixture-issuer"});
        let id_token = format!(
            "{}.{}.c2ln",
            encode(&json!({"alg":"none"})),
            encode(&claims)
        );
        let auth = json!({"tokens":{"id_token":id_token,"access_token":"TESTONLY-codex-access","account_id":"fixture-account"}});
        let write = |path: &std::path::Path, bytes: &[u8]| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        let saved = places.codex_copy("work").unwrap();
        write(&saved, &serde_json::to_vec(&auth).unwrap());
        let t3 = home.join(".codex-t3/work/auth.json");
        write(&t3, &serde_json::to_vec(&auth).unwrap());
        for profile in ["party", "me"] {
            write(
                &places.claude_signin(profile).unwrap(),
                br#"{"claudeAiOauth":{"accessToken":"TESTONLY-claude","expiresAt":1900000000000}}"#,
            );
            write(
                &home.join(format!(".claude-t3/{profile}/.claude.json")),
                format!(r#"{{"oauthAccount":{{"accountUuid":"00000000-0000-4000-8000-00000000000{}","organizationUuid":"00000000-0000-4000-8000-0000000000aa","emailAddress":"{profile}@example.test"}}}}"#, if profile == "me" { 1 } else { 2 }).as_bytes(),
            );
        }
        let codex_before = inventory(&places);
        let claude_inventory = || {
            failover::run(
                &json!({"version":1,"action":"inventory","provider":"claude",
                "homeDir":home.to_string_lossy(),"ccsDir":places.ccs.to_string_lossy()}),
            )
            .unwrap()
        };
        let claude_before = claude_inventory();
        assert_eq!(
            codex_before["candidates"].as_array().map(Vec::len),
            Some(2),
            "{codex_before}"
        );
        assert_eq!(
            claude_before["candidates"].as_array().map(Vec::len),
            Some(2),
            "{claude_before}"
        );

        let prepared = prepare(
            &places,
            true,
            &["party".into(), "me".into()],
            None,
            &codex_before,
            "2026-11-27T18:00:00Z",
        )
        .unwrap();
        let switch = prepared.switch.clone().unwrap();
        let t3_identity = codex_before["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["selectors"] == json!(["t3"]))
            .unwrap()["identity"]["key"]
            .clone();
        assert_eq!(
            json!(switch.codex.as_ref().unwrap()["work"].identity),
            t3_identity,
            "the pin is the account's identity"
        );
        let done = rollback::quarantine(
            &places,
            &prepared.planned,
            prepared.switch.as_ref(),
            "2026-11-27T18:00:00Z",
            "restore",
        )
        .unwrap();
        assert_eq!(done.moved.len(), 3);

        let codex_after = inventory(&places);
        let rows = codex_after["candidates"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "only the T3 instance's own login is left: {codex_after}"
        );
        assert_eq!(rows[0]["selectors"], json!(["t3"]));
        assert_eq!(
            claude_inventory()["candidates"].as_array().map(Vec::len),
            Some(0)
        );
        assert!(t3.exists(), "the T3 instance's own login is never touched");
        let held = std::fs::read_dir(places.folder()).unwrap().count();
        assert!(held >= 4, "three moved files and the record");
        let manifest = std::fs::read_to_string(places.folder().join("manifest.json")).unwrap();
        let switch_text = std::fs::read_to_string(places.ccs.join("logins-step3.json")).unwrap();
        for text in [&manifest, &switch_text] {
            assert!(!text.contains("TESTONLY"), "no secret in step 3 files");
        }

        let back = rollback::restore(&places).unwrap();
        assert_eq!(back.back.len(), 3);
        assert_eq!(inventory(&places), codex_before);
        assert_eq!(claude_inventory(), claude_before);
    }
}
