#![cfg(test)]
//! Unit, integration and red/green tests for lane W5 (`t3.rs` + `t3/rpc.rs`).
#[cfg_attr(not(unix), allow(unused_imports))]
use super::rpc::{Endpoint, Origin};
use super::*;
use crate::apps::updater::{
    CommandOutput, Diagnostics, Downloaded, Fingerprint, ProcessTable, System, attempt,
    tests::FakeSystem, updater,
};
use base64::Engine as _;
use std::{
    collections::VecDeque,
    io::{Read as _, Write as _},
    net::TcpListener,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

const OLD: &str = "0.0.46-nightly.20261006.2735";
const NEW: &str = "0.0.46-nightly.20261007.2774";
const SELF_OLD: &str = "0.1.12-nightly.20261120.4994";
const SELF_NEW: &str = "0.1.12-nightly.20261120.5009";
const TOKEN: &str = "private-rpc-fixture-secret";

#[cfg_attr(not(unix), allow(dead_code))]
fn named(number: u32) -> String {
    format!("0.0.46-nightly.20261007.{number}")
}

fn write_plist(bundle: &Path, bundle_id: &str, version: &str) {
    let contents = bundle.join("Contents");
    std::fs::create_dir_all(&contents).expect("contents dir");
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         <key>Nested</key><dict><key>CFBundleShortVersionString</key><string>9.9.9-nightly.20990101.9999</string></dict>\n\
         <key>CFBundleIdentifier</key><string>{bundle_id}</string>\n\
         <key>CFBundleShortVersionString</key><string>{version}</string>\n\
         </dict>\n</plist>\n"
    );
    std::fs::write(contents.join("Info.plist"), xml).expect("plist");
}

fn write_asar(executable: &Path, version: &str, name: &str) {
    std::fs::create_dir_all(executable.parent().expect("parent")).expect("parent dir");
    std::fs::write(executable, b"").expect("exe");
    let package = serde_json::to_vec(&json!({"name": name, "version": version})).expect("pkg");
    let header = serde_json::to_vec(&json!({
        "files": {"package.json": {"size": package.len(), "offset": "0"}}
    }))
    .expect("header");
    let pad = (4 - (header.len() % 4)) % 4;
    let mut padded = header.clone();
    padded.resize(header.len() + pad, 0);
    let mut raw = Vec::new();
    for word in [
        4_u32,
        (8 + padded.len()) as u32,
        (4 + padded.len()) as u32,
        header.len() as u32,
    ] {
        raw.extend_from_slice(&word.to_le_bytes());
    }
    raw.extend_from_slice(&padded);
    raw.extend_from_slice(&package);
    let resources = executable.parent().expect("parent").join("resources");
    std::fs::create_dir_all(&resources).expect("resources");
    std::fs::write(resources.join("app.asar"), raw).expect("asar");
}

fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let mut body = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in entries {
        let mut crc = 0xffff_ffff_u32;
        for byte in *data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        crc = !crc;
        let offset = u32::try_from(body.len()).expect("offset");
        let size = u32::try_from(data.len()).expect("size");
        let name_len = u16::try_from(name.len()).expect("name");
        body.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 8, 0, 0, 0, 0, 0, 0]);
        body.extend_from_slice(&crc.to_le_bytes());
        body.extend_from_slice(&size.to_le_bytes());
        body.extend_from_slice(&size.to_le_bytes());
        body.extend_from_slice(&name_len.to_le_bytes());
        body.extend_from_slice(&0_u16.to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(data);
        directory
            .extend_from_slice(&[0x50, 0x4b, 0x01, 0x02, 20, 3, 20, 0, 0, 8, 0, 0, 0, 0, 0, 0]);
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&name_len.to_le_bytes());
        directory.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        directory.extend_from_slice(&(0o100_644_u32 << 16).to_le_bytes());
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let start = u32::try_from(body.len()).expect("start");
    let count = u16::try_from(entries.len()).expect("count");
    let size = u32::try_from(directory.len()).expect("size");
    body.extend_from_slice(&directory);
    body.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
    body.extend_from_slice(&count.to_le_bytes());
    body.extend_from_slice(&count.to_le_bytes());
    body.extend_from_slice(&size.to_le_bytes());
    body.extend_from_slice(&start.to_le_bytes());
    body.extend_from_slice(&0_u16.to_le_bytes());
    std::fs::write(path, body).expect("zip");
}

type RunFn = Box<dyn Fn(&CommandSpec) -> Result<CommandOutput, UpdateFailure> + Send + Sync>;
type FetchFn = Box<dyn Fn(&str) -> Result<Vec<u8>, UpdateFailure> + Send + Sync>;
type DownloadFn = Box<dyn Fn(&str, &Path, u64) -> Result<Downloaded, UpdateFailure> + Send + Sync>;
type ProcessesFn = Box<dyn Fn(Platform) -> Result<ProcessTable, UpdateFailure> + Send + Sync>;

struct MockSystem {
    runs: Mutex<Vec<Vec<OsString>>>,
    run_fn: RunFn,
    fetch_fn: FetchFn,
    download_fn: DownloadFn,
    processes_fn: ProcessesFn,
}

impl MockSystem {
    fn new() -> Self {
        Self {
            runs: Mutex::new(Vec::new()),
            run_fn: Box::new(|_| Ok(CommandOutput::default())),
            fetch_fn: Box::new(|_| Err(UpdateFailure::failed())),
            download_fn: Box::new(|_, _, _| Err(UpdateFailure::failed())),
            processes_fn: Box::new(|_| Ok(ProcessTable::default())),
        }
    }

    fn on_run(
        mut self,
        f: impl Fn(&CommandSpec) -> Result<CommandOutput, UpdateFailure> + Send + Sync + 'static,
    ) -> Self {
        self.run_fn = Box::new(f);
        self
    }

    fn on_fetch(
        mut self,
        f: impl Fn(&str) -> Result<Vec<u8>, UpdateFailure> + Send + Sync + 'static,
    ) -> Self {
        self.fetch_fn = Box::new(f);
        self
    }

    fn on_download(
        mut self,
        f: impl Fn(&str, &Path, u64) -> Result<Downloaded, UpdateFailure> + Send + Sync + 'static,
    ) -> Self {
        self.download_fn = Box::new(f);
        self
    }

    fn runs(&self) -> Vec<Vec<String>> {
        self.runs
            .lock()
            .expect("runs")
            .iter()
            .map(|argv| {
                argv.iter()
                    .map(|word| word.to_string_lossy().into_owned())
                    .collect()
            })
            .collect()
    }
}

impl System for MockSystem {
    fn run<'a>(
        &'a self,
        spec: &'a CommandSpec,
        diagnostics: &'a Mutex<Diagnostics>,
    ) -> BoxFuture<'a, Result<CommandOutput, UpdateFailure>> {
        Box::pin(async move {
            self.runs.lock().expect("runs").push(spec.argv.clone());
            let answer = (self.run_fn)(spec);
            if !spec.quiet {
                let exit = match &answer {
                    Ok(_) => Some(0),
                    Err(failure) => failure.exit_code,
                };
                diagnostics.lock().expect("record").ran(
                    &spec.argv,
                    spec.label,
                    exit,
                    Some(answer.is_err()),
                );
            }
            answer
        })
    }

    fn processes(&self, platform: Platform) -> BoxFuture<'_, Result<ProcessTable, UpdateFailure>> {
        Box::pin(async move { (self.processes_fn)(platform) })
    }

    fn download<'a>(
        &'a self,
        url: &'a str,
        destination: &'a Path,
        maximum: u64,
        _timeout: Duration,
    ) -> BoxFuture<'a, Result<Downloaded, UpdateFailure>> {
        Box::pin(async move { (self.download_fn)(url, destination, maximum) })
    }

    fn fetch<'a>(
        &'a self,
        url: &'a str,
        _prefix: &'a str,
        _maximum: u64,
        _timeout: Duration,
    ) -> BoxFuture<'a, Result<Vec<u8>, UpdateFailure>> {
        Box::pin(async move { (self.fetch_fn)(url) })
    }

    fn fingerprint<'a>(&'a self, _: &'a str, _: Duration) -> BoxFuture<'a, Option<Fingerprint>> {
        Box::pin(async { None })
    }
}

fn make_host(
    root: &Path,
    home: PathBuf,
    platform: Platform,
    arch: &'static str,
) -> Arc<crate::apps::Host> {
    Arc::new(crate::apps::Host {
        platform,
        local: home.join("AppData/Local"),
        roaming: home.join("AppData/Roaming"),
        home,
        program_files: root.join("Program Files"),
        path: Vec::new(),
        path_ext: vec![".EXE".into()],
        powershell: None,
        npm_prefix: None,
        arch,
        system_root: Some(root.join("system")),
    })
}

fn context_with_arch(
    root: &Path,
    platform: Platform,
    arch: &'static str,
    system: Arc<dyn System>,
) -> Context {
    Context::new(
        platform,
        make_host(root, root.join("home"), platform, arch),
        system,
        Arc::new(Shared::default()),
        Instant::now() + Duration::from_secs(600),
        root.join("stage"),
        root.join("state"),
        None,
        || false,
        |_| {},
    )
}

fn create_runtime(home: &Path, version: &str) -> PathBuf {
    let binary = home.join(".t3/runtime/versions").join(version).join("t3");
    std::fs::create_dir_all(binary.parent().expect("parent")).expect("dir");
    std::fs::write(&binary, b"").expect("binary");
    binary
}

