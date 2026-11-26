//! Fixed native app/process guards; fixture values enter only through the feature-gated parent.
use super::*;
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(super) fn capture_command(
    mut cmd: Command,
    input: Vec<u8>,
    cap: usize,
    seconds: u64,
) -> Checked<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let mut child = cmd.spawn().map_err(|_| ())?;
    let mut stdin = child.stdin.take().ok_or(())?;
    let stdout = child.stdout.take().ok_or(())?;
    let stderr = child.stderr.take().ok_or(())?;
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let (tx, rx) = std::sync::mpsc::channel();
    let handles = [
        Box::new(stdout) as Box<dyn Read + Send>,
        Box::new(stderr) as Box<dyn Read + Send>,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, p)| {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = p
                .take(cap as u64 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = tx.send((i, result));
        })
    })
    .collect::<Vec<_>>();
    drop(tx);
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut outputs: [Option<Vec<u8>>; 2] = [None, None];
    let mut failed = false;
    let status = loop {
        for (i, r) in rx.try_iter() {
            match r {
                Ok(b) if b.len() <= cap => outputs[i] = Some(b),
                _ => failed = true,
            }
        }
        if failed || Instant::now() >= end {
            let _ = child.kill();
            break child.wait().map_err(|_| ())?;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                let _ = child.kill();
                failed = true;
                break child.wait().map_err(|_| ())?;
            }
        }
    };
    let wrote = writer.join().map_err(|_| ())?.is_ok();
    for h in handles {
        h.join().map_err(|_| ())?;
    }
    for (i, r) in rx {
        match r {
            Ok(b) if b.len() <= cap => outputs[i] = Some(b),
            _ => failed = true,
        }
    }
    if failed || !wrote {
        return Err(());
    }
    Ok((
        status,
        outputs[0].take().ok_or(())?,
        outputs[1].take().ok_or(())?,
    ))
}
fn private_environment(command: &mut Command) {
    command
        .env_clear()
        .env("SystemRoot", r"C:\Windows")
        .env("WINDIR", r"C:\Windows");
}
pub(super) fn ps(script: &str, input: Vec<u8>) -> Checked<Vec<u8>> {
    ps_query(script, input, false)
}
fn ps_query(script: &str, input: Vec<u8>, private: bool) -> Checked<Vec<u8>> {
    let program = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    let script = format!(
        "[Console]::InputEncoding=[Text.UTF8Encoding]::new($false);[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);{script}"
    );
    let encoded = STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let mut command = Command::new(program);
    command.args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded]);
    if private {
        private_environment(&mut command);
    }
    let (status, out, _) = capture_command(
        command,
        input,
        if private { 8192 } else { 1_048_576 },
        if private { 15 } else { 8 },
    )?;
    if !status.success() {
        return Err(());
    }
    Ok(out)
}
#[allow(unsafe_code)]
pub(super) fn closed(platform: &str, profile: &str, root: &Path) -> Checked<bool> {
    let rows = if platform == "mac" {
        let mut command = Command::new("/usr/bin/pgrep");
        command.args(["-x", "Claude"]);
        let (status, stdout, _) = capture_command(command, Vec::new(), 65536, 5)?;
        if !matches!(status.code(), Some(0 | 1)) {
            return Err(());
        }
        let mut rows = Vec::new();
        for pid in std::str::from_utf8(&stdout)
            .map_err(|_| ())?
            .split_ascii_whitespace()
        {
            let pid = pid.parse::<i32>().map_err(|_| ())?;
            match mac_args(pid) {
                Ok(a) => rows.push(a),
                Err(()) => {
                    #[cfg(unix)]
                    {
                        if unsafe { libc::kill(pid, 0) } == 0
                            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                        {
                            return Err(());
                        }
                    }
                    #[cfg(not(unix))]
                    return Err(());
                }
            }
        }
        rows
    } else {
        let raw = ps(include_str!("windows-processes.ps1"), Vec::new())?;
        serde_json::from_slice::<Vec<Vec<String>>>(&raw).map_err(|_| ())?
    };
    closed_rows(
        &rows,
        profile,
        if platform == "mac" { "platyr" } else { "gmail" },
        root,
    )
}
fn closed_rows(rows: &[Vec<String>], profile: &str, default: &str, root: &Path) -> Checked<bool> {
    classify(
        rows,
        profile,
        default,
        directory(root)?,
        |p| Path::new(p).is_absolute(),
        |p| directory(Path::new(p)),
    )
}
fn classify(
    rows: &[Vec<String>],
    profile: &str,
    default: &str,
    target: Value,
    absolute: impl Fn(&str) -> bool,
    mut resolve: impl FnMut(&str) -> Checked<Value>,
) -> Checked<bool> {
    for argv in rows {
        let mut dirs = vec![];
        for (i, arg) in argv.iter().enumerate() {
            if let Some(value) = arg.strip_prefix("--user-data-dir=") {
                dirs.push(value);
            } else if arg == "--user-data-dir" {
                dirs.push(argv.get(i + 1).ok_or(())?);
            }
        }
        if dirs.len() > 1 {
            return Err(());
        }
        if let Some(p) = dirs.first() {
            if p.is_empty() || !absolute(p) || p.chars().any(|c| c < ' ' || c == '\u{7f}') {
                return Err(());
            }
            if resolve(p)? == target {
                return Ok(false);
            }
        } else if profile == default {
            return Ok(false);
        }
    }
    Ok(true)
}
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn mac_args(pid: i32) -> Checked<Vec<String>> {
    let mut mib = [1, 49, pid];
    let mut size = 0usize;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size == 0
        || size > 262144
    {
        return Err(());
    }
    let mut bytes = vec![0u8; size];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(());
    }
    bytes.truncate(size);
    let argc = i32::from_ne_bytes(bytes.get(..4).ok_or(())?.try_into().map_err(|_| ())?);
    if !(1..1024).contains(&argc) {
        return Err(());
    }
    let end = bytes[4..].iter().position(|b| *b == 0).ok_or(())? + 5;
    let mut offset = end;
    while bytes.get(offset) == Some(&0) {
        offset += 1;
    }
    let parts = bytes
        .get(offset..)
        .ok_or(())?
        .split(|b| *b == 0)
        .take(argc as usize)
        .map(|b| String::from_utf8(b.to_vec()).map_err(|_| ()))
        .collect::<Checked<Vec<_>>>()?;
    if parts.len() != argc as usize {
        return Err(());
    }
    Ok(parts)
}
#[cfg(not(target_os = "macos"))]
fn mac_args(_pid: i32) -> Checked<Vec<String>> {
    Err(())
}

