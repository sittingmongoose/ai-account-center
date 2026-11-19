//! The Mac host steps, a native port of `claude-host-mac-program.ts`'s fixed Python program
//! (create, undo, state, session, trash, restore, purge). Data folders are direct children of
//! `~/Library/Application Support` named `Claude*`, launchers direct children of
//! `~/Applications` named `Claude (...).app`, trash folders live only in `~/.ccs/trash/claude`.
//! The parity harness runs the reference program and this port on the same fixture homes and
//! compares the answers and the resulting trees byte for byte (the launch script and the
//! Info.plist included). Exit 1 of the reference is `Refused`, exit 3 `InUse`.
use super::HostRequest;
use aac_core::{Cause, Failure};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

fn refused() -> Failure {
    Failure::new(Cause::Refused, "A Claude host step failed.")
}

fn exists_conflict() -> Failure {
    Failure::new(Cause::InUse, "The Claude profile already exists.")
}

/// `pathlib.Path(value)`'s lexical normalization: '.' and duplicate separators collapse,
/// '..' stays, the root/drive is kept. Rust's components do exactly this on both platforms.
pub(crate) fn py_path(raw: &str) -> PathBuf {
    let path = Path::new(raw);
    let mut out = PathBuf::new();
    for component in path.components() {
        out.push(component.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `Path.__eq__`: exact on POSIX, case-insensitive on Windows (WindowsPath casefolds).
fn path_eq(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn lexists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

fn exists(path: &Path) -> bool {
    fs::metadata(path).is_ok()
}

fn is_dir(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

fn is_file(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

/// `shutil.rmtree`: never follows a link, removes the tree depth-first. `ignore_errors`
/// swallows every failure as the reference's calls do; otherwise the first error surfaces.
fn remove_tree(path: &Path, ignore_errors: bool) -> Result<(), ()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(_) => return if ignore_errors { Ok(()) } else { Err(()) },
    };
    if meta.is_dir() && !meta.file_type().is_symlink() {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_) => return if ignore_errors { Ok(()) } else { Err(()) },
        };
        for entry in entries.flatten() {
            if remove_tree(&entry.path(), ignore_errors).is_err() && !ignore_errors {
                return Err(());
            }
        }
        match fs::remove_dir(path) {
            Ok(()) => Ok(()),
            Err(_) => {
                if ignore_errors {
                    Ok(())
                } else {
                    Err(())
                }
            }
        }
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(_) => {
                if ignore_errors {
                    Ok(())
                } else {
                    Err(())
                }
            }
        }
    }
}

/// `shlex.quote` (POSIX): the safe set is ASCII alphanumerics and `_@%+=:,./-`; anything
/// else (or the empty word) is single-quoted with `'` spelled `'"'"'`.
pub(crate) fn shlex_quote(word: &str) -> String {
    let safe = !word.is_empty()
        && word
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&c));
    if safe {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\"'\"'"))
    }
}

/// The reference's `quoteShell` (the Mac Open command): always single-quoted.
pub(crate) fn quote_shell(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\"'\"'"))
}

fn profile_id(req: &HostRequest) -> Result<String, Failure> {
    match req.profile_id.as_deref() {
        Some(id) if super::profile_id_valid(id) => Ok(id.to_string()),
        _ => Err(refused()),
    }
}

/// `profile_dir`: absolute, a direct child of Application Support named `Claude*`, no link.
fn profile_dir(value: Option<&str>, support: &Path) -> Result<PathBuf, Failure> {
    let p = py_path(value.ok_or_else(refused)?);
    let name = p.file_name().and_then(|n| n.to_str()).ok_or_else(refused)?;
    if !p.is_absolute()
        || !p.parent().is_some_and(|parent| path_eq(parent, support))
        || !name.starts_with("Claude")
        || is_symlink(&p)
    {
        return Err(refused());
    }
    Ok(p)
}