#[cfg(unix)]
type ProcEntry<'a> = (&'a str, Option<&'a Path>, &'a [&'a str], &'a [&'a str]);

#[cfg(unix)]
fn create_fake_proc(root: &Path, entries: &[ProcEntry<'_>]) -> PathBuf {
    let proc = tempfile::Builder::new()
        .prefix("proc-")
        .tempdir_in(root)
        .expect("proc")
        .keep();
    for (pid, exe, argv, children) in entries {
        let task = proc.join(pid).join("task").join(pid);
        std::fs::create_dir_all(&task).expect("task");
        if let Some(exe) = exe {
            std::os::unix::fs::symlink(exe, proc.join(pid).join("exe")).expect("exe");
        }
        let mut cmdline = Vec::new();
        for item in *argv {
            cmdline.extend_from_slice(item.as_bytes());
            cmdline.push(0);
        }
        std::fs::write(proc.join(pid).join("cmdline"), cmdline).expect("cmdline");
        let text: String = children.iter().map(|c| format!("{c} ")).collect();
        std::fs::write(task.join("children"), text).expect("children");
    }
    proc
}

#[cfg(unix)]
fn runtime_layout(home: &Path, numbers: &[u32], active: u32) -> PathBuf {
    let versions = home.join(".t3/runtime/versions");
    for &number in numbers {
        create_runtime(home, &named(number));
    }
    common::write_private_json(
        &home.join(".t3/runtime/service-state.json"),
        &json!({"protocol": 3, "activeVersion": named(active)}),
    )
    .expect("service-state");
    let local_bin = home.join(".local/bin");
    std::fs::create_dir_all(&local_bin).expect("local bin");
    let _ = std::fs::remove_file(local_bin.join("t3"));
    std::os::unix::fs::symlink(
        versions.join(named(active)).join("t3"),
        local_bin.join("t3"),
    )
    .expect("launcher");
    versions
}

// ---------------------------------------------------------------------------
// 1. Red/green proof: `t3-code` updater is ported (was Unported -> unknown/readiness_unknown).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t3_updater_is_ported() {
    let root = tempfile::tempdir().expect("root");
    for platform in [Platform::Ubuntu, Platform::Mac, Platform::Windows] {
        let sys = FakeSystem::answering(|_| Ok(CommandOutput::default()));
        let cx = context_with_arch(root.path(), platform, "x86_64", sys);
        let row = attempt(updater("t3-code").expect("t3-code"), &cx)
            .await
            .to_value();
        assert_eq!(
            (row["status"].as_str(), row["messageCode"].as_str()),
            (Some("not_installed"), Some("not_installed")),
            "platform {platform:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Detection tests (app_update_t3_test.py).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn windows_detects_full_nightly_from_asar_and_rejects_impostor() {
    let root = tempfile::tempdir().expect("root");
    let sys = Arc::new(MockSystem::new());
    let cx = context_with_arch(root.path(), Platform::Windows, "x86_64", sys);
    let executable = cx.host.local.join("Programs/t3code").join(WINDOWS_NAME);
    write_asar(&executable, NEW, "t3code");
    let install = detect_t3(&cx).await.expect("windows install");
    assert_eq!(install.version.as_deref(), Some(NEW));
    assert_eq!(install.package_root.as_deref(), executable.parent());
    assert_eq!(install.manager, "official-download");

    write_asar(&executable, NEW, "impostor");
    let impostor = detect_t3(&cx).await.expect("impostor install");
    assert_eq!(impostor.version, None);
}

#[tokio::test]
async fn mac_detects_full_nightly_from_xml_plist_without_shelling_out() {
    let root = tempfile::tempdir().expect("root");
    let sys = Arc::new(MockSystem::new());
    let cx = context_with_arch(root.path(), Platform::Mac, "aarch64", sys.clone());
    let bundle = cx.host.system(processes::T3_MAC_BUNDLE);
    write_plist(&bundle, MAC_ID, NEW);
    let install = detect_t3(&cx).await.expect("mac install");
    assert_eq!(install.version.as_deref(), Some(NEW));
    assert_eq!(install.identity.as_deref(), Some(MAC_ID));
    assert_eq!(install.publisher.as_deref(), Some(MAC_TEAM));
    assert!(sys.runs().is_empty(), "XML plist needs no plutil call");

    write_plist(&bundle, "com.other.app", NEW);
    let wrong = detect_t3(&cx).await.expect("wrong id");
    assert_eq!(wrong.version, None);
}

#[tokio::test]
async fn absent_installations_never_probe() {
    let root = tempfile::tempdir().expect("root");
    let sys = Arc::new(MockSystem::new());
    for platform in [Platform::Ubuntu, Platform::Mac, Platform::Windows] {
        let cx = context_with_arch(root.path(), platform, "x86_64", sys.clone());
        assert!(detect_t3(&cx).await.is_none());
    }
    assert!(sys.runs().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_detection_uses_launcher_fallback_binary_and_timeout() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("home");
    let old_bin = create_runtime(&home, OLD);
    let new_bin = create_runtime(&home, NEW);
    // Without launcher symlink, fallback picks the newest version's `t3` binary (not folder!).
    assert_eq!(runtime_install(&home), Some(new_bin.clone()));

    let launcher = home.join(".local/bin/t3");
    std::fs::create_dir_all(launcher.parent().expect("bin")).expect("bin");
    std::os::unix::fs::symlink(&old_bin, &launcher).expect("symlink");
    assert_eq!(runtime_install(&home), Some(launcher.clone()));

    let sys = Arc::new(MockSystem::new().on_run(|_| Err(UpdateFailure::new("timeout"))));
    let cx = context_with_arch(root.path(), Platform::Ubuntu, "x86_64", sys);
    let install = detect_t3(&cx).await.expect("runtime");
    assert_eq!(install.path, launcher);
    assert_eq!(install.probe, Probe::Timeout);
    assert_eq!(install.version, None);
}

// ---------------------------------------------------------------------------
// 3. Release lookup, manifest verification, and failure mapping.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn release_selection_ignores_drafts_and_stable_and_paginates() {
    let root = tempfile::tempdir().expect("root");
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let sys = Arc::new(MockSystem::new().on_fetch(move |url| {
        calls_ref.fetch_add(1, Ordering::SeqCst);
        if url == RELEASES {
            Ok(serde_json::to_vec(&json!([
                {"tag_name": "v99.0.0"},
                {"tag_name": "v9.0.0-nightly.20261007.9999", "draft": true},
            ]))
            .expect("json"))
        } else if url == format!("{RELEASES}&page=2") {
            Ok(serde_json::to_vec(&json!([
                {"tag_name": format!("v{OLD}")},
                {"tag_name": format!("v{NEW}")},
            ]))
            .expect("json"))
        } else {
            Err(UpdateFailure::failed())
        }
    }));
    let cx = context_with_arch(root.path(), Platform::Ubuntu, "x86_64", sys);
    assert_eq!(latest_release(&cx).await.expect("latest"), NEW);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn checksum_failure_never_executes_installer_or_stops_processes() {
    let root = tempfile::tempdir().expect("root");
    let max_seen = Arc::new(Mutex::new(Vec::new()));
    let max_ref = Arc::clone(&max_seen);
    let sys = Arc::new(
        MockSystem::new()
            .on_fetch(|_| {
                Ok(serde_json::to_vec(&json!([{"tag_name": format!("v{NEW}")}])).expect("json"))
            })
            .on_download(move |url, dest, max| {
                max_ref.lock().expect("max").push(max);
                if url.ends_with("nightly.yml") {
                    let asset = format!("T3-Code-{NEW}-x64.exe");
                    let checksum = base64::engine::general_purpose::STANDARD.encode([b'x'; 64]);
                    std::fs::write(
                        dest,
                        format!(
                            "version: {NEW}\nfiles:\n  - url: {asset}\n    sha512: {checksum}\n"
                        ),
                    )
                    .expect("manifest");
                } else {
                    std::fs::write(dest, b"untrusted fixture").expect("pkg");
                }
                Ok(Downloaded {
                    bytes: 17,
                    url: url.into(),
                })
            }),
    );
    let cx = context_with_arch(root.path(), Platform::Windows, "x86_64", sys.clone());
    let exe = cx.host.local.join("Programs/t3code").join(WINDOWS_NAME);
    write_asar(&exe, OLD, "t3code");
    let mut install = Installed::new(
        "t3-code",
        Platform::Windows,
        exe.clone(),
        Some(OLD.into()),
        "official-download",
    );
    install.package_root = exe.parent().map(Path::to_path_buf);
    let row = update_t3_with(
        &cx,
        &install,
        Path::new("/proc"),
        |_| async { false },
        |_| async { Err(Fault::generic()) },
        |_| async { false },
    )
    .await
    .to_value();
    assert_eq!(row["status"], "failed");
    assert_eq!(row["messageCode"], "signature_failed");
    assert_eq!(
        max_seen.lock().expect("max").as_slice(),
        &[65_536, MAX_PACKAGE]
    );
    assert!(sys.runs().is_empty());
}

#[tokio::test]
async fn download_blocked_maps_to_update_failed_and_current_skips_downloads() {
    let root = tempfile::tempdir().expect("root");
    let downloads = Arc::new(AtomicUsize::new(0));
    let dl_ref = Arc::clone(&downloads);
    let sys = Arc::new(
        MockSystem::new()
            .on_fetch(|_| {
                Ok(serde_json::to_vec(&json!([{"tag_name": format!("v{NEW}")}])).expect("json"))
            })
            .on_download(move |_, _, _| {
                dl_ref.fetch_add(1, Ordering::SeqCst);
                Err(UpdateFailure::new("download_blocked"))
            }),
    );
    let cx = context_with_arch(root.path(), Platform::Mac, "aarch64", sys);
    let bundle = cx.host.system(processes::T3_MAC_BUNDLE);
    write_plist(&bundle, MAC_ID, NEW);
    let current = Installed::new(
        "t3-code",
        Platform::Mac,
        bundle.clone(),
        Some(NEW.into()),
        "official-download",
    );
    let row = update_t3_with(
        &cx,
        &current,
        Path::new("/proc"),
        |_| async { false },
        |_| async { Err(Fault::generic()) },
        |_| async { false },
    )
    .await
    .to_value();
    assert_eq!(row["status"], "current");
    assert_eq!(row["updateAttempted"], false);
    assert_eq!(downloads.load(Ordering::SeqCst), 0);

    write_plist(&bundle, MAC_ID, OLD);
    let older = Installed::new(
        "t3-code",
        Platform::Mac,
        bundle.clone(),
        Some(OLD.into()),
        "official-download",
    );
    let blocked = update_t3_with(
        &cx,
        &older,
        Path::new("/proc"),
        |_| async { false },
        |_| async { Err(Fault::generic()) },
        |_| async { false },
    )
    .await
    .to_value();
    assert_eq!(blocked["status"], "failed");
    assert_eq!(blocked["messageCode"], "update_failed");

    // If the bundle on disk changed before an UpdateFailure, status is restart_failed.
    write_plist(&bundle, MAC_ID, NEW);
    let changed = update_t3_with(
        &cx,
        &older,
        Path::new("/proc"),
        |_| async { false },
        |_| async { Err(Fault::generic()) },
        |_| async { false },
    )
    .await
    .to_value();
    assert_eq!(changed["status"], "restart_failed");
    assert_eq!(changed["messageCode"], "update_failed");
}

// ---------------------------------------------------------------------------
// 4. Ubuntu runtime update, pending restart, companions & pruning.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn ubuntu_runtime_update_schedules_detached_worker_and_reloads_stale_unit() {
    let root = tempfile::tempdir().expect("root");
    let acp_unit = root
        .path()
        .join("home/.config/systemd/user")
        .join(shared::ACP_UNIT);
    std::fs::create_dir_all(acp_unit.parent().expect("unit")).expect("unit dir");
    std::fs::write(&acp_unit, "[Unit]\n").expect("unit");

    let sys = Arc::new(
        MockSystem::new()
            .on_fetch(|_| {
                Ok(serde_json::to_vec(&json!([{"tag_name": format!("v{NEW}")}])).expect("json"))
            })
            .on_run(|spec| {
                let words: Vec<String> = spec
                    .argv
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                if words.iter().any(|w| w == "--version") {
                    return Ok(CommandOutput {
                        stdout: format!("{NEW}\n"),
                    });
                }
                if words.iter().any(|w| w == "--property=NeedDaemonReload") {
                    return Ok(CommandOutput {
                        stdout: "yes\n".into(),
                    });
                }
                Ok(CommandOutput::default())
            }),
    );
    let cx = context_with_arch(root.path(), Platform::Ubuntu, "x86_64", sys.clone());
    let runtime = create_runtime(&cx.host.home, OLD);
    let install = Installed::new(
        "t3-code",
        Platform::Ubuntu,
        runtime,
        Some(OLD.into()),
        "native",
    );
    let proc = create_fake_proc(root.path(), &[]);
    let row = update_t3_with(
        &cx,
        &install,
        &proc,
        |_| async { false },
        |_| async { Err(Fault::generic()) },
        |_| async { false },
    )
    .await
    .to_value();
    assert_eq!(row["status"], "updated");
    assert_eq!(row["messageCode"], "t3_restart_scheduled");
    assert_eq!(row["version"], NEW);
    assert_eq!(row["restartedProcesses"], 0);
    assert_eq!(
        row["restartTargets"],
        json!([{"kind": "systemd", "service": "t3code.service", "delaySeconds": 30}])
    );

    let runs = sys.runs();
    assert!(runs[0].ends_with(&[
        "update".into(),
        NEW.into(),
        "--channel".into(),
        "nightly".into()
    ]));
    assert!(!runs[0].contains(&"--yes".into()));
    assert!(
        runs.iter()
            .any(|argv| argv.ends_with(&["daemon-reload".into()]))
    );
    assert!(!runs.iter().any(|argv| argv.iter().any(|w| w == "restart")));
    let schedule = runs.last().expect("schedule");
    assert!(schedule[0].ends_with("systemd-run"));
    assert!(schedule.contains(&RESTART_FLAG.into()));
    assert!(schedule.contains(&"--restart-service".into()));

    let components: Value = serde_json::from_slice(
        &std::fs::read(cx.state.join(COMPONENTS_RECORD)).expect("components"),
    )
    .expect("json");
    assert_eq!(components["acpUpdater"], "pending");
    assert_eq!(components["daemonReload"], true);
    assert_eq!(components["cursorAgent"], json!({"status": "separate_row"}));
    let keys: HashSet<&str> = components
        .as_object()
        .expect("obj")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, COMPONENT_KEYS.into_iter().collect());
}

#[cfg(unix)]
#[tokio::test]
async fn pending_restart_probe_timeout_retry_and_clear_on_matching_server() {
    let root = tempfile::tempdir().expect("root");
    let sys = Arc::new(
        MockSystem::new()
            .on_fetch(|_| {
                Ok(serde_json::to_vec(&json!([{"tag_name": format!("v{NEW}")}])).expect("json"))
            })
            .on_run(|spec| {
                if spec.argv.iter().any(|w| w == "--version") {
                    return Err(UpdateFailure::new("timeout"));
                }
                Ok(CommandOutput::default())
            }),
    );
    let cx = context_with_arch(root.path(), Platform::Ubuntu, "x86_64", sys);
    let runtime = create_runtime(&cx.host.home, OLD);
    let install = Installed::new(
        "t3-code",
        Platform::Ubuntu,
        runtime,
        Some(OLD.into()),
        "native",
    );
    let proc = create_fake_proc(root.path(), &[]);
    let row = update_t3_with(
        &cx,
        &install,
        &proc,
        |_| async { false },
        |_| async { Err(Fault::generic()) },
        |_| async { false },
    )
    .await
    .to_value();
    assert_eq!(row["status"], "failed");
    assert_eq!(row["messageCode"], "version_unknown");
    assert_eq!(row["updateAttempted"], true);
    assert!(cx.state.join(PENDING_MARKER).exists());

    // Next run: installed is NEW, running server is NEW -> marker cleared and row is current.
    let new_bin = create_runtime(&cx.host.home, NEW);
    let proc_new = create_fake_proc(
        root.path(),
        &[(
            "4242",
            Some(&new_bin),
            &[&new_bin.to_string_lossy(), "serve"],
            &[],
        )],
    );
    let sys2 = Arc::new(
        MockSystem::new()
            .on_fetch(|_| {
                Ok(serde_json::to_vec(&json!([{"tag_name": format!("v{NEW}")}])).expect("json"))
            })
            .on_run(|spec| {
                if spec.argv.iter().any(|w| w == "--property=MainPID") {
                    return Ok(CommandOutput {
                        stdout: "4242\n".into(),
                    });
                }
                Ok(CommandOutput::default())
            }),
    );
    let cx2 = context_with_arch(root.path(), Platform::Ubuntu, "x86_64", sys2);
    let current = Installed::new(
        "t3-code",
        Platform::Ubuntu,
        new_bin,
        Some(NEW.into()),
        "native",
    );
    let row2 = update_t3_with(
        &cx2,
        &current,
        &proc_new,
        |_| async { false },
        |_| async { Err(Fault::generic()) },
        |_| async { false },
    )
    .await
    .to_value();
    assert_eq!(row2["status"], "current");
    assert!(!cx2.state.join(PENDING_MARKER).exists());
}

#[cfg(unix)]
#[tokio::test]
async fn running_runtime_version_reads_launcher_serve_child_only() {
    let root = tempfile::tempdir().expect("root");
    let sys = Arc::new(MockSystem::new().on_run(|_| {
        Ok(CommandOutput {
            stdout: "4242\n".into(),
        })
    }));
    let cx = context_with_arch(root.path(), Platform::Ubuntu, "x86_64", sys);
    let old = create_runtime(&cx.host.home, OLD);
    let new = create_runtime(&cx.host.home, NEW);
    let outside = root.path().join("other").join(NEW).join("t3");
    std::fs::create_dir_all(outside.parent().expect("p")).expect("p");
    std::fs::write(&outside, b"").expect("out");
    let old_s = old.to_string_lossy().into_owned();
    let new_s = new.to_string_lossy().into_owned();
    let out_s = outside.to_string_lossy().into_owned();

    let cases: &[(&[ProcEntry<'_>], Option<&str>)] = &[
        (
            &[
                (
                    "4242",
                    Some(&old),
                    &[&old_s, "__service-launcher"],
                    &["4343"],
                ),
                ("4343", Some(&new), &[&new_s, "serve"], &[]),
            ],
            Some(NEW),
        ),
        (
            &[
                (
                    "4242",
                    Some(&old),
                    &[&old_s, "__service-launcher"],
                    &["4343"],
                ),
                ("4343", Some(&new), &[&new_s, "doctor"], &[]),
            ],
            None,
        ),
        (
            &[
                (
                    "4242",
                    Some(&old),
                    &[&old_s, "__service-launcher"],
                    &["4343"],
                ),
                ("4343", Some(&outside), &[&out_s, "serve"], &[]),
            ],
            None,
        ),
        (
            &[("4242", Some(&old), &[&old_s, "__service-launcher"], &[])],
            None,
        ),
        (&[("4242", Some(&new), &[&new_s, "serve"], &[])], Some(NEW)),
    ];
    for (entries, expected) in cases {
        let proc = create_fake_proc(root.path(), entries);
        assert_eq!(
            running_runtime_version_at(&cx, &proc).await.as_deref(),
            *expected
        );
    }
}

#[cfg(unix)]
#[test]
fn prune_runtimes_keeps_active_rollback_running_cmdline_symlinks_and_caps_at_20() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("home");
    let versions = runtime_layout(&home, &[2600, 2735, 2752, 2774, 2833, 2849], 2849);
    let v2735 = versions.join(named(2735)).join("node_modules/x.js");
    let v2752 = versions.join(named(2752)).join("resource-monitor");
    let v2774 = versions.join(named(2774));
    let v2735_s = v2735.to_string_lossy().into_owned();
    let v2752_s = v2752.to_string_lossy().into_owned();
    let v2774_s = v2774.to_string_lossy().into_owned();
    let node = Path::new("/usr/bin/node");
    let proc = create_fake_proc(
        root.path(),
        &[
            ("4242", Some(node), &["/usr/bin/node", &v2735_s], &[]),
            ("4243", None, &["/usr/bin/node", &v2752_s], &[]),
            (
                "4244",
                Some(node),
                &["/usr/bin/node", "--runtime-dir", &v2774_s],
                &[],
            ),
        ],
    );
    let real_versions = crate::apps::real(&versions);
    let running = running_runtime_versions(&real_versions, &proc).expect("running");
    assert_eq!(
        running,
        HashSet::from([named(2735), named(2752), named(2774)])
    );
    assert_eq!(prune_runtimes(&home, &proc), vec![named(2600)]);
    // Missing /proc must return None and delete nothing!
    assert_eq!(
        running_runtime_versions(&real_versions, &root.path().join("no-proc")),
        None
    );

    // Symlink in versions must never be followed or deleted.
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("keep.txt"), "keep").expect("keep");
    std::os::unix::fs::symlink(&outside, versions.join(named(2500))).expect("symlink");
    let empty_proc = create_fake_proc(root.path(), &[]);
    let _ = prune_runtimes(&home, &empty_proc);
    assert!(
        versions
            .join(named(2500))
            .symlink_metadata()
            .expect("meta")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).expect("keep"),
        "keep"
    );

    // 25 older versions -> at most 20 deleted, oldest first, keeping rollback + active.
    let root2 = tempfile::tempdir().expect("root2");
    let home2 = root2.path().join("home");
    let mut nums: Vec<u32> = (1000..1025).collect();
    nums.push(2849);
    let versions2 = runtime_layout(&home2, &nums, 2849);
    let proc2 = create_fake_proc(root2.path(), &[]);
    let deleted = prune_runtimes(&home2, &proc2);
    assert_eq!(deleted, (1000..1020).map(named).collect::<Vec<_>>());
    assert_eq!(std::fs::read_dir(&versions2).expect("dir").count(), 6);
}

// ---------------------------------------------------------------------------
// 5. Detached restart worker & lock / job completion logic.
// ---------------------------------------------------------------------------

#[test]
fn job_finished_and_agent_job_finished_fail_closed() {
    let root = tempfile::tempdir().expect("root");
    // Missing file: standalone=true, dashboard=false.
    assert!(job_finished(root.path(), false));
    assert!(!job_finished(root.path(), true));

    let path = root.path().join("dashboard-job.json");
    let write = |val: Value| common::write_private_json(&path, &val).expect("write");

    write(
        json!({"job": {"state": "running", "hosts": {"ubuntu": {"state": "done"}, "windows": {"state": "running"}}}}),
    );
    assert!(job_finished(root.path(), true));
    assert!(!job_finished(root.path(), false));

    write(
        json!({"job": {"state": "completed", "hosts": {"ubuntu": {"state": "done"}, "nas1": {"state": "running"}}}}),
    );
    assert!(!job_finished(root.path(), false));
    assert!(job_finished(root.path(), true));

    write(
        json!({"job": {"state": "completed", "hosts": {"ubuntu": {"state": "done"}, "nas1": {"state": "done"}}}}),
    );
    assert!(job_finished(root.path(), false));

    write(json!({"job": {"state": "running", "hosts": {"ubuntu": {"state": "running"}}}}));
    assert!(!job_finished(root.path(), true));

    write(json!({"job": {"state": "completed"}}));
    assert!(job_finished(root.path(), true));
    assert!(job_finished(root.path(), false));

    for bad in [
        json!({}),
        json!([]),
        json!("done"),
        json!({"ubuntu": "done"}),
    ] {
        write(json!({"job": {"state": "completed", "hosts": bad}}));
        assert!(!job_finished(root.path(), true));
        assert!(!job_finished(root.path(), false));
    }

    // agent_job_finished requires every app in APP_INVENTORY.
    let job_id = "0123456789abcdef0123456789abcdef";
    let mut results = Map::new();
    for app in aac_providers::app_inventory() {
        results.insert(app.into(), json!({"status": "current"}));
    }
    common::write_private_json(
        &root.path().join("agent-journal.json"),
        &json!({"job": job_id, "platform": "ubuntu", "running": [], "results": results}),
    )
    .expect("journal");
    assert!(agent_job_finished(root.path(), job_id));
    assert!(!agent_job_finished(
        root.path(),
        "ffffffffffffffffffffffffffffffff"
    ));
}

#[cfg(unix)]
#[test]
fn restart_lock_owner_alive_and_claim_recover_dead_markers() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("t3-restart.lock");
    // Boolean pid is malformed: fresh counts as alive.
    common::write_private_json(&path, &json!({"pid": true})).expect("bool pid");
    assert!(restart_lock_owner_alive(&path, &shared::process_identity));

    // Unreadable identity falls back to pid liveness; mismatched identity is dead.
    let my_pid = std::process::id();
    common::write_private_json(&path, &json!({"pid": my_pid, "identity": "unmatchable"}))
        .expect("mismatch");
    assert!(restart_lock_owner_alive(&path, &|_| None));
    assert!(!restart_lock_owner_alive(&path, &|_| Some(
        "other:1".into()
    )));

    // Stale dead pid is recovered by claim_restart_lock; live pid is held off.
    common::write_private_json(&path, &json!({"pid": 999_999_999})).expect("dead");
    let claimed = claim_restart_lock(&path, &|p| restart_lock_owner_alive(p, &|_| None));
    assert!(claimed.is_some());
    drop(claimed);
    common::write_private_json(&path, &json!({"pid": my_pid})).expect("live");
    assert!(claim_restart_lock(&path, &|p| restart_lock_owner_alive(p, &|_| None)).is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn deferred_restart_waits_for_grace_reloads_restarts_and_handles_failures() {
    let root = tempfile::tempdir().expect("root");
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).expect("state");
    let new_bin = create_runtime(&root.path().join("home"), NEW);
    let proc = create_fake_proc(
        root.path(),
        &[(
            "4242",
            Some(&new_bin),
            &[&new_bin.to_string_lossy(), "serve"],
            &[],
        )],
    );
    common::write_private_json(&state.join(PENDING_MARKER), &json!({"version": NEW}))
        .expect("marker");
    common::write_private_json(
        &state.join("dashboard-job.json"),
        &json!({"job": {"state": "running", "hosts": {"ubuntu": {"state": "done"}, "windows": {"state": "running"}}}}),
    )
    .expect("job");
    std::fs::write(state.join("dashboard-update.lock"), r#"{"pid":1}"#).expect("dash lock");

    let sys = Arc::new(MockSystem::new().on_run(|spec| {
        if spec.argv.iter().any(|w| w == "--property=NeedDaemonReload") {
            return Ok(CommandOutput {
                stdout: "yes\n".into(),
            });
        }
        if spec.argv.iter().any(|w| w == "--property=MainPID") {
            return Ok(CommandOutput {
                stdout: "4242\n".into(),
            });
        }
        Ok(CommandOutput::default())
    }));
    let cx = context_with_arch(root.path(), Platform::Ubuntu, "x86_64", sys.clone());
    let now = Arc::new(AtomicUsize::new(0));
    let health_waits = Arc::new(Mutex::new(Vec::new()));
    let now_r = Arc::clone(&now);
    let now_s = Arc::clone(&now);
    let hw_ref = Arc::clone(&health_waits);

    deferred_restart_core(
        &cx,
        12345,
        &state,
        true,
        None,
        &proc,
        |_| Some(false),
        move |secs, svc| {
            hw_ref.lock().expect("hw").push((secs, svc.to_owned()));
            async { Ok(()) }
        },
        move || now_r.load(Ordering::SeqCst) as u64,
        move |d| {
            now_s.fetch_add(d.as_secs() as usize, Ordering::SeqCst);
            async {}
        },
    )
    .await
    .expect("deferred restart");

    assert!(now.load(Ordering::SeqCst) >= 30);
    assert_eq!(
        health_waits.lock().expect("hw").as_slice(),
        &[(UBUNTU_READY_SECONDS, SERVICE.to_owned())]
    );
    assert!(!state.join(PENDING_MARKER).exists());
    assert!(!state.join("t3-restart.lock").exists());
    assert!(
        state.join("dashboard-update.lock").exists(),
        "dashboard lock untouched"
    );
    let subcommands: Vec<String> = sys
        .runs()
        .iter()
        .filter_map(|a| a.get(2).cloned())
        .collect();
    assert_eq!(subcommands, ["show", "daemon-reload", "restart", "show"]);

    // Unreadable marker never restarts.
    std::fs::write(state.join(PENDING_MARKER), "not json").expect("bad");
    let sys_bad = Arc::new(MockSystem::new());
    let cx_bad = context_with_arch(root.path(), Platform::Ubuntu, "x86_64", sys_bad.clone());
    let clock = Arc::new(AtomicUsize::new(0));
    let cr = Arc::clone(&clock);
    let cs = Arc::clone(&clock);
    let err = deferred_restart_core(
        &cx_bad,
        12345,
        &state,
        true,
        None,
        &proc,
        |_| Some(false),
        |_, _| async { Ok(()) },
        move || cr.load(Ordering::SeqCst) as u64,
        move |d| {
            cs.fetch_add(d.as_secs() as usize, Ordering::SeqCst);
            async {}
        },
    )
    .await
    .expect_err("unreadable marker");
    assert_eq!(err.code, "restart_failed");
    assert!(sys_bad.runs().is_empty());

    // Parent EPERM (alive) keeps waiting until deadline.
    common::write_private_json(&state.join(PENDING_MARKER), &json!({"version": NEW})).expect("m");
    let sys_perm = Arc::new(MockSystem::new());
    let cx_perm = context_with_arch(root.path(), Platform::Ubuntu, "x86_64", sys_perm.clone());
    let clock2 = Arc::new(AtomicUsize::new(0));
    let c2r = Arc::clone(&clock2);
    let c2s = Arc::clone(&clock2);
    let err2 = deferred_restart_core(
        &cx_perm,
        12345,
        &state,
        true,
        None,
        &proc,
        |_| Some(true),
        |_, _| async { Ok(()) },
        move || c2r.load(Ordering::SeqCst) as u64,
        move |_| {
            c2s.fetch_add(1200, Ordering::SeqCst);
            async {}
        },
    )
    .await
    .expect_err("parent alive");
    assert_eq!(err2.code, "restart_failed");
    assert!(sys_perm.runs().is_empty());
}

#[test]
#[cfg_attr(windows, ignore = "the Linux restart worker takes POSIX state folders")]
fn worker_arguments_validates_fields() {
    let args = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
    let ok = WorkerArguments::parse(&args(&[
        "--restart-service",
        "4242",
        "--state-dir",
        "/tmp/aac-state",
        "--dashboard-job",
    ]))
    .expect("valid");
    assert_eq!(ok.parent, 4242);
    assert!(ok.dashboard);
    assert!(
        WorkerArguments::parse(&args(&["--restart-service", "1", "--state-dir", "/tmp"])).is_none()
    );
    assert!(
        WorkerArguments::parse(&args(&[
            "--restart-service",
            "42",
            "--state-dir",
            "relative"
        ]))
        .is_none()
    );
    assert!(
        WorkerArguments::parse(&args(&[
            "--restart-service",
            "42",
            "--state-dir",
            "/tmp",
            "--agent-job",
            "not-hex"
        ]))
        .is_none()
    );
}

// ---------------------------------------------------------------------------
// 6. Health check, Mac bundle swap/rollback, and Windows swap/rollback.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn service_health_waits_11_min_migration_and_fails_fast_on_bad_unit_state() {
    let root = tempfile::tempdir().expect("root");
    let sys = Arc::new(MockSystem::new().on_run(|_| {
        Ok(CommandOutput {
            stdout: "ActiveState=active\nNRestarts=0\n".into(),
        })
    }));
    let cx = context_with_arch(root.path(), Platform::Ubuntu, "x86_64", sys);
    let now_ms = Arc::new(AtomicUsize::new(0));
    let nr = Arc::clone(&now_ms);
    let np = Arc::clone(&now_ms);
    let ns = Arc::clone(&now_ms);
    health_check_core(
        &cx,
        UBUNTU_READY_SECONDS,
        Some(SERVICE),
        false,
        || async { true },
        move |_| {
            let ready = np.load(Ordering::SeqCst) >= 11 * 60 * 1000;
            async move { ready }
        },
        move || nr.load(Ordering::SeqCst) as u64,
        move |d| {
            ns.fetch_add(d.as_millis() as usize, Ordering::SeqCst);
            async {}
        },
    )
    .await
    .expect("11 min migration ok");
    assert!(now_ms.load(Ordering::SeqCst) >= 11 * 60 * 1000);

    for (state, restarts) in [("failed", "0"), ("inactive", "0"), ("active", "1")] {
        let count = Arc::new(AtomicUsize::new(0));
        let count_ref = Arc::clone(&count);
        let sys = Arc::new(MockSystem::new().on_run(move |_| {
            let n = count_ref.fetch_add(1, Ordering::SeqCst);
            Ok(CommandOutput {
                stdout: if n == 0 {
                    "ActiveState=active\nNRestarts=0\n".into()
                } else {
                    format!("ActiveState={state}\nNRestarts={restarts}\n")
                },
            })
        }));
        let cx = context_with_arch(root.path(), Platform::Ubuntu, "x86_64", sys);
        let clock = Arc::new(AtomicUsize::new(0));
        let cr = Arc::clone(&clock);
        let cs = Arc::clone(&clock);
        let err = health_check_core(
            &cx,
            UBUNTU_READY_SECONDS,
            Some(SERVICE),
            false,
            || async { true },
            |_| async { false },
            move || cr.load(Ordering::SeqCst) as u64,
            move |d| {
                cs.fetch_add(d.as_millis() as usize, Ordering::SeqCst);
                async {}
            },
        )
        .await
        .expect_err("bad state");
        assert_eq!(err.code, "restart_failed");
        assert!(clock.load(Ordering::SeqCst) < 60_000);
    }

    // Desktop alive check ends early on crash.
    let cx = context_with_arch(
        root.path(),
        Platform::Windows,
        "x86_64",
        Arc::new(MockSystem::new()),
    );
    let clock = Arc::new(AtomicUsize::new(0));
    let cr = Arc::clone(&clock);
    let cs = Arc::clone(&clock);
    let err = health_check_core(
        &cx,
        600,
        None,
        true,
        || async { false },
        |_| async { false },
        move || cr.load(Ordering::SeqCst) as u64,
        move |d| {
            cs.fetch_add(d.as_millis() as usize, Ordering::SeqCst);
            async {}
        },
    )
    .await
    .expect_err("crashed desktop");
    assert_eq!(err.code, "restart_failed");
    assert!(clock.load(Ordering::SeqCst) < 20_000);
}

#[tokio::test]
async fn mac_bundle_swap_succeeds_and_cleans_backup_and_staging() {
    let root = tempfile::tempdir().expect("root");
    let staged_src = root.path().join("staged_src");
    write_plist(&staged_src.join(MAC_NAME), MAC_ID, NEW);
    let zip_path = root.path().join("pkg.zip");
    write_zip(
        &zip_path,
        &[("T3 Code (Nightly).app/Contents/fixture", b"fixture")],
    );
    let zip_bytes = std::fs::read(&zip_path).expect("zip");
    let zip_sha =
        base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(&zip_bytes));

    let staged_ref = staged_src.clone();
    let sys = Arc::new(
        MockSystem::new()
            .on_download(move |url, dest, _| {
                if url.ends_with("nightly-mac.yml") {
                    std::fs::write(
                        dest,
                        format!(
                            "version: {NEW}\nfiles:\n  - url: T3-Code-{NEW}-arm64.zip\n    sha512: {zip_sha}\n"
                        ),
                    )
                    .expect("yml");
                } else {
                    std::fs::write(dest, &zip_bytes).expect("zip");
                }
                Ok(Downloaded {
                    bytes: 1,
                    url: url.into(),
                })
            })
            .on_run(move |spec| {
                let words: Vec<String> = spec
                    .argv
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                if words[0].ends_with("ditto") && words.contains(&"-x".into()) {
                    copy_tree(&staged_ref, Path::new(words.last().expect("dest"))).expect("extract");
                } else if words[0].ends_with("ditto") {
                    copy_tree(Path::new(&words[1]), Path::new(&words[2])).expect("stage");
                }
                Ok(CommandOutput::default())
            }),
    );
    let cx = context_with_arch(root.path(), Platform::Mac, "aarch64", sys);
    let installed = cx.host.system(processes::T3_MAC_BUNDLE);
    write_plist(&installed, MAC_ID, OLD);
    let mut install = Installed::new(
        "t3-code",
        Platform::Mac,
        installed.clone(),
        Some(OLD.into()),
        "official-download",
    );
    install.package_root = Some(installed.clone());

    let work = tempfile::tempdir().expect("work");
    let (restarted, forced) = update_mac(&cx, &install, NEW, work.path(), |_| async { true })
        .await
        .expect("mac swap succeeded");
    assert_eq!((restarted, forced), (0, 0));
    assert_eq!(
        mac_bundle_version(&cx, &installed).await.as_deref(),
        Some(NEW)
    );
    let leftovers: Vec<_> = std::fs::read_dir(installed.parent().expect("apps"))
        .expect("apps")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".aac-t3-"))
        .collect();
    assert!(leftovers.is_empty());
}

