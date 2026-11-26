//! Contract tests: the generic attempt flow, row wire form and diagnostics match
//! `app_update_common.result`, `Diagnostics.export` and `app_updates.attempt_one`.
use super::*;
use base64::Engine as _;
use std::sync::atomic::{AtomicUsize, Ordering};

type Answer = Box<dyn Fn(&CommandSpec) -> Result<CommandOutput, UpdateFailure> + Send + Sync>;

pub(crate) struct FakeSystem {
    pub runs: Mutex<Vec<Vec<OsString>>>,
    pub answer: Answer,
    /// The one remote file `range` serves (empty: every range request fails).
    pub remote: Mutex<Vec<u8>>,
}

use std::ffi::OsString;

impl FakeSystem {
    pub(crate) fn answering(
        answer: impl Fn(&CommandSpec) -> Result<CommandOutput, UpdateFailure> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            runs: Mutex::new(Vec::new()),
            answer: Box::new(answer),
            remote: Mutex::new(Vec::new()),
        })
    }
}

impl System for FakeSystem {
    fn run<'a>(
        &'a self,
        spec: &'a CommandSpec,
        diagnostics: &'a Mutex<Diagnostics>,
    ) -> BoxFuture<'a, Result<CommandOutput, UpdateFailure>> {
        Box::pin(async move {
            self.runs.lock().expect("runs").push(spec.argv.clone());
            let answer = (self.answer)(spec);
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

    fn processes(&self, _platform: Platform) -> BoxFuture<'_, Result<ProcessTable, UpdateFailure>> {
        Box::pin(async { Ok(ProcessTable::default()) })
    }

    fn download<'a>(
        &'a self,
        _: &'a str,
        _: &'a Path,
        _: u64,
        _: Duration,
    ) -> BoxFuture<'a, Result<Downloaded, UpdateFailure>> {
        Box::pin(async { Err(UpdateFailure::failed()) })
    }

    fn fetch<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: u64,
        _: Duration,
    ) -> BoxFuture<'a, Result<Vec<u8>, UpdateFailure>> {
        Box::pin(async { Err(UpdateFailure::failed()) })
    }

    fn fingerprint<'a>(&'a self, _: &'a str, _: Duration) -> BoxFuture<'a, Option<Fingerprint>> {
        Box::pin(async { None })
    }

    fn range<'a>(
        &'a self,
        url: &'a str,
        range: &'a str,
        _: Duration,
    ) -> BoxFuture<'a, Result<shared::RangeReply, UpdateFailure>> {
        Box::pin(async move {
            let body = self.remote.lock().expect("remote").clone();
            if body.is_empty() {
                return Err(UpdateFailure::failed());
            }
            let total = body.len() as u64;
            let spec = range
                .strip_prefix("bytes=")
                .ok_or_else(UpdateFailure::failed)?;
            let (first, last) = match spec.strip_prefix('-') {
                Some(suffix) => (
                    total.saturating_sub(suffix.parse().map_err(|_| UpdateFailure::failed())?),
                    total - 1,
                ),
                None => {
                    let (first, last) = spec.split_once('-').ok_or_else(UpdateFailure::failed)?;
                    (
                        first.parse().map_err(|_| UpdateFailure::failed())?,
                        last.parse::<u64>()
                            .map_err(|_| UpdateFailure::failed())?
                            .min(total - 1),
                    )
                }
            };
            if first > last {
                return Err(UpdateFailure::failed());
            }
            Ok(shared::RangeReply {
                start: first,
                data: body[first as usize..=last as usize].to_vec(),
                total,
                url: url.to_owned(),
            })
        })
    }
}

pub(crate) fn host(root: &Path, platform: Platform) -> Arc<Host> {
    Arc::new(Host {
        platform,
        home: root.join("home"),
        local: root.join("home/AppData/Local"),
        roaming: root.join("home/AppData/Roaming"),
        program_files: root.join("Program Files"),
        path: Vec::new(),
        path_ext: vec![".EXE".into()],
        powershell: None,
        npm_prefix: None,
        arch: "x86_64",
        system_root: Some(root.join("system")),
    })
}

