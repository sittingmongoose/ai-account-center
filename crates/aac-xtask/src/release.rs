//! Release staging and installed-package validation.
use crate::{
    build::{self, Kind, Result, execute, read_json},
    bundle, lineage,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
/// Current CLI name and retained migration command names.
pub const COMMANDS: &[&str] = &[
    "ai-account-center",
    "ccs",
    "ccs-droid",
    "ccsd",
    "ccs-codex",
    "ccsx",
    "ccsxp",
];
fn suffix() -> &'static str {
    if cfg!(windows) { ".exe" } else { "" }
}
/// Validate native command metadata and required installed payloads.
pub fn installed_dashboard(root: &Path) -> Result<()> {
    if lineage::source_checkout(root) {
        return Ok(());
    }
    let m = read_json(&root.join("package.json"))?;
    for name in COMMANDS {
        if m["bin"][name] != format!("dist/native/{name}{}", suffix()) {
            return Err(
                format!("Dashboard command metadata is invalid for this platform: {name}").into(),
            );
        }
    }
    let mut paths = m["bin"]
        .as_object()
        .ok_or("Invalid command metadata")?
        .values()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    paths.extend([
        format!("dist/native/aac-http{}", suffix()),
        "dist/antigravity/terminal-signin-entry.js".into(),
        "dist/ui/app/index.html".into(),
    ]);
    for p in paths {
        let m = fs::metadata(root.join(&p))?;
        if !m.is_file() || m.len() == 0 {
            return Err(format!("Incomplete dashboard package: {p}").into());
        }
    }
    Ok(())
}
/// Verify installed agents and Cursor readers against release source and pins.
pub fn verify_artifacts(root: &Path, installed: &Path) -> Result<Vec<String>> {
    let pins = read_json(&root.join("config/agent-artifacts.json"))?;
    if read_json(&installed.join("config/agent-artifacts.json"))? != pins {
        return Err("Installed agent catalog differs".into());
    }
    for (kind, dir) in [
        (Kind::Agent, "dist/agent"),
        (Kind::Cursor, "dist/native/aac-cursor-account"),
    ] {
        let v = build::verify_dist(root, kind, &installed.join(dir))?;
        if v["present"] != true {
            return Err("Missing installed artifact manifest".into());
        }
        if kind == Kind::Agent && v["missing"].as_array().is_none_or(|a| !a.is_empty()) {
            return Err("Installed package is missing trusted agent pins".into());
        }
        let m = read_json(&installed.join(dir).join("manifest.json"))?;
        if m["sourceSha256"] != build::source_hash(root, kind)? {
            return Err(if kind == Kind::Agent {
                "Installed agent source mismatch"
            } else {
                "Installed Cursor reader source mismatch"
            }
            .into());
        }
        for t in build::TARGETS {
            if !v["targets"]
                .as_array()
                .is_some_and(|a| a.contains(&json!(t)))
            {
                return Err(format!(
                    "Missing installed {} for {t}",
                    if kind == Kind::Agent {
                        "agent"
                    } else {
                        "Cursor reader"
                    }
                )
                .into());
            }
            if kind == Kind::Agent
                && !pins["artifacts"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|e| e["target"] == *t))
            {
                return Err(format!("Missing agent pin for {t}").into());
            }
        }
    }
    Ok(build::TARGETS.iter().map(|t| t.to_string()).collect())
}
fn copy(root: &Path, relative: &Path, dest: &Path) -> Result<()> {
    let rel = relative.to_string_lossy().replace('\\', "/");
    let from = root.join(relative);
    if !from.exists() {
        return Ok(());
    }
    if rel == "dist/bin"
        || [
            "dist/ccs.js",
            "dist/ccs.js.map",
            "dist/ccs.d.ts",
            "dist/ccs.d.ts.map",
        ]
        .contains(&rel.as_str())
    {
        return Ok(());
    }
    if rel.starts_with("dist/native/") {
        let name = from.file_name().unwrap_or_default().to_string_lossy();
        let bare = name.strip_suffix(".exe").unwrap_or(&name);
        if (COMMANDS.contains(&bare) || bare == "aac-http")
            && name.ends_with(".exe") != cfg!(windows)
        {
            return Ok(());
        }
    }
    // Removed TS output can survive an incremental build; only keep emitted live sources.
    if let Some(tail) = rel.strip_prefix("dist/")
        && !["ui/", "native/", "app-updates/"]
            .iter()
            .any(|prefix| tail.starts_with(prefix))
    {
        let tail = tail.strip_suffix(".map").unwrap_or(tail);
        let source = tail
            .strip_suffix(".d.ts")
            .or_else(|| tail.strip_suffix(".js"));
        if source.is_some_and(|s| !root.join("src").join(format!("{s}.ts")).exists()) {
            return Ok(());
        }
    }
    if rel.starts_with("dist/native/aac-store.") || rel.starts_with("dist/native/aac-status.") {
        return Ok(());
    }
    let base = dest;
    let dest = base.join(relative);
    if from.is_dir() {
        fs::create_dir_all(&dest)?;
        for e in fs::read_dir(&from)? {
            copy(root, &relative.join(e?.file_name()), base)?;
        }
        fs::set_permissions(&dest, fs::metadata(&from)?.permissions())?;
    } else {
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p)?;
        }
        fs::copy(from, dest)?;
    }
    Ok(())
}
/// Stage an exact native package outside the source checkout.
pub fn stage(root: &Path, dest: &Path, expected: Option<&str>) -> Result<PathBuf> {
    let source = lineage::read_source(root).unwrap_or(Value::Null);
    if let Some(e) = expected {
        lineage::require_landed(root, &source, e)?;
    }
    if source["commit"].is_string() {
        let out = Command::new(root.join(format!("dist/native/aac-http{}", suffix())))
            .arg("--build-commit")
            .output()?;
        if !out.status.success()
            || String::from_utf8(out.stdout)?.trim() != source["commit"].as_str().unwrap_or("")
        {
            return Err(
                "The native dashboard was built from another commit. Rebuild before staging."
                    .into(),
            );
        }
    }
    build::verify_local_dist(root, &root.join("dist/native/aac-usage"))?;
    let dest = std::path::absolute(dest)?;
    let dest = fs::canonicalize(dest.parent().ok_or("staging parent")?)?
        .join(dest.file_name().ok_or("staging name")?);
    if dest.starts_with(fs::canonicalize(root)?) {
        return Err("Use a new package staging directory outside the source checkout.".into());
    }
    fs::create_dir(&dest)?;
    let m = read_json(&root.join("package.json"))?;
    let mut items = vec!["package.json".to_owned()];
    items.extend(
        m["files"]
            .as_array()
            .ok_or("package files")?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    for item in items {
        if !bundle::safe_path(&item) {
            return Err("Unsafe package file path".into());
        }
        copy(root, Path::new(&item), &dest)?;
    }
    build::stage_manifest(&dest)?;
    let metadata = lineage::record(root, &dest.join("dist"))?;
    if metadata["commit"] != source["commit"] {
        return Err("The source checkout changed while the package was staged.".into());
    }
    if let Some(e) = expected {
        lineage::require_landed(root, &metadata, e)?;
        verify_artifacts(root, &dest)?;
    }
    Ok(dest)
}
/// Dispatch this module’s command line operations.
pub fn run(root: &Path, args: &[String]) -> Result<()> {
    let Some((cmd, rest)) = args.split_first() else {
        return Err("Usage: aac-xtask release verify-bundle|lineage|stage|rebuild|postinstall|postuninstall".into());
    };
    match cmd.as_str() {
        "verify-bundle" => verify_package(root),
        "lineage" => lineage::run(root, rest),
        "stage" => match rest {
            [flag, dir] if flag == "--verify-release-artifacts" => { println!("Installed exact pinned agents and Cursor readers: {}", verify_artifacts(root, Path::new(dir))?.join(", ")); Ok(()) }
            [flag, sha, dir] if flag == "--stage-landed-release" => { println!("{}", stage(root, Path::new(dir), Some(sha))?.display()); Ok(()) }
            [dir] => { println!("{}", stage(root, Path::new(dir), None)?.display()); Ok(()) }
            _ => Err("Usage: aac-xtask release stage <directory> | --stage-landed-release <commit> <directory> | --verify-release-artifacts <package>".into()),
        },
        "postinstall" => {
            if !lineage::source_checkout(root) { lineage::print_verdict(lineage::check(root, &lineage::ccs_dir()?, std::env::var("AAC_ALLOW_DEPLOY_REGRESSION").as_deref() == Ok("1")))?; }
            (|| -> Result<()> {
                installed_dashboard(root)?;
                lineage::initialize(&lineage::ccs_dir()?)
            })().map_err(|e| format!("AI Account Center storage could not be prepared: {e}"))?;
            println!("[OK] AI Account Center storage is ready."); Ok(())
        }
        "postuninstall" => Ok(()),
        "rebuild" => rebuild(root, rest),
        _ => Err(format!("Unknown release command: {cmd}").into()),
    }
}
/// Verify every surviving release artifact, reporting independent failures together.
pub fn verify_package(root: &Path) -> Result<()> {
    let mut failed = false;
    let mut check = |result: Result<()>| {
        if let Err(error) = result {
            eprintln!("[X] {error}");
            failed = true;
        }
    };
    check((|| {
        let v = bundle::verify(&root.join("dist/ui"))?;
        println!(
            "[OK] Leptos {} bundle: {:.1}KB gzipped.",
            v["manifest"]["version"].as_str().unwrap_or(""),
            v["totalSize"].as_f64().unwrap_or(0.0) / 1024.0
        );
        Ok(())
    })());
    check((|| {
        for name in [
            "aac-http",
            "ai-account-center",
            "ccs",
            "ccs-droid",
            "ccsd",
            "ccs-codex",
            "ccsx",
            "ccsxp",
        ] {
            let p = root.join(format!("dist/native/{name}{}", suffix()));
            if !fs::metadata(&p).is_ok_and(|m| m.is_file() && m.len() > 0) {
                return Err(format!("Missing native dashboard command: {name}.").into());
            }
        }
        Ok(())
    })());
    check((|| {
        let v = build::verify_local_dist(root, &root.join("dist/native/aac-usage"))?;
        let targets = v["targets"]
            .as_array()
            .ok_or("scanner targets")?
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        println!(
            "[OK] Usage-log scanner for {} matches its manifest.",
            targets.join(", ")
        );
        Ok(())
    })());
    check((|| {
        let v = build::verify_dist(root, Kind::Agent, &root.join("dist/agent"))?;
        let targets = v["targets"]
            .as_array()
            .ok_or("agent targets")?
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if targets.is_empty() {
            eprintln!(
                "[!] No host agent in this build. Update apps requires a pinned agent. Build it on each target with: bun run agent:build"
            );
        } else {
            println!(
                "[OK] Host agent for {} matches its manifest.",
                targets.join(", ")
            );
        }
        let missing = v["missing"]
            .as_array()
            .ok_or("missing agent targets")?
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            eprintln!(
                "[!] Trusted catalog names {} but this build carries no such agent; those computers need a packaged agent and a matching installation for Update apps.",
                missing.join(", ")
            );
        }
        Ok(())
    })());
    if failed {
        Err(Box::new(crate::SilentExit(1)))
    } else {
        Ok(())
    }
}
fn value<'a>(args: &'a [String], flag: &str) -> Result<&'a str> {
    args.iter()
        .position(|s| s == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
        .ok_or_else(|| format!("Missing {flag}").into())
}
/// Rehearse npm installation with all home and provider paths isolated in child processes.
fn rebuild(root: &Path, args: &[String]) -> Result<()> {
    let expected = value(args, "--commit")?;
    let ledger = Path::new(value(args, "--wave84-ledger")?);
    let output = fs::canonicalize(value(args, "--output")?)?;
    if output.starts_with(fs::canonicalize(root)?) {
        return Err("--output must be outside the source checkout".into());
    }
    let previous = fs::read(ledger)?;
    let old: Value = serde_json::from_slice(&previous)?;
    if old["version"] != 1
        || old["commit"] != "e85a32edcf731557e635d3e689499946d558fd34"
        || old["dirty"] == true
    {
        return Err("--wave84-ledger must be a clean Wave 84 deployment ledger".into());
    }
    let scratch = tempfile::Builder::new()
        .prefix("landed-package-")
        .tempdir_in(output)?
        .keep();
    println!(
        "Release verification receipt directory {}",
        scratch.display()
    );
    let home = scratch.join("home");
    let mut env = std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>();
    for key in [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_CONFIG_HOME",
        "CCS_HOME",
        "CCS_DIR",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "AAC_AGENT_REPORTS_DIR",
        "AAC_PM_MAIL_REGISTRY_DIR",
    ] {
        env.insert(key.into(), home.join(key).into_os_string());
    }
    for key in [
        "AAC_ALLOW_DEPLOY_REGRESSION",
        "AAC_TS_SIDECAR",
        "SSH_AUTH_SOCK",
        "AAC_RUST_GROUP_COLLECTION",
        "AAC_RUST_STORE",
        "XDG_RUNTIME_DIR",
    ] {
        env.remove(std::ffi::OsStr::new(key));
    }
    for key in [
        "AAC_AGENT_VERBS",
        "AAC_AGENT_TRANSPORT",
        "AAC_STATUS_DOORBELL",
        "AAC_STORE_SHARE_BACKUP_DIR",
    ] {
        env.insert(key.into(), "off".into());
    }
    let tmp = scratch.join("tmp");
    fs::create_dir(&tmp)?;
    build::mode(&tmp, 0o700)?;
    for key in ["TMPDIR", "TMP", "TEMP"] {
        env.insert(key.into(), tmp.clone().into_os_string());
    }
    env.insert(
        "npm_config_userconfig".into(),
        home.join("absent-npmrc").into_os_string(),
    );
    env.insert(
        "npm_config_cache".into(),
        std::path::absolute(value(args, "--npm-cache")?)?.into_os_string(),
    );
    let ccs = home.join("CCS_DIR");
    lineage::initialize(&ccs)?;
    let ledger = ccs.join("deployed-build.json");
    fs::write(&ledger, &previous)?;
    let stage = scratch.join("stage");
    let exe = std::env::current_exe()?;
    execute(
        Command::new(&exe)
            .current_dir(root)
            .env_clear()
            .envs(&env)
            .args(["release", "stage", "--stage-landed-release", expected])
            .arg(&stage),
    )?;
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let packed = Command::new(npm)
        .current_dir(&stage)
        .env_clear()
        .envs(&env)
        .args(["pack", "--ignore-scripts", "--json", "--pack-destination"])
        .arg(&scratch)
        .output()?;
    if !packed.status.success() {
        return Err("npm pack failed".into());
    }
    let packed: Value = serde_json::from_slice(&packed.stdout)?;
    let archive = scratch.join(packed[0]["filename"].as_str().ok_or("pack filename")?);
    let prefix = scratch.join("prefix");
    let mut install = Command::new(npm);
    install
        .current_dir(&scratch)
        .env_clear()
        .envs(&env)
        .arg("install");
    if args.iter().any(|s| s == "--offline") {
        install.arg("--offline");
    }
    execute(
        install
            .args([
                "--no-audit",
                "--no-fund",
                "--foreground-scripts",
                "--ignore-scripts=false",
                "--prefix",
            ])
            .arg(&prefix)
            .arg(&archive),
    )?;
    let installed = prefix.join("node_modules/@sittingmongoose/ai-account-center");
    verify_artifacts(root, &installed)?;
    build::verify_local_dist(root, &installed.join("dist/native/aac-usage"))?;
    let metadata = read_json(&installed.join("dist/build-info.json"))?;
    if metadata["commit"] != expected
        || metadata["dirty"] != false
        || metadata["onMain"] != true
        || !metadata["lineage"]
            .as_array()
            .is_some_and(|a| a.contains(&old["commit"]))
    {
        return Err("Installed package has the wrong release lineage".into());
    }
    if fs::read(&ledger)? != previous {
        return Err("Postinstall altered the previous deployment ledger".into());
    }
    smoke(
        &installed.join(format!("dist/native/ai-account-center{}", suffix())),
        &scratch,
        &env,
        &ledger,
        expected,
    )?;
    println!(
        "{}  {}",
        build::sha(&fs::read(&archive)?),
        archive.display()
    );
    Ok(())
}
fn http(port: u16, path: &str) -> Result<Vec<u8>> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_secs(1),
    )?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut b = Vec::new();
    stream.read_to_end(&mut b)?;
    if !b.starts_with(b"HTTP/1.1 200 ") && !b.starts_with(b"HTTP/1.0 200 ") {
        return Err("Dashboard response was not 200".into());
    }
    Ok(b)
}
fn smoke(
    binary: &Path,
    scratch: &Path,
    env: &std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>,
    ledger: &Path,
    expected: &str,
) -> Result<()> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let log = fs::File::create(scratch.join("dashboard.log"))?;
    let mut command = Command::new(binary);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000200);
    }
    let mut child = command
        .args([
            "dashboard",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--no-open",
        ])
        .current_dir(scratch)
        .env_clear()
        .envs(env)
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let result = (|| -> Result<()> {
        let mut ready = false;
        for _ in 0..120 {
            if child.try_wait()?.is_some() {
                return Err("Dashboard exited before readiness. See dashboard.log".into());
            }
            if http(port, "/api/health").is_ok() {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        if !ready {
            return Err("Dashboard failed to become ready. See dashboard.log".into());
        }
        if !String::from_utf8_lossy(&http(port, "/app/home")?)
            .to_ascii_lowercase()
            .contains("<html")
        {
            return Err("Dashboard failed to serve the packaged Leptos page".into());
        }
        for _ in 0..40 {
            if read_json(ledger)?["commit"] == expected {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err("The installed dashboard did not advance the deployment ledger".into())
    })();
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(child.id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    #[cfg(windows)]
    {
        // The child has its own process group; target only that group with CTRL_BREAK.
        // SAFETY: the API takes integer identifiers, with no pointer lifetime requirements.
        #[allow(
            unsafe_code,
            reason = "Windows console API targets the isolated smoke-test process group"
        )]
        unsafe {
            windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(1, child.id());
        }
    }
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() > std::time::Duration::from_secs(15) {
            child.kill()?;
            break child.wait()?;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    result?;
    if !status.success() {
        return Err(format!("Dashboard stopped with {status}. See dashboard.log").into());
    }
    println!("Health 200, Leptos 200, ledger {expected}, clean stop");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staging_filters_retired_and_foreign_outputs() {
        let root = tempfile::Builder::new()
            .prefix("aac-x2-copy-")
            .tempdir()
            .expect("fixture")
            .keep();
        let dest = tempfile::Builder::new()
            .prefix("aac-x2-package-")
            .tempdir()
            .expect("fixture")
            .keep();
        for f in [
            "dist/ccs.js",
            "dist/deleted.js",
            "dist/retained.js",
            "dist/ui/asset.js",
            "dist/native/aac-store.linux-x64.node",
            "dist/native/ccs",
            "dist/native/ccs.exe",
            "src/retained.ts",
        ] {
            let p = root.join(f);
            fs::create_dir_all(p.parent().expect("fixture")).expect("fixture");
            fs::write(p, "fixture").expect("fixture");
        }
        copy(&root, Path::new("dist"), &dest).expect("fixture");
        assert!(!dest.join("dist/ccs.js").exists());
        assert!(!dest.join("dist/deleted.js").exists());
        assert!(!dest.join("dist/native/aac-store.linux-x64.node").exists());
        assert!(dest.join("dist/retained.js").exists());
        assert!(dest.join("dist/ui/asset.js").exists());
        assert_eq!(dest.join("dist/native/ccs.exe").exists(), cfg!(windows));
        assert_eq!(dest.join("dist/native/ccs").exists(), !cfg!(windows));
    }
}
