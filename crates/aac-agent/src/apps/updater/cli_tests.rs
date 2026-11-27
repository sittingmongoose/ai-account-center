#![cfg(test)]
//! Ports of `tests/unit/app-updates/app_updates_test.py` for the CLI updaters:
//! managed installs and strays, Claude's channel pointers, Muse's quota words,
//! readiness and the native update flow. Every path lives under a temporary root.
use super::*;
use crate::apps::updater::{
    Downloaded, Fingerprint, Shared, System,
    common::{CommandOutput, Diagnostics},
};
use std::{
    ffi::OsString,
    sync::{Arc, Mutex},
};

type RunAnswer = Box<dyn Fn(&CommandSpec) -> Result<CommandOutput, UpdateFailure> + Send + Sync>;
type FetchAnswer = Box<dyn Fn(&str) -> Result<Vec<u8>, UpdateFailure> + Send + Sync>;

struct Fake {
    runs: Mutex<Vec<Vec<OsString>>>,
    run: RunAnswer,
    fetch: FetchAnswer,
    table: ProcessTable,
}

impl Fake {
    fn new(
        run: impl Fn(&CommandSpec) -> Result<CommandOutput, UpdateFailure> + Send + Sync + 'static,
    ) -> Self {
        Self {
            runs: Mutex::new(Vec::new()),
            run: Box::new(run),
            fetch: Box::new(|_| Err(UpdateFailure::failed())),
            table: ProcessTable::default(),
        }
    }

    fn fetching(
        mut self,
        fetch: impl Fn(&str) -> Result<Vec<u8>, UpdateFailure> + Send + Sync + 'static,
    ) -> Self {
        self.fetch = Box::new(fetch);
        self
    }

    fn ran(&self) -> Vec<Vec<String>> {
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

impl System for Fake {
    fn run<'a>(
        &'a self,
        spec: &'a CommandSpec,
        diagnostics: &'a Mutex<Diagnostics>,
    ) -> BoxFuture<'a, Result<CommandOutput, UpdateFailure>> {
        Box::pin(async move {
            self.runs.lock().expect("runs").push(spec.argv.clone());
            let answer = (self.run)(spec);
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

    fn processes(&self, _: Platform) -> BoxFuture<'_, Result<ProcessTable, UpdateFailure>> {
        Box::pin(async { Ok(self.table.clone()) })
    }

    fn download<'a>(
        &'a self,
        _: &'a str,
        destination: &'a Path,
        _: u64,
        _: Duration,
    ) -> BoxFuture<'a, Result<Downloaded, UpdateFailure>> {
        Box::pin(async move {
            std::fs::write(destination, b"#!/bin/sh\n").map_err(|_| UpdateFailure::failed())?;
            Ok(Downloaded {
                bytes: 10,
                url: "https://dev.meta.ai/install.sh".into(),
            })
        })
    }

    fn fetch<'a>(
        &'a self,
        url: &'a str,
        _: &'a str,
        _: u64,
        _: Duration,
    ) -> BoxFuture<'a, Result<Vec<u8>, UpdateFailure>> {
        Box::pin(async move { (self.fetch)(url) })
    }

    fn fingerprint<'a>(&'a self, _: &'a str, _: Duration) -> BoxFuture<'a, Option<Fingerprint>> {
        Box::pin(async { None })
    }
}

fn host(root: &Path, platform: Platform, path: &[&str]) -> Arc<Host> {
    Arc::new(Host {
        platform,
        home: root.join("home"),
        local: root.join("home/AppData/Local"),
        roaming: root.join("home/AppData/Roaming"),
        program_files: root.join("Program Files"),
        path: path.iter().map(|folder| root.join(folder)).collect(),
        path_ext: vec![".EXE".into(), ".CMD".into()],
        powershell: None,
        npm_prefix: None,
        arch: "x86_64",
        system_root: Some(root.join("system")),
    })
}