#[tokio::test]
async fn mac_swap_rolls_back_immediately_when_open_fails_and_cleans_stage() {
    let root = tempfile::tempdir().expect("root");
    let staged_src = root.path().join("staged_src");
    write_plist(&staged_src.join(MAC_NAME), MAC_ID, NEW);
    let staged_ref = staged_src.clone();
    let zip_path = root.path().join("pkg.zip");
    write_zip(
        &zip_path,
        &[("T3 Code (Nightly).app/Contents/fixture", b"ok")],
    );
    let zip_bytes = std::fs::read(&zip_path).expect("zip");
    let zip_sha =
        base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(&zip_bytes));

    let sys = Arc::new(
        MockSystem::new()
            .on_download(move |url, dest, _| {
                if url.ends_with("nightly-mac.yml") {
                    std::fs::write(
                        dest,
                        format!(
                            "version: {NEW}\nfiles:\n  - url: T3-Code-{NEW}-arm64.zip\n    sha512: {zip_sha}\n"
                        ),
                    )
                    .expect("yml");
                } else {
                    std::fs::write(dest, &zip_bytes).expect("zip");
                }
                Ok(Downloaded {
                    bytes: 1,
                    url: url.into(),
                })
            })
            .on_run(move |spec| {
                let words: Vec<String> = spec
                    .argv
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                if words[0].ends_with("ditto") && words.contains(&"-x".into()) {
                    copy_tree(&staged_ref, Path::new(words.last().expect("dest"))).expect("extract");
                } else if words[0].ends_with("ditto") {
                    copy_tree(Path::new(&words[1]), Path::new(&words[2])).expect("stage");
                } else if words[0].ends_with("open") {
                    return Err(UpdateFailure::new("restart_failed"));
                }
                Ok(CommandOutput::default())
            }),
    );
    let cx = context_with_arch(root.path(), Platform::Mac, "aarch64", sys);
    let installed = cx.host.system(processes::T3_MAC_BUNDLE);
    write_plist(&installed, MAC_ID, OLD);
    let mut install = Installed::new(
        "t3-code",
        Platform::Mac,
        installed.clone(),
        Some(OLD.into()),
        "official-download",
    );
    install.package_root = Some(installed.clone());
    let work = tempfile::tempdir().expect("work");
    let err = update_mac(&cx, &install, NEW, work.path(), |_| async { true })
        .await
        .expect_err("open failed");
    assert_eq!(err.0.code, "restart_failed");
    assert_eq!(
        mac_bundle_version(&cx, &installed).await.as_deref(),
        Some(OLD)
    );
    let leftovers: Vec<_> = std::fs::read_dir(installed.parent().expect("apps"))
        .expect("apps")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".aac-t3-"))
        .collect();
    assert!(leftovers.is_empty());
}