/// `launcher_app`: absolute, a direct child of ~/Applications named `Claude (...).app`.
fn launcher_app(value: Option<&str>, apps: &Path) -> Result<PathBuf, Failure> {
    let p = py_path(value.ok_or_else(refused)?);
    let name = p.file_name().and_then(|n| n.to_str()).ok_or_else(refused)?;
    if !p.is_absolute()
        || !p.parent().is_some_and(|parent| path_eq(parent, apps))
        || !name.starts_with("Claude (")
        || !name.ends_with(").app")
        || is_symlink(&p)
    {
        return Err(refused());
    }
    Ok(p)
}

/// `trash_dir`: the validated trash name, bound to the profile id when one is given.
fn trash_dir(req: &HostRequest, pid: Option<&str>, trash_root: &Path) -> Result<PathBuf, Failure> {
    let name = req.trash_name.as_deref().ok_or_else(refused)?;
    if !super::trash_name_valid(name)
        || pid.is_some_and(|pid| !name.starts_with(&format!("{pid}-")))
    {
        return Err(refused());
    }
    Ok(trash_root.join(name))
}

fn mkdir_mode(path: &Path, mode: u32) -> Result<(), Failure> {
    fs::create_dir(path).map_err(|_| refused())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|_| refused())?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

/// `plistlib.dump`'s XML form: the doctype, tab-indented dict, keys in sorted order,
/// `& < >` escaped, booleans as `<true/>`. The reference's plists hold only strings and
/// the two `True` flags.
pub(crate) fn plist_xml(entries: &[(String, Value)]) -> String {
    let mut sorted: Vec<&(String, Value)> = entries.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    for (key, value) in sorted {
        out.push_str(&format!("\t<key>{}</key>\n", xml_escape(key)));
        match value {
            Value::Bool(true) => out.push_str("\t<true/>\n"),
            Value::Bool(false) => out.push_str("\t<false/>\n"),
            Value::String(text) => {
                out.push_str(&format!("\t<string>{}</string>\n", xml_escape(text)))
            }
            _ => out.push_str("\t<string></string>\n"),
        }
    }
    out.push_str("</dict>\n</plist>\n");
    out
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `Path(icon).suffix`: the last dot of the final component, a leading dot not counting.
fn icon_suffix(icon: &str) -> bool {
    let name = icon.rsplit(['/', '\\']).next().unwrap_or(icon);
    match name.rfind('.') {
        Some(0) => false,
        Some(_) => true,
        None => false,
    }
}

/// The icon step: read the installed Claude.app's `CFBundleIconFile` and copy the icon.
/// The reference reads it with plistlib (XML or binary); this port asks plutil, which reads
/// both forms, and skips on any failure exactly as `except (OSError, ValueError)` does.
/// Only macOS has the installed app and plutil; elsewhere the reference's read fails too.
fn copy_icon(resources: &Path, plist: &mut Vec<(String, Value)>) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let app = Path::new("/Applications/Claude.app");
    let source_plist = app.join("Contents").join("Info.plist");
    let Ok(output) = std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIconFile", "raw", "-o", "-"])
        .arg(&source_plist)
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let Ok(text) = String::from_utf8(output.stdout) else {
        return;
    };
    let icon = text.strip_suffix('\n').unwrap_or(&text);
    if icon.is_empty() {
        return;
    }
    let icon = if icon_suffix(icon) {
        icon.to_string()
    } else {
        format!("{icon}.icns")
    };
    let source = app.join("Contents").join("Resources").join(&icon);
    let target = resources.join(&icon);
    if fs::copy(&source, &target).is_err() {
        return;
    }
    // copy2 keeps the source's modification time.
    if let Ok(meta) = fs::metadata(&source)
        && let Ok(modified) = meta.modified()
    {
        let _ = fs::File::options()
            .write(true)
            .open(&target)
            .and_then(|f| f.set_modified(modified));
    }
    plist.push(("CFBundleIconFile".into(), json!(icon)));
}