fn context(root: &Path, host: Arc<Host>, system: Arc<dyn System>) -> Context {
    Context::new(
        host.platform,
        host,
        system,
        Arc::new(Shared::default()),
        tokio::time::Instant::now() + Duration::from_secs(600),
        root.join("stage"),
        root.join("state"),
        None,
        || false,
        |_| {},
    )
}

fn program(path: &Path) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
    std::fs::write(path, b"#!/bin/sh\n").expect("program");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("mode");
    }
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) {
    std::fs::create_dir_all(at.parent().expect("parent")).expect("folder");
    std::os::unix::fs::symlink(target, at).expect("link");
}

#[cfg_attr(not(unix), allow(dead_code))]
fn version_of(
    spec: &CommandSpec,
    answers: &[(&str, &str)],
) -> Result<CommandOutput, UpdateFailure> {
    let program = spec.argv[0].to_string_lossy();
    for (suffix, version) in answers {
        if program.ends_with(suffix) {
            return Ok(CommandOutput {
                stdout: format!("{version}\n"),
            });
        }
    }
    Err(UpdateFailure::failed())
}

fn found(detection: Detection) -> Installed {
    match detection {
        Detection::Found(install) => *install,
        Detection::Absent => panic!("expected an installation"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn managed_codex_wins_over_an_earlier_path_copy_and_names_the_stray() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    let release = root.join("home/.codex/packages/standalone/releases/0.60.0/bin/codex");
    program(&release);
    link(&release, &root.join("home/.local/bin/codex"));
    program(&root.join("system/usr/local/bin/codex"));
    let host = host(
        root,
        Platform::Ubuntu,
        &["system/usr/local/bin", "home/.local/bin"],
    );
    let fake = Arc::new(Fake::new(|spec| {
        version_of(
            spec,
            &[
                ("releases/0.60.0/bin/codex", "codex-cli 0.60.0"),
                ("home/.local/bin/codex", "codex-cli 0.60.0"),
                ("usr/local/bin/codex", "codex-cli 0.50.0"),
            ],
        )
    }));
    let cx = context(root, host, fake);
    let install = found(CODEX.detect(&cx).await);
    assert_eq!(install.path, root.join("home/.local/bin/codex"));
    assert_eq!(install.version.as_deref(), Some("0.60.0"));
    assert_eq!(install.manager, "native");
    assert_eq!(
        install.package_root.as_deref(),
        Some(
            root.join("home/.codex/packages/standalone/releases")
                .as_path()
        )
    );
    assert_eq!(
        install.strays,
        vec![json!({"location": "usr-local", "version": "0.50.0", "shadows": true})]
    );
    // An older shadowing copy is the one that runs: never "current".
    assert_eq!(current_block(&install), Some("older"));
    let row = blocked_current(&install, Some("0.60.0"), true, 0).expect("blocked");
    assert_eq!(row.status, Status::ActionRequired);
    assert_eq!(row.message_code(), "older_copy_first");
}

#[cfg(unix)]
#[tokio::test]
async fn a_stray_later_on_path_does_not_shadow_and_an_unreadable_one_blocks_current() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    let release = root.join("home/.local/share/claude/versions/2.1.0");
    program(&release);
    link(&release, &root.join("home/.local/bin/claude"));
    program(&root.join("home/.bun/bin/claude"));
    let host = host(
        root,
        Platform::Ubuntu,
        &["home/.local/bin", "home/.bun/bin"],
    );
    let fake = Arc::new(Fake::new(|spec| {
        version_of(
            spec,
            &[
                ("versions/2.1.0", "2.1.0 (Claude Code)"),
                ("local/bin/claude", "2.1.0 (Claude Code)"),
                ("bun/bin/claude", "2.0.9"),
            ],
        )
    }));
    let cx = context(root, host.clone(), fake);
    let install = found(CLAUDE.detect(&cx).await);
    assert_eq!(
        install.strays,
        vec![json!({"location": "bun", "version": "2.0.9", "shadows": false})]
    );
    assert_eq!(current_block(&install), None);
    let mut shadowing = install.clone();
    shadowing.strays = vec![json!({"location": "other", "version": null, "shadows": true})];
    assert_eq!(current_block(&shadowing), Some("unknown"));
}

#[tokio::test]
async fn the_agy_launcher_shim_is_never_the_ubuntu_install() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    program(&root.join("home").join(MANAGED_AGY));
    let host = host(
        root,
        Platform::Ubuntu,
        &["home/.local/share/ai-account-center/antigravity-runtime/bin"],
    );
    let cx = context(
        root,
        host,
        Arc::new(Fake::new(|_| Err(UpdateFailure::failed()))),
    );
    assert!(matches!(
        ANTIGRAVITY_CLI.detect(&cx).await,
        Detection::Absent
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn a_global_npm_codex_under_a_posix_prefix_is_npm_managed() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    let package = root.join("prefix/lib/node_modules/@openai/codex");
    program(&package.join("bin/codex.js"));
    std::fs::write(
        package.join("package.json"),
        br#"{"name":"@openai/codex","version":"0.61.0"}"#,
    )
    .expect("package");
    link(
        &package.join("bin/codex.js"),
        &root.join("prefix/bin/codex"),
    );
    let host = host(root, Platform::Mac, &["prefix/bin"]);
    let fake = Arc::new(Fake::new(|spec| {
        version_of(spec, &[("bin/codex", "codex-cli 0.61.0")])
    }));
    let cx = context(root, host, fake);
    let install = found(CODEX.detect(&cx).await);
    assert_eq!(install.manager, "npm");
    assert_eq!(install.package_root.as_deref(), Some(package.as_path()));
    assert_eq!(npm_prefix(&install), root.join("prefix"));
}

#[tokio::test]
async fn windows_muse_outside_its_folder_is_unsupported_and_readiness_refuses_it() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    program(&root.join("home/AppData/Local/Programs/muse/muse.exe"));
    let host = host(root, Platform::Windows, &[]);
    let fake = Arc::new(Fake::new(|_| Err(UpdateFailure::failed())));
    let cx = context(root, host, fake.clone());
    let install = found(MUSE.detect(&cx).await);
    assert_eq!(install.manager, "unsupported");
    assert_eq!(
        check_readiness(&cx, &install).await,
        Err((Status::Failed, "unsupported"))
    );
    assert!(
        fake.ran().is_empty(),
        "nothing runs for an unsupported copy"
    );
}

#[tokio::test]
async fn windows_muse_cmd_with_its_launcher_is_native_and_probed_through_powershell() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    let folder = root.join("home/AppData/Local/Programs/muse");
    program(&folder.join("muse.cmd"));
    program(&folder.join(".muse-launcher.ps1"));
    let host = host(root, Platform::Windows, &[]);
    let fake = Arc::new(Fake::new(|spec| {
        assert!(spec.argv.iter().any(|word| word == "--version"));
        Ok(CommandOutput {
            stdout: "Muse Code (1.4.2)".into(),
        })
    }));
    let cx = context(root, host, fake.clone());
    let install = found(MUSE.detect(&cx).await);
    assert_eq!(install.manager, "native");
    assert_eq!(install.version.as_deref(), Some("1.4.2"));
    assert_eq!(install.path, folder.join("muse.cmd"));
    let ran = fake.ran();
    assert!(
        ran[0]
            .iter()
            .any(|word| word.ends_with(".muse-launcher.ps1"))
    );
    // Never stopped or restarted on Windows: readiness has nothing to judge.
    assert_eq!(check_readiness(&cx, &install).await, Ok(()));
}

