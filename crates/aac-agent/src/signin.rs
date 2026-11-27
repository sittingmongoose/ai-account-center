//! Read-only Claude login expiry and profile discovery. Credential values never enter replies.
use aac_core::{Cause, Failure, UtcMs};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
};

#[cfg(feature = "parity-fixtures")]
thread_local! { static FIXTURE_IO: std::cell::RefCell<Value> = const { std::cell::RefCell::new(Value::Null) }; }
fn fixture_io(section: &str, path: &Path) -> Option<Value> {
    #[cfg(feature = "parity-fixtures")]
    {
        FIXTURE_IO.with(|v| {
            v.borrow()
                .get(section)
                .and_then(|v| v.get(path.to_string_lossy().as_ref()))
                .cloned()
        })
    }
    #[cfg(not(feature = "parity-fixtures"))]
    {
        let _ = (section, path);
        None
    }
}

const SELECTORS: [&str; 6] = [
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "USE_LOCAL_OAUTH",
    "USE_STAGING_OAUTH",
    "CLAUDE_LOCAL_OAUTH_API_BASE",
    "CLAUDE_CODE_CUSTOM_OAUTH_URL",
];
const FIELDS: [&str; 12] = [
    "OPENAI_API_KEY",
    "auth_mode",
    "last_refresh",
    "tokens",
    "access_token",
    "refresh_token",
    "id_token",
    "account_id",
    "agent_identity",
    "personal_access_token",
    "bedrock_api_key",
    "bedrock_access_keys",
];

pub(crate) fn epoch(value: &Value) -> Option<i64> {
    if let Value::String(s) = value
        && s.bytes()
            .any(|c| !c.is_ascii_digit() && !b" \t\r\n".contains(&c))
    {
        crate::proof::reject();
    }
    let number = match value {
        Value::Number(n) => n
            .as_f64()
            .filter(|n| n.is_finite())
            .map(|n| n.trunc() as i64),
        Value::String(s)
            if !crate::proof::strip(s).is_empty()
                && crate::proof::strip(s).len() <= 16
                && crate::proof::strip(s).bytes().all(|c| c.is_ascii_digit()) =>
        {
            crate::proof::strip(s).parse().ok()
        }
        _ => None,
    }?;
    (1_000_000_000_000..=10_000_000_000_000)
        .contains(&number)
        .then_some(number)
}

/// `signin_expiry.iso`: `datetime.fromtimestamp(ms / 1000, utc)` to the millisecond, which divides in binary64 and
/// rounds to the microsecond first, so some far-future milliseconds print as the one before (8796093022210 is
/// `.209Z` in Python). `aac_num::py::iso_millis` reproduces that path; `epoch` admits only its proven domain.
fn iso(ms: Option<i64>) -> Value {
    ms.and_then(aac_num::py::iso_millis)
        .map(Value::String)
        .unwrap_or(Value::Null)
}

pub(crate) fn credentials(raw: &Value) -> Option<Value> {
    let obj = raw.as_object()?;
    let data = obj.get("claudeAiOauth").unwrap_or(raw).as_object()?;
    Some(
        json!({"access": epoch(data.get("expiresAt").unwrap_or(&Value::Null)), "refresh": epoch(data.get("refreshTokenExpiresAt").unwrap_or(&Value::Null)), "has_access": data.get("accessToken").and_then(Value::as_str).is_some_and(|s| !s.is_empty()), "has_refresh": data.get("refreshToken").and_then(Value::as_str).is_some_and(|s| !s.is_empty())}),
    )
}

pub(crate) fn capability(data: &Value, now: i64) -> (Option<i64>, &'static str) {
    let a = data["access"].as_i64();
    let r = data["refresh"].as_i64();
    let ha = data["has_access"] == true;
    let hr = data["has_refresh"] == true;
    if !ha && !hr {
        return (None, "logged_out");
    }
    if (ha && a.is_none()) || (hr && r.is_none()) {
        return (None, "unavailable");
    }
    if !hr {
        return (
            a,
            if a.is_some_and(|a| a <= now) {
                "logged_out"
            } else {
                "critical"
            },
        );
    }
    if !ha {
        return (
            None,
            if r.is_some_and(|r| r <= now) {
                "logged_out"
            } else {
                "warn"
            },
        );
    }
    let end = a.filter(|a| r.is_some_and(|r| *a >= r));
    if let Some(end) = end {
        return (
            Some(end),
            if end <= now {
                "logged_out"
            } else if end - now < 14_400_000 {
                "critical"
            } else if end - now < 86_400_000 {
                "warn"
            } else {
                "ok"
            },
        );
    }
    (
        None,
        if r.is_some_and(|r| r <= now) {
            "logged_out"
        } else if r.is_some_and(|r| r - now < 86_400_000) {
            "warn"
        } else {
            "ok"
        },
    )
}

