//! Port of src/commands/bar/native-app-paths.ts: the owned macOS app resolution, the
//! non-expanding XML plist reader and the bounded native command runner. Fixture tests invent
//! app bundles and fake runners; nothing here launches a real app or reads a real credential.

use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

pub const PRODUCT_NAME: &str = "AI Account Center";
pub const MAC_APP_NAME: &str = "AI Account Center.app";
pub const MAC_LEGACY_APP_NAME: &str = "CCS Bar.app";
pub const MAC_BUNDLE_ID: &str = "party.sittingmongoose.ccs.accounts-bar";
pub const MAC_EXECUTABLE: &str = "CCSBar";

pub struct NativeResult {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub type NativeRunner<'a> = &'a dyn Fn(&str, &[&str]) -> Result<NativeResult, String>;

/// runNativeCommand: spawn without a shell, five-second bound, 1 MiB output cap.
pub fn run_native_command(command: &str, args: &[&str]) -> Result<NativeResult, String> {
    let mut child = std::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|_| {
            format!(
                "Unable to run {} safely.",
                Path::new(command)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(command)
            )
        })?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                use std::io::Read;
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(handle) = child.stdout.take() {
                    handle.take(1024 * 1024).read_to_string(&mut stdout).ok();
                }
                if let Some(handle) = child.stderr.take() {
                    handle.take(1024 * 1024).read_to_string(&mut stderr).ok();
                }
                return Ok(NativeResult {
                    status: status.code(),
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "Unable to run {} safely.",
                        Path::new(command)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(command)
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => {
                return Err(format!(
                    "Unable to run {} safely.",
                    Path::new(command)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(command)
                ));
            }
        }
    }
}

pub struct MacAppPaths {
    pub apps_dir: PathBuf,
    pub canonical: PathBuf,
    pub legacy: PathBuf,
}

pub fn mac_app_paths(home: &Path, apps_dir_override: Option<&Path>) -> MacAppPaths {
    let apps_dir = apps_dir_override
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("Applications"));
    let apps_dir = std::path::absolute(&apps_dir).unwrap_or(apps_dir);
    MacAppPaths {
        canonical: apps_dir.join(MAC_APP_NAME),
        legacy: apps_dir.join(MAC_LEGACY_APP_NAME),
        apps_dir,
    }
}