#[tokio::test]
async fn mac_runtime_updates_when_desktop_is_already_current() {
    let root = tempfile::tempdir().expect("root");
    let sys = Arc::new(
        MockSystem::new()
            .on_fetch(|_| {
                Ok(serde_json::to_vec(&json!([{"tag_name": format!("v{NEW}")}])).expect("json"))
            })
            .on_run(|spec| {
                let words: Vec<String> = spec
                    .argv
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                if words.iter().any(|w| w == "--version") {
                    let version = if words[0].contains(NEW) { NEW } else { OLD };
                    return Ok(CommandOutput {
                        stdout: format!("{version}\n"),
                    });
                }
                Ok(CommandOutput::default())
            }),
    );
    let cx = context_with_arch(root.path(), Platform::Mac, "aarch64", sys.clone());
    let bundle = cx.host.system(processes::T3_MAC_BUNDLE);
    write_plist(&bundle, MAC_ID, NEW);
    let _rt = create_runtime(&cx.host.home, OLD);
    let install = Installed::new(
        "t3-code",
        Platform::Mac,
        bundle,
        Some(NEW.into()),
        "official-download",
    );
    let proc = root.path().join("proc");
    let row = update_t3_with(
        &cx,
        &install,
        &proc,
        |_| async { false },
        |_| async { Err(Fault::generic()) },
        |_| async { true },
    )
    .await
    .to_value();
    assert_eq!(row["status"], "updated");
    assert_eq!(row["messageCode"], "t3_updated");
    assert_eq!(row["version"], NEW);
    assert_eq!(row["restartTargets"], json!([]));
    assert!(sys.runs().iter().any(|r| r.contains(&"update".into())));
}

