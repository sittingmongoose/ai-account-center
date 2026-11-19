use super::*;
use std::io::{Read as _, Seek, SeekFrom};

pub(super) async fn desktop(host: &Host, probe: &impl Probe, codex: bool) -> Option<Install> {
    match host.platform {
        Platform::Mac => {
            let (name, identity) = if codex {
                ("ChatGPT.app", "com.openai.codex")
            } else {
                ("Claude.app", "com.anthropic.claudefordesktop")
            };
            for path in [
                host.system("/Applications").join(name),
                host.home.join("Applications").join(name),
            ] {
                if path.exists() {
                    let release = bundle_version(host, probe, &path, identity).await;
                    return Some(Install::new(
                        "official-download",
                        version(release.as_deref()),
                    ));
                }
            }
            None
        }
        Platform::Windows => {
            let (identity, executable) = if codex {
                ("OpenAI.Codex", "app/ChatGPT.exe")
            } else {
                ("Claude", "app/Claude.exe")
            };
            let shell = host.powershell.as_deref()?;
            let result = probe.run(shell, &ps_command(format!(
                "$ErrorActionPreference='Stop'; $p=Get-AppxPackage -Name '{identity}' | Sort-Object {{[Version]$_.Version}} -Descending | Select-Object -First 1; if($p){{@{{name=$p.Name;version=$p.Version.ToString();publisher=$p.Publisher;root=$p.InstallLocation;family=$p.PackageFamilyName}}|ConvertTo-Json -Compress}}"
            )), 30).await;
            if result.output().is_some_and(|text| text.trim().is_empty()) {
                return None;
            }
            let current = result
                .output()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .filter(|package| package["name"] == identity)
                .filter(|package| {
                    package["root"]
                        .as_str()
                        .is_some_and(|root| Path::new(root).join(executable).is_file())
                })
                .and_then(|package| version(package["version"].as_str()));
            Some(Install::new("msix", current))
        }
        Platform::Ubuntu => {
            let (package, executable) = if codex {
                ("chatgpt", "/usr/lib/chatgpt/ChatGPT")
            } else {
                ("claude-desktop", "/usr/lib/claude-desktop/claude-desktop")
            };
            if !host.system(executable).is_file() {
                return None;
            }
            let result = probe
                .run(
                    &host.system("/usr/bin/dpkg-query"),
                    &args(&["-W", "-f=${Version}", package]),
                    20,
                )
                .await;
            Some(Install::new("apt", result.version()))
        }
    }
}

async fn bundle_version(
    host: &Host,
    probe: &impl Probe,
    bundle: &Path,
    identity: &str,
) -> Option<String> {
    let plist = bundle.join("Contents/Info.plist");
    if !plist.is_file() {
        return None;
    }
    // Decode both XML and binary plists with the system reader. It writes JSON to stdout.
    let mut argv = args(&["-convert", "json", "-o", "-", "--"]);
    argv.push(plist.into_os_string());
    let result = probe.run(&host.system("/usr/bin/plutil"), &argv, 20).await;
    let info: Value = serde_json::from_str(result.output()?).ok()?;
    (info["CFBundleIdentifier"] == identity)
        .then(|| {
            info["CFBundleShortVersionString"]
                .as_str()
                .map(str::to_owned)
        })
        .flatten()
}

static T3_VERSION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A[0-9]+\.[0-9]+\.[0-9]+-nightly\.[0-9]{8}\.[0-9]+\z")
        .expect("T3 release pattern")
});
static ZCODE_VERSION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A[0-9]+\.[0-9]+\.[0-9]+\z").expect("ZCode release pattern")
});

fn digits_key(text: &str) -> Vec<u64> {
    text.split(|ch: char| !ch.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn t3_runtime(host: &Host) -> Option<PathBuf> {
    let versions = host.home.join(".t3/runtime/versions");
    let launcher = host.home.join(".local/bin/t3");
    if launcher.is_file()
        && real(&launcher).parent().and_then(Path::parent) == Some(real(&versions).as_path())
    {
        return Some(launcher);
    }
    read::entries(&versions, 4096)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let path = entry.path().join("t3");
            (checked_version(Some(&name), &T3_VERSION, 64).is_some() && path.is_file())
                .then(|| (digits_key(&name), path))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, path)| path)
}

pub(super) async fn t3(host: &Host, probe: &impl Probe) -> Option<Install> {
    if host.platform == Platform::Windows {
        let location = host.local.join("Programs/t3code");
        if !location.join("T3 Code (Nightly).exe").is_file() {
            return None;
        }
        let package = asar_package(&location.join("resources/app.asar"), 2 * 1024 * 1024);
        return Some(Install::new(
            "official-download",
            package
                .filter(|info| info["name"] == "t3code")
                .and_then(|info| checked_version(info["version"].as_str(), &T3_VERSION, 64)),
        ));
    }
    if host.platform == Platform::Mac {
        let bundle = host.system("/Applications/T3 Code (Nightly).app");
        if bundle.is_dir() {
            let release = bundle_version(host, probe, &bundle, "com.t3tools.t3code").await;
            return Some(Install::new(
                "official-download",
                checked_version(release.as_deref(), &T3_VERSION, 64),
            ));
        }
    }
    let path = t3_runtime(host)?;
    let release = probe.version(&path).await;
    Some(Install::new(
        "native",
        checked_version(release.as_deref(), &T3_VERSION, 64),
    ))
}