pub(crate) fn context(
    root: &Path,
    platform: Platform,
    system: Arc<dyn System>,
    cancelled: bool,
) -> Context {
    Context::new(
        platform,
        host(root, platform),
        system,
        Arc::new(Shared::default()),
        Instant::now() + Duration::from_secs(60),
        root.join("stage"),
        root.join("state"),
        None,
        move || cancelled,
        |_| {},
    )
}

/// A scripted updater: detection and update answers are fixed per test.
struct Scripted {
    detection: Detection,
    outcome: fn(&Installed) -> Result<AppRow, UpdateFailure>,
    updates: AtomicUsize,
}

impl AppUpdater for Scripted {
    fn app_id(&self) -> &'static str {
        "omp"
    }

    fn detect<'a>(&'a self, _cx: &'a Context) -> BoxFuture<'a, Detection> {
        Box::pin(async { self.detection.clone() })
    }

    fn update<'a>(
        &'a self,
        cx: &'a Context,
        install: &'a Installed,
    ) -> BoxFuture<'a, Result<AppRow, UpdateFailure>> {
        Box::pin(async move {
            self.updates.fetch_add(1, Ordering::SeqCst);
            cx.run(&CommandSpec::new(["/usr/bin/omp", "update"]))
                .await?;
            (self.outcome)(install)
        })
    }
}

fn found(probe: Probe, reason: Option<&'static str>) -> Detection {
    let mut install = Installed::new(
        "omp",
        Platform::Ubuntu,
        PathBuf::from("/x/omp"),
        Some("1.2.3".into()),
        "native",
    );
    install.probe = probe;
    install.probe_reason = reason;
    Detection::Found(Box::new(install))
}

fn scripted(
    detection: Detection,
    outcome: fn(&Installed) -> Result<AppRow, UpdateFailure>,
) -> Scripted {
    Scripted {
        detection,
        outcome,
        updates: AtomicUsize::new(0),
    }
}

fn updated(install: &Installed) -> Result<AppRow, UpdateFailure> {
    Ok(AppRow::of(
        install,
        Status::Updated,
        Some("1.2.3"),
        Some("1.3.0"),
        "updated",
    )
    .attempted(0))
}

fn ok(_: &CommandSpec) -> Result<CommandOutput, UpdateFailure> {
    Ok(CommandOutput::default())
}

async fn attempt_with(
    updater: &Scripted,
    cancelled: bool,
    answer: fn(&CommandSpec) -> Result<CommandOutput, UpdateFailure>,
) -> Value {
    let root = tempfile::tempdir().expect("root");
    let cx = context(
        root.path(),
        Platform::Ubuntu,
        FakeSystem::answering(answer),
        cancelled,
    );
    attempt(updater, &cx).await.to_value()
}