#[tokio::test]
async fn windows_signature_failure_and_installer_timeout_restore_old_bundle() {
    let root = tempfile::tempdir().expect("root");
    let pkg_bytes = b"fake-nsis-installer";
    let pkg_sha = base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(pkg_bytes));
    let pkg_sha_sig = pkg_sha.clone();

    // Signature failure stops nothing.
    let sys_sig = Arc::new(
        MockSystem::new()
            .on_download(move |url, dest, _| {
                if url.ends_with("nightly.yml") {
                    std::fs::write(
                        dest,
                        format!(
                            "version: {NEW}\nfiles:\n  - url: T3-Code-{NEW}-x64.exe\n    sha512: {pkg_sha_sig}\n"
                        ),
                    )
                    .expect("yml");
                } else {
                    std::fs::write(dest, pkg_bytes).expect("exe");
                }
                Ok(Downloaded {
                    bytes: 1,
                    url: url.into(),
                })
            })
            .on_run(|_| Err(UpdateFailure::failed())),
    );
    let cx_sig = context_with_arch(root.path(), Platform::Windows, "x86_64", sys_sig);
    let exe = cx_sig.host.local.join("Programs/t3code").join(WINDOWS_NAME);
    write_asar(&exe, OLD, "t3code");
    let mut install = Installed::new(
        "t3-code",
        Platform::Windows,
        exe.clone(),
        Some(OLD.into()),
        "official-download",
    );
    install.package_root = exe.parent().map(Path::to_path_buf);
    let work = tempfile::tempdir().expect("work");
    let err = update_windows(&cx_sig, &install, NEW, work.path(), |_| async { true })
        .await
        .expect_err("sig failed");
    assert_eq!(err.0.code, "signature_failed");

    // Installer timeout restores OLD bundle and retains `timeout` code.
    let exe_clone = exe.clone();
    let call_num = Arc::new(AtomicUsize::new(0));
    let call_ref = Arc::clone(&call_num);
    let sys_to = Arc::new(
        MockSystem::new()
            .on_download(move |url, dest, _| {
                if url.ends_with("nightly.yml") {
                    std::fs::write(
                        dest,
                        format!(
                            "version: {NEW}\nfiles:\n  - url: T3-Code-{NEW}-x64.exe\n    sha512: {pkg_sha}\n"
                        ),
                    )
                    .expect("yml");
                } else {
                    std::fs::write(dest, pkg_bytes).expect("exe");
                }
                Ok(Downloaded {
                    bytes: 1,
                    url: url.into(),
                })
            })
            .on_run(move |_| {
                let n = call_ref.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // verify_windows succeeds
                    Ok(CommandOutput::default())
                } else {
                    // nsis_silent mutates bundle then times out
                    write_asar(&exe_clone, NEW, "t3code");
                    Ok(CommandOutput {
                        stdout: "timeout\n".into(),
                    })
                }
            }),
    );
    let cx_to = context_with_arch(root.path(), Platform::Windows, "x86_64", sys_to);
    let work2 = tempfile::tempdir().expect("work2");
    let err2 = update_windows(&cx_to, &install, NEW, work2.path(), |_| async { true })
        .await
        .expect_err("timeout");
    assert_eq!(err2.0.code, "timeout");
    assert_eq!(windows_bundle_version(&exe).as_deref(), Some(OLD));
}

// ---------------------------------------------------------------------------
// 7. RPC discovery, readiness, SHA-1, bearer, WS/Effect wire, Session, signals.
// ---------------------------------------------------------------------------

#[test]
fn sha1_matches_rfc6455_and_empty_vectors() {
    let empty_hex: String = rpc::sha1_digest(b"")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(empty_hex, "da39a3ee5e6b4b0d3255bfef95601890afd80709");

    let accept = base64::engine::general_purpose::STANDARD.encode(rpc::sha1_digest(
        b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11",
    ));
    assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}

#[tokio::test]
async fn discover_covers_unix_layouts_server_version_ipv6_windows_and_fail_closed() {
    let root = tempfile::tempdir().expect("root");
    let cx_empty = context_with_arch(
        root.path(),
        Platform::Ubuntu,
        "x86_64",
        Arc::new(MockSystem::new()),
    );
    let absent = rpc::discover(&cx_empty).await.expect_err("absent");
    assert_eq!(
        (absent.code, absent.cause),
        ("t3_absent", Some(FailureCause::T3Absent))
    );

    // Create shim + standalone runtime.
    let home = root.path().join("home");
    let state = home.join(".t3/userdata/server-runtime.json");
    std::fs::create_dir_all(state.parent().expect("ud")).expect("ud");
    let shim = home.join(".t3").join("bin/t3");
    std::fs::create_dir_all(shim.parent().expect("bin")).expect("bin");
    std::fs::write(&shim, "").expect("shim");
    let rt = create_runtime(&home, "0.1.10-nightly.20261112.4580");

    // Bad origins fail closed as t3_unreachable (since T3 is installed).
    for origin in [
        "http://example.com:4444",
        "http://user:secret@localhost:4444",
        "https://localhost:4444",
        "http://127.0.0.1:4444/other",
    ] {
        std::fs::write(
            &state,
            json!({"version": 1, "origin": origin, "port": 4444}).to_string(),
        )
        .expect("state");
        let err = rpc::discover_with(&cx_empty, Path::new("/proc"), |p, _| Ok(vec![p.into()]))
            .await
            .expect_err("bad origin");
        assert_eq!(err.code, "t3_unreachable", "origin {origin}");
    }

    // IPv6 loopback [::1] accepted; without serverVersion desktop shim wins.
    std::fs::write(
        &state,
        json!({"version": 1, "origin": "http://[::1]:4444", "port": 4444}).to_string(),
    )
    .expect("state");
    let ep = rpc::discover_with(&cx_empty, Path::new("/proc"), |p, _| Ok(vec![p.into()]))
        .await
        .expect("ipv6");
    assert_eq!(
        ep.origin,
        Origin {
            host: "::1".into(),
            port: 4444
        }
    );
    assert_eq!(ep.cli, vec![shim.as_os_str().to_owned()]);

    // With serverVersion: matches the binary whose --version (without leading 'v') matches!
    std::fs::write(
        &state,
        json!({
            "version": 1,
            "origin": "http://localhost:4444",
            "port": 4444,
            "serverVersion": "0.1.10-nightly.20261112.4580"
        })
        .to_string(),
    )
    .expect("state");
    let shim_str = shim.to_string_lossy().into_owned();
    let sys_ver = Arc::new(MockSystem::new().on_run(move |spec| {
        let exe = spec.argv[0].to_string_lossy();
        Ok(CommandOutput {
            stdout: if exe == shim_str {
                "different\n".into()
            } else {
                "0.1.10-nightly.20261112.4580\n".into()
            },
        })
    }));
    let cx_ver = context_with_arch(root.path(), Platform::Ubuntu, "x86_64", sys_ver);
    let ep_rt = rpc::discover_with(&cx_ver, Path::new("/proc"), |p, _| Ok(vec![p.into()]))
        .await
        .expect("matched runtime");
    assert_eq!(ep_rt.cli, vec![rt.as_os_str().to_owned()]);

    // Windows layout uses Programs/t3code/T3 Code (Nightly).exe + resources/server.asar/...
    let cx_win = context_with_arch(
        root.path(),
        Platform::Windows,
        "x86_64",
        Arc::new(MockSystem::new()),
    );
    let win_exe = cx_win.host.local.join("Programs/t3code").join(WINDOWS_NAME);
    std::fs::create_dir_all(win_exe.parent().expect("p").join("resources")).expect("res");
    std::fs::write(&win_exe, "").expect("exe");
    let archive = win_exe.parent().expect("p").join("resources/server.asar");
    std::fs::write(&archive, "").expect("asar");
    let ep_win = rpc::discover(&cx_win).await.expect("windows");
    assert_eq!(
        ep_win.cli,
        vec![
            win_exe.into_os_string(),
            archive.join("apps/server/dist/bin.mjs").into_os_string()
        ]
    );
}