#[tokio::test]
async fn windows_cmd_codex_reads_its_package_and_other_cmd_shims_are_unsupported() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    let npm = root.join("home/AppData/Roaming/npm");
    program(&npm.join("codex.cmd"));
    program(&npm.join("omp.cmd"));
    std::fs::create_dir_all(npm.join("node_modules/@openai/codex")).expect("package");
    std::fs::write(
        npm.join("node_modules/@openai/codex/package.json"),
        br#"{"name":"@openai/codex","version":"0.62.1"}"#,
    )
    .expect("package");
    let host = host(root, Platform::Windows, &[]);
    let cx = context(
        root,
        host,
        Arc::new(Fake::new(|_| Err(UpdateFailure::failed()))),
    );
    let codex = found(CODEX.detect(&cx).await);
    assert_eq!(
        (codex.manager, codex.version.as_deref()),
        ("npm", Some("0.62.1"))
    );
    assert_eq!(codex.probe, Probe::Ok);
    assert_eq!(found(OMP.detect(&cx).await).manager, "unsupported");
}

#[tokio::test]
async fn a_version_probe_timeout_reports_check_timeout() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    program(&root.join("home/.local/bin/omp"));
    let host = host(root, Platform::Ubuntu, &[]);
    let cx = context(
        root,
        host,
        Arc::new(Fake::new(|_| Err(UpdateFailure::new("timeout")))),
    );
    let install = found(OMP.detect(&cx).await);
    assert_eq!(install.probe, Probe::Timeout);
}

