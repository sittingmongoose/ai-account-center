//! Native artifact building, source fingerprints and trusted catalog verification.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
/// Build command result.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
/// Supported native targets, in the original staging order.
pub const TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
];
const CRATES: &[&str] = &[
    "crates/aac-agent",
    "crates/aac-core",
    "crates/aac-files",
    "crates/aac-num",
    "crates/aac-protocol",
    "crates/aac-providers",
    "crates/aac-status",
    "native/aac-usage",
];
const EMBEDDED: &[&str] = &[
    "config/private-files.json",
    "config/tables/providers.json",
    "config/tables/tools.json",
    "config/tables/hosts.json",
    "config/tables/antigravity-columns.json",
    "web-dashboard/public/computers.json",
    "tests/spec/wire-words.json",
];
/// SHA-256 in lowercase hex.
pub fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
/// Encode digest bytes as lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// Read JSON from disk.
pub fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
/// Write indented JSON with a trailing newline.
pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}
/// Set a packaged file's Unix mode.
pub fn mode(path: &Path, bits: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(bits))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, bits);
    }
    Ok(())
}
/// Execute a command and reject failed exit codes.
pub fn execute(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("Command failed with {status}").into());
    }
    Ok(())
}
fn walk(root: &Path, relative: &str, files: &mut Vec<String>) -> Result<()> {
    for e in fs::read_dir(root.join(relative))? {
        let e = e?;
        let child = format!("{relative}/{}", e.file_name().to_string_lossy());
        if e.file_type()?.is_dir() {
            walk(root, &child, files)?
        } else {
            files.push(child)
        }
    }
    Ok(())
}
/// Kind of independently staged binary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Remote host agent.
    Agent,
    /// Usage scanner.
    Usage,
    /// Local Cursor account reader.
    Cursor,
}
impl Kind {
    /// Binary stem.
    pub fn name(self) -> &'static str {
        match self {
            Self::Agent => "aac-agent",
            Self::Usage => "aac-usage-scan",
            Self::Cursor => "aac-cursor-account",
        }
    }
    /// Crate path relative to the workspace.
    pub fn crate_dir(self) -> &'static str {
        match self {
            Self::Agent => "crates/aac-agent",
            Self::Usage => "native/aac-usage",
            Self::Cursor => "native/aac-cursor-account",
        }
    }
    /// Output path relative to workspace.
    pub fn dist(self) -> &'static str {
        match self {
            Self::Agent => "dist/agent",
            Self::Usage => "dist/native/aac-usage",
            Self::Cursor => "dist/native/aac-cursor-account",
        }
    }
    /// Target-dependent binary name.
    pub fn binary(self, target: &str) -> String {
        format!(
            "{}{}",
            self.name(),
            if target.ends_with("windows-msvc") {
                ".exe"
            } else {
                ""
            }
        )
    }
}
/// Preserve the JavaScript path+NUL+LF-normalized-source+NUL fingerprint algorithm.
pub fn source_hash(root: &Path, kind: Kind) -> Result<String> {
    let base = if kind == Kind::Agent {
        root.to_path_buf()
    } else {
        root.join(kind.crate_dir())
    };
    let mut files: Vec<String> = [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo/config.toml",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if kind == Kind::Agent {
        for c in CRATES {
            for e in fs::read_dir(root.join(c))? {
                let e = e?;
                let name = e.file_name().to_string_lossy().into_owned();
                let rel = format!("{c}/{name}");
                if e.file_type()?.is_dir() {
                    if name == "src" {
                        walk(root, &rel, &mut files)?
                    }
                } else {
                    files.push(rel)
                }
            }
        }
        files.extend(EMBEDDED.iter().map(|s| s.to_string()));
    } else {
        walk(&base, "src", &mut files)?;
        if kind == Kind::Cursor {
            for dep in ["aac-files", "aac-core", "aac-num"] {
                files.push(format!("../../crates/{dep}/Cargo.toml"));
                walk(&base, &format!("../../crates/{dep}/src"), &mut files)?
            }
        }
    }
    files.sort();
    let mut hash = Sha256::new();
    for rel in files {
        let path = base.join(&rel);
        if kind == Kind::Agent && !path.exists() {
            continue;
        }
        hash.update(rel.as_bytes());
        hash.update([0]);
        hash.update(
            String::from_utf8_lossy(&fs::read(path)?)
                .replace("\r\n", "\n")
                .as_bytes(),
        );
        hash.update([0]);
    }
    Ok(hex(&hash.finalize()))
}
/// Current version inherited by the agent or declared by a native crate.
pub fn version(root: &Path, kind: Kind) -> Result<Value> {
    let path = if kind == Kind::Agent {
        root.join("Cargo.toml")
    } else {
        root.join(kind.crate_dir()).join("Cargo.toml")
    };
    let source = fs::read_to_string(path)?;
    // Keep the script's null result when its canonical version declaration is absent.
    let pattern = if kind == Kind::Agent {
        r#"(?ms)\[workspace\.package\].*?^version = "([^"]+)""#
    } else {
        r#"(?m)^version = "([^"]+)""#
    };
    let regex = regex::Regex::new(pattern)?;
    Ok(regex
        .captures(&source)
        .map(|c| json!(&c[1]))
        .unwrap_or(Value::Null))
}