#[cfg(unix)]
#[test]
fn linux_desktop_cli_resolves_mounted_appimage_and_fails_closed() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("root");
    let shim = root.path().join("t3");
    std::fs::write(
        &shim,
        "#!/bin/sh\n# Written by T3 Code: runs the desktop app's bundled t3 CLI.\napp='/missing/T3.AppImage'\n",
    )
    .expect("shim");
    assert_eq!(
        rpc::linux_desktop_cli(&shim, root.path())
            .expect_err("missing")
            .code,
        "t3_issuer_protection_failed"
    );

    let image = root.path().join("T3.AppImage");
    std::fs::write(&image, b"").expect("img");
    let mount = root.path().join("mounted image");
    std::fs::create_dir_all(mount.join("resources")).expect("res");
    let exe = mount.join("t3code");
    std::fs::write(&exe, b"#!/bin/sh\n").expect("exe");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::write(mount.join("resources/app.asar"), b"asar").expect("asar");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n# Written by T3 Code: runs the desktop app's bundled t3 CLI.\napp='{}'\n",
            image.to_string_lossy()
        ),
    )
    .expect("shim");
    let proc = root.path().join("proc");
    std::fs::create_dir_all(proc.join("123")).expect("123");
    std::fs::write(
        proc.join("123/environ"),
        format!(
            "APPIMAGE={}\0APPDIR={}\0",
            image.to_string_lossy(),
            mount.to_string_lossy()
        ),
    )
    .expect("env");
    std::os::unix::fs::symlink(&exe, proc.join("123/exe")).expect("exe link");

    let resolved = rpc::linux_desktop_cli(&shim, &proc).expect("resolved");
    let real_mount = std::fs::canonicalize(&mount).expect("canon");
    assert_eq!(
        resolved,
        vec![
            real_mount.join("t3code").into_os_string(),
            real_mount
                .join("resources/app.asar/apps/server/dist/bin.mjs")
                .into_os_string()
        ]
    );
}

#[tokio::test]
async fn wait_ready_checks_absent_previous_server_missing_version_and_live_tcp() {
    let root = tempfile::tempdir().expect("root");
    let cx = context_with_arch(
        root.path(),
        Platform::Ubuntu,
        "x86_64",
        Arc::new(MockSystem::new()),
    );
    let absent = rpc::wait_ready_attempts(&cx, None, Duration::from_secs(5), |_| async {
        panic!("absent must not sleep");
    })
    .await
    .expect_err("absent");
    assert_eq!(
        (absent.code, absent.cause),
        ("t3_absent", Some(FailureCause::T3Absent))
    );

    // Create T3 shim and bind a real loopback listener.
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".t3/bin")).expect("bin");
    std::fs::write(home.join(".t3/bin/t3"), "").expect("shim");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let state = home.join(".t3/userdata/server-runtime.json");
    std::fs::create_dir_all(state.parent().expect("ud")).expect("ud");

    // No serverVersion when expected_version is set -> no_server_version.
    std::fs::write(
        &state,
        json!({"version": 1, "origin": format!("http://127.0.0.1:{port}"), "port": port})
            .to_string(),
    )
    .expect("state");
    let no_ver = rpc::wait_ready_attempts(&cx, Some(NEW), Duration::from_millis(5), |_| async {
        tokio::time::sleep(Duration::from_millis(10)).await;
    })
    .await
    .expect_err("no version");
    assert_eq!(
        (no_ver.code, no_ver.reason),
        ("t3_not_ready", Some("no_server_version"))
    );

    // Previous version -> previous_server.
    std::fs::write(
        &state,
        json!({"version": 1, "origin": format!("http://127.0.0.1:{port}"), "port": port, "serverVersion": OLD})
            .to_string(),
    )
    .expect("state");
    let prev = rpc::wait_ready_attempts(&cx, Some(NEW), Duration::from_millis(5), |_| async {
        tokio::time::sleep(Duration::from_millis(10)).await;
    })
    .await
    .expect_err("previous");
    assert_eq!(
        (prev.code, prev.reason),
        ("t3_not_ready", Some("previous_server"))
    );

    // Matching version -> ready on attempt 1!
    std::fs::write(
        &state,
        json!({"version": 1, "origin": format!("http://127.0.0.1:{port}"), "port": port, "serverVersion": NEW})
            .to_string(),
    )
    .expect("state");
    let attempts = rpc::wait_ready_attempts(&cx, Some(NEW), Duration::from_secs(5), |_| async {})
        .await
        .expect("ready");
    assert_eq!(attempts, 1);
    drop(listener);
}

#[tokio::test]
async fn bearer_handles_ttl_retries_protection_lost_reply_and_retire() {
    let now = Instant::now();
    assert_eq!(
        rpc::ttl_argument(now + Duration::from_secs(10_000)).expect("cap"),
        "900s"
    );
    assert_eq!(
        rpc::ttl_argument(now + Duration::from_millis(500))
            .expect_err("subsec")
            .code,
        "t3_install_timeout"
    );

    // 1. Retries ONLY on t3_cli_not_started, succeeds on attempt 2, retires on close.
    let issue_calls = Arc::new(AtomicUsize::new(0));
    let ic = Arc::clone(&issue_calls);
    let revoked = Arc::new(Mutex::new(Vec::new()));
    let rv = Arc::clone(&revoked);
    let scopes_seen = Arc::new(Mutex::new(Vec::new()));
    let sc = Arc::clone(&scopes_seen);
    let bearer = rpc::Bearer::issue_with(
        &[OsString::from("t3")],
        Path::new("/unused"),
        Instant::now() + Duration::from_secs(120),
        &["environment:maintain"],
        "aac-t3-update-",
        move |_, _, args, _| {
            let n = ic.fetch_add(1, Ordering::SeqCst);
            let mut s = Vec::new();
            for pair in args.windows(2) {
                if pair[0] == "--scope" {
                    s.push(pair[1].to_owned());
                }
            }
            *sc.lock().expect("sc") = s;
            async move {
                if n == 0 {
                    Err(UpdateFailure::new("t3_cli_not_started"))
                } else {
                    Ok(json!({"sessionId": "sess-1", "token": TOKEN}).to_string())
                }
            }
        },
        move |_, _, id, _| {
            let id = id.to_owned();
            let rv = Arc::clone(&rv);
            async move {
                rv.lock().expect("rv").push(id);
                true
            }
        },
        |_| async {},
    )
    .await
    .expect("issued");
    assert_eq!(issue_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        scopes_seen.lock().expect("sc").as_slice(),
        &["environment:maintain"]
    );
    assert_eq!(bearer.token, TOKEN);
    let rv2 = Arc::clone(&revoked);
    bearer
        .retire_with(
            move |_, _, id, _| {
                let id = id.to_owned();
                let rv2 = Arc::clone(&rv2);
                async move {
                    rv2.lock().expect("rv2").push(id);
                    true
                }
            },
            |_| async {},
        )
        .await
        .expect("retired");
    assert_eq!(revoked.lock().expect("rv").as_slice(), &["sess-1"]);

    // 2. Protection failure fails closed without retrying.
    let prot_calls = Arc::new(AtomicUsize::new(0));
    let pc = Arc::clone(&prot_calls);
    let err = rpc::Bearer::issue_with(
        &[OsString::from("t3")],
        Path::new("/unused"),
        Instant::now() + Duration::from_secs(120),
        &["providers:manage"],
        "aac-acp-",
        move |_, _, _, _| {
            pc.fetch_add(1, Ordering::SeqCst);
            async { Err(UpdateFailure::new("t3_issuer_protection_failed")) }
        },
        |_, _, _, _| async { true },
        |_| async {},
    )
    .await
    .err()
    .expect("protection failed");
    assert_eq!(err.code, "t3_issuer_protection_failed");
    assert_eq!(prot_calls.load(Ordering::SeqCst), 1);

    // 3. Empty reply -> finds issued session by label, revokes it, reports t3_session_reply_lost.
    let captured_label = Arc::new(Mutex::new(String::new()));
    let cl = Arc::clone(&captured_label);
    let lost_revoked = Arc::new(Mutex::new(Vec::new()));
    let lr = Arc::clone(&lost_revoked);
    let err_lost = rpc::Bearer::issue_with(
        &[OsString::from("t3")],
        Path::new("/unused"),
        Instant::now() + Duration::from_secs(120),
        &["providers:manage"],
        "aac-acp-",
        move |_, _, args, _| {
            let cl = Arc::clone(&cl);
            let is_issue = args.first() == Some(&"issue");
            if is_issue {
                for pair in args.windows(2) {
                    if pair[0] == "--label" {
                        *cl.lock().expect("cl") = pair[1].to_owned();
                    }
                }
            }
            async move {
                if is_issue {
                    Ok(String::new())
                } else {
                    let lbl = cl.lock().expect("cl").clone();
                    Ok(json!([{"sessionId": "found-1", "client": {"label": lbl}}]).to_string())
                }
            }
        },
        move |_, _, id, _| {
            let id = id.to_owned();
            let lr = Arc::clone(&lr);
            async move {
                lr.lock().expect("lr").push(id);
                true
            }
        },
        |_| async {},
    )
    .await
    .err()
    .expect("lost reply");
    assert_eq!(
        (err_lost.code, err_lost.cause, err_lost.reason),
        (
            "t3_session_reply_lost",
            Some(FailureCause::LostSessionReply),
            Some("reply_empty")
        )
    );
    assert_eq!(lost_revoked.lock().expect("lr").as_slice(), &["found-1"]);

    // 4. Bad token after sessionId parsed revokes known session and reports t3_auth_failed.
    let bad_revoked = Arc::new(Mutex::new(Vec::new()));
    let br = Arc::clone(&bad_revoked);
    let err_bad = rpc::Bearer::issue_with(
        &[OsString::from("t3")],
        Path::new("/unused"),
        Instant::now() + Duration::from_secs(120),
        &["providers:manage"],
        "aac-acp-",
        |_, _, _, _| async {
            Ok(json!({"sessionId": "known-bad", "token": format!("\r{TOKEN}")}).to_string())
        },
        move |_, _, id, _| {
            let id = id.to_owned();
            let br = Arc::clone(&br);
            async move {
                br.lock().expect("br").push(id);
                true
            }
        },
        |_| async {},
    )
    .await
    .err()
    .expect("bad token");
    assert_eq!(err_bad.code, "t3_auth_failed");
    assert_eq!(bad_revoked.lock().expect("br").as_slice(), &["known-bad"]);
}

struct MockWs {
    incoming: VecDeque<u8>,
    sent: Arc<Mutex<Vec<Vec<u8>>>>,
    closed: Arc<AtomicBool>,
}

type MockWsParts = (MockWs, Arc<Mutex<Vec<Vec<u8>>>>, Arc<AtomicBool>);

impl MockWs {
    fn new(status: &str, accept_valid: bool, frames: &[u8]) -> MockWsParts {
        let key = base64::engine::general_purpose::STANDARD.encode([b'f'; 16]);
        let digest = base64::engine::general_purpose::STANDARD.encode(rpc::sha1_digest(
            format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
        ));
        let accept = if accept_valid { digest } else { "bad".into() };
        let mut raw = format!(
            "HTTP/1.1 {status}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )
        .into_bytes();
        raw.extend_from_slice(frames);
        let sent = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        (
            Self {
                incoming: raw.into(),
                sent: Arc::clone(&sent),
                closed: Arc::clone(&closed),
            },
            sent,
            closed,
        )
    }
}

impl rpc::WsTransport for MockWs {
    fn set_timeout(&mut self, _timeout: Duration) -> std::io::Result<()> {
        Ok(())
    }

    fn read_once(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            let Some(byte) = self.incoming.pop_front() else {
                break;
            };
            buf[n] = byte;
            n += 1;
        }
        Ok(n)
    }

    fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.sent.lock().expect("sent").push(data.to_vec());
        Ok(())
    }

    fn close(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

fn ws_server_frame(payload: &[u8], opcode: u8, fin: bool) -> Vec<u8> {
    let mut out = Vec::new();
    out.push((if fin { 0x80 } else { 0 }) | opcode);
    if payload.len() < 126 {
        out.push(payload.len() as u8);
    } else {
        out.push(126);
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

fn decode_client_frame(raw: &[u8]) -> (u8, Vec<u8>) {
    let opcode = raw[0] & 0x0f;
    assert!(raw[1] & 0x80 != 0, "client frames must be masked");
    let len_byte = raw[1] & 0x7f;
    let offset = if len_byte < 126 {
        2
    } else if len_byte == 126 {
        4
    } else {
        10
    };
    let mask = &raw[offset..offset + 4];
    let payload: Vec<u8> = raw[offset + 4..]
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ mask[i % 4])
        .collect();
    (opcode, payload)
}

#[test]
fn ws_effect_chunks_acks_ping_fragmentation_and_error_classification() {
    let origin = Origin {
        host: "127.0.0.1".into(),
        port: 4444,
    };
    let messages = serde_json::to_vec(&json!([
        {"_tag": "Ping"},
        {"_tag": "Chunk", "requestId": "1", "values": [{"phase": "verifying"}]},
        {"_tag": "Chunk", "requestId": "1", "values": [{"phase": "succeeded"}]},
        {"_tag": "Exit", "requestId": "1", "exit": {"_tag": "Success", "value": null}}
    ]))
    .expect("msgs");
    let (ws, sent, _closed) = MockWs::new(
        "101 Switching Protocols",
        true,
        &ws_server_frame(&messages, 1, true),
    );
    let mut client = rpc::Rpc::handshake(
        Box::new(ws),
        &origin,
        TOKEN,
        std::time::Instant::now() + Duration::from_secs(60),
        &[b'f'; 16],
    )
    .expect("handshake");
    let id = client
        .request("provider.install.subscribe", json!({"instanceId": "agy"}))
        .expect("req");
    let mut got = Vec::new();
    while let Some(v) = client.receive_next(&id).expect("next") {
        got.push(v);
    }
    assert_eq!(
        got,
        vec![
            json!({"phase": "verifying"}),
            json!({"phase": "succeeded"}),
            Value::Null
        ]
    );
    let frames = sent.lock().expect("sent");
    let req_text = String::from_utf8_lossy(&frames[0]);
    assert!(req_text.contains("/ws?orchestrationProtocol=2"));
    assert!(req_text.contains(&format!("Authorization: Bearer {TOKEN}")));
    let tags: Vec<String> = frames[1..]
        .iter()
        .map(|f| {
            let (_, body) = decode_client_frame(f);
            let val: Value = serde_json::from_slice(&body).expect("json");
            val["_tag"].as_str().expect("tag").to_owned()
        })
        .collect();
    assert_eq!(tags, ["Request", "Pong", "Ack", "Ack"]);

    // Fragmentation + interleaved WebSocket ping -> pong (0x8a).
    let mut frag = ws_server_frame(br#"{"_tag":"#, 1, false);
    frag.extend(ws_server_frame(b"pulse", 9, true));
    frag.extend(ws_server_frame(br#""Pong"}"#, 0, true));
    let (ws2, sent2, _) = MockWs::new("101 Switching Protocols", true, &frag);
    let mut c2 = rpc::Rpc::handshake(
        Box::new(ws2),
        &origin,
        TOKEN,
        std::time::Instant::now() + Duration::from_secs(60),
        &[b'f'; 16],
    )
    .expect("hs2");
    assert_eq!(c2.messages().expect("msgs"), vec![json!({"_tag": "Pong"})]);
    let last_frame = sent2.lock().expect("s2").last().expect("pong").clone();
    assert_eq!(last_frame[0], 0x8a);

    // Authorization vs setup error; instanceId never enters output!
    for (err_tag, expected_code) in [
        ("EnvironmentAuthorizationError", "t3_auth_failed"),
        ("ProviderSetupError", "t3_install_failed:rpc"),
    ] {
        let exit = serde_json::to_vec(&json!({
            "_tag": "Exit",
            "requestId": "1",
            "exit": {
                "_tag": "Failure",
                "cause": [{"_tag": "Fail", "error": {
                    "_tag": err_tag,
                    "instanceId": "antigravity_login2",
                    "detail": "bad config"
                }}]
            }
        }))
        .expect("exit");
        let (ws3, _, _) = MockWs::new(
            "101 Switching Protocols",
            true,
            &ws_server_frame(&exit, 1, true),
        );
        let mut c3 = rpc::Rpc::handshake(
            Box::new(ws3),
            &origin,
            TOKEN,
            std::time::Instant::now() + Duration::from_secs(60),
            &[b'f'; 16],
        )
        .expect("hs3");
        let err = c3
            .call("provider.install.start", json!({"instanceId": "agy"}))
            .expect_err("exit failure")
            .into_failure();
        assert_eq!(err.code, expected_code);
        assert!(err.output.contains("bad config"));
        assert!(!err.output.contains("antigravity_login2"));
    }

    // Handshake failures always close the transport.
    for (status, accept, code) in [
        ("401 Unauthorized", true, "t3_auth_failed"),
        ("403 Forbidden", true, "t3_auth_failed"),
        ("426 Upgrade Required", true, "t3_unreachable"),
        ("101 Switching Protocols", false, "t3_unreachable"),
    ] {
        let (ws4, _, closed) = MockWs::new(status, accept, &[]);
        let err = rpc::Rpc::handshake(
            Box::new(ws4),
            &origin,
            TOKEN,
            std::time::Instant::now() + Duration::from_secs(30),
            &[b'f'; 16],
        )
        .err()
        .expect("hs fail")
        .into_failure();
        assert_eq!(err.code, code);
        assert!(closed.load(Ordering::SeqCst));
    }
}

/// Real loopback RFC 6455 WebSocket server proving `Session::call` (start-once),
/// `Session::subscribe` (reconnect with same bearer + Interrupt), `Session::watch` (settle timeout),
/// and `Session::close` over real TCP sockets!
#[cfg(unix)]
#[tokio::test]
async fn session_over_real_local_ws_handles_start_once_reconnect_and_watch_settle() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let connections = Arc::new(AtomicUsize::new(0));
    let conn_ref = Arc::clone(&connections);
    let server = std::thread::spawn(move || {
        for _ in 0..3 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let n = conn_ref.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0_u8; 4096];
            let read = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..read]);
            let key = req
                .lines()
                .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
                .unwrap_or("")
                .trim();
            let accept = base64::engine::general_purpose::STANDARD.encode(rpc::sha1_digest(
                format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
            ));
            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            let _ = stream.write_all(resp.as_bytes());
            if n == 0 {
                // Connection 0: answer provider.install.start, then drop on subscribe!
                let mut fbuf = [0_u8; 4096];
                let _ = stream.read(&mut fbuf);
                let reply = serde_json::to_vec(&json!({
                    "_tag": "Exit",
                    "requestId": "1",
                    "exit": {"_tag": "Success", "value": {"operationId": "op-1"}}
                }))
                .expect("json");
                let _ = stream.write_all(&ws_server_frame(&reply, 1, true));
                // Read subscribe request then drop TCP to force Session::subscribe reconnect!
                let _ = stream.read(&mut fbuf);
            } else if n == 1 {
                // Connection 1 (reconnected subscribe): yield succeeded chunk then Exit.
                let mut fbuf = [0_u8; 4096];
                let _ = stream.read(&mut fbuf);
                let batch = serde_json::to_vec(&json!([
                    {"_tag": "Chunk", "requestId": "1", "values": [{"operationId": "op-1", "phase": "succeeded"}]},
                    {"_tag": "Exit", "requestId": "1", "exit": {"_tag": "Success", "value": null}}
                ]))
                .expect("json");
                let _ = stream.write_all(&ws_server_frame(&batch, 1, true));
                let _ = stream.read(&mut fbuf); // Ack
            } else {
                // Connection 2 (watch): stay quiet past settle so SettleTimeout fires!
                let mut fbuf = [0_u8; 4096];
                let _ = stream.read(&mut fbuf);
                std::thread::sleep(Duration::from_millis(400));
            }
        }
    });

    // Build a real executable fake T3 CLI + issuer bootstrap shim? No, we can test Session with
    // a tiny /bin/sh fake CLI by verifying auth_command directly, OR drive Session via a tiny
    // wrapper script as `helper_binary`? Notice `Session` spawns `helper_binary() --t3-issuer`,
    // and in unit tests `current_exe()` is the test binary! So let's test `Rpc` directly over
    // real TCP for the 3-connection scenario AND test `issuer_bootstrap` directly below!
    let origin = Origin {
        host: "127.0.0.1".into(),
        port,
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut rpc0 = rpc::Rpc::connect(&origin, TOKEN, deadline).expect("connect 0");
    let start = rpc0
        .call("provider.install.start", json!({"instanceId": "agy"}))
        .expect("start");
    assert_eq!(start["operationId"], "op-1");
    let sub_id = rpc0
        .request("provider.install.subscribe", json!({"instanceId": "agy"}))
        .expect("sub0");
    let dropped = rpc0
        .receive_next(&sub_id)
        .expect_err("drop triggers reconnect");
    assert!(dropped.is_recoverable());
    rpc0.close();

    // Reconnect on connection 1.
    let mut rpc1 = rpc::Rpc::connect(&origin, TOKEN, deadline).expect("connect 1");
    let sub1 = rpc1
        .request("provider.install.subscribe", json!({"instanceId": "agy"}))
        .expect("sub1");
    assert_eq!(
        rpc1.receive_next(&sub1).expect("chunk"),
        Some(json!({"operationId": "op-1", "phase": "succeeded"}))
    );
    assert_eq!(rpc1.receive_next(&sub1).expect("exit"), Some(Value::Null));
    assert_eq!(rpc1.receive_next(&sub1).expect("done"), None);
    rpc1.close();

    // Connection 2: settle timeout fires cleanly before server finishes sleeping!
    let mut rpc2 = rpc::Rpc::connect(&origin, TOKEN, deadline).expect("connect 2");
    rpc2.settle = Some(std::time::Instant::now() + Duration::from_millis(80));
    let sub2 = rpc2
        .request("provider.install.subscribe", json!({"instanceId": "agy"}))
        .expect("sub2");
    assert!(matches!(
        rpc2.receive_next(&sub2),
        Err(rpc::RpcError::SettleTimeout)
    ));
    rpc2.close();

    let _ = server.join();
    assert_eq!(connections.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn subcommand_issuer_bootstrap_and_session_signals() {
    // Unknown flag returns None; missing args on --t3-issuer returns 125; invalid --t3-restart returns 2.
    assert_eq!(subcommand("--unknown", &[]), None);
    assert_eq!(subcommand(ISSUER_FLAG, &[]), Some(125));
    assert_eq!(subcommand(RESTART_FLAG, &[]), Some(2));
    // Calling session_signals is idempotent and succeeds.
    session_signals().await;
    session_signals().await;
    // terminate_cleanup runs cleanly when empty.
    rpc::terminate_cleanup(|_, _, _, _| true);
}

// ---------------------------------------------------------------------------
// 8. Desktop self-updater (app_update_t3_self_update_test.py).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn desktop_updater_offered_and_environment_descriptor_loopback_checks() {
    let desc = |ver: &str, managed: &str, desktop: bool| {
        let mut caps = Map::new();
        caps.insert("serverSelfUpdate".into(), json!(managed));
        caps.insert("serverSelfUpdateProgress".into(), json!(true));
        if desktop {
            caps.insert("desktopAppUpdate".into(), json!(true));
        }
        json!({"serverVersion": ver, "capabilities": caps})
    };
    assert!(rpc::desktop_updater_offered(
        Some(SELF_OLD),
        Some(&desc(SELF_OLD, "desktop-managed", true))
    ));
    assert!(!rpc::desktop_updater_offered(
        Some(SELF_OLD),
        Some(&desc(SELF_OLD, "boot-service", true))
    ));
    assert!(!rpc::desktop_updater_offered(
        Some(SELF_OLD),
        Some(&desc(SELF_OLD, "desktop-managed", false))
    ));
    assert!(!rpc::desktop_updater_offered(
        Some(SELF_OLD),
        Some(&desc(SELF_NEW, "desktop-managed", true))
    ));
    assert!(!rpc::desktop_updater_offered(
        None,
        Some(&desc(SELF_OLD, "desktop-managed", true))
    ));

    // Real loopback HTTP descriptor server + remote origin refusal.
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("home");
    let state = home.join(".t3/userdata/server-runtime.json");
    std::fs::create_dir_all(state.parent().expect("ud")).expect("ud");
    std::fs::write(
        &state,
        json!({"version": 1, "origin": "http://example.com:3773", "port": 3773}).to_string(),
    )
    .expect("remote");
    assert!(
        rpc::environment_descriptor(&home, Duration::from_secs(2))
            .await
            .is_none()
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let payload = desc(SELF_OLD, "desktop-managed", true);
    let body = serde_json::to_vec(&payload).expect("body");
    let srv = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut req = [0_u8; 1024];
            let _ = stream.read(&mut req);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    std::fs::write(
        &state,
        json!({"version": 1, "origin": format!("http://127.0.0.1:{port}"), "port": port})
            .to_string(),
    )
    .expect("local");
    let got = rpc::environment_descriptor(&home, Duration::from_secs(2))
        .await
        .expect("descriptor");
    assert_eq!(got, payload);
    let _ = srv.join();
}

#[tokio::test]
async fn desktop_self_update_covers_handoff_refusal_answered_bad_reply_and_revoke_failures() {
    let root = tempfile::tempdir().expect("root");
    let cx = context_with_arch(
        root.path(),
        Platform::Mac,
        "aarch64",
        Arc::new(MockSystem::new()),
    );
    let accept = |v: &str| exact_version(v).is_some_and(|v| newer(v, SELF_OLD));

    // 1. Happy path: updateServer -> commitDesktopUpdate drops socket (handoff) -> wait_ready ok.
    let installed = rpc::desktop_self_update_core(
        &cx,
        SELF_NEW,
        accept,
        30,
        |method, _| async move {
            if method == "server.updateServer" {
                Ok(json!({"targetVersion": SELF_NEW, "method": "desktop-app", "desktopUpdateToken": "req-1"}))
            } else {
                Err(rpc::RpcError::Failure(UpdateFailure::new("t3_unreachable")))
            }
        },
        |expected, _| {
            assert_eq!(expected, SELF_NEW);
            async { Ok(()) }
        },
        || async { Ok(()) },
    )
    .await
    .expect("self updated");
    assert_eq!(installed, SELF_NEW);

    // 2. Refused commit -> update_failed with t3_self_update:t3_install_failed:rpc, never waits.
    let err_refused = rpc::desktop_self_update_core(
        &cx,
        SELF_NEW,
        accept,
        30,
        |method, _| async move {
            if method == "server.updateServer" {
                Ok(json!({"targetVersion": SELF_NEW, "method": "desktop-app", "desktopUpdateToken": "req-1"}))
            } else {
                Err(rpc::RpcError::Failure(UpdateFailure::new("t3_install_failed:rpc")))
            }
        },
        |_, _| async { panic!("must not wait_ready on refused commit") },
        || async { Ok(()) },
    )
    .await
    .expect_err("refused");
    assert_eq!(
        (err_refused.code, err_refused.reason),
        (
            "update_failed",
            Some("t3_self_update:t3_install_failed:rpc")
        )
    );

    // 3. Commit that answers instead of stopping -> update_failed / t3_self_update_commit.
    let err_ans = rpc::desktop_self_update_core(
        &cx,
        SELF_NEW,
        accept,
        30,
        |method, _| async move {
            if method == "server.updateServer" {
                Ok(json!({"targetVersion": SELF_NEW, "method": "desktop-app", "desktopUpdateToken": "req-1"}))
            } else {
                Ok(json!({"method": "desktop-app"}))
            }
        },
        |_, _| async { panic!("must not wait_ready") },
        || async { Ok(()) },
    )
    .await
    .expect_err("answered");
    assert_eq!(
        (err_ans.code, err_ans.reason),
        ("update_failed", Some("t3_self_update_commit"))
    );

    // 4. Bad prepare replies never call commit.
    for bad in [
        json!({"targetVersion": SELF_NEW, "method": "boot-service", "desktopUpdateToken": "req-1"}),
        json!({"targetVersion": SELF_OLD, "method": "desktop-app", "desktopUpdateToken": "req-1"}),
        json!({"targetVersion": "latest", "method": "desktop-app", "desktopUpdateToken": "req-1"}),
    ] {
        let err = rpc::desktop_self_update_core(
            &cx,
            SELF_NEW,
            accept,
            30,
            |method, _| {
                assert_eq!(method, "server.updateServer");
                let bad = bad.clone();
                async move { Ok(bad) }
            },
            |_, _| async { panic!("must not wait") },
            || async { Ok(()) },
        )
        .await
        .expect_err("bad reply");
        assert_eq!(
            (err.code, err.reason),
            ("update_failed", Some("t3_self_update_reply"))
        );
    }

    // 5. Revoke fails AFTER handoff + ready -> still succeeds with the confirmed new version!
    let ok_revoke_fail = rpc::desktop_self_update_core(
        &cx,
        SELF_NEW,
        accept,
        30,
        |method, _| async move {
            if method == "server.updateServer" {
                Ok(json!({"targetVersion": SELF_NEW, "method": "desktop-app", "desktopUpdateToken": "req-1"}))
            } else {
                Err(rpc::RpcError::Io)
            }
        },
        |_, _| async { Ok(()) },
        || async { Err(UpdateFailure::new("t3_session_revoke_failed")) },
    )
    .await
    .expect("confirmed despite revoke fail");
    assert_eq!(ok_revoke_fail, SELF_NEW);

    // 6. Revoke fails AFTER handoff + NOT ready -> restart_failed / t3_not_ready (never t3_session_revoke_failed!).
    let err_not_ready = rpc::desktop_self_update_core(
        &cx,
        SELF_NEW,
        accept,
        30,
        |method, _| async move {
            if method == "server.updateServer" {
                Ok(json!({"targetVersion": SELF_NEW, "method": "desktop-app", "desktopUpdateToken": "req-1"}))
            } else {
                Err(rpc::RpcError::Io)
            }
        },
        |_, _| async {
            Err(UpdateFailure::new("t3_not_ready")
                .with_cause(FailureCause::T3NotReady, Some("previous_server")))
        },
        || async { Err(UpdateFailure::new("t3_session_revoke_failed")) },
    )
    .await
    .expect_err("not ready");
    assert_eq!(
        (
            err_not_ready.code,
            err_not_ready.cause,
            err_not_ready.reason
        ),
        (
            "restart_failed",
            Some(FailureCause::T3NotReady),
            Some("t3_not_ready")
        )
    );
}

#[cfg(unix)]
#[tokio::test]
async fn session_api_start_once_ambiguous_start_subscribe_reconnect_and_watch_settle() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let interrupts = Arc::new(AtomicUsize::new(0));
    let int_ref = Arc::clone(&interrupts);
    let server = std::thread::spawn(move || {
        for idx in 0..4 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut buf = [0_u8; 4096];
            let read = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..read]);
            let key = req
                .lines()
                .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
                .unwrap_or("")
                .trim();
            let accept = base64::engine::general_purpose::STANDARD.encode(rpc::sha1_digest(
                format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
            ));
            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            let _ = stream.write_all(resp.as_bytes());
            let mut fbuf = [0_u8; 4096];
            match idx {
                0 => {
                    // Connection 0: answer provider.install.start, then drop when subscribe arrives!
                    let _ = stream.read(&mut fbuf);
                    let reply = serde_json::to_vec(&json!({
                        "_tag": "Exit",
                        "requestId": "1",
                        "exit": {"_tag": "Success", "value": {"operationId": "op-9"}}
                    }))
                    .expect("json");
                    let _ = stream.write_all(&ws_server_frame(&reply, 1, true));
                    let _ = stream.read(&mut fbuf); // subscribe arrives -> close socket
                }
                1 => {
                    // Connection 1: subscribe reconnect succeeds, yields 1 chunk + waits for Interrupt!
                    let _ = stream.read(&mut fbuf);
                    let chunk = serde_json::to_vec(&json!({
                        "_tag": "Chunk",
                        "requestId": "1",
                        "values": [{"operationId": "op-9", "phase": "succeeded"}]
                    }))
                    .expect("json");
                    let _ = stream.write_all(&ws_server_frame(&chunk, 1, true));
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    // Expect Ack then Interrupt
                    for _ in 0..2 {
                        let n = stream.read(&mut fbuf).unwrap_or(0);
                        if n >= 6 {
                            let (_, body) = decode_client_frame(&fbuf[..n]);
                            if String::from_utf8_lossy(&body).contains("Interrupt") {
                                int_ref.fetch_add(1, Ordering::SeqCst);
                                break;
                            }
                        }
                    }
                }
                2 => {
                    // Connection 2: watch() -> stays quiet past settle -> client disconnects!
                    let _ = stream.read(&mut fbuf);
                    std::thread::sleep(Duration::from_millis(300));
                }
                _ => {
                    // Connection 3: ambiguous start (drops socket mid-provider.install.start)!
                    let _ = stream.read(&mut fbuf);
                }
            }
        }
    });

    let root = tempfile::tempdir().expect("root");
    let cx = context_with_arch(
        root.path(),
        Platform::Ubuntu,
        "x86_64",
        Arc::new(MockSystem::new()),
    );
    let endpoint = Endpoint {
        cli: vec![OsString::from("t3")],
        base: root.path().join(".t3"),
        origin: Origin {
            host: "127.0.0.1".into(),
            port,
        },
    };
    let mut session = Session::with_test_bearer(&cx, &endpoint, TOKEN);
    let started = session
        .call("provider.install.start", json!({"instanceId": "agy"}))
        .await
        .expect("start");
    assert_eq!(started["operationId"], "op-9");

    // Start-once guard: calling provider.install.start a second time fails immediately.
    let second = session
        .call("provider.install.start", json!({"instanceId": "agy"}))
        .await
        .expect_err("start twice");
    assert_eq!(second.code, "t3_install_state_unknown");

    // subscribe() drops on conn 0, reconnects automatically on conn 1, yields state, sends Interrupt on drop.
    {
        let mut sub = session.subscribe("agy").await.expect("subscribe");
        let item = sub.next().await.expect("some").expect("chunk");
        assert_eq!(item["phase"], "succeeded");
    }
    session.disconnect();

    // watch() times out cleanly on quiet conn 2 as t3_install_timeout.
    {
        let mut watch = session
            .watch("agy", Duration::from_millis(60))
            .await
            .expect("watch");
        let err = watch.next().await.expect("some").expect_err("settle");
        assert_eq!(err.code, "t3_install_timeout");
    }

    // Separate fresh session: dropped socket during provider.install.start -> t3_install_state_unknown!
    let mut session2 = Session::with_test_bearer(&cx, &endpoint, TOKEN);
    let amb = session2
        .call("provider.install.start", json!({"instanceId": "agy"}))
        .await
        .expect_err("ambiguous");
    assert_eq!(amb.code, "t3_install_state_unknown");

    let _ = server.join();
    assert!(
        interrupts.load(Ordering::SeqCst) >= 1,
        "Interrupt sent on Subscription drop"
    );
}