fn claude(root: &Path, version: &str) -> Installed {
    Installed::new(
        "claude-code",
        Platform::Ubuntu,
        root.join("home/.local/bin/claude"),
        Some(version.into()),
        "native",
    )
}

fn pointers(latest: &'static str, stable: &'static str) -> Fake {
    Fake::new(|_| Err(UpdateFailure::failed())).fetching(move |url| {
        Ok(if url.ends_with("/latest") {
            latest
        } else {
            stable
        }
        .as_bytes()
        .to_vec())
    })
}

#[tokio::test]
async fn claude_is_current_only_at_or_above_the_newer_of_both_pointers() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    let host = host(root, Platform::Ubuntu, &[]);
    let cx = context(root, host.clone(), Arc::new(pointers("2.1.3\n", "2.1.1")));
    assert_eq!(latest_claude_version(&cx).await.as_deref(), Some("2.1.3"));
    let row = CLAUDE
        .current(&cx, &claude(root, "2.1.3"))
        .await
        .expect("current");
    assert_eq!(
        row.to_value(),
        json!({"appId":"claude-code","platform":"ubuntu","status":"current","previousVersion":"2.1.3",
               "version":"2.1.3","manager":"native","messageCode":"current","updateAttempted":false,
               "restartedProcesses":0})
    );
    assert!(CLAUDE.current(&cx, &claude(root, "2.1.2")).await.is_none());
    // An unusual pointer (or one missing) leaves the usual checks to decide.
    let cx = context(
        root,
        host.clone(),
        Arc::new(pointers("2.1.3-beta", "2.1.1")),
    );
    assert!(CLAUDE.current(&cx, &claude(root, "2.1.3")).await.is_none());
    // An npm or unusual install never reads the pointers.
    let fetched = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = fetched.clone();
    let cx = context(
        root,
        host.clone(),
        Arc::new(
            Fake::new(|_| Err(UpdateFailure::failed())).fetching(move |_| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(b"9.9.9".to_vec())
            }),
        ),
    );
    let mut npm = claude(root, "2.1.3");
    npm.manager = "npm";
    assert!(CLAUDE.current(&cx, &npm).await.is_none());
    assert!(CLAUDE.current(&cx, &claude(root, "2.1")).await.is_none());
    assert_eq!(fetched.load(std::sync::atomic::Ordering::SeqCst), 0);
    // A pending restart marker is never current.
    let cx = context(root, host, Arc::new(pointers("2.1.3", "2.1.3")));
    let marker = root.join("home/.ccs/app-updates/claude-code-pending-restart.json");
    std::fs::create_dir_all(marker.parent().expect("parent")).expect("folder");
    std::fs::write(&marker, b"{}").expect("marker");
    assert!(CLAUDE.current(&cx, &claude(root, "2.1.3")).await.is_none());
}