/// `build`: the launcher bundle's sh script, Info.plist and optional icon.
fn build(launcher: &Path, profile: &Path, pid: &str) -> Result<(), Failure> {
    let executable = launcher.join("Contents").join("MacOS").join("launch");
    let parent = executable.parent().ok_or_else(refused)?;
    fs::create_dir_all(parent).map_err(|_| refused())?;
    let resources = launcher.join("Contents").join("Resources");
    fs::create_dir_all(&resources).map_err(|_| refused())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in [parent, resources.as_path()] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).map_err(|_| refused())?;
        }
    }
    let command = [
        "/usr/bin/open".to_string(),
        "-n".to_string(),
        "-a".to_string(),
        "Claude".to_string(),
        "--args".to_string(),
        format!("--user-data-dir={}", profile.to_string_lossy()),
    ];
    let script = format!(
        "#!/bin/sh\nexec {}\n",
        command
            .iter()
            .map(|a| shlex_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    );
    fs::write(&executable, script.as_bytes()).map_err(|_| refused())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .map_err(|_| refused())?;
    }
    let name = launcher
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(refused)?;
    let name = name.strip_suffix(".app").ok_or_else(refused)?;
    let mut plist: Vec<(String, Value)> = vec![
        (
            "CFBundleIdentifier".into(),
            json!(format!("com.aac.claudeprofile.{pid}")),
        ),
        ("CFBundleName".into(), json!(name)),
        ("CFBundleDisplayName".into(), json!(name)),
        ("CFBundleExecutable".into(), json!("launch")),
        ("CFBundlePackageType".into(), json!("APPL")),
        ("CFBundleVersion".into(), json!("1.0")),
        ("CFBundleShortVersionString".into(), json!("1.0")),
        ("NSHighResolutionCapable".into(), json!(true)),
        ("LSUIElement".into(), json!(true)),
    ];
    copy_icon(&resources, &mut plist);
    fs::write(
        launcher.join("Contents").join("Info.plist"),
        plist_xml(&plist).as_bytes(),
    )
    .map_err(|_| refused())?;
    if Path::new("/usr/bin/plutil").exists() {
        let lint = std::process::Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(launcher.join("Contents").join("Info.plist"))
            .output()
            .map_err(|_| refused())?;
        if !lint.status.success() {
            return Err(refused());
        }
    }
    Ok(())
}

/// The `state` needle rule, pure so a captured `ps` listing proves it on any platform.
pub(crate) fn ps_running(listing: &str, needle: &str) -> bool {
    let spaced = format!("{needle} ");
    listing
        .lines()
        .any(|line| line.ends_with(needle) || line.contains(&spaced))
}

fn uuid_valid(value: &str) -> bool {
    let groups = [8usize, 4, 4, 4, 12];
    let mut chars = value.chars();
    for (i, width) in groups.iter().enumerate() {
        if i > 0 && chars.next() != Some('-') {
            return false;
        }
        for _ in 0..*width {
            match chars.next() {
                Some(c) if c.is_ascii_hexdigit() => {}
                _ => return false,
            }
        }
    }
    chars.next().is_none()
}

/// Python's json.load admits NaN/Infinity; serde_json does not. When a parse failure is
/// only those tokens, the verb cannot prove the reference's answer and delegates.
fn python_only_number(raw: &str) -> bool {
    let mut in_string = false;
    let mut escaped = false;
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
        } else if c == b'"' {
            in_string = true;
        } else if c.is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word = &raw[start..i];
            if matches!(word, "NaN" | "Infinity") {
                return true;
            }
            continue;
        }
        i += 1;
    }
    false
}

/// The session marker read: the account uuid plus the presence of an encrypted token
/// cache. Tokens are never decrypted or printed.
fn session(profile: &Path) -> Result<Value, Failure> {
    let config_path = profile.join("config.json");
    if is_symlink(&config_path) {
        return Err(refused());
    }
    if !is_file(&config_path) {
        return Ok(json!({"signedIn": false}));
    }
    let meta = fs::metadata(&config_path).map_err(|_| refused())?;
    if meta.len() > 1_048_576 {
        return Err(refused());
    }
    let raw = fs::read(&config_path).map_err(|_| refused())?;
    let Ok(text) = String::from_utf8(raw) else {
        // Python's utf-8 decode error is a ValueError subclass: signed out.
        return Ok(json!({"signedIn": false}));
    };
    let config: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => {
            if python_only_number(&text) {
                crate::proof::reject();
                return Err(refused());
            }
            return Ok(json!({"signedIn": false}));
        }
    };
    let uuid = config.get("lastKnownAccountUuid").and_then(Value::as_str);
    let cache = config.get("oauth:tokenCacheV2").and_then(Value::as_str);
    let signed_in = uuid.is_some_and(uuid_valid) && cache.is_some_and(|c| !c.is_empty());
    Ok(json!({"signedIn": signed_in}))
}