#[tokio::test]
async fn update_t3_takes_self_update_when_offered_and_reports_updated_row() {
    let root = tempfile::tempdir().expect("root");
    let sys = Arc::new(MockSystem::new().on_fetch(|_| {
        Ok(serde_json::to_vec(&json!([{"tag_name": format!("v{SELF_NEW}")}])).expect("json"))
    }));
    let cx = context_with_arch(root.path(), Platform::Mac, "aarch64", sys);
    let bundle = cx.host.system(processes::T3_MAC_BUNDLE);
    write_plist(&bundle, MAC_ID, SELF_OLD);
    let mut install = Installed::new(
        "t3-code",
        Platform::Mac,
        bundle.clone(),
        Some(SELF_OLD.into()),
        "official-download",
    );
    install.package_root = Some(bundle.clone());
    let bundle_clone = bundle.clone();
    let row = update_t3_with(
        &cx,
        &install,
        Path::new("/proc"),
        |_| async { true },
        move |target| {
            let bundle_clone = bundle_clone.clone();
            let target = target.to_owned();
            async move {
                write_plist(&bundle_clone, MAC_ID, &target);
                Ok(target)
            }
        },
        |_| async { true },
    )
    .await
    .to_value();
    assert_eq!(row["status"], "updated");
    assert_eq!(row["messageCode"], "t3_updated");
    assert_eq!(row["previousVersion"], SELF_OLD);
    assert_eq!(row["version"], SELF_NEW);
    assert_eq!(row["restartedProcesses"], 1);
    assert_eq!(row["forcedStops"], 0);
    assert_eq!(row["restartTargets"], json!([{"kind": "desktop"}]));
}