#[test]
fn muse_quota_words_are_muses_own_and_the_reset_is_verbatim() {
    assert_eq!(
        muse_quota_reset(
            "Error: Subscription quota exhausted. Usage window resets at 4:00 PM PST\nmore"
        ),
        (true, Some("4:00 PM PST".into()))
    );
    assert_eq!(
        muse_quota_reset("subscription quota exhausted"),
        (true, None)
    );
    assert_eq!(
        muse_quota_reset(&format!(
            "subscription quota exhausted; usage window resets at {}",
            "x".repeat(65)
        )),
        (true, None)
    );
    assert_eq!(
        muse_quota_reset("HTTP 429 Too Many Requests"),
        (false, None)
    );
}

#[test]
fn muse_update_busy_needs_a_live_holder() {
    let root = tempfile::tempdir().expect("root");
    let lock = root.path().join(".muse-update-lock");
    assert!(!muse_update_busy(root.path()));
    std::fs::create_dir_all(&lock).expect("lock");
    std::fs::write(lock.join("pid"), b"garbage").expect("pid");
    assert!(!muse_update_busy(root.path()));
    std::fs::write(lock.join("pid"), b"0123").expect("pid");
    assert!(!muse_update_busy(root.path()));
    std::fs::write(lock.join("pid"), std::process::id().to_string()).expect("pid");
    assert!(muse_update_busy(root.path()));
}