/// The volume identity the reference compares (`os.stat().st_dev`): the device number on
/// Unix; on Windows the serial's stable std API is not available yet, and the decision only
/// needs equality, so the volume root keys it (profile and trash roots are plain paths).
fn volume_key(path: &Path) -> Result<String, Failure> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(path).map_err(|_| refused())?;
        Ok(meta.dev().to_string())
    }
    #[cfg(windows)]
    {
        let root = path
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .unwrap_or_default();
        Ok(root)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(String::new())
    }
}

pub(crate) fn run(req: &HostRequest, home: &Path) -> Result<Value, Failure> {
    let home = py_path(&home.to_string_lossy());
    let support = home.join("Library").join("Application Support");
    let apps = home.join("Applications");
    let trash_root = home.join(".ccs").join("trash").join("claude");
    let launcher_record = req.launcher.as_ref();
    match req.op {
        super::Op::Create => {
            let pid = profile_id(req)?;
            let profile = support.join(format!("Claude-{pid}"));
            let launcher = apps.join(format!("Claude ({pid}).app"));
            if lexists(&profile) || lexists(&launcher) {
                return Err(exists_conflict());
            }
            fs::create_dir_all(&support).map_err(|_| refused())?;
            fs::create_dir_all(&apps).map_err(|_| refused())?;
            mkdir_mode(&profile, 0o700)?;
            if build(&launcher, &profile, &pid).is_err() {
                let _ = remove_tree(&launcher, true);
                let _ = fs::remove_dir(&profile);
                return Err(refused());
            }
            Ok(json!({
                "launcherName": format!("Claude ({pid})"),
                "launcherPath": launcher.to_string_lossy(),
                "profilePath": profile.to_string_lossy(),
            }))
        }
        super::Op::Undo => {
            profile_id(req)?;
            let launcher = launcher_app(
                launcher_record.and_then(|l| l.launcher_path.as_deref()),
                &apps,
            )?;
            let profile = profile_dir(launcher_record.map(|l| l.profile_path.as_str()), &support)?;
            let _ = remove_tree(&launcher, true);
            let _ = fs::remove_dir(&profile);
            Ok(json!({"ok": true}))
        }
        super::Op::State => {
            let profile = profile_dir(launcher_record.map(|l| l.profile_path.as_str()), &support)?;
            let needle = format!("--user-data-dir={}", profile.to_string_lossy());
            // The fixture seam: a captured listing proves the needle rule on any platform.
            #[cfg(feature = "parity-fixtures")]
            if let Some(listing) = req.ps_output.as_deref() {
                return Ok(json!({"running": ps_running(listing, &needle)}));
            }
            let listing = std::process::Command::new("/bin/ps")
                .args(["-axww", "-o", "command="])
                .output()
                .map_err(|_| refused())?;
            if !listing.status.success() {
                return Err(refused());
            }
            let text = String::from_utf8(listing.stdout).map_err(|_| refused())?;
            Ok(json!({"running": ps_running(&text, &needle)}))
        }
        super::Op::Session => {
            let profile = profile_dir(launcher_record.map(|l| l.profile_path.as_str()), &support)?;
            session(&profile)
        }
        super::Op::Trash => {
            let pid = profile_id(req)?;
            let launcher = launcher_app(
                launcher_record.and_then(|l| l.launcher_path.as_deref()),
                &apps,
            )?;
            let profile = profile_dir(launcher_record.map(|l| l.profile_path.as_str()), &support)?;
            if !lexists(&trash_root) {
                fs::create_dir_all(&trash_root).map_err(|_| refused())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&trash_root, fs::Permissions::from_mode(0o700))
                        .map_err(|_| refused())?;
                }
            }
            let target = trash_dir(req, Some(&pid), &trash_root)?;
            if lexists(&target) {
                return Err(refused());
            }
            let mut moved = false;
            if exists(&profile) {
                if volume_key(&profile)? != volume_key(&trash_root)? {
                    return Ok(json!({"result": "cross_volume"}));
                }
                match fs::rename(&profile, &target) {
                    Ok(()) => moved = true,
                    Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                        return Ok(json!({"result": "cross_volume"}));
                    }
                    Err(_) => return Err(refused()),
                }
            } else {
                mkdir_mode(&target, 0o700)?;
            }
            if exists(&launcher) && remove_tree(&launcher, false).is_err() {
                if moved {
                    let _ = fs::rename(&target, &profile);
                }
                return Err(refused());
            }
            Ok(json!({"result": "moved"}))
        }
        super::Op::Restore => {
            let pid = profile_id(req)?;
            let launcher = launcher_app(
                launcher_record.and_then(|l| l.launcher_path.as_deref()),
                &apps,
            )?;
            let profile = profile_dir(launcher_record.map(|l| l.profile_path.as_str()), &support)?;
            let source = trash_dir(req, Some(&pid), &trash_root)?;
            if !is_dir(&source) || is_symlink(&source) || lexists(&profile) {
                return Err(exists_conflict());
            }
            fs::rename(&source, &profile).map_err(|_| refused())?;
            if !exists(&launcher) {
                build(&launcher, &profile, &pid)?;
            }
            Ok(json!({"ok": true}))
        }
        super::Op::Purge => {
            let target = trash_dir(req, None, &trash_root)?;
            if is_symlink(&target) {
                return Err(refused());
            }
            if exists(&target) {
                remove_tree(&target, false).map_err(|_| refused())?;
            }
            Ok(json!({"ok": true}))
        }
        super::Op::Open => {
            // openClaudeMacLauncher's fixed command, run locally instead of over ssh.
            let record = launcher_record.ok_or_else(refused)?;
            open_mac_launcher(record.launcher_path.as_deref().ok_or_else(refused)?)
        }
    }
}