pub(super) async fn zcode(host: &Host, probe: &impl Probe) -> Option<Install> {
    let location = match host.platform {
        Platform::Mac => {
            let bundle = host.system("/Applications/ZCode.app");
            if !bundle.is_dir() {
                return None;
            }
            let release = bundle_version(host, probe, &bundle, "dev.zcode.app").await;
            return Some(Install::new(
                "official-download",
                checked_version(release.as_deref(), &ZCODE_VERSION, 32),
            ));
        }
        Platform::Windows => {
            let location = host.local.join("Programs/ZCode");
            if !location.join("ZCode.exe").is_file() {
                return None;
            }
            location
        }
        Platform::Ubuntu => {
            let location = host.home.join(".local/opt/zcode/app");
            if !location.is_dir() {
                return None;
            }
            location
        }
    };
    let mut release = asar_package(&location.join("resources/app.asar"), 16 * 1024 * 1024)
        .filter(|package| package["name"] == "@zcode/desktop")
        .and_then(|package| checked_version(package["version"].as_str(), &ZCODE_VERSION, 32));
    if release.is_none() && host.platform == Platform::Ubuntu {
        let stamp = location.parent()?.join(".installed-version");
        if let Ok((file, _)) = read::open(&stamp, &ReadPolicy::capped(u64::MAX)) {
            let mut text = String::new();
            if file.take(64).read_to_string(&mut text).is_ok() {
                release = checked_version(Some(text.trim()), &ZCODE_VERSION, 32);
            }
        }
    }
    Some(Install::new("official-download", release))
}

fn asar_package(path: &Path, limit: usize) -> Option<Value> {
    let (mut stream, meta) = read::open(path, &ReadPolicy::capped(u64::MAX)).ok()?;
    let mut prefix = [0u8; 16];
    stream.read_exact(&mut prefix).ok()?;
    let header_size = u32::from_le_bytes(prefix[4..8].try_into().ok()?) as usize;
    let size = u32::from_le_bytes(prefix[12..16].try_into().ok()?) as usize;
    if !(1..=limit).contains(&size) || !(size + 8..=limit + 16).contains(&header_size) {
        return None;
    }
    let mut header = vec![0; size];
    stream.read_exact(&mut header).ok()?;
    let header: Value = serde_json::from_slice(&header).ok()?;
    let entry = header.get("files")?.get("package.json")?;
    let size = number(entry.get("size")?)?;
    let offset = number(entry.get("offset")?)?;
    if !(1..=65_536).contains(&size) || truthy(&entry["unpacked"]) || truthy(&entry["link"]) {
        return None;
    }
    let start = (8 + header_size as u64).checked_add(offset)?;
    if start.checked_add(size)? > meta.len() {
        return None;
    }
    stream.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = vec![0; size as usize];
    stream.read_exact(&mut bytes).ok()?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(Value::is_object)
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
        Value::Bool(true) => true,
    }
}

static RELEASE_ID: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\A[a-f0-9]{64}\z").expect("ACP release ID pattern"));
static ACP_VERSION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9._+-]{0,63}\z").expect("ACP release pattern")
});

pub(super) fn antigravity_acp(host: &Host) -> Option<Install> {
    let os = match host.platform {
        Platform::Ubuntu => "linux",
        Platform::Mac => "darwin",
        Platform::Windows => "win32",
    };
    let arch = match host.arch {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => return None,
    };
    let root = host
        .home
        .join(".t3/tools/antigravity-acp")
        .join(format!("{os}-{arch}"));
    let active = root.join("active.json");
    if let Err(err) = active.symlink_metadata()
        && err.kind() == std::io::ErrorKind::NotFound
    {
        return None;
    }
    Some(Install::new(
        "official-download",
        completed_acp(host.platform, &root, &active),
    ))
}

fn completed_acp(platform: Platform, root: &Path, active: &Path) -> Option<String> {
    let marker = read_json(active, 8192)?;
    let id = marker["releaseId"]
        .as_str()
        .filter(|id| RELEASE_ID.is_match(id))?;
    let folder = root.join("versions").join(id);
    if folder.is_symlink() {
        return None;
    }
    let record = read_json(&folder.join(".install-complete.json"), 8192)?;
    if record["releaseId"] != id {
        return None;
    }
    let release = checked_version(record["version"].as_str(), &ACP_VERSION, 64)?;
    let windows = platform == Platform::Windows;
    if record.get("harness").is_some_and(Value::is_null) {
        let binary = if windows { "agy.exe" } else { "antigravity" };
        if !artifact(&folder, &record["executable"], binary, windows) {
            return None;
        }
    } else {
        let (server, harness) = if windows {
            ("agy_acp_server.exe", "localharness_external.exe")
        } else {
            ("agy_acp_server.par", "localharness_external")
        };
        if !artifact(&folder, &record["executable"], server, windows)
            || !artifact(&folder, &record["harness"], harness, windows)
        {
            return None;
        }
    }
    Some(release)
}

fn artifact(folder: &Path, value: &Value, binary: &str, windows: bool) -> bool {
    if value["name"] != binary {
        return false;
    }
    let Some(size) = value["bytes"]
        .as_u64()
        .filter(|size| (1..=9_007_199_254_740_991).contains(size))
    else {
        return false;
    };
    let path = folder.join(binary);
    path.symlink_metadata()
        .is_ok_and(|meta| meta.is_file() && meta.len() == size && (windows || executable(&path)))
}
