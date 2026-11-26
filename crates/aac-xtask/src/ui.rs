//! Deterministic Leptos packaging and source validation.
use crate::build::{Result, execute, mode, read_json, sha, write_json};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};
/// Pinned Leptos version.
pub const LEPTOS_VERSION: &str = "0.9.0-rc1";
const INPUTS: &[&str] = &[
    ".cargo/config.toml",
    "rust-toolchain.toml",
    "Cargo.toml",
    "Cargo.lock",
    "index.html",
    "build",
    "fonts",
    "src",
    "style",
];
const SKIP: &[&str] = &[".gitignore", ".npmignore", "package.json", "README.md"];
/// Sorted POSIX paths, refusing symbolic links.
pub fn list_files(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    collect(root, "", &mut out)?;
    Ok(out)
}
fn collect(root: &Path, rel: &str, out: &mut Vec<String>) -> Result<()> {
    let p = root.join(rel);
    let stat = match fs::symlink_metadata(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if stat.is_symlink() {
        return Err("Dashboard build inputs/output must not contain symlinks.".into());
    }
    if stat.is_dir() {
        let mut entries = fs::read_dir(p)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            collect(
                root,
                &if rel.is_empty() {
                    name
                } else {
                    format!("{rel}/{name}")
                },
                out,
            )?
        }
    } else if stat.is_file() {
        out.push(rel.to_string())
    }
    Ok(())
}
fn hash_rows(rows: impl IntoIterator<Item = (String, Vec<u8>)>) -> Value {
    let mut rows: Vec<_> = rows.into_iter().collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let count = rows.len();
    let mut h = Sha256::new();
    for (p, b) in rows {
        h.update(p);
        h.update([0]);
        h.update(b);
        h.update([0]);
    }
    json!({"sha256":crate::build::hex(&h.finalize()),"files":count})
}
/// Fingerprint raw bytes and normalized relative paths, without following links.
pub fn fingerprint_inputs(root: &Path, inputs: &[String]) -> Result<Value> {
    let stat = fs::symlink_metadata(root)?;
    if stat.is_symlink() || !stat.is_dir() {
        return Err("Leptos build root must be a real folder.".into());
    }
    let mut files = Vec::new();
    for input in inputs {
        let normalized = input.replace('\\', "/");
        let parts: Vec<_> = normalized.split('/').collect();
        for n in 1..parts.len() {
            match fs::symlink_metadata(root.join(parts[..n].join("/"))) {
                Ok(s) if s.is_symlink() || !s.is_dir() => {
                    return Err("Leptos build inputs must not be symlinks.".into());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                Err(e) => return Err(e.into()),
                _ => (),
            }
        }
        collect(root, &normalized, &mut files)?;
    }
    let mut rows = Vec::new();
    for p in files {
        rows.push((p.clone(), fs::read(root.join(p))?))
    }
    Ok(hash_rows(rows))
}
/// Source stamp includes shared view and number types, as in the JavaScript oracle.
pub fn leptos_fingerprint(root: &Path) -> Result<Value> {
    fingerprint_inputs(
        &root.join("web-dashboard/leptos"),
        &INPUTS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    )?;
    let mut inputs: Vec<_> = INPUTS
        .iter()
        .map(|p| format!("web-dashboard/leptos/{p}"))
        .collect();
    inputs.extend(
        [
            "web-dashboard/public/computers.json",
            "Cargo.toml",
            "crates/aac-api/Cargo.toml",
            "crates/aac-api/src",
            "crates/aac-num/Cargo.toml",
            "crates/aac-num/src",
        ]
        .map(str::to_string),
    );
    fingerprint_inputs(root, &inputs)
}
/// Render app files with runtime and stylesheet addresses.
pub fn staged_app_files(crate_dir: &Path, id: &str) -> Result<BTreeMap<String, Vec<u8>>> {
    let template = fs::read_to_string(crate_dir.join("index.html"))?;
    if !template.contains("{{PKG}}") || !template.contains("{{CSS}}") {
        return Err("The Leptos page template must reference {{PKG}} and {{CSS}}.".into());
    }
    let css = fs::read(crate_dir.join("style/app.css"))?;
    let html = template
        .replace("{{PKG}}", &format!("/pkg/{id}"))
        .replace("{{CSS}}", &format!("/app/app.css?v={}", &sha(&css)[..12]));
    let mut map = BTreeMap::from([
        ("app/index.html".into(), html.into_bytes()),
        ("app/app.css".into(), css),
    ]);
    for font in ["MartianMono.ttf", "OFL-MartianMono.txt"] {
        map.insert(
            format!("app/{font}"),
            fs::read(crate_dir.join("fonts").join(font))?,
        );
    }
    Ok(map)
}
/// Shell hash covers original service worker placeholders and the rendered frame.
pub fn shell_fingerprint(public: &Path, app: &BTreeMap<String, Vec<u8>>) -> Result<Value> {
    let mut rows = Vec::new();
    for p in list_files(public)? {
        rows.push((p.clone(), fs::read(public.join(p))?))
    }
    rows.extend(app.iter().map(|(p, b)| (p.clone(), b.clone())));
    Ok(hash_rows(rows))
}
/// Canonical pages and sorted requestable assets for the service worker.
pub fn precache_list(files: &[String], css: &[u8]) -> Vec<String> {
    let mut out: Vec<_> = [
        "/",
        "/app/home",
        "/app/analytics",
        "/app/accounts",
        "/app/status",
        "/app/scoreboard",
    ]
    .map(str::to_string)
    .into();
    let mut assets: Vec<_> = files
        .iter()
        .filter(|p| {
            p.as_str() != "app/index.html"
                && !matches!(
                    p.as_str(),
                    "sw.js" | "sw-route.js" | "ui-build-manifest.json"
                )
                && ![".d.ts", ".txt", ".md", ".br", ".gz"]
                    .iter()
                    .any(|ext| p.ends_with(ext))
        })
        .cloned()
        .collect();
    assets.sort();
    out.extend(assets.iter().map(|p| {
        if p == "app/app.css" {
            format!("/app/app.css?v={}", &sha(css)[..12])
        } else {
            format!("/{p}")
        }
    }));
    out
}
/// Require unique placeholders before stamping.
pub fn stamped_sw(source: &str, id: &str, precache: Option<&[String]>) -> Result<String> {
    if source.matches("__AAC_BUILD_ID__").count() != 1 {
        return Err("sw.js must name the __AAC_BUILD_ID__ placeholder exactly once.".into());
    }
    let mut out = source.replace("__AAC_BUILD_ID__", id);
    if let Some(list) = precache {
        let token = "[/*__AAC_PRECACHE__*/]";
        if out.matches(token).count() != 1 {
            return Err(
                "sw.js must name the [/*__AAC_PRECACHE__*/] placeholder exactly once.".into(),
            );
        }
        out = out.replace(token, &serde_json::to_string(list)?)
    }
    Ok(out)
}
/// Validate exact lock versions before invoking any compiler.
pub fn assert_pins(crate_dir: &Path) -> Result<()> {
    let lock: toml::Value = toml::from_str(&fs::read_to_string(crate_dir.join("Cargo.lock"))?)?;
    let packages = lock["package"].as_array().ok_or("Missing lock packages")?;
    for (name, version) in [
        ("leptos", LEPTOS_VERSION),
        ("wasm-bindgen", "0.2.130"),
        ("wasm-bindgen-futures", "0.4.80"),
        ("js-sys", "0.3.107"),
        ("web-sys", "0.3.107"),
    ] {
        let found: Vec<_> = packages
            .iter()
            .filter(|p| p["name"].as_str() == Some(name))
            .collect();
        if found.len() != 1 || found[0]["version"].as_str() != Some(version) {
            return Err(format!("Leptos Cargo.lock must resolve {name} to {version}.").into());
        }
    }
    Ok(())
}
fn leb(bytes: &[u8], at: &mut usize) -> Result<usize> {
    let mut value = 0usize;
    for shift in (0..usize::BITS).step_by(7) {
        let b = *bytes.get(*at).ok_or("truncated LEB128")?;
        *at += 1;
        value |= ((b & 127) as usize)
            .checked_shl(shift)
            .ok_or("oversize LEB128")?;
        if b < 128 {
            return Ok(value);
        }
    }
    Err("oversize LEB128".into())
}
/// Remove only WebAssembly name custom sections.
pub fn strip_names(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.get(..8) != Some(&[0, 97, 115, 109, 1, 0, 0, 0]) {
        return Err("The dashboard build output is not a valid WebAssembly module.".into());
    }
    let mut out = bytes[..8].to_vec();
    let mut at = 8;
    while at < bytes.len() {
        let start = at;
        let id = bytes[at];
        at += 1;
        let size = leb(bytes, &mut at)?;
        let end = at
            .checked_add(size)
            .filter(|e| *e <= bytes.len())
            .ok_or("truncated section")?;
        let mut keep = true;
        if id == 0 {
            let mut n = at;
            let len = leb(bytes, &mut n)?;
            let name_end = n
                .checked_add(len)
                .filter(|e| *e <= end)
                .ok_or("truncated custom section name")?;
            keep = &bytes[n..name_end] != b"name";
        }
        if keep {
            out.extend_from_slice(&bytes[start..end])
        }
        at = end;
    }
    Ok(out)
}
fn cache_root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".cache")
        })
        .join("ai-account-center")
}
/// Owner-file lock used alongside the legacy builder on platforms without flock.
/// The exact record is checked before release so an old owner cannot unlink a new one.
pub struct OwnerLock {
    file: PathBuf,
    record: Vec<u8>,
}
impl Drop for OwnerLock {
    fn drop(&mut self) {
        if fs::read(&self.file).ok().as_ref() == Some(&self.record) {
            let _ = fs::remove_file(&self.file);
        }
    }
}
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn start_ticks(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(
        stat.get(stat.rfind(')')? + 2..)?
            .split_whitespace()
            .nth(19)?
            .to_string(),
    )
}
fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        matches!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None),
            Ok(()) | Err(nix::errno::Errno::EPERM)
        )
    }
    #[cfg(not(unix))]
    {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
    }
}
/// Classify legacy owner records using age, liveness and Linux process identity.
pub fn owner_is_stale(text: &[u8], now: u64) -> bool {
    let Ok(v) = serde_json::from_slice::<Value>(text) else {
        return true;
    };
    let Some(pid) = v["pid"]
        .as_u64()
        .filter(|p| *p > 0 && *p <= i32::MAX as u64)
    else {
        return true;
    };
    let Some(acquired) = v["acquiredAt"].as_u64() else {
        return true;
    };
    if now.saturating_sub(acquired) > 3 * 60 * 60 * 1000 || !alive(pid as u32) {
        return true;
    }
    if let Some(expected) = v["startTicks"].as_str()
        && let Some(current) = start_ticks(pid as u32)
    {
        return current != expected;
    }
    false
}
/// Acquire an exclusive-create owner file, reclaiming only stale records.
pub fn try_owner_file(file: &Path, now: u64) -> Result<Option<OwnerLock>> {
    fs::create_dir_all(file.parent().ok_or("Missing lock parent")?)?;
    let record = serde_json::to_vec(
        &json!({"pid":std::process::id(),"startTicks":start_ticks(std::process::id()),"acquiredAt":now,"host":std::env::var("HOSTNAME").or_else(|_|std::env::var("COMPUTERNAME")).unwrap_or_default()}),
    )?;
    for _ in 0..2 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file)
        {
            Ok(mut f) => {
                f.write_all(&record)?;
                return Ok(Some(OwnerLock {
                    file: file.to_path_buf(),
                    record,
                }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        let existing = match fs::read(file) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        if !owner_is_stale(&existing, now) {
            return Ok(None);
        }
        let aside = file.with_file_name(format!(
            "{}.stale-{}-{now}",
            file.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        if fs::rename(file, &aside).is_err() {
            continue;
        }
        if fs::read(&aside).ok().as_ref() != Some(&existing) {
            let _ = fs::hard_link(&aside, file);
        }
        fs::remove_file(aside)?;
    }
    Ok(None)
}
/// Shared kernel lock, released on process death, with the original bounded wait.
pub fn with_build_lock<T>(
    file: &Path,
    wait: std::time::Duration,
    run: impl FnOnce() -> Result<T>,
) -> Result<T> {
    use fs2::FileExt;
    // Match the legacy script's backend while both implementations coexist.
    let use_flock = cfg!(target_os = "linux")
        && Command::new("flock")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
    if !use_flock {
        let start = std::time::Instant::now();
        loop {
            if let Some(_guard) = try_owner_file(file, now_millis())? {
                return run();
            }
            if start.elapsed() >= wait {
                return Err(
                    "UI build lane busy (another release wasm build is running); retry later"
                        .into(),
                );
            }
            std::thread::sleep(
                std::time::Duration::from_secs(3).min(wait.saturating_sub(start.elapsed())),
            );
        }
    }
    fs::create_dir_all(file.parent().ok_or("Missing lock parent")?)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(file)?;
    let start = std::time::Instant::now();
    let mut next = std::time::Duration::from_secs(60);
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
            Err(e) => return Err(e.into()),
        }
        let elapsed = start.elapsed();
        if elapsed >= wait {
            return Err(
                "UI build lane busy (another release wasm build is running); retry later".into(),
            );
        }
        if elapsed >= next {
            println!(
                "[i] Waiting for the UI build lane: another release wasm build is running ({}s of {}s).",
                elapsed.as_secs(),
                wait.as_secs()
            );
            next += std::time::Duration::from_secs(60);
        }
        std::thread::sleep(std::time::Duration::from_secs(3).min(wait.saturating_sub(elapsed)));
    }
    let result = run();
    lock.unlock()?;
    result
}
fn capture(command: &mut Command) -> Result<String> {
    let out = command.output()?;
    if !out.status.success() {
        return Err(format!("Command failed with {}", out.status).into());
    }
    Ok(String::from_utf8(out.stdout)?)
}
fn copy_tree(src: &Path, dst: &Path, skip: bool) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if skip && SKIP.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_symlink() {
            return Err("Dashboard build output must not contain symlinks.".into());
        }
        let dest = dst.join(name);
        if metadata.is_dir() {
            copy_tree(&entry.path(), &dest, skip)?
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}
fn normalize_modes(dir: &Path) -> Result<()> {
    mode(dir, 0o755)?;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let meta = fs::symlink_metadata(entry.path())?;
        if meta.is_symlink() {
            return Err("Dashboard build output must not contain symlinks.".into());
        }
        if meta.is_dir() {
            normalize_modes(&entry.path())?
        } else {
            mode(&entry.path(), 0o644)?
        }
    }
    Ok(())
}
fn tool(name: &str) -> Command {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_default();
    let bin = home.join(".cargo/bin");
    let candidate = bin.join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }));
    let mut command = Command::new(if candidate.exists() {
        candidate
    } else {
        PathBuf::from(name)
    });
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    if let Ok(path) = std::env::join_paths(paths) {
        command.env("PATH", path);
    }
    command
}
/// Precompress text and wasm assets with deterministic gzip and Brotli streams.
pub fn precompress_ui(dir: &Path) -> Result<Vec<Value>> {
    let mut entries = Vec::new();
    for p in list_files(dir)? {
        let ext = Path::new(&p)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !["wasm", "js", "mjs", "css", "html", "svg", "json", "txt"].contains(&ext.as_str()) {
            continue;
        }
        let bytes = fs::read(dir.join(&p))?;
        if bytes.len() < 1024 {
            continue;
        }
        let encoder = brotlic::BrotliEncoderOptions::new()
            .quality(brotlic::Quality::best())
            .size_hint(bytes.len().try_into()?)
            .mode(if ext == "wasm" {
                brotlic::CompressionMode::Generic
            } else {
                brotlic::CompressionMode::Text
            })
            .build()?;
        let mut writer = brotlic::CompressorWriter::with_encoder(encoder, Vec::new());
        writer.write_all(&bytes)?;
        let br = writer.into_inner()?;
        let mut gz = flate2::GzBuilder::new()
            .mtime(0)
            .operating_system(3)
            .write(Vec::new(), flate2::Compression::new(9));
        gz.write_all(&bytes)?;
        for (encoding, suffix, compressed) in [("br", ".br", br), ("gzip", ".gz", gz.finish()?)] {
            if compressed.len() as f64 >= bytes.len() as f64 * 0.9 {
                continue;
            }
            let file = format!("{p}{suffix}");
            fs::write(dir.join(&file), &compressed)?;
            entries.push(json!({"path":p,"encoding":encoding,"file":file,"bytes":compressed.len(),"sha256":sha(&compressed)}));
        }
    }
    Ok(entries)
}
/// Commit only when this directory itself is the repository root.
pub fn read_build_commit(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-toplevel", "--short=8", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let mut lines = text.lines();
    if fs::canonicalize(lines.next()?).ok()? != fs::canonicalize(root).ok()? {
        return None;
    }
    let commit = lines.next()?.trim();
    if (7..=40).contains(&commit.len())
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Some(commit.to_string())
    } else {
        None
    }
}
/// Package an already built wasm runtime. All inputs are validated before replacing the output.
pub fn package_ui(root: &Path) -> Result<Value> {
    let crate_dir = root.join("web-dashboard/leptos");
    let pkg = crate_dir.join("pkg");
    let public = root.join("web-dashboard/public");
    let source = leptos_fingerprint(root)?;
    let wasm = strip_names(&fs::read(pkg.join("aac_web_bg.wasm"))?)?;
    fs::metadata(pkg.join("aac_web.js"))?;
    let wasm_hash = sha(&wasm);
    let id = &wasm_hash[..12];
    let app = staged_app_files(&crate_dir, id)?;
    let shell = shell_fingerprint(&public, &app)?;
    let shell_hash = shell["sha256"].as_str().ok_or("Missing shell hash")?;
    let mut files = list_files(&public)?;
    files.extend(app.keys().cloned());
    files.extend(
        list_files(&pkg)?
            .into_iter()
            .filter(|p| {
                !SKIP.contains(
                    &Path::new(p)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .as_ref(),
                )
            })
            .map(|p| format!("pkg/{id}/{p}")),
    );
    files.sort();
    files.dedup();
    let precache = precache_list(&files, app.get("app/app.css").ok_or("Missing stylesheet")?);
    let sw = if public.join("sw.js").exists() {
        Some(stamped_sw(
            &fs::read_to_string(public.join("sw.js"))?,
            &shell_hash[..12],
            Some(&precache),
        )?)
    } else {
        None
    };
    let out = root.join("dist/ui");
    if out.exists() {
        fs::remove_dir_all(&out)?
    }
    copy_tree(&public, &out, false)?;
    copy_tree(&pkg, &out.join("pkg").join(id), true)?;
    fs::write(pkg.join("aac_web_bg.wasm"), &wasm)?;
    fs::write(out.join("pkg").join(id).join("aac_web_bg.wasm"), &wasm)?;
    if let Some(sw) = sw {
        fs::write(out.join("sw.js"), sw)?
    }
    for (p, b) in app {
        let dest = out.join(p);
        fs::create_dir_all(dest.parent().ok_or("Missing app parent")?)?;
        fs::write(dest, b)?;
    }
    let compressed = precompress_ui(&out)?;
    let mut manifest = json!({"schemaVersion":2,"framework":"leptos","version":LEPTOS_VERSION,"target":"wasm32-unknown-unknown","entry":"app/index.html","source":source,"buildId":id,"shellBuildId":&shell_hash[..12],"shell":shell});
    if let Some(commit) = read_build_commit(root) {
        manifest["commit"] = json!(commit)
    }
    manifest["wasm"] =
        json!({"path":format!("pkg/{id}/aac_web_bg.wasm"),"bytes":wasm.len(),"sha256":wasm_hash});
    manifest["precompressed"] = json!(compressed);
    write_json(&out.join("ui-build-manifest.json"), &manifest)?;
    normalize_modes(&out)?;
    Ok(manifest)
}
/// Validate toolchain and pins, serialize compilation, then package its output.
pub fn build_ui(root: &Path) -> Result<Value> {
    if std::env::var("AAC_NO_LOCAL_RUST_BUILD").as_deref() == Ok("1") {
        return Err("This machine sets AAC_NO_LOCAL_RUST_BUILD=1, so the dashboard is not compiled here. Build on a build host (the landing gate or the Windows build recipe) instead.".into());
    }
    let crate_dir = root.join("web-dashboard/leptos");
    leptos_fingerprint(root)?;
    assert_pins(&crate_dir)?;
    let rust = capture(tool("rustc").arg("--version").current_dir(&crate_dir))?;
    let re = regex::Regex::new(r"^rustc ([0-9]+)\.([0-9]+)\.")?;
    let caps = re.captures(&rust).ok_or("Missing Rust toolchain version")?;
    let major = caps[1].parse::<u64>()?;
    if major < 1 || (major == 1 && caps[2].parse::<u64>()? < 99) {
        return Err("The dashboard requires Rust 1.99.1 or newer.".into());
    }
    if !capture(
        tool("rustup")
            .args(["target", "list", "--installed"])
            .current_dir(&crate_dir),
    )?
    .split_whitespace()
    .any(|s| s == "wasm32-unknown-unknown")
    {
        return Err(
            "Install the browser target with rustup target add wasm32-unknown-unknown.".into(),
        );
    }
    if capture(tool("wasm-pack").arg("--version").current_dir(&crate_dir))?.trim()
        != "wasm-pack 0.15.1"
    {
        return Err("The dashboard requires wasm-pack 0.15.1.".into());
    }
    let cache = cache_root();
    let wait = std::env::var("AAC_UI_BUILD_LOCK_WAIT_S")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n >= 0.0)
        .unwrap_or(600.0);
    with_build_lock(
        &cache.join("ui-build.lock"),
        std::time::Duration::from_secs_f64(wait),
        || {
            execute(
                tool("wasm-pack")
                    .arg("build")
                    .arg(&crate_dir)
                    .args([
                        "--release",
                        "--target",
                        "web",
                        "--out-dir",
                        "pkg",
                        "--out-name",
                        "aac_web",
                        "--",
                        "--locked",
                    ])
                    .current_dir(root)
                    .env(
                        "CARGO_TARGET_DIR",
                        std::env::var_os("CARGO_TARGET_DIR")
                            .map(PathBuf::from)
                            .unwrap_or_else(|| cache.join("cargo-target-wasm")),
                    ),
            )
        },
    )?;
    let m = package_ui(root)?;
    println!(
        "[OK] Leptos {LEPTOS_VERSION} dashboard: {} bytes of WebAssembly.",
        m["wasm"]["bytes"]
    );
    Ok(m)
}
/// Validate source and shell stamps after the release lane's complete bundle verification.
pub fn validate_stamps(root: &Path) -> Result<Value> {
    let manifest = read_json(&root.join("dist/ui/ui-build-manifest.json"))?;
    let source = leptos_fingerprint(root)?;
    if manifest["source"] != source {
        return Err(
            "Dashboard source changed after the build. Run cargo run -p aac-xtask -- build ui."
                .into(),
        );
    }
    let id = manifest["buildId"].as_str().ok_or("Missing build id")?;
    let shell = shell_fingerprint(
        &root.join("web-dashboard/public"),
        &staged_app_files(&root.join("web-dashboard/leptos"), id)?,
    )?;
    let hash = shell["sha256"].as_str().ok_or("Missing shell hash")?;
    if manifest["shell"] != shell || manifest["shellBuildId"] != hash[..12] {
        return Err(
            "Dashboard shell changed after the build. Run cargo run -p aac-xtask -- build ui."
                .into(),
        );
    }
    Ok(source)
}
/// UI validation entry point, retaining the release lane's oracle until its port lands.
pub fn run(root: &Path, _args: &[String]) -> Result<()> {
    validate_with(root, |dir| crate::bundle::verify(dir).map(|_| ()))
}
/// Validate source stamps together with the complete release verifier.
pub fn validate_with(root: &Path, verify_bundle: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let crate_dir = root.join("web-dashboard/leptos");
    assert_pins(&crate_dir)?;
    execute(
        tool("cargo")
            .args(["fmt", "--manifest-path"])
            .arg(crate_dir.join("Cargo.toml"))
            .arg("--check")
            .current_dir(&crate_dir),
    )?;
    let mut tests = list_files(&root.join("web-dashboard/tests"))?
        .into_iter()
        .filter(|s| !s.contains('/') && s.ends_with(".test.mjs"))
        .collect::<Vec<_>>();
    tests.sort();
    execute(
        Command::new("node")
            .arg("--test")
            .args(
                tests
                    .iter()
                    .map(|p| root.join("web-dashboard/tests").join(p)),
            )
            .current_dir(root),
    )?;
    verify_bundle(&root.join("dist/ui"))?;
    let source = validate_stamps(root)?;
    println!(
        "[OK] Leptos {LEPTOS_VERSION} UI validated against {} current build inputs.",
        source["files"]
    );
    Ok(())
}