/// The fixed Mac Open command text (`macOpenCommand`), byte for byte. The path checks are
/// `checkPath`'s posix rules: UTF-16 length, C0+DEL only, leading '/', `.app` suffix.
pub(crate) fn mac_open_command(launcher_path: &str) -> Result<String, Failure> {
    let units: usize = launcher_path.chars().map(|c| c.len_utf16()).sum();
    if launcher_path.is_empty()
        || units > 4096
        || launcher_path
            .chars()
            .any(|c| (c as u32) < 0x20 || c as u32 == 0x7f)
        || !launcher_path.starts_with('/')
        || !launcher_path.ends_with(".app")
    {
        return Err(refused());
    }
    let app = quote_shell(launcher_path);
    Ok([
        format!("test -d {app} || exit 1"),
        "console_user=$(/usr/bin/stat -f '%Su' /dev/console)".to_string(),
        "test \"$console_user\" != root && test \"$console_user\" != loginwindow || exit 1"
            .to_string(),
        "console_uid=$(/usr/bin/id -u \"$console_user\") || exit 1".to_string(),
        "current_uid=$(/usr/bin/id -u) || exit 1".to_string(),
        "test \"$console_uid\" = \"$current_uid\" || exit 1".to_string(),
        format!("exec /usr/bin/open {app}"),
    ]
    .join("\n"))
}