#[tokio::test]
async fn an_absent_app_is_not_installed_and_never_updates() {
    let updater = scripted(Detection::Absent, updated);
    let row = attempt_with(&updater, false, ok).await;
    assert_eq!(row["status"], "not_installed");
    assert_eq!(row["messageCode"], "not_installed");
    assert_eq!(row["manager"], Value::Null);
    assert_eq!(row["diagnostics"], json!({"cause": "none"}));
    assert_eq!(updater.updates.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn probe_failures_are_honest_unknowns() {
    let row = attempt_with(&scripted(found(Probe::Timeout, None), updated), false, ok).await;
    assert_eq!(
        (row["status"].as_str(), row["messageCode"].as_str()),
        (Some("unknown"), Some("check_timeout"))
    );
    assert_eq!(row["manager"], "native");
    assert_eq!(row["diagnostics"]["cause"], "timeout");

    let row = attempt_with(
        &scripted(found(Probe::Failed, Some("no_version_dir")), updated),
        false,
        ok,
    )
    .await;
    assert_eq!(
        (row["status"].as_str(), row["messageCode"].as_str()),
        (Some("unknown"), Some("readiness_unknown"))
    );
    assert_eq!(row["manager"], Value::Null);
    assert_eq!(
        row["diagnostics"],
        json!({"cause": "unknown", "reason": "no_version_dir"})
    );

    let row = attempt_with(
        &scripted(found(Probe::Failed, Some("policy_blocked")), updated),
        false,
        ok,
    )
    .await;
    assert_eq!(
        (row["status"].as_str(), row["messageCode"].as_str()),
        (Some("action_required"), Some("policy_blocked"))
    );
    assert_eq!(
        row["diagnostics"],
        json!({"cause": "policy_blocked", "reason": "execution_policy"})
    );
}

#[tokio::test]
async fn a_cancelled_job_skips_before_detection() {
    let updater = scripted(found(Probe::Ok, None), updated);
    let row = attempt_with(&updater, true, ok).await;
    assert_eq!(
        (row["status"].as_str(), row["messageCode"].as_str()),
        (Some("skipped"), Some("skipped_cancelled"))
    );
    assert_eq!(updater.updates.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_update_row_carries_the_deciding_run() {
    let row = attempt_with(&scripted(found(Probe::Ok, None), updated), false, ok).await;
    assert_eq!(row["status"], "updated");
    assert_eq!(row["previousVersion"], "1.2.3");
    assert_eq!(row["version"], "1.3.0");
    assert_eq!(row["updateAttempted"], true);
    assert_eq!(row["diagnostics"]["cause"], "none");
    assert_eq!(row["diagnostics"]["tool"], "omp");
    assert_eq!(row["diagnostics"]["exitCode"], 0);
    let phases = row["diagnostics"]["phases"].as_object().expect("phases");
    assert_eq!(phases.keys().collect::<Vec<_>>(), ["checking", "updating"]);
}

#[tokio::test]
async fn a_failed_command_is_a_failed_row_with_the_installer_cause() {
    fn refused(_: &CommandSpec) -> Result<CommandOutput, UpdateFailure> {
        Err(UpdateFailure {
            exit_code: Some(3),
            output: "secret output never kept".into(),
            ..UpdateFailure::failed()
        })
    }
    let row = attempt_with(&scripted(found(Probe::Ok, None), updated), false, refused).await;
    assert_eq!(
        (row["status"].as_str(), row["messageCode"].as_str()),
        (Some("failed"), Some("update_failed"))
    );
    assert_eq!(row["previousVersion"], "1.2.3");
    assert_eq!(row["version"], "1.2.3");
    assert_eq!(row["diagnostics"]["cause"], "installer_failed");
    assert_eq!(row["diagnostics"]["exitCode"], 3);
    assert!(!row.to_string().contains("secret"));
}

#[tokio::test]
async fn a_failure_cause_named_by_the_updater_wins() {
    fn signin(_: &Installed) -> Result<AppRow, UpdateFailure> {
        Err(UpdateFailure::new("signin_needed")
            .with_cause(FailureCause::SigninNeeded, Some("muse_login")))
    }
    let row = attempt_with(&scripted(found(Probe::Ok, None), signin), false, ok).await;
    assert_eq!(row["messageCode"], "signin_needed");
    assert_eq!(row["diagnostics"]["cause"], "signin_needed");
    assert_eq!(row["diagnostics"]["reason"], "muse_login");
}

#[test]
fn the_row_wire_form_matches_python_result() {
    let row = AppRow::new("zcode", Platform::Mac, Status::Current).to_value();
    assert_eq!(
        row,
        json!({"appId":"zcode","platform":"mac","status":"current","previousVersion":null,"version":null,
               "manager":null,"messageCode":"current","updateAttempted":false,"restartedProcesses":0})
    );
    let mut row = AppRow::new("codex-desktop", Platform::Windows, Status::Updated);
    row.forced_stops = Some(0);
    row.restart_targets = Some(vec![json!({"kind": "desktop"})]);
    let value = row.to_value();
    assert_eq!(value["forcedStops"], 0);
    assert_eq!(value["restartTargets"], json!([{"kind": "desktop"}]));
    assert!(value.get("heldVersion").is_none());
}

#[test]
fn resources_follow_update_resources() {
    let mut install = Installed::new("codex-cli", Platform::Ubuntu, PathBuf::new(), None, "npm");
    assert_eq!(
        Resources::for_install(&install).0,
        Resources::NPM | Resources::CODEX
    );
    install.app_id = "zcode";
    install.manager = "official-download";
    assert_eq!(Resources::for_install(&install).0, Resources::NPM);
    install.platform = Platform::Mac;
    assert_eq!(Resources::for_install(&install).0, 0);
    install.app_id = "claude-desktop";
    install.manager = "msix";
    assert_eq!(Resources::for_install(&install).0, Resources::MSIX);
}

#[test]
fn causes_and_tools_are_the_fixed_words() {
    assert_eq!(
        common::cause_for(&json!({"status":"failed","messageCode":"quit_first"}), None),
        ("in_use", None)
    );
    assert_eq!(
        common::cause_for(&json!({"status":"updated","messageCode":"updated"}), None),
        ("none", None)
    );
    assert_eq!(
        common::cause_for(
            &json!({"status":"failed","messageCode":"update_failed"}),
            Some(1)
        ),
        ("installer_failed", None)
    );
    assert_eq!(
        common::cause_for(&json!({"status":"failed","messageCode":"Bad Code"}), None),
        ("unknown", Some("unlisted".into()))
    );
    let argv = |text: &str| vec![OsString::from(text)];
    assert_eq!(
        common::tool_id(&argv("C:/x/powershell.exe"), None),
        "powershell"
    );
    assert_eq!(
        common::tool_id(&argv("/usr/bin/python3.12"), None),
        "python"
    );
    assert_eq!(common::tool_id(&argv("/x/mystery"), None), "unlisted");
    assert_eq!(common::tool_id(&argv("/x/y"), Some("npm install")), "npm");
}

#[test]
fn every_inventory_app_has_an_updater() {
    for app in aac_providers::app_inventory() {
        assert_eq!(updater(app).map(AppUpdater::app_id), Some(app));
    }
    assert!(updater("not-an-app").is_none());
}

#[tokio::test]
async fn an_unported_app_reports_readiness_unknown() {
    let root = tempfile::tempdir().expect("root");
    let cx = context(
        root.path(),
        Platform::Ubuntu,
        FakeSystem::answering(ok),
        false,
    );
    let row = attempt(updater("omp").expect("omp"), &cx).await.to_value();
    assert_eq!(
        (row["status"].as_str(), row["messageCode"].as_str()),
        (Some("unknown"), Some("readiness_unknown"))
    );
}

#[test]
fn a_stage_is_private_and_only_replaces_its_own_marker() {
    let root = tempfile::tempdir().expect("root");
    let stage = Stage::open(root.path(), "zcode").expect("stage");
    let folder = stage.path().to_path_buf();
    std::fs::write(folder.join("scratch"), b"x").expect("scratch");
    drop(stage);
    assert!(!folder.exists());
    std::fs::create_dir_all(&folder).expect("foreign folder");
    assert_eq!(
        Stage::open(root.path(), "zcode")
            .err()
            .map(|failure| failure.code),
        Some("readiness_unknown")
    );
    assert!(
        folder.exists(),
        "a folder without the marker is never removed"
    );
}

#[tokio::test]
async fn the_native_runner_records_a_tool_and_exit_code() {
    if cfg!(not(unix)) {
        return;
    }
    let record = Mutex::new(Diagnostics::default());
    let spec = CommandSpec::new(["/bin/sh", "-c", "echo out; echo err >&2; exit 4"]).capture();
    let failure = common::Native
        .run(&spec, &record)
        .await
        .expect_err("exit 4");
    assert_eq!(
        (failure.code, failure.exit_code),
        ("update_failed", Some(4))
    );
    assert_eq!(failure.output.trim(), "err");
    let spec = CommandSpec::new(["/bin/sh", "-c", "printf ok"]).capture();
    assert_eq!(
        common::Native.run(&spec, &record).await.expect("ok").stdout,
        "ok"
    );
    let spec = CommandSpec::new(["/bin/sh", "-c", "sleep 5"]).timeout(Duration::from_millis(200));
    assert_eq!(
        common::Native
            .run(&spec, &record)
            .await
            .expect_err("timeout")
            .code,
        "timeout"
    );
    let runs = record.lock().expect("record").deciding(false).cloned();
    assert_eq!(
        runs.map(|run| (run.tool, run.exit_code)),
        Some(("unlisted", None))
    );
}

/// A stored (uncompressed) ZIP, with optional symlink entries, for the archive checks.
fn write_zip(path: &Path, entries: &[(&str, Option<&str>)]) {
    let mut body = Vec::new();
    let mut directory = Vec::new();
    for (name, target) in entries {
        let data = target.unwrap_or("fixture").as_bytes();
        let crc = {
            let mut crc = 0xffff_ffff_u32;
            for byte in data {
                crc ^= u32::from(*byte);
                for _ in 0..8 {
                    crc = if crc & 1 == 1 {
                        (crc >> 1) ^ 0xedb8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        };
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
        let mode: u32 = if target.is_some() {
            0o120_777
        } else {
            0o100_644
        };
        directory
            .extend_from_slice(&[0x50, 0x4b, 0x01, 0x02, 20, 3, 20, 0, 0, 8, 0, 0, 0, 0, 0, 0]);
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&name_len.to_le_bytes());
        directory.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        directory.extend_from_slice(&(mode << 16).to_le_bytes());
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

#[test]
fn mac_archive_rejects_symlink_ancestors_chains_and_cycles_in_any_order() {
    // app_update_t3_test.test_mac_archive_rejects_symlink_ancestors_chains_and_cycles_in_any_order
    let fixtures: [&[(&str, Option<&str>)]; 6] = [
        &[
            ("a", Some(".")),
            ("a/b", Some("../outside")),
            ("a/b/payload", None),
        ],
        &[("a", Some(".")), ("b", Some("a/../outside"))],
        &[("a", Some("b")), ("b", Some("a"))],
        &[("dir/link", Some("../data")), ("dir/link/payload", None)],
        &[("Alias", Some(".")), ("alias/payload", None)],
        &[("caf\u{e9}", Some(".")), ("cafe\u{301}/payload", None)],
    ];
    let root = tempfile::tempdir().expect("root");
    let package = root.path().join("fixture.zip");
    for entries in fixtures {
        let mut reversed = entries.to_vec();
        reversed.reverse();
        for ordered in [entries.to_vec(), reversed] {
            write_zip(&package, &ordered);
            assert_eq!(
                shared::check_mac_archive(&package)
                    .err()
                    .map(|failure| failure.code),
                Some("signature_failed"),
                "{ordered:?}"
            );
        }
    }
    write_zip(
        &package,
        &[
            ("T3.app/Contents/Frameworks/A.framework/Versions/A/A", None),
            (
                "T3.app/Contents/Frameworks/A.framework/A",
                Some("Versions/Current/A"),
            ),
            (
                "T3.app/Contents/Frameworks/A.framework/Versions/Current",
                Some("A"),
            ),
        ],
    );
    assert!(
        shared::check_mac_archive(&package).is_ok(),
        "electron framework aliases pass"
    );
}

fn process(pid: u32, ppid: u32, exe: &str, args: &[&str]) -> Process {
    Process {
        pid,
        ppid,
        uid: 1,
        exe: exe.into(),
        identity: pid.to_string(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        ..Process::default()
    }
}

#[test]
fn family_does_not_select_generic_node_or_other_native_binaries() {
    // app_updates_test.test_family_does_not_select_generic_node_or_other_native_binaries
    let root = tempfile::tempdir().expect("root");
    let host = host(root.path(), Platform::Ubuntu);
    let install = Installed::new(
        "muse-code",
        Platform::Ubuntu,
        PathBuf::from("/fixture/bin/muse"),
        None,
        "native",
    );
    let rows = [
        "/fixture/bin/muse-bin-1.4.2",
        "/fixture/bin/agy",
        "/usr/bin/node",
        "/elsewhere/muse-bin-1.4.2",
    ]
    .iter()
    .zip(1..)
    .map(|(exe, pid)| process(pid, 1, exe, &[]))
    .collect();
    let found = processes::family(&host, &install, &ProcessTable::new(rows), true);
    assert_eq!(found.iter().map(|item| item.pid).collect::<Vec<_>>(), [1]);
}

#[test]
fn npm_selection_requires_exact_owned_package_argument() {
    let root = tempfile::tempdir().expect("root");
    let host = host(root.path(), Platform::Ubuntu);
    let mut install = Installed::new(
        "codex-cli",
        Platform::Ubuntu,
        PathBuf::from("/fixture/codex.cmd"),
        None,
        "npm",
    );
    install.package_root = Some(PathBuf::from("/fixture/node_modules/@openai/codex"));
    let rows = vec![
        process(
            1,
            0,
            "/usr/bin/node",
            &["node", "/fixture/node_modules/@openai/codex/bin/codex.js"],
        ),
        process(2, 0, "/usr/bin/node", &["node", "/other/server.js"]),
    ];
    let found = processes::family(&host, &install, &ProcessTable::new(rows), true);
    assert_eq!(found.iter().map(|item| item.pid).collect::<Vec<_>>(), [1]);
}

#[test]
fn prompt_arguments_are_not_replayed_and_restart_env_is_idle() {
    let value = "01998a55-88aa-7666-a999-010101010101";
    let args = |items: &[&str]| {
        items
            .iter()
            .map(|item| (*item).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        terminal::resume_arguments(
            "claude-code",
            &args(&["claude", "--resume", value, "--print", "PROMPT"])
        ),
        args(&["--resume", value])
    );
    assert!(
        terminal::resume_arguments("codex-cli", &args(&["codex", "exec", "PROMPT"])).is_empty()
    );
    assert!(
        terminal::resume_arguments(
            "omp",
            &args(&["omp", "--resume", "/private/file", "PROMPT"])
        )
        .is_empty()
    );
    let mut context = process(1, 0, "/fixture/agy", &[]);
    context.env = [
        ("HOME", "/fixture"),
        ("TEST_SECRET", "PRIVATE"),
        ("MUSE_LAUNCHER_INSTALL", "1"),
        ("MUSE_UPGRADE_MODE", "1"),
    ]
    .iter()
    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
    .collect();
    let values = terminal::restart_environment(&context);
    let get = |key: &str| {
        values
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(get("TEST_SECRET"), Some("PRIVATE"));
    assert_eq!(get("MUSE_LAUNCHER_INSTALL"), None);
    assert_eq!(get("CODEX_NON_INTERACTIVE"), None);
    assert_eq!(get("DISABLE_AUTOUPDATER"), Some("1"));
}

#[test]
fn tool_ids_are_fixed_words() {
    // app_update_fix_round2_test.test_h1_tool_ids_are_fixed_words
    let argv = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(
        common::tool_id(&argv(&["/usr/bin/systemctl", "--user"]), None),
        "systemctl"
    );
    assert_eq!(
        common::tool_id(
            &argv(&["C:\\Program Files\\nodejs\\node.exe", "cli.js"]),
            None
        ),
        "node"
    );
    assert_eq!(
        common::tool_id(&argv(&["/usr/bin/python3.14"]), None),
        "python"
    );
    assert_eq!(
        common::tool_id(&argv(&["x"]), Some("cursor-agent update (Mac GUI)")),
        "cursor-agent"
    );
    assert_eq!(
        common::tool_id(&argv(&["/home/someone/--pass=zebraWitness"]), None),
        "unlisted"
    );
    assert_eq!(
        common::tool_id(&argv(&["x"]), Some("fixture updater")),
        "unlisted"
    );
    assert_eq!(common::tool_id(&[], None), "unlisted");
    assert!(
        common::TOOLS
            .iter()
            .all(|tool| common::REASON.is_match(tool))
    );
}

#[test]
fn desktop_profiles_reproduce_only_a_given_data_folder() {
    let mut context = process(
        1,
        0,
        "/opt/app",
        &[
            "/opt/app",
            "--user-data-dir=profiles/a",
            "--",
            "--user-data-dir=/x",
        ],
    );
    context.cwd = Some("/home/me".into());
    assert_eq!(
        processes::desktop_profile(&context, Platform::Ubuntu),
        (vec!["--user-data-dir=/home/me/profiles/a".to_owned()], true)
    );
    context.cwd = None;
    assert_eq!(
        processes::desktop_profile(&context, Platform::Ubuntu),
        (Vec::new(), false)
    );
    let context = process(1, 0, "C:/app.exe", &["app", "/User-Data-Dir=D:named"]);
    assert_eq!(
        processes::desktop_profile(&context, Platform::Windows),
        (Vec::new(), false)
    );
    let context = process(1, 0, "/opt/app", &["/opt/app", "--user-data-dir"]);
    assert_eq!(
        processes::desktop_profile(&context, Platform::Ubuntu),
        (Vec::new(), true)
    );
}

#[test]
fn ancestry_stops_at_a_reused_parent_pid() {
    let mut table = ProcessTable::new(vec![process(30, 20, "/x/codex", &[])]);
    table.lineage = Some(std::collections::HashMap::from([
        (30, (20, Some(300))),
        (20, (10, Some(200))),
        (10, (1, Some(400))),
    ]));
    assert_eq!(processes::ancestors(&table, &table.items[0].clone()), [20]);
}

#[test]
fn manifest_assets_bind_digest_size_and_version() {
    let digest = base64::engine::general_purpose::STANDARD.encode([7_u8; 64]);
    let text = format!(
        "version: 1.2.3\nfiles:\n  - url: ZCode-1.2.3.zip\n    sha512: {digest}\n    size: 1234\n  - url: other.zip\n    sha512: {digest}\npath: ZCode-1.2.3.zip\n"
    );
    assert_eq!(
        shared::manifest_asset(&text, "1.2.3", "ZCode-1.2.3.zip").expect("asset"),
        (vec![7; 64], Some(1234))
    );
    assert_eq!(
        shared::manifest_asset(&text, "1.2.3", "other.zip")
            .expect("asset")
            .1,
        None
    );
    assert_eq!(
        shared::manifest_asset(&text, "1.2.4", "ZCode-1.2.3.zip")
            .err()
            .map(|f| f.code),
        Some("signature_failed")
    );
    assert_eq!(
        shared::manifest_asset(&text, "1.2.3", "missing.zip")
            .err()
            .map(|f| f.code),
        Some("signature_failed")
    );
}

#[test]
fn npm_keys_order_prereleases_first() {
    let key = |value| shared::npm_version_key(value).expect("semver");
    assert!(key("1.2.3-beta.2") < key("1.2.3"));
    assert!(key("1.2.3-beta.2") < key("1.2.3-beta.10"));
    assert!(key("v1.10.0") > key("1.9.9"));
    assert_eq!(
        shared::npm_version_key("latest").err().map(|f| f.code),
        Some("version_unknown")
    );
}

#[test]
fn systemd_units_never_put_values_on_argv() {
    let root = tempfile::tempdir().expect("root");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let runner = bin.join("systemd-run");
    std::fs::write(&runner, "").expect("runner");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).expect("mode");
    }
    let mut host = Arc::into_inner(host(root.path(), Platform::Ubuntu)).expect("host");
    host.path = vec![bin];
    let env = vec![
        ("HOME".to_owned(), "/home/me".to_owned()),
        ("LD_PRELOAD".to_owned(), "/x.so".to_owned()),
        ("BASH_FUNC_x%%".to_owned(), "() {}".to_owned()),
        ("CCS_SESSION_SECRET".to_owned(), "hidden".to_owned()),
    ];
    let argv = shared::systemd_user_service_argv(
        &host,
        "u",
        Some("/w"),
        &env,
        &["/opt/a$b".into()],
        "forking",
        &[],
    )
    .expect("argv");
    let joined = argv.join(" ");
    assert!(joined.contains("--setenv=HOME --setenv=AAC_LOADER_LD_PRELOAD --"));
    assert!(
        !joined.contains("/home/me") && !joined.contains("hidden") && !joined.contains("BASH_FUNC")
    );
    assert!(joined.contains("--service-type=forking"));
    assert_eq!(argv.last().map(String::as_str), Some("/opt/a$$b"));
    assert!(argv.contains(&"/bin/sh".to_owned()));
}

#[test]
fn shell_words_quote_like_shlex() {
    assert_eq!(terminal::shell_quote(""), "''");
    assert_eq!(terminal::shell_quote("/usr/bin/codex"), "/usr/bin/codex");
    assert_eq!(terminal::shell_quote("it's here"), "'it'\"'\"'s here'");
    assert_eq!(
        terminal::shell_join(&["/a b/claude".into(), "--resume".into()]),
        "'/a b/claude' --resume"
    );
}

#[test]
fn the_terminal_child_refuses_anything_but_its_own_endpoint() {
    let args = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(terminal::terminal_child(&args(&[])), 1);
    assert_eq!(
        terminal::terminal_child(&args(&["--socket", "/tmp/terminal-0.sock"])),
        1
    );
    assert_eq!(terminal::terminal_child(&args(&["--exec", "/bin/sh"])), 1);
}

#[test]
fn display_paths_hide_homes_users_and_credential_flags() {
    let root = tempfile::tempdir().expect("root");
    let mut host = Arc::into_inner(host(root.path(), Platform::Ubuntu)).expect("host");
    host.home = PathBuf::from("/home/me");
    assert_eq!(
        shared::display_path(&host, Path::new("/home/me/.local/bin/agy")),
        "~/.local/bin/agy"
    );
    assert_eq!(
        shared::display_path(&host, Path::new("/home/meadow/x")),
        "/home/<user>/x"
    );
    assert_eq!(
        shared::display_path(&host, Path::new("/Users/someone else/x")),
        "/Users/<user>/x"
    );
    assert_eq!(
        shared::display_path(&host, Path::new("C:\\Users\\Other\\npm")),
        "C:\\Users\\<user>\\npm"
    );
    assert_eq!(
        shared::display_path(&host, Path::new("/opt/x --token=abc")),
        shared::CREDENTIAL_REST
    );
    assert_eq!(
        shared::display_path(&host, Path::new("/opt/x -p 'secret'")),
        shared::CREDENTIAL_REST
    );
    assert_eq!(
        shared::display_path(&host, Path::new("/opt/my-p='x'")),
        "/opt/my-p='x'"
    );
    let long = format!("/opt/{}", "a".repeat(300));
    assert_eq!(
        shared::display_path(&host, Path::new(&long))
            .chars()
            .count(),
        160
    );
}

#[tokio::test]
async fn a_remote_zip_member_is_read_through_ranges() {
    let root = tempfile::tempdir().expect("root");
    let package = root.path().join("remote.msix");
    let mut entries: Vec<(String, Option<String>)> = (0..40)
        .map(|index| (format!("pad/{index}"), None))
        .collect();
    entries.push(("AppxManifest.xml".into(), None));
    let borrowed: Vec<(&str, Option<&str>)> = entries
        .iter()
        .map(|(name, target)| (name.as_str(), target.as_deref()))
        .collect();
    write_zip(&package, &borrowed);
    let system = FakeSystem::answering(ok);
    *system.remote.lock().expect("remote") = std::fs::read(&package).expect("zip");
    let cx = context(root.path(), Platform::Windows, system.clone(), false);
    let url = "https://example.invalid/app.msix";
    let member = shared::remote_zip_member(&cx, url, "AppxManifest.xml", Duration::from_secs(5))
        .await
        .expect("read");
    assert_eq!(member.as_deref(), Some(&b"fixture"[..]));
    assert_eq!(
        shared::remote_zip_member(&cx, url, "missing.xml", Duration::from_secs(5))
            .await
            .expect("read"),
        None
    );
    system.remote.lock().expect("remote").clear();
    assert!(
        shared::remote_zip_member(&cx, url, "AppxManifest.xml", Duration::from_secs(5))
            .await
            .is_err()
    );
}

#[test]
fn an_execution_lock_is_exclusive() {
    let root = tempfile::tempdir().expect("root");
    let held = shared::execution_lock(root.path(), "helper-update.lock").expect("first");
    assert_eq!(
        shared::execution_lock(root.path(), "helper-update.lock")
            .err()
            .map(|f| f.code),
        Some("busy")
    );
    drop(held);
    assert!(shared::execution_lock(root.path(), "helper-update.lock").is_ok());
}