pub(super) fn guard(platform: &str) -> Checked<Value> {
    if platform == "mac" {
        return super::mac_native();
    }
    let raw = ps(
        "$ErrorActionPreference='Stop'; ConvertTo-Json -InputObject @(Get-AppxPackage -Name Claude | Select-Object Name,Version,PackageFullName,InstallLocation) -Compress",
        Vec::new(),
    )?;
    windows_guard(
        &serde_json::from_slice::<Value>(&raw).map_err(|_| ())?,
        asar,
    )
}
fn windows_guard(
    packages: &Value,
    read: impl FnOnce(&Path, &str) -> Checked<String>,
) -> Checked<Value> {
    let rows = packages.as_array().ok_or(())?;
    if rows.len() != 1 {
        return Err(());
    }
    let p = &rows[0];
    if p["PackageFullName"] != "Claude_2.19675.0.0_x64__pzs8sxrjxfjjc"
        || p["Version"] != "2.19675.0.0"
        || p["InstallLocation"]
            != r"C:\Program Files\WindowsApps\Claude_2.19675.0.0_x64__pzs8sxrjxfjjc"
    {
        return Err(());
    }
    let archive = Path::new(text(&p["InstallLocation"])?).join("app/resources/app.asar");
    let sha = read(&archive, "index.chunk-DmADLdv3.js")?;
    if sha != "492242eb2c352f60a886031275b7a37fd2fc791946be98b8c8fdd4bbfc41ded6" {
        return Err(());
    }
    Ok(
        json!({"version":"2.19675.0.0","managerSha256":sha,"warmGuardVerified":true,"autoResumeGuardVerified":true}),
    )
}
fn asar(path: &Path, member: &str) -> Checked<String> {
    use std::io::{Seek, SeekFrom};
    directory(path.parent().ok_or(())?)?;
    let identity = aac_files::history_transaction::identity(path).map_err(|_| ())?;
    let (mut file, before) =
        aac_files::read::open(path, &aac_files::read::ReadPolicy::capped(1_000_000_000))
            .map_err(|_| ())?;
    let mut header = [0u8; 16];
    file.read_exact(&mut header).map_err(|_| ())?;
    let word =
        |i| u32::from_le_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]) as usize;
    let len = word(12);
    let header_len = word(4);
    if len == 0 || len >= 20_000_000 || header_len < len {
        return Err(());
    }
    let mut raw = vec![0u8; len];
    file.read_exact(&mut raw).map_err(|_| ())?;
    let mut node: Value = serde_json::from_slice(&raw).map_err(|_| ())?;
    for part in [".vite", "build", member] {
        node = node["files"][part].clone();
    }
    let len = node["size"]
        .as_u64()
        .filter(|n| *n > 0 && *n < 10_000_000)
        .ok_or(())?;
    if js_truthy(&node["unpacked"]) || js_truthy(&node["link"]) {
        return Err(());
    }
    let start = 8u64
        .checked_add(header_len as u64)
        .and_then(|n| n.checked_add(text(&node["offset"]).ok()?.parse::<u64>().ok()?))
        .ok_or(())?;
    if start.checked_add(len).is_none_or(|end| end > before.len()) {
        return Err(());
    }
    file.seek(SeekFrom::Start(start)).map_err(|_| ())?;
    let mut bytes = vec![0; len as usize];
    file.read_exact(&mut bytes).map_err(|_| ())?;
    let after = fs::symlink_metadata(path).map_err(|_| ())?;
    if aac_files::history_transaction::identity(path).map_err(|_| ())? != identity
        || before.len() != after.len()
        || before.modified().map_err(|_| ())? != after.modified().map_err(|_| ())?
    {
        return Err(());
    }
    Ok(hash(&bytes))
}
fn js_truthy(v: &Value) -> bool {
    !v.is_null() && *v != false && *v != ""
}
fn private_reply(bytes: &[u8]) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    bytes.len()<=4096&&std::str::from_utf8(bytes).is_ok_and(|s|RE.get_or_init(||regex::Regex::new(r#"^[ \t\r\n]*\{[ \t\r\n]*"private"[ \t\r\n]*:[ \t\r\n]*true[ \t\r\n]*\}[ \t\r\n]*$"#).expect("fixed private receipt grammar")).is_match(s))
}
pub(super) fn private(path: &Path, dir: bool) -> bool {
    if !cfg!(windows) {
        return true;
    }
    let input = json!({"path":path,"kind":if dir{"directory"}else{"file"}});
    ps_query(
        include_str!("windows-private.ps1"),
        input.to_string().into_bytes(),
        true,
    )
    .is_ok_and(|b| private_reply(&b))
}

pub(super) fn host_pattern(pattern: &str, alias: &str) -> Checked<bool> {
    if pattern.chars().count() > 4096 {
        return Err(());
    }
    let p = pattern.chars().collect::<Vec<_>>();
    let a = alias.chars().collect::<Vec<_>>();
    let mut matched = vec![false; a.len() + 1];
    matched[0] = true;
    let mut i = 0;
    while i < p.len() {
        let ch = p[i];
        if ch == '*' {
            for n in 1..matched.len() {
                matched[n] = matched[n] || matched[n - 1];
            }
            i += 1;
            continue;
        }
        let mut end = i + 1;
        let mut class = None;
        if ch == '[' {
            let mut k = i + 1;
            if p.get(k) == Some(&'!') {
                k += 1;
            }
            if p.get(k) == Some(&']') {
                k += 1;
            }
            if let Some(close) = p[k..].iter().position(|c| *c == ']') {
                end = k + close + 1;
                class = Some(&p[i + 1..end - 1]);
            }
        }
        let mut next = vec![false; a.len() + 1];
        for n in 0..a.len() {
            let ok = if let Some(class) = class {
                let negative = class.first() == Some(&'!');
                let chars = if negative { &class[1..] } else { class };
                let mut found = false;
                let mut k = 0;
                while k < chars.len() {
                    if k + 2 < chars.len() && chars[k + 1] == '-' {
                        found |= chars[k] <= a[n] && a[n] <= chars[k + 2];
                        k += 3;
                    } else {
                        found |= chars[k] == a[n];
                        k += 1;
                    }
                }
                found != negative
            } else {
                ch == '?' || ch == a[n]
            };
            next[n + 1] = matched[n] && ok;
        }
        matched = next;
        i = end;
    }
    Ok(matched[a.len()])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn acl_queries_cannot_inherit_module_or_process_configuration() {
        let mut command = Command::new("/usr/bin/env");
        command
            .env("PSModulePath", "untrusted-fixture")
            .env("PYTHONPATH", "untrusted-fixture");
        private_environment(&mut command);
        let out = command.output().expect("environment double");
        let text = String::from_utf8(out.stdout).expect("environment text");
        assert!(!text.contains("untrusted-fixture"));
        assert_eq!(text.lines().count(), 2);
    }
    #[test]
    fn unrelated_ssh_wildcards_do_not_decline_the_fixed_endpoint() {
        for (pattern, expected) in [
            ("fixture", true),
            ("fi*", true),
            ("f?xture", true),
            ("[a-z]ixture", true),
            ("[!a-z]ixture", false),
            ("*.github.com", false),
            ("[]]", false),
            ("[", false),
        ] {
            assert_eq!(host_pattern(pattern, "fixture"), Ok(expected), "{pattern}");
        }
    }
    #[test]
    fn private_receipts_never_accept_duplicate_keys_or_extra_output() {
        assert!(private_reply(b" { \"private\": true }\n"));
        for raw in [
            b"{\"private\":false,\"private\":true}".as_slice(),
            b"{\"private\":true,\"extra\":true}",
            b"{\"private\":true} {}",
            b"{\"private\":1}",
        ] {
            assert!(!private_reply(raw));
        }
    }
    #[test]
    fn python_windows_os_guard_corpus() {
        let cases: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/windows-native-guard.json"
        ))
        .expect("guard oracle corpus");
        for c in cases.as_array().expect("cases") {
            let result = windows_guard(&c["packages"], |_, _| {
                Ok(c["sha"].as_str().expect("sha").into())
            });
            assert_eq!(result.is_ok(), c["success"] == true);
            if let Ok(value) = result {
                assert_eq!(value, c["expected"]);
            }
        }
        let cases: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/windows-processes.json"))
                .expect("process oracle corpus");
        for c in cases.as_array().expect("cases") {
            let rows: Vec<Vec<String>> = serde_json::from_value(c["argv"].clone()).expect("argv");
            let result = classify(
                &rows,
                c["profile"].as_str().expect("profile"),
                "gmail",
                json!("target"),
                |s| s.starts_with("C:\\"),
                |s| c["identities"].get(s).cloned().ok_or(()),
            );
            assert_eq!(result.is_ok(), c["success"] == true, "{c}");
            if let Ok(value) = result {
                assert_eq!(json!(value), c["expected"], "{c}");
            }
        }
    }
    #[test]
    fn windows_native_guard_pins_every_identity_field_and_manager() {
        let good = json!([{"PackageFullName":"Claude_2.19675.0.0_x64__pzs8sxrjxfjjc","Version":"2.19675.0.0","InstallLocation":r"C:\Program Files\WindowsApps\Claude_2.19675.0.0_x64__pzs8sxrjxfjjc"}]);
        let sha = "492242eb2c352f60a886031275b7a37fd2fc791946be98b8c8fdd4bbfc41ded6";
        assert!(
            windows_guard(&good, |_, member| {
                assert_eq!(member, "index.chunk-DmADLdv3.js");
                Ok(sha.into())
            })
            .is_ok()
        );
        for key in ["PackageFullName", "Version", "InstallLocation"] {
            let mut bad = good.clone();
            bad[0][key] = "wrong".into();
            assert!(windows_guard(&bad, |_, _| panic!("unbound archive opened")).is_err());
        }
        assert!(windows_guard(&good, |_, _| Ok("wrong".into())).is_err());
    }
}