fn normalized(path: &str) -> PathBuf {
    crate::proof::ascii(path);
    let text: String = path.into();
    let mut out = PathBuf::new();
    for part in Path::new(&text).components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !out.has_root() {
                    out.push("..");
                }
            }
            p => out.push(p.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        out
    }
}

fn real(path: &Path) -> Option<PathBuf> {
    if let Ok(p) = fs::canonicalize(path) {
        return Some(p);
    }
    let parent = path.parent()?;
    if parent == path {
        return None;
    }
    Some(real(parent)?.join(path.file_name()?))
}
fn allowed(path: &Path, home: &Path, ccs: &Path) -> bool {
    let Some(p) = real(path) else {
        return false;
    };
    [home, ccs]
        .iter()
        .filter_map(|p| real(p))
        .any(|r| p.starts_with(r))
}
fn dir_state(path: &Path) -> (bool, bool) {
    if let Some(state) = fixture_io("states", path) {
        return (state[0] == true, state[1] == true);
    }
    match fs::metadata(path) {
        Ok(m) => (m.is_dir(), false),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            let broken = path
                .ancestors()
                .find_map(|p| match fs::symlink_metadata(p) {
                    Ok(m) => Some(m.is_symlink() && fs::metadata(p).is_err()),
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                        ) =>
                    {
                        None
                    }
                    Err(_) => Some(true),
                })
                .unwrap_or(false);
            (false, broken)
        }
        Err(_) => (false, true),
    }
}
fn read(path: &Path) -> Option<Value> {
    if let Some(raw) = fixture_io("reads", path) {
        use base64::Engine;
        return raw
            .as_str()
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
            .and_then(|b| crate::proof::lenient_json(&b, 65536, false));
    }
    // Python's `_read_regular_bytes`: only an oversized file is unproved; anything else that is
    // not one ordinary file reads as absent.
    match crate::files::read_regular(path, 65536) {
        crate::files::Regular::Read(bytes, _) => crate::proof::lenient_json(&bytes, 65536, false),
        crate::files::Regular::TooLarge => {
            crate::proof::reject();
            None
        }
        _ => None,
    }
}
/// The T3 settings file as Python's `failover_core._read` reads it: through `_local_path`, then
/// `_read_present` (one ordinary file of at most 1 MiB that this user owns), then one JSON
/// object. Nothing there is silent; a file that is present but not read marks discovery
/// incomplete, as Python's `read_failed()` does.
fn read_settings(home: &Path, complete: &mut bool) -> Option<Value> {
    use aac_files::read::{Ancestors, Owner, Read, ReadPolicy};
    const CAP: usize = 1_048_576;
    let path = home.join(".t3/userdata/settings.json");
    // Python follows a link on this path only while every written target stays inside the home,
    // and otherwise reads no settings at all. Nothing here proves that chain, so a link anywhere
    // below the home, the file included, declines the verb (big review Wonder M1).
    let policy = ReadPolicy::capped(CAP as u64)
        .ancestors(Ancestors::NoLinkBelow(home))
        .owner(Owner::Mine);
    let value = match aac_files::read::read(&path, &policy) {
        // A denied ancestor chain cannot prove Python's named read would fail: decline the
        // whole verb. Where the chain open needs read access (no O_PATH/O_SEARCH), a leaf
        // PermissionDenied may be the chain's too, so it declines there as well.
        Read::Link | Read::Unproved | Read::ChainUnreadable(_) => {
            crate::proof::reject();
            return None;
        }
        Read::Unreadable(std::io::ErrorKind::PermissionDenied)
            if !aac_files::read::search_capable() =>
        {
            crate::proof::reject();
            return None;
        }
        Read::Absent | Read::Unreadable(std::io::ErrorKind::NotADirectory) => return None,
        Read::TooLarge => {
            crate::proof::reject();
            return None;
        }
        Read::Bytes(bytes, _) => crate::proof::lenient_json(&bytes, CAP, true),
        // Present but not one ordinary file of this user, or the leaf itself unreadable:
        // Python's `read_failed()` — discovery incomplete, the inventory still answers
        // (qwen lane 2 recheck L1).
        Read::Foreign
        | Read::Directory
        | Read::NotRegular
        | Read::BadMode
        | Read::Unreadable(_) => None,
    };
    if !value.as_ref().is_some_and(Value::is_object) {
        *complete = false;
    }
    value
}
fn json_bytes(bytes: &[u8]) -> Option<Value> {
    crate::proof::lenient_json(bytes, 65536, false)
}
fn display(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(p) if p.as_os_str().is_empty() => "~".into(),
        // Python's Path.as_posix(): only Windows separators become "/"; on POSIX a backslash is
        // an ordinary filename character and stays (final Sol max M1).
        Ok(p) if cfg!(windows) => format!("~/{}", p.to_string_lossy().replace('\\', "/")),
        Ok(p) => format!("~/{}", p.to_string_lossy()),
        Err(_) => path.to_string_lossy().into(),
    }
}
fn quote(raw: &str, home: &Path) -> String {
    let h = home.to_string_lossy();
    let suffix = if raw == h {
        Some("")
    } else {
        raw.strip_prefix(h.as_ref())
            .filter(|s| s.starts_with(['/', '\\']))
    };
    match suffix {
        None => format!("'{}'", raw.replace('\'', "'\\''")),
        Some(s) if s.contains('!') => format!("\"$HOME\"'{}'", s.replace('\'', "'\\''")),
        Some(s) => format!(
            "\"$HOME{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('$', "\\$")
                .replace('`', "\\`")
        ),
    }
}
fn win_command(overrides: &BTreeMap<&str, String>) -> String {
    let save = SELECTORS
        .iter()
        .enumerate()
        .map(|(i, n)| format!("$aac{i} = $env:{n}"))
        .collect::<Vec<_>>()
        .join("; ");
    let apply = SELECTORS
        .iter()
        .map(|n| {
            format!(
                "$env:{n} = {}",
                overrides.get(n).map(String::as_str).unwrap_or("$null")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let restore = SELECTORS
        .iter()
        .enumerate()
        .map(|(i, n)| format!("$env:{n} = $aac{i}"))
        .collect::<Vec<_>>()
        .join("; ");
    format!("{save}; {apply}; try {{ claude auth login }} finally {{ {restore} }}")
}
pub(crate) fn fix(
    raw: &str,
    home: &Path,
    kind: &str,
    platform: &str,
    env: &BTreeMap<String, String>,
) -> String {
    if kind == "default" {
        return if platform == "win32" {
            win_command(&BTreeMap::new())
        } else {
            format!("(unset {} && claude auth login)", SELECTORS.join(" "))
        };
    }
    let var = if kind == "storage-dir" {
        SELECTORS[1]
    } else {
        SELECTORS[0]
    };
    let absolute = |s: &str| {
        if platform == "win32" {
            s.starts_with(['/', '\\']) || s.as_bytes().get(1) == Some(&b':')
        } else {
            s.starts_with('/')
        }
    };
    let co = if kind == "storage-dir" {
        env.get(SELECTORS[0])
    } else {
        None
    };
    if !absolute(raw) || co.is_some_and(|s| !absolute(s)) {
        return String::new();
    }
    if platform == "win32" {
        if raw
            .chars()
            .chain(co.into_iter().flat_map(|s| s.chars()))
            .any(|c| ('\u{2018}'..='\u{201f}').contains(&c))
        {
            return String::new();
        }
        let home_expr = "$(if (Test-Path env:USERPROFILE) { $env:USERPROFILE } else { $env:HOMEDRIVE + $env:HOMEPATH })";
        let word = |s: &str, combined: bool| {
            let h = home.to_string_lossy();
            let suffix = if s == h {
                Some("")
            } else {
                s.strip_prefix(h.as_ref())
                    .filter(|s| s.starts_with(['/', '\\']))
            };
            match suffix {
                None => format!("'{}'", s.replace('\'', "''")),
                Some(s) if combined => format!("\"{home_expr}\" + '{}'", s.replace('\'', "''")),
                Some(s) => format!(
                    "\"{home_expr}{}\"",
                    s.replace('`', "``").replace('"', "`\"").replace('$', "`$")
                ),
            }
        };
        let native_raw = if kind == "dir-native" {
            raw.replace('/', "\\")
        } else {
            raw.to_string()
        };
        let native_home = home.to_string_lossy().replace('/', "\\");
        let rendered = if kind == "dir-native" && native_raw.starts_with(&native_home) {
            word(
                &format!(
                    "{}{}",
                    home.to_string_lossy(),
                    &native_raw[native_home.len()..]
                ),
                false,
            )
        } else {
            word(raw, false)
        };
        let mut overrides = BTreeMap::from([(var, rendered)]);
        if let Some(co) = co {
            overrides.insert(SELECTORS[0], word(co, true));
        }
        return win_command(&overrides);
    }
    let cleared = SELECTORS
        .iter()
        .filter(|n| **n != var && !(co.is_some() && **n == SELECTORS[0]))
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let co = co
        .map(|s| format!("CLAUDE_CONFIG_DIR={} ", quote(s, home)))
        .unwrap_or_default();
    format!(
        "(unset {cleared} && {var}={} {co}claude auth login)",
        quote(raw, home)
    )
}

#[derive(Clone)]
struct Root {
    name: String,
    path: PathBuf,
    kind: String,
    spelling: String,
}
fn identity(root: &Root, platform: &str) -> String {
    crate::proof::ascii(&root.spelling);
    crate::proof::ascii(&root.name);
    if platform == "darwin" {
        return format!(
            "{}:{}",
            if root.kind == "default" {
                "default"
            } else {
                "hashed"
            },
            root.spelling.clone()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = fs::metadata(&root.path)
            && m.ino() != 0
        {
            return format!("{}:{}", m.dev(), m.ino());
        }
    }
    real(&root.path)
        .unwrap_or_else(|| root.path.clone())
        .to_string_lossy()
        .into()
}
fn add(roots: &mut Vec<Root>, root: Root, platform: &str) {
    let key = identity(&root, platform);
    if let Some(i) = roots.iter().position(|r| identity(r, platform) == key) {
        if i > 0
            && fs::symlink_metadata(&roots[i].path).is_ok_and(|m| m.is_symlink())
            && !fs::symlink_metadata(&root.path).is_ok_and(|m| m.is_symlink())
        {
            roots[i] = root;
        }
    } else {
        roots.push(root);
    }
}
/// One folder's homes. `sibling` names the scope of `failover_core._children` (the CCS folder
/// for saved profiles, the home for T3 ones) when this pass stands in for it; `None` is the
/// collector's own scan, which keeps every folder.
fn scan(base: &Path, complete: &mut bool, sibling: Option<&Path>) -> Vec<PathBuf> {
    let (exists, failed) = dir_state(base);
    *complete &= !failed;
    if !exists {
        return vec![];
    }
    let Ok(entries) = aac_files::read::entries(base, 1024) else {
        *complete = false;
        return vec![];
    };
    let mut out = vec![];
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(aac_files::read::Listed::TooMany) => {
                *complete = false;
                break;
            }
            Err(aac_files::read::Listed::Entry(_)) => {
                *complete = false;
                continue;
            }
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || (sibling.is_some() && !crate::files::safe_name(&name)) {
            continue;
        }
        // `_children` sends a linked entry through `_local_path`: it is a sibling home only when
        // the link stays inside the scope and names a folder; one refused for leaving the scope
        // joins later, from the collector's own scan (big review Wonder M1).
        let candidate = match sibling {
            Some(scope) if entry.file_type().map_or(true, |t| t.is_symlink()) => {
                match crate::files::hop(scope, &entry.path()) {
                    crate::files::Hop::Inside(folder) => folder,
                    crate::files::Hop::Outside => false,
                    crate::files::Hop::Missing => {
                        *complete = false;
                        false
                    }
                    crate::files::Hop::Unproved => {
                        crate::proof::reject();
                        continue;
                    }
                }
            }
            _ => true,
        };
        let (is_dir, failed) = dir_state(&entry.path());
        *complete &= !failed;
        if is_dir && candidate {
            out.push(entry.path());
            if out.len() > 64 {
                out.truncate(64);
                *complete = false;
                // Python sorts the whole listing before it takes 64; this pass keeps the first
                // 64 found, so past the bound the chosen homes differ: fall back (big review MED-1).
                if sibling.is_some() {
                    crate::proof::reject();
                }
                break;
            }
        }
    }
    out.sort();
    out
}
fn roots(
    home: &Path,
    ccs: &Path,
    ambient: bool,
    platform: &str,
    env: &BTreeMap<String, String>,
) -> (Vec<Root>, bool) {
    let default = home.join(".claude");
    let mut complete = true;
    let make = |name: String, path: PathBuf, kind: &str| Root {
        spelling: path.to_string_lossy().into(),
        name,
        path,
        kind: if kind == "dir" {
            "dir-native".into()
        } else {
            kind.into()
        },
    };
    let mut roots = vec![make("default".into(), default.clone(), "default")];
    let (_, failed) = dir_state(&default);
    complete &= !failed;
    // Python admits a configured home by its spelling alone (`_settings_homes`,
    // `_launcher_spelling`); a home whose real path leaves the allowed roots still publishes its
    // own row, which `profile` refuses (big review Wonder M1).
    let admitted = |p: &Path| p.is_absolute() && (p.starts_with(home) || p.starts_with(ccs));
    #[cfg(not(feature = "parity-fixtures"))]
    let injected: Option<Value> = None;
    #[cfg(feature = "parity-fixtures")]
    let injected = FIXTURE_IO.with(|v| v.borrow().get("savedRoots").cloned());
    if let Some(saved) = injected {
        if let Some(pairs) = saved.as_array() {
            for pair in pairs {
                let raw = pair[0].as_str().unwrap_or("");
                let p = normalized(crate::proof::strip(raw));
                if !admitted(&p) {
                    complete = false;
                    continue;
                }
                let (exists, failed) = dir_state(&p);
                complete &= !failed;
                if failed || (!exists && !(platform == "darwin" && ambient)) {
                    continue;
                }
                let selectors = pair[1].as_array();
                if selectors.is_some_and(|s| s.iter().any(|v| v == "discover")) {
                    continue;
                }
                let name = selectors
                    .and_then(|s| {
                        s.iter().find_map(|v| {
                            v.as_str().and_then(|s| s.strip_prefix("claude-profile:"))
                        })
                    })
                    .map(str::to_string)
                    .unwrap_or_else(|| p.file_name().unwrap_or_default().to_string_lossy().into());
                let name = if p == default {
                    "default-configured".into()
                } else {
                    name
                };
                add(&mut roots, make(name, p, "dir"), platform);
            }
        } else {
            complete = false;
        }
    } else {
        // Sibling discovery order: saved profiles, T3 directories, configured homes.
        // `failover_core._children` reaches each folder through `_local_path` from its own
        // scope, and lists a linked folder under its target's spelling. That is not ported: a
        // link to a folder declines (big review Wonder M1). A dangling link or a link to a
        // file lists nothing in either scan.
        for (scope, base) in [
            (ccs, ccs.join("instances")),
            (home, home.join(".claude-t3")),
        ] {
            if crate::files::linked_within(scope, &base) && dir_state(&base).0 {
                crate::proof::reject();
            }
            for p in scan(&base, &mut complete, Some(scope)) {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into())
                    .unwrap_or_default();
                add(&mut roots, make(name, p, "dir"), platform);
            }
        }
        if let Some(settings) = read_settings(home, &mut complete)
            && let Some(instances) = settings.get("providerInstances").filter(|v| !v.is_null())
        {
            if let Some(instances) = instances.as_object() {
                if instances.len() > 64 {
                    complete = false;
                }
                for item in instances.values().take(64) {
                    if item.is_null() {
                        continue;
                    }
                    if !item.is_object() {
                        complete = false;
                        continue;
                    }
                    if item["driver"] != "claudeAgent" {
                        continue;
                    }
                    let config = &item["config"];
                    if !config.is_null() && !config.is_object() {
                        complete = false;
                        continue;
                    }
                    let v = &config["homePath"];
                    if v.is_null() {
                        continue;
                    }
                    let Some(raw) = v
                        .as_str()
                        .filter(|s| s.chars().count() <= 1024 && !s.contains('\0'))
                    else {
                        complete = false;
                        continue;
                    };
                    let raw = crate::proof::strip(raw);
                    if raw.is_empty() {
                        continue;
                    }
                    let expanded = if raw == "~" {
                        home.to_path_buf()
                    } else if let Some(s) =
                        raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\"))
                    {
                        home.join(s)
                    } else {
                        PathBuf::from(raw)
                    };
                    // pathlib keeps a leading "//" as its own root and refuses the home;
                    // normalizing it to "/" would admit it (big review MED-2).
                    if crate::files::double_root(&expanded.to_string_lossy()) {
                        continue;
                    }
                    let p = normalized(&expanded.to_string_lossy());
                    if !admitted(&p) {
                        complete = false;
                        continue;
                    }
                    let (is_dir, failed) = dir_state(&p);
                    complete &= !failed;
                    if failed || (!is_dir && !(platform == "darwin" && ambient)) {
                        continue;
                    }
                    let name = if p == default {
                        "default-configured".into()
                    } else {
                        p.file_name()
                            .map(|s| s.to_string_lossy().into())
                            .unwrap_or_default()
                    };
                    add(&mut roots, make(name, p, "dir"), platform);
                }
            } else {
                complete = false;
            }
        }
    }
    for base in [ccs.join("instances"), home.join(".claude-t3")] {
        for p in scan(&base, &mut complete, None) {
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().into())
                .unwrap_or_default();
            add(&mut roots, make(name, p, "dir"), platform);
        }
    }
    if ambient {
        let selector = if let Some(s) = env.get(SELECTORS[1]) {
            (!s.is_empty()).then_some((s, "storage-dir"))
        } else {
            env.get(SELECTORS[0]).map(|s| (s, "dir"))
        };
        if let Some((raw, kind)) = selector {
            // Same pathlib "//" root rule as configured homes (big review MED-2).
            crate::files::double_root(raw);
            let path = normalized(raw);
            let (exists, failed) = dir_state(&path);
            complete &= !failed;
            if !failed && (exists || platform == "darwin") {
                if allowed(&path, home, ccs) {
                    let name = path
                        .file_name()
                        .map(|s| s.to_string_lossy().into())
                        .unwrap_or_else(|| "ambient".into());
                    let mut root = make(name, path, kind);
                    root.spelling = raw.clone();
                    root.kind = kind.into();
                    add(&mut roots, root, platform);
                } else {
                    complete = false;
                }
            }
        }
    }
    if roots.len() > 64 {
        complete = false;
        roots.truncate(64);
    }
    (roots, complete)
}
fn apply(row: &mut Value, data: &Value, now: i64) {
    let (end, state) = capability(data, now);
    row["accessExpiresAt"] = if data["has_access"] == true {
        iso(data["access"].as_i64())
    } else {
        Value::Null
    };
    row["refreshExpiresAt"] = if data["has_refresh"] == true {
        iso(data["refresh"].as_i64())
    } else {
        Value::Null
    };
    row["loggedOutAt"] = iso(end);
    row["state"] = state.into();
    if state == "unavailable" {
        row["error"] = "expiry times unreadable".into();
    } else if let Some(o) = row.as_object_mut() {
        o.remove("error");
    }
}
fn profile(
    root: &Root,
    home: &Path,
    ccs: &Path,
    now: i64,
    platform: &str,
    env: &BTreeMap<String, String>,
) -> Value {
    let mut row = json!({"name":root.name,"configDir":display(&root.path,home),"provider":"claude","accessExpiresAt":null,"refreshExpiresAt":null,"loggedOutAt":null,"state":"unavailable","fixCommand":fix(&root.spelling,home,&root.kind,platform,env)});
    let dir = root.path.clone();
    if !allowed(&dir, home, ccs) {
        row["error"] = "profile directory resolves outside the allowed roots".into();
        row["fixCommand"] = "".into();
        return row;
    }
    let file = dir.join(".credentials.json");
    let data = allowed(&file, home, ccs)
        .then(|| read(&file))
        .flatten()
        .and_then(|v| credentials(&v));
    if let Some(data) = data {
        apply(&mut row, &data, now);
    } else {
        row["error"] = "credentials file missing or unreadable".into();
    }
    row
}
fn service(root: &Root, suffix: &str) -> String {
    if root.kind == "default" || root.spelling.is_empty() {
        format!("Claude Code{suffix}-credentials")
    } else {
        format!(
            "Claude Code{suffix}-credentials-{}",
            &aac_core::sha256_hex(root.spelling.clone().as_bytes())[..8]
        )
    }
}
fn keychain(service: &str, fixtures: Option<&Value>) -> Option<Value> {
    #[cfg(feature = "parity-fixtures")]
    if let Some(fixtures) = fixtures {
        return fixtures.get(service).filter(|v| !v.is_null()).cloned();
    }
    let _ = fixtures;
    native_keychain(service)
}
#[cfg(not(target_os = "macos"))]
fn native_keychain(_: &str) -> Option<Value> {
    None
}
#[cfg(target_os = "macos")]
fn native_keychain(service: &str) -> Option<Value> {
    use security_framework::{os::macos::keychain::SecKeychain, passwords::get_generic_password};
    let Ok(_interaction_guard) = SecKeychain::disable_user_interaction() else {
        return Some(json!({"status":"indeterminate"}));
    };
    let user = std::env::var("USER")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::process::Command::new("/usr/bin/id")
                .arg("-un")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| crate::proof::strip(&s).to_string())
        })
        .filter(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        })
        .unwrap_or_else(|| "claude-code-user".into());
    match get_generic_password(service, &user) {
        Ok(bytes) if bytes.len() > 65536 => Some(json!({"status":"oversized"})),
        Ok(bytes) => Some(json!({"status":"payload","text":String::from_utf8_lossy(&bytes)})),
        Err(e) if e.code() == -25300 => None,
        Err(_) => Some(json!({"status":"locked"})),
    }
}
fn merge_keychain(row: &mut Value, candidate: &Value, now: i64) -> (bool, bool) {
    for k in ["accessExpiresAt", "refreshExpiresAt", "loggedOutAt"] {
        row[k] = Value::Null;
    }
    row["state"] = "unavailable".into();
    match candidate["status"].as_str() {
        Some("locked") => {
            row["error"] = "keychain locked or access not allowed".into();
            (false, true)
        }
        Some("indeterminate") => {
            row["error"] = "keychain state could not be checked".into();
            (false, true)
        }
        Some("oversized") => {
            crate::proof::reject();
            row["error"] = "keychain item unreadable".into();
            (true, false)
        }
        _ => {
            let data = candidate["text"]
                .as_str()
                .and_then(|s| json_bytes(s.as_bytes()))
                .and_then(|v| credentials(&v));
            if let Some(data) = data {
                apply(row, &data, now);
            } else {
                row["error"] = "keychain item holds no readable credentials".into();
            }
            (true, false)
        }
    }
}
fn codex(home: &Path) -> Option<Value> {
    let p = home.join(".codex/auth.json");
    if fs::symlink_metadata(&p).is_err() && !dir_state(&p).1 {
        return None;
    }
    let unavailable = json!({"name":"codex","provider":"codex","configDir":"~/.codex/auth.json","state":"unavailable","error":"auth file missing or unreadable","fixCommand":""});
    if !allowed(&p, home, home) {
        return Some(unavailable);
    }
    let Some(value) = read(&p) else {
        return Some(unavailable);
    };
    let mut keys = value
        .as_object()
        .map(|o| {
            o.keys()
                .filter(|s| FIELDS.contains(&s.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    keys.sort();
    Some(
        json!({"name":"codex","provider":"codex","configDir":"~/.codex/auth.json","state":"unsupported","fieldsAvailable":keys,"fixCommand":""}),
    )
}

pub(crate) fn run(args: &Value) -> Result<Value, Failure> {
    let mut failure = None;
    let value = crate::proof::scope(|| match run_inner(args) {
        Ok(v) => v,
        Err(e) => {
            failure = Some(e);
            Value::Null
        }
    });
    match failure {
        Some(e) => Err(e),
        None => Ok(value),
    }
}
fn run_inner(args: &Value) -> Result<Value, Failure> {
    if !crate::proof::proven(args) {
        return Ok(crate::proof::fallback());
    }
    #[cfg(feature = "parity-fixtures")]
    {
        FIXTURE_IO.with(|v| *v.borrow_mut() = args.get("io").cloned().unwrap_or(Value::Null));
        if args["op"] == "fix" {
            let env = args["environment"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let raw = args["spelling"]
                .as_str()
                .or_else(|| args["path"].as_str())
                .unwrap_or("");
            let kind = match args["storageVar"].as_str() {
                Some("CLAUDE_SECURESTORAGE_CONFIG_DIR") => "storage-dir",
                _ if args["path"].is_null() => "default",
                _ => "dir",
            };
            let kind = if kind == "dir" && args["spelling"].is_null() {
                "dir-native"
            } else {
                kind
            };
            return Ok(Value::String(fix(
                raw,
                Path::new(args["home"].as_str().unwrap_or("/")),
                kind,
                args["platform"].as_str().unwrap_or("linux"),
                &env,
            )));
        }
    }
    let obj = args
        .as_object()
        .ok_or_else(|| Failure::new(Cause::Refused, "invalid signin-expiry arguments"))?;
    let permitted = ["home", "ccsDir", "nowMs"];
    for k in obj.keys() {
        #[cfg(feature = "parity-fixtures")]
        if [
            "platform",
            "environment",
            "keychain",
            "includeKeychain",
            "io",
        ]
        .contains(&k.as_str())
        {
            continue;
        }
        if !permitted.contains(&k.as_str()) {
            return Err(Failure::new(
                Cause::Refused,
                "unsupported signin-expiry argument",
            ));
        }
    }
    let fixture = args.get("home").is_some();
    let home = match args.get("home") {
        Some(Value::String(s)) if Path::new(s).is_absolute() => PathBuf::from(s),
        None => std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or_else(|| Failure::new(Cause::NotInstalled, "home directory unavailable"))?,
        _ => return Ok(crate::proof::fallback()),
    };
    let ccs = match args.get("ccsDir") {
        Some(Value::String(s)) if Path::new(s).is_absolute() => PathBuf::from(s),
        Some(_) => return Ok(crate::proof::fallback()),
        None if fixture => home.join(".ccs"),
        None => std::env::var_os("CCS_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("CCS_HOME")
                    .filter(|s| !s.is_empty())
                    .map(|p| PathBuf::from(p).join(".ccs"))
            })
            .unwrap_or_else(|| home.join(".ccs")),
    };
    // Python reads both as pathlib paths, which rewrite other spellings; repair commands and
    // displayed paths use this text, so only already-canonical spellings are proved.
    if !home.to_str().is_some_and(crate::files::canonical_home)
        || !ccs.to_str().is_some_and(crate::files::canonical_home)
    {
        return Ok(crate::proof::fallback());
    }
    let now = match args.get("nowMs") {
        None => i64::try_from(UtcMs::now()?.get())
            .map_err(|_| Failure::new(Cause::Bug, "clock unavailable"))?,
        Some(v) => epoch(v).ok_or_else(|| Failure::new(Cause::Refused, "invalid nowMs"))?,
    };
    #[allow(unused_mut)]
    let mut platform = if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    #[allow(unused_mut)]
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    if !fixture {
        for name in SELECTORS {
            let Some(value) = std::env::var_os(name) else {
                continue;
            };
            // Python reads a selector that is not UTF-8 as text (surrogateescape) and keeps it
            // as an identity; this port only spells text, so it declines (big review Wonder L8).
            let Some(value) = value.to_str() else {
                return Ok(crate::proof::fallback());
            };
            env.insert(name.to_string(), value.to_string());
        }
    }
    #[allow(unused_mut)]
    let mut ambient = !fixture;
    #[cfg(feature = "parity-fixtures")]
    {
        if let Some(p) = args["platform"].as_str() {
            platform = p;
        }
        if let Some(e) = args["environment"].as_object() {
            env = e
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.into())))
                .collect();
        }
        if args["includeKeychain"] == true {
            ambient = true;
        }
    }
    if env.values().any(|s| !crate::proof::ascii(s)) {
        return Ok(crate::proof::fallback());
    }
    let (roots, mut complete) = roots(&home, &ccs, ambient, platform, &env);
    #[cfg(feature = "parity-fixtures")]
    {
        complete &= args["io"]["discoveryIncomplete"] != true;
    }
    let mut rows = Vec::new();
    let mut variants = Vec::new();
    for (i, root) in roots.iter().enumerate() {
        let mut row = profile(root, &home, &ccs, now, platform, &env);
        let mut evidence = false;
        let mut unchecked = false;
        if ambient
            && platform == "darwin"
            && row["error"] != "profile directory resolves outside the allowed roots"
        {
            if let Some(candidate) = keychain(&service(root, ""), args.get("keychain")) {
                (evidence, unchecked) = merge_keychain(&mut row, &candidate, now);
            }
            if env.get(SELECTORS[5]).is_some_and(|s| !s.is_empty())
                && let Some(candidate) =
                    keychain(&service(root, "-custom-oauth"), args.get("keychain"))
            {
                let mut variant = row.clone();
                let (seen, _) = merge_keychain(&mut variant, &candidate, now);
                if seen {
                    variant["name"] = format!("{}-custom-oauth", root.name).into();
                    variant["fixCommand"] = "".into();
                    variants.push(variant);
                }
            }
        }
        if ambient
            && platform == "darwin"
            && i > 0
            && !dir_state(&normalized(&root.spelling)).0
            && !evidence
            && row["state"] == "unavailable"
        {
            if unchecked {
                complete = false;
            }
            continue;
        }
        rows.push(row);
    }
    rows.extend(variants);
    if !complete {
        rows.push(json!({"name":"saved-profiles","provider":"claude","configDir":"discovery","accessExpiresAt":null,"refreshExpiresAt":null,"loggedOutAt":null,"state":"unavailable","fixCommand":"","error":"profile discovery was incomplete; this machine's list may be missing homes"}));
    }
    if let Some(row) = codex(&home) {
        rows.push(row);
    }
    let reserved: HashSet<String> = rows
        .iter()
        .filter_map(|r| r["name"].as_str().map(str::to_owned))
        .collect();
    let mut used = HashSet::new();
    for row in &mut rows {
        let base = row["name"].as_str().unwrap_or("unnamed").to_string();
        if !used.insert(base.clone()) {
            let mut n = 2;
            while reserved.contains(&format!("{base}-{n}")) || used.contains(&format!("{base}-{n}"))
            {
                n += 1;
            }
            let name = format!("{base}-{n}");
            used.insert(name.clone());
            row["name"] = name.into();
        }
    }
    Ok(json!({"schemaVersion":1,"generatedAt":iso(Some(now)),"profiles":rows}))
}
