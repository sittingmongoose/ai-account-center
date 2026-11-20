//! M4b `claude-host`: the Claude profile host steps (create, undo, state, session, trash,
//! restore, purge, open) as one agent verb, replacing the ssh bootstraps of
//! `claude-host-transport.ts` and the Open commands of `claude-desktop-transport.ts`.
//! The Mac backend is a native port of the fixed Python program (`claude-host-mac-program.ts`);
//! the Windows backend generates the reference's fixed PowerShell byte for byte and runs it
//! locally behind the same encoded bootstrap. Guards stay in the front door: `purge` also
//! needs the TS-issued permit (`permit.rs`), and every unproven input delegates to the
//! labelled reference transport.
mod categories;
mod mac;
mod permit;
mod windows;

use crate::proof;
use aac_core::{Cause, Failure, UtcMs};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Create,
    Undo,
    State,
    Session,
    Trash,
    Restore,
    Purge,
    Open,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Host {
    Mac,
    Windows,
}

impl Host {
    fn word(self) -> &'static str {
        match self {
            Host::Mac => "mac",
            Host::Windows => "windows",
        }
    }
}

/// The launcher record's path fields, after `parseLauncher`'s shape checks. The name and
/// ssh alias are validated and dropped: no backend reads them.
pub(crate) struct Launcher {
    pub launcher_path: Option<String>,
    pub start_menu_path: Option<String>,
    pub profile_path: String,
}

pub(crate) struct HostRequest {
    pub op: Op,
    pub host: Host,
    pub profile_id: Option<String>,
    pub trash_name: Option<String>,
    pub launcher: Option<Launcher>,
    pub account_list: Option<Vec<String>>,
    pub token: Option<String>,
    pub home: Option<PathBuf>,
    pub now_ms: Option<u64>,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub ps_output: Option<String>,
}

fn refused(message: &str) -> Failure {
    Failure::new(Cause::Refused, message)
}