#[cfg(unix)]
#[tokio::test]
async fn a_native_update_runs_the_cli_updater_and_reports_the_new_version() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    let path = root.join("home/.local/bin/omp");
    program(&path);
    let host = host(root, Platform::Ubuntu, &[]);
    let updated = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = updated.clone();
    let fake = Arc::new(Fake::new(move |spec| {
        if spec.argv.get(1).is_some_and(|word| word == "update") {
            seen.store(true, std::sync::atomic::Ordering::SeqCst);
            return Ok(CommandOutput::default());
        }
        let version = if seen.load(std::sync::atomic::Ordering::SeqCst) {
            "omp 1.3.0"
        } else {
            "omp 1.2.0"
        };
        Ok(CommandOutput {
            stdout: version.into(),
        })
    }));
    let cx = context(root, host, fake.clone());
    let row = super::super::attempt(&OMP, &cx).await;
    assert_eq!(row.status, Status::Updated, "{:?}", row);
    assert_eq!(row.previous_version.as_deref(), Some("1.2.0"));
    assert_eq!(row.version.as_deref(), Some("1.3.0"));
    assert!(row.update_attempted);
    assert_eq!(row.restart_targets, Some(Vec::new()));
    assert_eq!(row.forced_stops, Some(0));
    assert_eq!(
        row.diagnostics.as_ref().expect("diagnostics")["tool"],
        "omp"
    );
    let ran = fake.ran();
    assert!(
        ran.iter()
            .any(|argv| argv == &[path.to_string_lossy().into_owned(), "update".to_owned()])
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_updater_is_failed_with_both_versions_the_installed_one() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    program(&root.join("home/.local/bin/agy"));
    let host = host(root, Platform::Ubuntu, &[]);
    let fake = Arc::new(Fake::new(|spec| {
        if spec.argv.get(1).is_some_and(|word| word == "update") {
            return Err(UpdateFailure {
                exit_code: Some(1),
                ..UpdateFailure::failed()
            });
        }
        Ok(CommandOutput {
            stdout: "1.0.4".into(),
        })
    }));
    let cx = context(root, host, fake);
    let row = super::super::attempt(&ANTIGRAVITY_CLI, &cx).await;
    assert_eq!(
        (
            row.status,
            row.message_code(),
            row.version.as_deref(),
            row.update_attempted
        ),
        (Status::Failed, "update_failed", Some("1.0.4"), true)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn the_same_version_after_the_updater_is_current_and_a_stale_marker_goes() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    program(&root.join("home/.local/bin/omp"));
    let marker = root.join("home/.ccs/app-updates/omp-pending-restart.json");
    std::fs::create_dir_all(marker.parent().expect("parent")).expect("folder");
    std::fs::write(&marker, br#"{"version":"1.2.0"}"#).expect("marker");
    let host = host(root, Platform::Ubuntu, &[]);
    let fake = Arc::new(Fake::new(|_| {
        Ok(CommandOutput {
            stdout: "1.2.0".into(),
        })
    }));
    let cx = context(root, host, fake);
    let row = super::super::attempt(&OMP, &cx).await;
    assert_eq!(
        (row.status, row.message_code()),
        (Status::Current, "current")
    );
    assert!(row.update_attempted);
    assert!(!marker.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn muse_outside_its_fixed_launcher_is_unsupported_and_never_downloads() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    program(&root.join("home/.bun/bin/muse"));
    let host = host(root, Platform::Ubuntu, &[]);
    let fake = Arc::new(Fake::new(|_| {
        Ok(CommandOutput {
            stdout: "1.0.0".into(),
        })
    }));
    let cx = context(root, host, fake);
    let row = super::super::attempt(&MUSE, &cx).await;
    assert_eq!(
        (row.status, row.message_code()),
        (Status::Failed, "unsupported")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn muse_runs_metas_installer_under_bash_with_upgrade_mode() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path();
    let muse = root.join("home/.local/bin/muse");
    program(&muse);
    program(&root.join("system/bin/bash"));
    let host = host(root, Platform::Ubuntu, &[]);
    let installed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = installed.clone();
    let fake = Arc::new(Fake::new(move |spec| {
        let first = spec.argv[0].to_string_lossy().into_owned();
        if first.ends_with("bin/bash") {
            assert!(spec.argv[1].to_string_lossy().ends_with("muse-install.sh"));
            assert!(
                spec.env
                    .iter()
                    .any(|(key, value)| key == "MUSE_UPGRADE_MODE" && value == "1")
            );
            seen.store(true, std::sync::atomic::Ordering::SeqCst);
            return Ok(CommandOutput::default());
        }
        if spec.argv.get(1).is_some_and(|word| word == "exec") {
            return Err(UpdateFailure {
                output: "Subscription quota exhausted. Usage window resets at 5 PM".into(),
                ..UpdateFailure::failed()
            });
        }
        let version = if seen.load(std::sync::atomic::Ordering::SeqCst) {
            "2.0.0"
        } else {
            "1.9.0"
        };
        Ok(CommandOutput {
            stdout: version.into(),
        })
    }));
    let cx = context(root, host, fake);
    let row = super::super::attempt(&MUSE, &cx).await;
    // The binary changed and restarted; only the warm met a used-up quota: still updated.
    assert_eq!(
        (row.status, row.message_code(), row.version.as_deref()),
        (Status::Updated, "muse_quota_exhausted", Some("2.0.0"))
    );
    // The reset Muse printed reaches the wire (it was dropped before W6).
    assert_eq!(row.quota_reset.as_deref(), Some("5 PM"));
    assert_eq!(row.to_value()["quotaReset"], "5 PM");
    assert_eq!(
        super::super::runner::sanitize(row.to_value())["quotaReset"],
        "5 PM"
    );
}

#[test]
fn the_platform_floor_clamps_install_timeouts_like_python() {
    let root = tempfile::tempdir().expect("root");
    let host = host(root.path(), Platform::Ubuntu, &[]);
    let mut cx = context(
        root.path(),
        host,
        Arc::new(Fake::new(|_| Err(UpdateFailure::failed()))),
    );
    cx.deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    assert_eq!(clamp30(&cx, 600), Duration::from_secs(30));
    cx.deadline = tokio::time::Instant::now() + Duration::from_secs(10_000);
    assert_eq!(clamp30(&cx, 600), Duration::from_secs(600));
}