fn protocol(root: &Path) -> Result<Value> {
    let s = fs::read_to_string(root.join("native/aac-usage/src/protocol.rs"))?;
    let re = regex::Regex::new(r"(?m)^pub const PROTOCOL: u64 = ([0-9]+);$")?;
    Ok(json!(
        re.captures(&s).ok_or("Missing scanner protocol")?[1].parse::<u64>()?
    ))
}
/// Stage only binaries whose note matches their current source and bytes.
pub fn copy_only(root: &Path, kind: Kind) -> Result<Value> {
    let current = source_hash(root, kind)?;
    let mut targets = serde_json::Map::new();
    let mut wire = if kind == Kind::Usage {
        protocol(root)?
    } else {
        Value::Null
    };
    for target in TARGETS {
        let binary = kind.binary(target);
        let file = root
            .join(kind.crate_dir())
            .join("prebuilt")
            .join(target)
            .join(&binary);
        if !file.exists() {
            continue;
        }
        let note = read_json(&file.with_file_name(format!("{binary}.json"))).unwrap_or(Value::Null);
        if note["sourceSha256"] != current || note["sha256"] != sha(&fs::read(&file)?) {
            eprintln!(
                "[!] {target} {} was built from other sources; it is not shipped.",
                match kind {
                    Kind::Agent => "agent",
                    Kind::Usage => "scanner",
                    Kind::Cursor => "Cursor account reader",
                }
            );
            continue;
        }
        if (kind == Kind::Usage || !wire.is_null()) && note["protocol"] != wire {
            eprintln!(
                "[!] {target} {} speaks protocol {}; it is not shipped.",
                match kind {
                    Kind::Agent => "agent",
                    Kind::Usage => "scanner",
                    Kind::Cursor => "Cursor account reader",
                },
                note["protocol"]
            );
            continue;
        }
        wire = note["protocol"].clone();
        let out = root.join(kind.dist()).join(target);
        fs::create_dir_all(&out)?;
        fs::copy(&file, out.join(&binary))?;
        mode(&out.join(&binary), 0o755)?;
        targets.insert(target.to_string(),json!({"file":format!("{target}/{binary}"),"sha256":note["sha256"],"size":note["size"]}));
    }
    let manifest = json!({"name":kind.name(),"version":version(root,kind)?,"protocol":wire,"sourceSha256":current,"targets":targets});
    write_json(&root.join(kind.dist()).join("manifest.json"), &manifest)?;
    if targets.is_empty() {
        println!(
            "{}",
            match kind {
                Kind::Agent =>
                    "[i] No host agent built from these sources: every computer keeps the labelled helper.",
                Kind::Usage => "[i] No usage-log scanner built from these sources.",
                Kind::Cursor =>
                    "[i] No Cursor account reader built from these sources: the Cursor account is not read (Analytics says so).",
            }
        );
    } else {
        println!(
            "[OK] {} for {} copied into {}/",
            match kind {
                Kind::Agent => "Host agent",
                Kind::Usage => "Usage-log scanner",
                Kind::Cursor => "Cursor account reader",
            },
            targets.keys().cloned().collect::<Vec<_>>().join(", "),
            kind.dist()
        );
    }
    Ok(manifest)
}
/// Verify staged bytes and the agent catalog pins. Missing catalog targets stay explicit.
pub fn verify_dist(root: &Path, kind: Kind, dir: &Path) -> Result<Value> {
    let path = dir.join("manifest.json");
    if !path.exists() {
        return Ok(if kind == Kind::Agent {
            json!({"present":false,"targets":[],"missing":[]})
        } else {
            json!({"present":false,"targets":[]})
        });
    }
    let manifest = read_json(&path)?;
    if kind == Kind::Agent && manifest["name"] != "aac-agent" {
        return Err("Not an agent manifest.".into());
    }
    let empty = serde_json::Map::new();
    let targets = manifest["targets"].as_object().unwrap_or(&empty);
    for (target, entry) in targets {
        if !TARGETS.contains(&target.as_str()) {
            return Err(format!("Unknown {} target {target}.", kind.name()).into());
        }
        let expected = format!("{target}/{}", kind.binary(target));
        let rel = entry["file"].as_str().ok_or("Missing artifact file")?;
        if rel != expected {
            return Err("Artifact path does not match target".into());
        }
        let file = dir.join(rel);
        if !file.is_file()
            || sha(&fs::read(&file)?) != entry["sha256"]
            || json!(fs::metadata(&file)?.len()) != entry["size"]
        {
            return Err(
                format!("The {target} {} does not match its manifest.", kind.name()).into(),
            );
        }
    }
    let mut missing = Vec::new();
    if kind == Kind::Agent {
        let catalog = read_json(&root.join("config/agent-artifacts.json"))?;
        if let Some(entries) = catalog["artifacts"].as_array() {
            for pin in entries {
                let Some(rel) = pin["file"]
                    .as_str()
                    .and_then(|s| s.strip_prefix("dist/agent/"))
                else {
                    continue;
                };
                let target = pin["target"].as_str().ok_or("Missing pin target")?;
                let entry = targets.get(target).unwrap_or(&Value::Null);
                let file = dir.join(rel);
                if entry.is_null() || entry["file"] != rel || !file.exists() {
                    missing.push(target.to_string());
                    continue;
                }
                if entry["sha256"] != pin["sha256"]
                    || sha(&fs::read(file)?) != pin["sha256"]
                    || pin["version"] != manifest["version"]
                {
                    return Err(format!(
                        "The {target} agent does not match its trusted catalog pin."
                    )
                    .into());
                }
            }
        }
        Ok(json!({"present":true,"targets":targets.keys().collect::<Vec<_>>(),"missing":missing}))
    } else {
        Ok(json!({"present":true,"targets":targets.keys().collect::<Vec<_>>()}))
    }
}
/// Target for the current operating system and architecture.
pub fn local_target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok(TARGETS[0]),
        ("macos", "aarch64") => Ok(TARGETS[1]),
        ("macos", "x86_64") => Ok(TARGETS[2]),
        ("windows", "x86_64") => Ok(TARGETS[3]),
        _ => Err("Unsupported native target".into()),
    }
}
/// Run a binary's offline version probe, bounded to ten seconds.
pub fn version_of(file: &Path) -> Result<Value> {
    use std::{
        io::Read,
        thread,
        time::{Duration, Instant},
    };
    let mut child = Command::new(file)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let out = child.stdout.take().ok_or("Missing version pipe")?;
    let reader = thread::spawn(move || {
        let mut b = Vec::new();
        out.take(1024 * 1024).read_to_end(&mut b).map(|_| b)
    });
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err("Version probe failed".into());
            }
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            child.kill()?;
            child.wait()?;
            return Err("Version probe timed out".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(serde_json::from_slice(
        &reader.join().map_err(|_| "Version reader failed")??,
    )?)
}
/// Require a current, runnable local scanner in a staged package.
pub fn verify_local_dist(root: &Path, dir: &Path) -> Result<Value> {
    let result = verify_dist(root, Kind::Usage, dir)?;
    let target = local_target()?;
    let manifest = read_json(&dir.join("manifest.json"))?;
    let answer = version_of(&dir.join(target).join(Kind::Usage.binary(target)))?;
    if manifest["name"] != "aac-usage-scan"
        || manifest["protocol"] != protocol(root)?
        || manifest["version"] != version(root, Kind::Usage)?
        || manifest["sourceSha256"] != source_hash(root, Kind::Usage)?
        || answer["name"] != manifest["name"]
        || answer["target"] != target
        || answer["protocol"] != manifest["protocol"]
        || answer["version"] != manifest["version"]
    {
        return Err("Local scanner does not match current source or protocol".into());
    }
    Ok(result)
}
/// Copy the dashboard binary and compatibility command names.
pub fn stage_dashboard(destination: &Path, artifacts: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
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
        let ext = if cfg!(windows) { ".exe" } else { "" };
        let original = if name == "aac-http" {
            "aac-http"
        } else {
            "ccs"
        };
        let out = destination.join(format!("{name}{ext}"));
        fs::copy(artifacts.join(format!("{original}{ext}")), &out)?;
        mode(&out, 0o755)?;
    }
    Ok(())
}
/// Update npm command paths only inside a package staging directory.
pub fn stage_manifest(root: &Path) -> Result<()> {
    if root.join(".git").exists() || root.join("src").exists() {
        return Err("Platform metadata must be written in a package staging directory.".into());
    }
    let path = root.join("package.json");
    let mut m = read_json(&path)?;
    for name in [
        "ai-account-center",
        "ccs",
        "ccs-droid",
        "ccsd",
        "ccs-codex",
        "ccsx",
        "ccsxp",
    ] {
        let rel = format!(
            "dist/native/{name}{}",
            if cfg!(windows) { ".exe" } else { "" }
        );
        if !root.join(&rel).is_file() {
            return Err(format!("Missing {rel}").into());
        }
        m["bin"][name] = json!(rel)
    }
    write_json(&path, &m)
}
fn target_dir(cwd: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|p| if p.is_absolute() { p } else { cwd.join(p) })
        .unwrap_or_else(|| cwd.join("target"))
}
fn catalog_entry(root: &Path) -> Result<()> {
    let target = local_target()?;
    let k = Kind::Agent;
    let file = root
        .join(k.crate_dir())
        .join("prebuilt")
        .join(target)
        .join(format!("{}.json", k.binary(target)));
    let note = read_json(&file)?;
    println!(
        "[i] Trusted-catalog entry (review, then commit into agent-artifacts.json):\n{}",
        serde_json::to_string_pretty(
            &json!({"target":target,"version":note["version"],"sha256":note["sha256"],"file":format!("dist/agent/{target}/{}",k.binary(target))})
        )?
    );
    Ok(())
}
fn build_native(root: &Path, kind: Kind) -> Result<()> {
    let cwd = if kind == Kind::Agent {
        root.to_path_buf()
    } else {
        root.join(kind.crate_dir())
    };
    let target = local_target()?;
    let output = target_dir(&cwd);
    let nas = std::env::var("HOSTNAME").unwrap_or_default() == "ubuntu-agent-nas1";
    if nas && kind == Kind::Agent {
        loop {
            let text = fs::read_to_string("/proc/meminfo")?;
            let kb = text
                .lines()
                .find_map(|l| l.strip_prefix("MemAvailable:"))
                .and_then(|l| l.split_whitespace().next())
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(u64::MAX);
            if kb >= 6 * 1024 * 1024 {
                break;
            }
            println!(
                "[i] Nas1: waiting for 6 GB available memory before cargo (re-check in 30 s)..."
            );
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
    }
    let mut command = Command::new(if nas && kind == Kind::Agent {
        "nice"
    } else {
        "cargo"
    });
    if nas && kind == Kind::Agent {
        command.args(["-n", "19", "cargo"]);
    }
    command.args(["build", "--release", "--locked"]);
    if kind == Kind::Agent {
        command.args(["-p", "aac-agent"]);
    }
    command
        .args([
            "-j",
            &std::env::var("CARGO_BUILD_JOBS").unwrap_or("2".into()),
        ])
        .current_dir(&cwd)
        .env("CARGO_TARGET_DIR", &output);
    execute(&mut command)?;
    let binary = kind.binary(target);
    let dest = root
        .join(kind.crate_dir())
        .join("prebuilt")
        .join(target)
        .join(&binary);
    fs::create_dir_all(dest.parent().ok_or("Missing artifact parent")?)?;
    fs::copy(output.join("release").join(&binary), &dest)?;
    mode(&dest, 0o755)?;
    let answer = version_of(&dest)?;
    if answer["target"] != target
        || (kind == Kind::Agent && answer["name"] != kind.name())
        || (kind == Kind::Usage
            && (answer["name"] != kind.name()
                || answer["protocol"] != protocol(root)?
                || answer["version"] != version(root, kind)?))
    {
        return Err("Built binary failed version identity check".into());
    }
    let rustc = Command::new("rustc")
        .arg("--version")
        .current_dir(cwd)
        .output()?;
    write_json(
        &dest.with_file_name(format!("{binary}.json")),
        &json!({"target":target,"sha256":sha(&fs::read(&dest)?),"size":fs::metadata(&dest)?.len(),"sourceSha256":source_hash(root,kind)?,"version":answer["version"],"protocol":answer["protocol"],"rustc":String::from_utf8_lossy(&rustc.stdout).trim(),"builtAt":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true)}),
    )?;
    if kind == Kind::Agent {
        copy_only(root, kind)?;
        catalog_entry(root)?
    }
    Ok(())
}
/// `build {ui|dashboard|agent|usage-scan|cursor-account}` dispatch.
pub fn run(root: &Path, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("ui");
    let copy = args.iter().any(|a| a == "--copy-only");
    match sub {
        "ui" => crate::ui::build_ui(root).map(|_| ()),
        "dashboard" | "dashboard-native" => {
            if let Some(i) = args.iter().position(|a| a == "--manifest") {
                return stage_manifest(Path::new(args.get(i + 1).ok_or("Missing staging root")?));
            }
            let output = target_dir(root);
            let no = std::env::var("AAC_NO_LOCAL_RUST_BUILD").as_deref() == Ok("1");
            if !copy && !no {
                execute(
                    Command::new("cargo")
                        .args([
                            "build",
                            "--release",
                            "--locked",
                            "-j",
                            &std::env::var("CARGO_BUILD_JOBS").unwrap_or("2".into()),
                            "-p",
                            "aac-http",
                            "--bin",
                            "aac-http",
                            "--bin",
                            "ccs",
                        ])
                        .current_dir(root)
                        .env("CARGO_TARGET_DIR", &output)
                        .env("CARGO_PROFILE_RELEASE_LTO", "thin")
                        .env("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "16"),
                )?
            }
            if no
                && !output
                    .join("release")
                    .join(if cfg!(windows) {
                        "aac-http.exe"
                    } else {
                        "aac-http"
                    })
                    .exists()
            {
                eprintln!(
                    "[!] AAC_NO_LOCAL_RUST_BUILD=1 and no prebuilt dashboard binaries: skipping the native dashboard stage."
                );
                Ok(())
            } else {
                stage_dashboard(&root.join("dist/native"), &output.join("release"))
            }
        }
        "agent" | "usage-scan" | "cursor-account" => {
            let kind = match sub {
                "agent" => Kind::Agent,
                "usage-scan" => Kind::Usage,
                _ => Kind::Cursor,
            };
            if copy {
                copy_only(root, kind)?;
            } else if kind == Kind::Agent && args.iter().any(|a| a == "--catalog-entry") {
                catalog_entry(root)?
            } else {
                build_native(root, kind)?;
                if args.iter().any(|a| a == "--stage") {
                    copy_only(root, kind)?;
                    if kind == Kind::Usage {
                        verify_local_dist(root, &root.join(kind.dist()))?;
                    }
                }
            }
            Ok(())
        }
        _ => Err(format!("Unknown build command: {sub}").into()),
    }
}
