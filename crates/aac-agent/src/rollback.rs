//! Central Logins step 3 on one computer: `aac-agent --logins-quarantine`, `--logins-restore` and
//! `--logins-status` (the owner or the lead runs them over ssh; the dashboard never does).
//!
//! ```text
//! aac-agent --logins-quarantine [--codex] [--claude <profile,...>] [--claude-renewal-host <name>] [--dry-run]
//! aac-agent --logins-restore
//! aac-agent --logins-status
//! ```
//!
//! - `--codex` moves every AAC saved Codex copy (`<ccs>/codex-instances/<name>/auth.json`) and
//!   switches the dashboard on this computer to read each Codex account only through a T3
//!   instance's own login with the identity the saved copy proved (pinned before the move).
//! - `--claude a,b` moves those Claude sign-in files (`default` is `~/.claude`, others are
//!   `~/.claude-t3/<profile>`). Name only profiles that already run on a static token.
//! - `--claude-renewal-host <name>` (on the dashboard computer) cuts Claude renewal to that
//!   computer's sign-ins, one per account. It never goes with `--claude` in the same run: the
//!   renewal computer keeps its full sign-ins.
//!
//! Nothing is deleted. The quarantine prints the one command that puts everything back.
use crate::failover;
use aac_files::login_rollback::{
    self as rollback, CodexPin, Kind, Manifest, Places, Planned, Refused, Switch,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

/// The parsed quarantine request.
#[derive(Debug, Default, PartialEq, Eq)]
struct Request {
    codex: bool,
    claude: Vec<String>,
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
                request.claude = list.split(',').map(str::to_owned).collect();
                if request.claude.iter().any(|p| !rollback::valid_name(p)) {
                    return Err("--claude takes profile names like default,party".into());
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
    if !request.claude.is_empty() && request.renewal_host.is_some() {
        return Err(
            "the Claude renewal computer keeps its sign-ins: run --claude on the other computers"
                .into(),
        );
    }
    Ok(request)
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
                dry_run: false
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
            "--claude default --claude-renewal-host ubuntu",
            "--purge",
        ] {
            assert!(parse(&words(bad)).is_err(), "{bad}");
        }
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