/// The Mac Open: the console user must be this user, and the wrapper must exist.
/// `/bin/sh` runs the reference command locally instead of over ssh.
pub(crate) fn open_mac_launcher(launcher_path: &str) -> Result<Value, Failure> {
    let command = mac_open_command(launcher_path)?;
    let status = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(&command)
        .status()
        .map_err(|_| refused())?;
    if !status.success() {
        return Err(refused());
    }
    Ok(json!({"ok": true}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shlex_quote_matches_posix() {
        assert_eq!(shlex_quote("plain"), "plain");
        assert_eq!(shlex_quote(""), "''");
        assert_eq!(shlex_quote("a'b"), "'a'\"'\"'b'");
        assert_eq!(
            shlex_quote("--user-data-dir=/x/Claude-a"),
            "--user-data-dir=/x/Claude-a"
        );
        assert_eq!(
            shlex_quote("/x/with space/Claude-a"),
            "'/x/with space/Claude-a'"
        );
        assert_eq!(shlex_quote("a@%+=:,./-_9"), "a@%+=:,./-_9");
        assert_eq!(shlex_quote("a$b"), "'a$b'");
    }

    #[test]
    fn plist_xml_matches_plistlib() {
        let entries: Vec<(String, Value)> = vec![
            (
                "CFBundleIdentifier".into(),
                json!("com.aac.claudeprofile.work2"),
            ),
            ("CFBundleName".into(), json!("Claude (work2)")),
            ("CFBundleDisplayName".into(), json!("Claude (work2)")),
            ("CFBundleExecutable".into(), json!("launch")),
            ("CFBundlePackageType".into(), json!("APPL")),
            ("CFBundleVersion".into(), json!("1.0")),
            ("CFBundleShortVersionString".into(), json!("1.0")),
            ("NSHighResolutionCapable".into(), json!(true)),
            ("LSUIElement".into(), json!(true)),
        ];
        // Captured from CPython 3.13 plistlib.dump on the same dict.
        let reference = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>CFBundleDisplayName</key>\n\t<string>Claude (work2)</string>\n\t<key>CFBundleExecutable</key>\n\t<string>launch</string>\n\t<key>CFBundleIdentifier</key>\n\t<string>com.aac.claudeprofile.work2</string>\n\t<key>CFBundleName</key>\n\t<string>Claude (work2)</string>\n\t<key>CFBundlePackageType</key>\n\t<string>APPL</string>\n\t<key>CFBundleShortVersionString</key>\n\t<string>1.0</string>\n\t<key>CFBundleVersion</key>\n\t<string>1.0</string>\n\t<key>LSUIElement</key>\n\t<true/>\n\t<key>NSHighResolutionCapable</key>\n\t<true/>\n</dict>\n</plist>\n";
        assert_eq!(plist_xml(&entries), reference);
    }

    #[test]
    fn ps_needle_rule_matches_the_reference() {
        let needle = "--user-data-dir=/x/Claude-work2";
        assert!(ps_running(&format!("/usr/bin/open {needle}"), needle));
        assert!(ps_running(
            &format!("/usr/bin/open {needle} --flag"),
            needle
        ));
        assert!(!ps_running(&format!("/usr/bin/open {needle}x"), needle));
        assert!(!ps_running(
            "/usr/bin/open --user-data-dir=/x/Claude-work",
            needle
        ));
        assert!(!ps_running("", needle));
        // The needle mid-line with a space after it counts, as the reference's `in` does.
        assert!(ps_running(&format!("sh -c {needle} rest"), needle));
    }

    #[test]
    fn uuid_rule_matches_the_reference() {
        assert!(uuid_valid("12345678-90ab-cdef-1234-567890abcdef"));
        assert!(uuid_valid("12345678-90AB-CDEF-1234-567890ABCDEF"));
        assert!(!uuid_valid("12345678-90ab-cdef-1234-567890abcde"));
        assert!(!uuid_valid("1234567890abcdef1234567890abcdef"));
        assert!(!uuid_valid("12345678-90ab-cdef-1234-567890abcdeg"));
        assert!(!uuid_valid(""));
    }

    #[test]
    fn python_only_numbers_are_detected_outside_strings() {
        assert!(python_only_number("{\"a\": NaN}"));
        assert!(python_only_number("{\"a\": Infinity}"));
        assert!(python_only_number("{\"a\": -Infinity}"));
        assert!(!python_only_number("{\"a\": \"NaN\"}"));
        assert!(!python_only_number("{oops"));
        assert!(!python_only_number("{\"a\": 1}"));
    }

    #[test]
    fn icon_suffix_rule() {
        assert!(!icon_suffix("icon"));
        assert!(icon_suffix("icon.icns"));
        assert!(icon_suffix("icon.other"));
        assert!(!icon_suffix(".hidden"));
        assert!(icon_suffix(".hidden.icns"));
    }
}