/// `CLAUDE_PROFILE_ID`: `^[a-z][a-z0-9-]{1,31}$`.
pub(crate) fn profile_id_valid(id: &str) -> bool {
    let bytes = id.as_bytes();
    (2..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `TRASH_NAME`: `^[a-z][a-z0-9-]{1,31}-\d{8}T\d{6}Z$` (ASCII digits, as JS `\d`).
pub(crate) fn trash_name_valid(name: &str) -> bool {
    let Some((prefix, stamp)) = name.rsplit_once('-') else {
        return false;
    };
    if !profile_id_valid(prefix) {
        return false;
    }
    let stamp = stamp.as_bytes();
    stamp.len() == 16
        && stamp[8] == b'T'
        && stamp[15] == b'Z'
        && stamp[..8]
            .iter()
            .chain(stamp[9..15].iter())
            .all(|c| c.is_ascii_digit())
}

/// `CLAUDE_PROFILE_ID_PATTERN` (the desktop Open and account list): `^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$`.
pub(crate) fn desktop_profile_id_valid(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
}

/// `SSH_ALIAS`: `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`.
fn ssh_alias_valid(alias: &str) -> bool {
    let bytes = alias.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(c))
}

fn in_ranges(cp: u32, ranges: &[(u32, u32)]) -> bool {
    ranges
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `SAFE_PATH`: `^[\p{L}\p{M}\p{N} _.,()[\]{}\\/:+@#&=!%~;-]{1,1024}$` with the categories
/// the Node engine defines (the generated tables), counted in code points as `/u` does.
pub(crate) fn safe_path(value: &str) -> bool {
    let mut count = 0usize;
    for c in value.chars() {
        count += 1;
        if count > 1024 {
            return false;
        }
        let cp = c as u32;
        let admitted = if cp < 128 {
            c.is_ascii_alphanumeric() || b" _.,()[]{}\\/:+@#&=!%~;-".contains(&(cp as u8))
        } else {
            in_ranges(cp, categories::LETTER)
                || in_ranges(cp, categories::MARK)
                || in_ranges(cp, categories::NUMBER)
        };
        if !admitted {
            return false;
        }
    }
    count >= 1
}

/// `parseLauncher`'s `text`: non-blank, bounded in UTF-16 units, no C0 controls or DEL.
fn launcher_text(value: &Value, max: usize) -> Option<String> {
    let text = value.as_str()?;
    let trimmed = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if trimmed.is_empty() || text.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f) {
        return None;
    }
    let units: usize = text.chars().map(|c| c.len_utf16()).sum();
    if units > max {
        return None;
    }
    Some(text.to_string())
}

fn parse_launcher(value: &Value) -> Result<Launcher, Failure> {
    let map = value
        .as_object()
        .ok_or_else(|| refused("Claude launcher record is not valid."))?;
    for key in map.keys() {
        if !matches!(
            key.as_str(),
            "launcherName" | "launcherPath" | "profilePath" | "startMenuPath" | "sshHost"
        ) {
            return Err(refused("Claude launcher record is not valid."));
        }
    }
    let name = map.get("launcherName").and_then(|v| launcher_text(v, 255));
    let profile_path = map.get("profilePath").and_then(|v| launcher_text(v, 4096));
    let ssh_host = map
        .get("sshHost")
        .and_then(Value::as_str)
        .filter(|h| ssh_alias_valid(h));
    if name.is_none() || profile_path.is_none() || ssh_host.is_none() {
        return Err(refused("Claude launcher record is not valid."));
    }
    let mut launcher = Launcher {
        launcher_path: None,
        start_menu_path: None,
        profile_path: profile_path.unwrap_or_default(),
    };
    for (key, slot) in [
        ("launcherPath", &mut launcher.launcher_path),
        ("startMenuPath", &mut launcher.start_menu_path),
    ] {
        if let Some(value) = map.get(key)
            && !value.is_null()
        {
            *slot = Some(
                launcher_text(value, 4096)
                    .ok_or_else(|| refused("Claude launcher record is not valid."))?,
            );
        }
    }
    Ok(launcher)
}

fn parse_op(value: &Value) -> Result<Op, Failure> {
    match value.as_str() {
        Some("create") => Ok(Op::Create),
        Some("undo") => Ok(Op::Undo),
        Some("state") => Ok(Op::State),
        Some("session") => Ok(Op::Session),
        Some("trash") => Ok(Op::Trash),
        Some("restore") => Ok(Op::Restore),
        Some("purge") => Ok(Op::Purge),
        Some("open") => Ok(Op::Open),
        // The reference program's final sys.exit(2).
        _ => Err(Failure::new(Cause::Damaged, "A Claude host step failed.")),
    }
}

fn parse_request(args: &Value) -> Result<HostRequest, Failure> {
    let map = args
        .as_object()
        .ok_or_else(|| refused("invalid claude-host request"))?;
    let mut req = HostRequest {
        op: Op::Create,
        host: Host::Mac,
        profile_id: None,
        trash_name: None,
        launcher: None,
        account_list: None,
        token: None,
        home: None,
        now_ms: None,
        ps_output: None,
    };
    let mut op = None;
    let mut host = None;
    for (key, value) in map {
        match key.as_str() {
            "op" => op = Some(parse_op(value)?),
            "host" => {
                host = Some(match value.as_str() {
                    Some("mac") => Host::Mac,
                    Some("windows") => Host::Windows,
                    _ => return Err(refused("invalid claude-host request")),
                });
            }
            "profileId" | "trashName" | "token" => {
                let text = value
                    .as_str()
                    .ok_or_else(|| refused("invalid claude-host request"))?;
                match key.as_str() {
                    "profileId" => req.profile_id = Some(text.to_string()),
                    "trashName" => req.trash_name = Some(text.to_string()),
                    _ => req.token = Some(text.to_string()),
                }
            }
            "launcher" => req.launcher = Some(parse_launcher(value)?),
            "accountList" => {
                let list = value
                    .as_array()
                    .ok_or_else(|| refused("invalid claude-host request"))?;
                let mut ids = Vec::new();
                for id in list {
                    ids.push(
                        id.as_str()
                            .ok_or_else(|| refused("invalid claude-host request"))?
                            .to_string(),
                    );
                }
                req.account_list = Some(ids);
            }
            #[cfg(feature = "parity-fixtures")]
            "home" => {
                let text = value
                    .as_str()
                    .ok_or_else(|| refused("invalid claude-host fixture"))?;
                let home = PathBuf::from(text);
                if !home.is_absolute() {
                    return Err(refused("invalid claude-host fixture"));
                }
                req.home = Some(home);
            }
            #[cfg(feature = "parity-fixtures")]
            "nowMs" => {
                req.now_ms = Some(
                    value
                        .as_u64()
                        .ok_or_else(|| refused("invalid claude-host fixture"))?,
                );
            }
            #[cfg(feature = "parity-fixtures")]
            "psOutput" => {
                req.ps_output = Some(
                    value
                        .as_str()
                        .ok_or_else(|| refused("invalid claude-host fixture"))?
                        .to_string(),
                );
            }
            _ => {
                proof::reject();
                return Err(refused("invalid claude-host request"));
            }
        }
    }
    req.op = op.ok_or_else(|| refused("invalid claude-host request"))?;
    req.host = host.ok_or_else(|| refused("invalid claude-host request"))?;
    Ok(req)
}

pub(crate) fn run(args: &Value) -> Result<Value, Failure> {
    // The proof scope covers the whole verb: a reject anywhere (an unknown key, an
    // unproven spelling, a Python-only JSON number) delegates it to the reference.
    let mut outcome: Result<Value, Failure> = Ok(json!({}));
    let value = proof::scope(|| {
        outcome = run_inner(args);
        outcome.clone().unwrap_or_else(|_| json!({}))
    });
    if value == proof::fallback() {
        Ok(proof::fallback())
    } else {
        outcome
    }
}

fn run_inner(args: &Value) -> Result<Value, Failure> {
    let req = parse_request(args)?;
    let now_ms = match req.now_ms {
        Some(now) => now,
        None => UtcMs::now()?.get(),
    };
    // The purge permit is checked before any path is touched, on every platform.
    if req.op == Op::Purge {
        permit::verify(
            req.token.as_deref(),
            req.host.word(),
            req.trash_name.as_deref().unwrap_or(""),
            now_ms,
        )?;
    }
    let fixture_home = req.home.as_deref();
    match req.host {
        Host::Mac => {
            if fixture_home.is_none() && !cfg!(target_os = "macos") {
                return Err(refused("claude-host does not match this computer"));
            }
            let home = match fixture_home {
                Some(home) => home.to_path_buf(),
                None => std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|home| home.is_absolute())
                    .ok_or_else(|| refused("claude host home is unavailable"))?,
            };
            mac::run(&req, &home)
        }
        Host::Windows => {
            if fixture_home.is_none() && !cfg!(windows) {
                return Err(refused("claude-host does not match this computer"));
            }
            // Fixture homes cannot carry the shell-folder, COM and scheduled-task steps:
            // those read the real Desktop/Programs folders and the real task store. The
            // spec captures them as generated text; session, state and purge run for real.
            if fixture_home.is_some()
                && matches!(
                    req.op,
                    Op::Create | Op::Undo | Op::Trash | Op::Restore | Op::Open
                )
            {
                return Err(refused("fixture mode cannot run shell-folder steps"));
            }
            windows::run(&req, fixture_home)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_trash_patterns_match_the_reference() {
        assert!(profile_id_valid("work2"));
        assert!(profile_id_valid("ab"));
        assert!(!profile_id_valid("a"));
        assert!(!profile_id_valid("A1"));
        assert!(!profile_id_valid("1abc"));
        assert!(profile_id_valid(&format!("a{}", "b".repeat(31))));
        assert!(!profile_id_valid(&format!("a{}", "b".repeat(32))));
        assert!(trash_name_valid("work2-20261002T080000Z"));
        assert!(!trash_name_valid("work2-20261002T08000Z"));
        assert!(!trash_name_valid("work2-20261002t080000Z"));
        assert!(!trash_name_valid("../outside"));
        assert!(!trash_name_valid("work2-20261002T080000"));
        assert!(desktop_profile_id_valid("Store"));
        assert!(desktop_profile_id_valid("a_b-c9"));
        assert!(!desktop_profile_id_valid("-x"));
        assert!(!desktop_profile_id_valid(&"x".repeat(65)));
    }

    #[test]
    fn safe_path_matches_the_ts_allowlist() {
        assert!(safe_path("C:\\Users\\x\\AppData\\Roaming\\Claude-party"));
        assert!(safe_path("C:\\Users\\José\\AppData\\Roaming\\Claude-party"));
        assert!(safe_path(&format!("C:\\Users\\{}\\x", "张".repeat(200))));
        assert!(safe_path("a\u{301}")); // combining mark
        assert!(safe_path("Ⅻ")); // number letter (Nl)
        assert!(!safe_path("C:\\x\\Claude-a'; Remove-Item C:\\"));
        assert!(!safe_path("C:\\x\\$env:TEMP"));
        assert!(!safe_path("C:\\x\\`n"));
        for c in [
            '\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}', '\u{201c}', '\u{201d}',
        ] {
            assert!(!safe_path(&format!("C:\\x\\Claude-a{c}")), "{c:?}");
        }
        assert!(!safe_path("C:\\x\\Claude\r\nexit"));
        assert!(!safe_path(""));
        assert!(!safe_path(&"x".repeat(1025)));
        assert!(safe_path(&"x".repeat(1024)));
        // Astral letters count one code point each, as /u quantifiers do.
        assert!(safe_path(&"𐀀".repeat(1024)));
        assert!(!safe_path(&"𐀀".repeat(1025)));
    }

    #[test]
    fn launcher_record_shape_matches_parse_launcher() {
        let good = json!({
            "launcherName": "Claude (party)",
            "launcherPath": "C:\\Users\\x\\Desktop\\Claude (party).lnk",
            "startMenuPath": "C:\\Users\\x\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Claude (party).lnk",
            "profilePath": "C:\\Users\\x\\AppData\\Roaming\\Claude-party",
            "sshHost": "windows-host"
        });
        let launcher = parse_launcher(&good).expect("good record");
        assert_eq!(
            launcher.profile_path,
            "C:\\Users\\x\\AppData\\Roaming\\Claude-party"
        );
        assert!(launcher.launcher_path.is_some());
        assert!(launcher.start_menu_path.is_some());
        for bad in [
            json!({"launcherName": " ", "profilePath": "p", "sshHost": "h"}),
            json!({"launcherName": "n", "profilePath": "p", "sshHost": "bad host"}),
            json!({"launcherName": "n", "profilePath": "p", "sshHost": "h", "extra": 1}),
            json!({"launcherName": "n", "profilePath": "p\u{0}", "sshHost": "h"}),
        ] {
            assert!(parse_launcher(&bad).is_err(), "{bad}");
        }
    }

    /// The M4b gate: every generated script, Open command, list script and permit the
    /// real TS builders wrote must equal the port, byte for byte.
    #[test]
    fn claude_host_table_matches_the_reference() {
        let table: Value = serde_json::from_str(include_str!(
            "../../../tests/spec/agent/claude-host-table.json"
        ))
        .expect("claude-host table parses");

        let boot = table["bootstrap"]["windows"].as_str().expect("bootstrap");
        let b64 = boot.rsplit(' ').next().expect("bootstrap argv");
        assert_eq!(windows::encoded_command(windows::BOOTSTRAP_TEXT), b64);

        let scripts = table["scripts"].as_array().expect("scripts");
        assert!(scripts.len() >= 40);
        for case in scripts {
            let name = case["name"].as_str().expect("name");
            let op = parse_op(&case["op"]).expect("op");
            let values = &case["values"];
            let launcher = values.get("launcher").map(|l| Launcher {
                launcher_path: l
                    .get("launcherPath")
                    .and_then(Value::as_str)
                    .map(String::from),
                start_menu_path: l
                    .get("startMenuPath")
                    .and_then(Value::as_str)
                    .map(String::from),
                profile_path: l["profilePath"].as_str().expect("profilePath").to_string(),
            });
            let result = windows::windows_host_script(
                op,
                values.get("profileId").and_then(Value::as_str),
                values.get("trashName").and_then(Value::as_str),
                launcher.as_ref(),
            );
            match &case["expected"] {
                Value::Object(o) if o.get("error") == Some(&json!(true)) => {
                    assert!(result.is_err(), "{name}: must refuse");
                }
                Value::String(text) => {
                    assert_eq!(
                        &result.unwrap_or_else(|_| panic!("{name}: refused")),
                        text,
                        "{name}"
                    );
                }
                other => panic!("{name}: bad expected {other}"),
            }
        }

        for case in table["open"].as_array().expect("open") {
            let name = case["name"].as_str().expect("name");
            let result = match case["kind"].as_str() {
                Some("mac") => mac::mac_open_command(case["launcherPath"].as_str().unwrap_or("")),
                Some("windows") => {
                    let list: Option<Vec<String>> = case["accountList"].as_array().map(|ids| {
                        ids.iter()
                            .map(|id| id.as_str().expect("id").to_string())
                            .collect()
                    });
                    windows::windows_open_command(
                        case["profileId"].as_str().expect("id"),
                        list.as_deref(),
                    )
                }
                other => panic!("{name}: bad kind {other:?}"),
            };
            match &case["expected"] {
                Value::Object(o) if o.get("error") == Some(&json!(true)) => {
                    assert!(result.is_err(), "{name}: must refuse");
                }
                Value::String(text) => {
                    assert_eq!(
                        &result.unwrap_or_else(|_| panic!("{name}: refused")),
                        text,
                        "{name}"
                    );
                }
                other => panic!("{name}: bad expected {other}"),
            }
        }

        for case in table["accountList"].as_array().expect("accountList") {
            let name = case["name"].as_str().expect("name");
            let ids: Vec<String> = case["ids"]
                .as_array()
                .expect("ids")
                .iter()
                .map(|id| id.as_str().expect("id").to_string())
                .collect();
            let result = windows::windows_account_list_script(&ids);
            match &case["expected"] {
                Value::Object(o) if o.get("error") == Some(&json!(true)) => {
                    assert!(result.is_err(), "{name}: must refuse");
                }
                Value::Array(lines) => {
                    let want: Vec<String> = lines
                        .iter()
                        .map(|l| l.as_str().expect("line").to_string())
                        .collect();
                    assert_eq!(
                        result.unwrap_or_else(|_| panic!("{name}: refused")),
                        want,
                        "{name}"
                    );
                }
                other => panic!("{name}: bad expected {other}"),
            }
        }

        for case in table["permits"].as_array().expect("permits") {
            let host = case["host"].as_str().expect("host");
            let trash = case["trashName"].as_str().expect("trashName");
            let now = case["now"].as_u64().expect("now");
            // The reference issues at now + CONFIRMATION_TTL_MS (120 s).
            assert_eq!(
                permit::issue(host, trash, now + 120_000).as_deref(),
                case["token"].as_str(),
                "permit {host}/{trash}"
            );
            assert!(permit::verify(case["token"].as_str(), host, trash, now).is_ok());
        }
    }
}