fn lstat_if_present(file: &Path) -> Result<Option<std::fs::Metadata>, String> {
    match std::fs::symlink_metadata(file) {
        Ok(stat) => Ok(Some(stat)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn current_uid() -> Option<u32> {
    #[cfg(unix)]
    {
        #[allow(unsafe_code)] // libc::getuid reads the caller's uid; no memory is touched.
        Some(unsafe { libc::getuid() })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn require_owned_regular(file: &Path, directory: bool) -> Result<std::fs::Metadata, String> {
    let stat = lstat_if_present(file)?;
    let refuse = || {
        format!(
            "Refusing an unowned or unexpected native app path: {}",
            file.display()
        )
    };
    let Some(stat) = stat else {
        return Err(refuse());
    };
    if stat.file_type().is_symlink() {
        return Err(refuse());
    }
    if directory {
        if !stat.is_dir() {
            return Err(refuse());
        }
    } else if !stat.is_file() {
        return Err(refuse());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Some(uid) = current_uid()
            && stat.uid() != uid
        {
            return Err(refuse());
        }
    }
    Ok(stat)
}

fn decode_xml_text(value: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find(';') else {
            return Err("Invalid native app plist text.".into());
        };
        let entity = &after[..end];
        let decoded = match entity {
            "amp" => "&".to_string(),
            "lt" => "<".to_string(),
            "gt" => ">".to_string(),
            "quot" => "\"".to_string(),
            "apos" => "'".to_string(),
            _ => {
                let code = if let Some(hex) = entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                {
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(dec) = entity.strip_prefix('#') {
                    dec.parse::<u32>().ok()
                } else {
                    None
                };
                match code.and_then(char::from_u32) {
                    Some(ch) => ch.to_string(),
                    None => return Err("Invalid native app plist text.".into()),
                }
            }
        };
        out.push_str(&decoded);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The non-expanding XML plist reader: no DTD, no external entity, bounded depth.
pub fn parse_xml_plist(source: &str) -> Result<Value, String> {
    let mut xml = source.to_string();
    // Strip the XML declaration, comments and DOCTYPE exactly as the reference does.
    xml = strip_pattern(&xml, "<?xml", "?>");
    xml = strip_pattern(&xml, "<!--", "-->");
    xml = strip_doctype(&xml);
    let xml = xml.trim().to_string();
    let mut offset;
    fn fail() -> String {
        "Invalid native app plist structure.".into()
    }
    fn space(xml: &str, offset: &mut usize) {
        while let Some(ch) = xml[*offset..].chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            *offset += ch.len_utf8();
        }
    }
    fn take(xml: &str, offset: &mut usize, tag: &str) -> bool {
        space(xml, offset);
        if xml[*offset..].starts_with(tag) {
            *offset += tag.len();
            true
        } else {
            false
        }
    }
    fn text(xml: &str, offset: &mut usize, tag: &str) -> Result<String, String> {
        if take(xml, offset, &format!("<{tag}/>")) {
            return Ok(String::new());
        }
        if !take(xml, offset, &format!("<{tag}>")) {
            return Err(fail());
        }
        let Some(end) = xml[*offset..].find(&format!("</{tag}>")) else {
            return Err(fail());
        };
        let value = &xml[*offset..*offset + end];
        if value.contains('<') {
            return Err(fail());
        }
        *offset += end + tag.len() + 3;
        decode_xml_text(value)
    }
    fn value(xml: &str, offset: &mut usize, depth: usize) -> Result<Value, String> {
        if depth > 32 {
            return Err(fail());
        }
        if take(xml, offset, "<dict/>") {
            return Ok(Value::Object(Map::new()));
        }
        if take(xml, offset, "<dict>") {
            let mut result = Map::new();
            while !take(xml, offset, "</dict>") {
                let key = text(xml, offset, "key")?;
                if result.contains_key(&key) {
                    return Err(fail());
                }
                result.insert(key, value(xml, offset, depth + 1)?);
            }
            return Ok(Value::Object(result));
        }
        if take(xml, offset, "<array/>") {
            return Ok(Value::Array(Vec::new()));
        }
        if take(xml, offset, "<array>") {
            let mut result = Vec::new();
            while !take(xml, offset, "</array>") {
                result.push(value(xml, offset, depth + 1)?);
            }
            return Ok(Value::Array(result));
        }
        if take(xml, offset, "<true/>") || take(xml, offset, "<true />") {
            return Ok(Value::Bool(true));
        }
        if take(xml, offset, "<false/>") || take(xml, offset, "<false />") {
            return Ok(Value::Bool(false));
        }
        space(xml, offset);
        for tag in ["string", "date", "data"] {
            if xml[*offset..].starts_with(&format!("<{tag}>"))
                || xml[*offset..].starts_with(&format!("<{tag}/>"))
            {
                return Ok(Value::String(text(xml, offset, tag)?));
            }
        }
        for tag in ["integer", "real"] {
            if xml[*offset..].starts_with(&format!("<{tag}>")) {
                let raw = text(xml, offset, tag)?;
                let number: f64 = raw.parse().map_err(|_| fail())?;
                if !number.is_finite() {
                    return Err(fail());
                }
                if number.fract() == 0.0 && number >= i64::MIN as f64 && number <= i64::MAX as f64 {
                    return Ok(json!(number as i64));
                }
                return serde_json::Number::from_f64(number)
                    .map(Value::Number)
                    .ok_or_else(fail);
            }
        }
        Err(fail())
    }
    let opening = {
        let rest = xml.as_str();
        if !rest.starts_with("<plist") {
            return Err(fail());
        }
        let Some(end) = rest.find('>') else {
            return Err(fail());
        };
        // /^<plist(?:\s+[^>]*)?>/ — only whitespace may follow the name.
        let between = &rest[6..end];
        if !between.is_empty() && !between.starts_with(char::is_whitespace) {
            return Err(fail());
        }
        end + 1
    };
    offset = opening;
    let result = value(&xml, &mut offset, 0)?;
    if !take(&xml, &mut offset, "</plist>") {
        return Err(fail());
    }
    space(&xml, &mut offset);
    if offset != xml.len() || !result.is_object() {
        return Err(fail());
    }
    Ok(result)
}

fn strip_pattern(source: &str, open: &str, close: &str) -> String {
    let mut out = String::new();
    let mut rest = source;
    while let Some(start) = rest.find(open) {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        match after.find(close) {
            Some(end) => rest = &after[end + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn strip_doctype(source: &str) -> String {
    // <!DOCTYPE[^>[\]]*> — stops at the first '>', '[' or ']'.
    let mut out = String::new();
    let mut rest = source;
    while let Some(start) = rest.find("<!DOCTYPE") {
        let after = &rest[start + 9..];
        let bracket = after.find(['[', ']']);
        if let Some(gt) = after.find('>')
            && bracket.is_none_or(|b| gt < b)
        {
            out.push_str(&rest[..start]);
            rest = &after[gt + 1..];
        } else {
            out.push_str(&rest[..start + 9]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

pub type PlistReader<'a> = &'a dyn Fn(&Path) -> Result<Value, String>;

pub fn read_mac_plist(
    file: &Path,
    runner: NativeRunner,
    read_plist: Option<PlistReader>,
) -> Result<Value, String> {
    if let Some(reader) = read_plist {
        return reader(file);
    }
    let stat = require_owned_regular(file, false)?;
    if stat.len() > 1024 * 1024 {
        return Err("Native app plist is too large.".into());
    }
    let data = std::fs::read(file).map_err(|e| e.to_string())?;
    if data
        .get(..8)
        .map(|head| head != b"bplist00")
        .unwrap_or(true)
    {
        let text = String::from_utf8(data).map_err(|_| "Invalid native app plist structure.")?;
        return parse_xml_plist(&text);
    }
    let result = runner(
        "/usr/bin/plutil",
        &["-convert", "json", "-o", "-", &file.to_string_lossy()],
    )?;
    if result.status != Some(0) {
        return Err("Unable to read native app binary plist.".into());
    }
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&result.stdout) {
        return Ok(Value::Object(map));
    }
    Err("Invalid native app binary plist.".into())
}

pub struct OwnedMacApp {
    pub path: PathBuf,
    pub version: Option<String>,
}

pub fn inspect_owned_mac_app(
    app_path: &Path,
    runner: NativeRunner,
    read_plist: Option<PlistReader>,
) -> Result<Option<OwnedMacApp>, String> {
    if lstat_if_present(app_path)?.is_none() {
        return Ok(None);
    }
    require_owned_regular(app_path, true)?;
    let contents = app_path.join("Contents");
    require_owned_regular(&contents, true)?;
    require_owned_regular(&contents.join("MacOS"), true)?;
    let info_file = contents.join("Info.plist");
    require_owned_regular(&info_file, false)?;
    let info = read_mac_plist(&info_file, runner, read_plist)?;
    if info.get("CFBundleIdentifier").and_then(Value::as_str) != Some(MAC_BUNDLE_ID)
        || info.get("CFBundleExecutable").and_then(Value::as_str) != Some(MAC_EXECUTABLE)
    {
        return Err(format!(
            "Refusing a foreign native app bundle: {}",
            app_path.display()
        ));
    }
    let executable = require_owned_regular(&contents.join("MacOS").join(MAC_EXECUTABLE), false)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if executable.mode() & 0o111 == 0 {
            return Err(format!(
                "Native app executable is not executable: {}",
                app_path.display()
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = executable;
    }
    let version = ["CFBundleShortVersionString", "CFBundleVersion"]
        .iter()
        .filter_map(|key| info.get(*key).and_then(Value::as_str))
        .find(|value| !value.trim().is_empty() && value.len() <= 128)
        .map(|value| value.trim().to_string());
    Ok(Some(OwnedMacApp {
        path: app_path.to_path_buf(),
        version,
    }))
}

/// Preflight both names; an occupied foreign target never silently falls back.
pub fn resolve_owned_mac_app(
    apps_dir: &Path,
    runner: NativeRunner,
    read_plist: Option<PlistReader>,
) -> Result<Option<OwnedMacApp>, String> {
    let paths = mac_app_paths(Path::new(""), Some(apps_dir));
    if lstat_if_present(&paths.apps_dir)?.is_some() {
        require_owned_regular(&paths.apps_dir, true)?;
    }
    let canonical = inspect_owned_mac_app(&paths.canonical, runner, read_plist)?;
    let legacy_stat = lstat_if_present(&paths.legacy)?;
    let mut legacy: Option<OwnedMacApp> = None;
    if let Some(stat) = legacy_stat {
        if stat.file_type().is_symlink() {
            let target = std::fs::read_link(&paths.legacy).map_err(|e| e.to_string())?;
            let real_legacy = std::fs::canonicalize(&paths.legacy).map_err(|e| e.to_string())?;
            let real_canonical =
                std::fs::canonicalize(&paths.canonical).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            let uid_ok = {
                use std::os::unix::fs::MetadataExt;
                current_uid().is_none_or(|uid| stat.uid() == uid)
            };
            #[cfg(not(unix))]
            let uid_ok = true;
            if canonical.is_none()
                || !uid_ok
                || target.to_string_lossy() != MAC_APP_NAME
                || real_legacy != real_canonical
            {
                return Err(format!(
                    "Refusing a foreign native app alias: {}",
                    paths.legacy.display()
                ));
            }
        } else {
            legacy = inspect_owned_mac_app(&paths.legacy, runner, read_plist)?;
        }
    }
    Ok(canonical.or(legacy))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::fs;

    const INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>party.sittingmongoose.ccs.accounts-bar</string>
	<key>CFBundleExecutable</key>
	<string>CCSBar</string>
	<key>CFBundleShortVersionString</key>
	<string>1.2.3</string>
	<key>CFBundleVersion</key>
	<string>45</string>
	<key>LSUIElement</key>
	<true/>
	<key>Count</key>
	<integer>7</integer>
	<key>Ratio</key>
	<real>0.5</real>
	<key>Escaped</key>
	<string>a &amp; b &#65;&#x42;</string>
	<key>Items</key>
	<array><string>x</string><false/></array>
</dict>
</plist>"#;

    #[test]
    fn xml_plist_parses_the_reference_shapes() {
        let value = parse_xml_plist(INFO).unwrap();
        assert_eq!(value["CFBundleIdentifier"], json!(MAC_BUNDLE_ID));
        assert_eq!(value["Count"], json!(7));
        assert_eq!(value["Ratio"], json!(0.5));
        assert_eq!(value["LSUIElement"], json!(true));
        assert_eq!(value["Escaped"], json!("a & b AB"));
        assert_eq!(value["Items"], json!(["x", false]));
        assert!(parse_xml_plist("<plist><dict><key>a</key><string>1</string><key>a</key><string>2</string></dict></plist>").is_err());
        assert!(parse_xml_plist("<plist><dict></plist>").is_err());
        assert!(parse_xml_plist("<plist version='1.0'><string>x</string></plist>").is_err());
        assert!(parse_xml_plist("<plist><dict><key>a</key><bad/></dict></plist>").is_err());
        // An internal DTD subset is refused, never expanded.
        assert!(
            parse_xml_plist(
                "<!DOCTYPE plist [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><plist><dict/></plist>"
            )
            .is_err()
        );
    }

    fn make_app(root: &Path, name: &str, bundle_id: &str, version: &str, exe_mode: u32) -> PathBuf {
        let app = root.join(name);
        let macos = app.join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        fs::write(
            app.join("Contents/Info.plist"),
            format!(
                "<plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>{bundle_id}</string><key>CFBundleExecutable</key><string>CCSBar</string><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>"
            ),
        )
        .unwrap();
        let exe = macos.join("CCSBar");
        fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(exe_mode)).unwrap();
        }
        app
    }

    fn no_runner(_c: &str, _a: &[&str]) -> Result<NativeResult, String> {
        Err("no native commands in fixtures".into())
    }

    #[test]
    fn inspection_enforces_ownership_and_identity() {
        let dir = tempfile::tempdir().unwrap();
        let app = make_app(dir.path(), MAC_APP_NAME, MAC_BUNDLE_ID, "1.2.3", 0o755);
        let owned = inspect_owned_mac_app(&app, &no_runner, None)
            .unwrap()
            .unwrap();
        assert_eq!(owned.version.as_deref(), Some("1.2.3"));
        let foreign = make_app(dir.path(), "Foreign.app", "other.bundle", "9.9.9", 0o755);
        assert!(inspect_owned_mac_app(&foreign, &no_runner, None).is_err());
        let unexecutable = make_app(dir.path(), "NoExec.app", MAC_BUNDLE_ID, "1.0.0", 0o644);
        assert!(inspect_owned_mac_app(&unexecutable, &no_runner, None).is_err());
        assert!(
            inspect_owned_mac_app(&dir.path().join("Absent.app"), &no_runner, None)
                .unwrap()
                .is_none()
        );
        #[cfg(unix)]
        {
            let link = dir.path().join("Link.app");
            std::os::unix::fs::symlink(&app, &link).unwrap();
            assert!(inspect_owned_mac_app(&link, &no_runner, None).is_err());
        }
    }

    #[test]
    fn resolution_preflight_canonical_and_legacy_names() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        fs::create_dir_all(&apps).unwrap();
        assert!(
            resolve_owned_mac_app(&apps, &no_runner, None)
                .unwrap()
                .is_none()
        );
        let app = make_app(&apps, MAC_APP_NAME, MAC_BUNDLE_ID, "2.0.0", 0o755);
        let resolved = resolve_owned_mac_app(&apps, &no_runner, None)
            .unwrap()
            .unwrap();
        assert_eq!(resolved.path, app);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(MAC_APP_NAME, apps.join(MAC_LEGACY_APP_NAME)).unwrap();
            assert!(
                resolve_owned_mac_app(&apps, &no_runner, None)
                    .unwrap()
                    .is_some()
            );
            fs::remove_file(apps.join(MAC_LEGACY_APP_NAME)).unwrap();
            std::os::unix::fs::symlink("Foreign.app", apps.join(MAC_LEGACY_APP_NAME)).unwrap();
            assert!(resolve_owned_mac_app(&apps, &no_runner, None).is_err());
        }
    }

    #[test]
    fn binary_plist_goes_through_the_fixed_converter() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Info.plist");
        let mut bytes = b"bplist00".to_vec();
        bytes.extend_from_slice(&[0u8; 16]);
        fs::write(&file, bytes).unwrap();
        let runner = |c: &str, a: &[&str]| -> Result<NativeResult, String> {
            assert_eq!(c, "/usr/bin/plutil");
            assert_eq!(a[..3], ["-convert", "json", "-o"]);
            Ok(NativeResult {
                status: Some(0),
                stdout: r#"{"CFBundleIdentifier":"x"}"#.into(),
                stderr: String::new(),
            })
        };
        let value = read_mac_plist(&file, &runner, None).unwrap();
        assert_eq!(value["CFBundleIdentifier"], json!("x"));
        let bad = |_c: &str, _a: &[&str]| {
            Ok(NativeResult {
                status: Some(1),
                stdout: String::new(),
                stderr: String::new(),
            })
        };
        assert!(read_mac_plist(&file, &bad, None).is_err());
    }
}
