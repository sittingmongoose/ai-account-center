//! Host update coordinator exercised with fixed installers in parity builds.
use super::*;
use aac_files::{BackupClass, Class, OwnedStore, Registry, StoreSpec};
use aac_protocol::{Event, Request};
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use tokio::{io::AsyncWrite, task::JoinSet};

const ROOT_ENV: &str = "AAC_APPS_FIXTURE_ROOT";
const MAX_JOURNAL: usize = 64 * 1024;
const DESKTOP: &str = "codex-desktop";
const TAILED: [&str; 2] = ["t3-code", "antigravity-acp"];
const MAX_PARALLEL: usize = 4;
const NEXT_VERSION: &str = "26.1113.36452.0";
const OLD_VERSION: &str = "26.1101.9.0";

pub(crate) struct Fixture {
    root: PathBuf,
}

impl Fixture {
    pub(crate) fn open() -> Option<Self> {
        let root = std::env::var_os(ROOT_ENV).map(PathBuf::from)?;
        if !root.is_absolute() {
            return None;
        }
        let marker = read_text(&root.join("app-updates-fixture"), 64)?;
        if marker.trim() != "AAC fixture installers v1" {
            return None;
        }
        Some(Self { root })
    }

    pub(crate) async fn run(
        self,
        request: &Request,
        out: &mut (impl AsyncWrite + Unpin),
    ) -> Result<Value, Failure> {
        let platform = match request.args["platform"].as_str() {
            Some("ubuntu") => Platform::Ubuntu,
            Some("mac") => Platform::Mac,
            Some("windows") => Platform::Windows,
            _ => return Err(refused()),
        };
        let Some(job) = request.args["jobId"].as_str().filter(|job| valid_job(job)) else {
            return Err(refused());
        };
        if request.args.as_object().is_none_or(|args| args.len() != 3) {
            return Err(refused());
        }
        match request.args["action"].as_str() {
            Some("update") => self.update(request, out, platform, job).await,
            Some("cancel") => self.cancel(platform, job),
            _ => Err(refused()),
        }
    }

    async fn update(
        &self,
        request: &Request,
        out: &mut (impl AsyncWrite + Unpin),
        platform: Platform,
        job: &str,
    ) -> Result<Value, Failure> {
        let journal_store = claim(&self.root, "fixture-journal.json")?;
        let saved = journal(&self.root)?;
        // Know exactly which partial belongs to this installer. Never search
        // for or remove executables, installed versions or other scratch files.
        self.clear_partial()?;
        if let Some(mut journal) = saved
            && journal.job == job
        {
            if journal.platform != platform.name() {
                return Err(refused());
            }
            // A foreground owner disappeared. Preserve known results and
            // close missing rows as unknown. An explicit new job can try again.
            for app in ordered(platform) {
                if !journal.results.contains_key(app) {
                    journal.finish(
                        app,
                        result(platform, app, "unknown", "host_unknown", None, None, None),
                    );
                }
            }
            journal.save(&journal_store)?;
            return Ok(journal.payload(platform));
        }

        let mut journal = Journal {
            job: job.into(),
            platform: platform.name().into(),
            results: serde_json::Map::new(),
            running: Vec::new(),
        };
        journal.save(&journal_store)?;
        let mut progress = Progress {
            id: &request.id,
            out,
            active: Vec::new(),
            visible: None,
        };
        progress.app(None, "checking").await?;
        let all_installed = self.root.join("fixture-install-all").is_file();
        let mut pending = ordered(platform);
        let mut workers = JoinSet::new();
        while !pending.is_empty() || !workers.is_empty() {
            let mut position = 0;
            while position < pending.len() && workers.len() < MAX_PARALLEL {
                let app = pending[position];
                let cancelled = self.cancelled(job)?;
                let installed_here = all_installed || app == DESKTOP;
                let needed = if installed_here && !cancelled {
                    resources(platform, app)
                } else {
                    0
                };
                if !ready_to_start(app, platform, &journal)
                    || journal
                        .running
                        .iter()
                        .any(|active| needed & resources(platform, active) != 0)
                {
                    position += 1;
                    continue;
                }
                pending.remove(position);
                if cancelled || !installed_here {
                    let row = if cancelled {
                        result(
                            platform,
                            app,
                            "skipped",
                            "skipped_cancelled",
                            None,
                            None,
                            None,
                        )
                    } else {
                        result(
                            platform,
                            app,
                            "not_installed",
                            "not_installed",
                            None,
                            None,
                            None,
                        )
                    };
                    journal.finish(app, row.clone());
                    journal.save(&journal_store)?;
                    progress.result(app, row).await?;
                    continue;
                }
                journal.running.push(app.into());
                journal.save(&journal_store)?;
                progress.app(Some(app), "checking").await?;
                progress.app(Some(app), "updating").await?;
                let installed = self.installed(app)?;
                if app == DESKTOP && !installed {
                    self.start_download()?;
                    progress.app(Some(app), "downloading").await?;
                }
                let root = self.root.clone();
                workers.spawn(async move {
                    let fixture = Fixture { root };
                    let row = fixture.install(platform, app, installed).await?;
                    Ok::<_, Failure>((app, row))
                });
            }
            if let Some(result) = workers.join_next().await {
                let (app, row) = result
                    .map_err(|_| Failure::new(Cause::Bug, "app installer worker failed"))??;
                journal.finish(app, row.clone());
                journal.save(&journal_store)?;
                progress.result(app, row).await?;
            } else if !pending.is_empty() {
                return Err(Failure::new(Cause::Bug, "app update dependencies stalled"));
            }
        }
        Ok(journal.payload(platform))
    }

    fn installed(&self, app: &str) -> Result<bool, Failure> {
        match read::read(&self.root.join(receipt(app)), &ReadPolicy::capped(64)) {
            Read::Absent => Ok(false),
            Read::Bytes(bytes, _) if bytes == NEXT_VERSION.as_bytes() => Ok(true),
            _ => Err(damaged()),
        }
    }

    fn start_download(&self) -> Result<(), Failure> {
        claim(&self.root, "fixture-download.part")?.write(b"fixture package")
    }

    fn clear_partial(&self) -> Result<(), Failure> {
        match std::fs::remove_file(self.root.join("fixture-download.part")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn install(
        &self,
        platform: Platform,
        app: &str,
        installed: bool,
    ) -> Result<Value, Failure> {
        let (status, before) = if installed {
            ("current", NEXT_VERSION)
        } else {
            if app == DESKTOP {
                // A fixture can stop at the download boundary for cancel/kill tests.
                while self.root.join("fixture-hold-download").is_file()
                    && !self.root.join("fixture-release-download").is_file()
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            claim(&self.root, &receipt(app))?.write(NEXT_VERSION.as_bytes())?;
            if app == DESKTOP {
                self.clear_partial()?;
            }
            ("updated", OLD_VERSION)
        };
        Ok(result(
            platform,
            app,
            status,
            status,
            Some(before),
            Some(NEXT_VERSION),
            Some(manager(platform, app)),
        ))
    }

    fn cancelled(&self, job: &str) -> Result<bool, Failure> {
        let path = self.root.join(format!("fixture-cancel-{job}.json"));
        match read::read(&path, &ReadPolicy::capped(4)) {
            Read::Absent => Ok(false),
            Read::Bytes(bytes, _) if bytes == b"true" => Ok(true),
            _ => Err(damaged()),
        }
    }

    fn cancel(&self, platform: Platform, job: &str) -> Result<Value, Failure> {
        let active = journal(&self.root)?.is_some_and(|journal| {
            journal.job == job
                && journal.platform == platform.name()
                && journal.results.len() < APPS.len()
        });
        if active {
            claim(&self.root, &format!("fixture-cancel-{job}.json"))?.write(b"true")?;
        }
        Ok(json!({"cancelled": active}))
    }
}

fn receipt(app: &str) -> String {
    format!("fixture-installed-{app}.txt")
}

fn manager(platform: Platform, app: &str) -> &'static str {
    match app {
        "codex-cli" | "t3-acp-adapters" => "npm",
        "codex-desktop" | "claude-desktop" => match platform {
            Platform::Windows => "msix",
            Platform::Mac => "official-download",
            Platform::Ubuntu => "apt",
        },
        "zcode" | "t3-code" | "antigravity-acp" => "official-download",
        _ => "native",
    }
}

fn resources(platform: Platform, app: &str) -> u8 {
    const NPM: u8 = 1;
    const CODEX: u8 = 2;
    const APT: u8 = 4;
    const MSIX: u8 = 8;
    let manager_lock = match manager(platform, app) {
        "npm" => NPM,
        "apt" => APT,
        "msix" => MSIX,
        _ if platform == Platform::Ubuntu && app == "zcode" => NPM,
        _ => 0,
    };
    manager_lock
        | if matches!(app, "codex-cli" | DESKTOP) {
            CODEX
        } else {
            0
        }
}

fn ordered(platform: Platform) -> Vec<&'static str> {
    let mut order: Vec<_> = APPS
        .into_iter()
        .filter(|app| *app != DESKTOP && !TAILED.contains(app))
        .collect();
    if platform == Platform::Ubuntu {
        order.push(DESKTOP);
        order.extend(TAILED);
    } else {
        order.extend(TAILED);
        order.push(DESKTOP);
    }
    order
}

fn ready_to_start(app: &str, platform: Platform, journal: &Journal) -> bool {
    let done = |id: &str| journal.results.contains_key(id);
    match app {
        DESKTOP => done("codex-cli"),
        "t3-acp-adapters" => done("zcode"),
        "antigravity-acp" => done("t3-code"),
        "t3-code" => APPS
            .into_iter()
            .filter(|id| !TAILED.contains(id) && (*id != DESKTOP || platform == Platform::Ubuntu))
            .all(done),
        _ => true,
    }
}

fn refused() -> Failure {
    Failure::new(Cause::Refused, "invalid app update fixture request")
}

fn damaged() -> Failure {
    Failure::new(Cause::Damaged, "invalid app update fixture journal")
}

fn claim(root: &Path, path: &str) -> Result<OwnedStore, Failure> {
    let mut registry = Registry::default();
    registry.register(StoreSpec {
        path: path.into(),
        class: Class::State,
        mode: 0o600,
        max_size: MAX_JOURNAL,
        owner: "fixture-apps".into(),
        schema: "fixture-apps-v1".into(),
        backup: BackupClass::Rebuild,
    })?;
    registry.claim(root, Path::new(path), "fixture-apps")
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    job: String,
    platform: String,
    results: serde_json::Map<String, Value>,
    running: Vec<String>,
}

fn journal(root: &Path) -> Result<Option<Journal>, Failure> {
    match read::read(
        &root.join("fixture-journal.json"),
        &ReadPolicy::capped(MAX_JOURNAL as u64),
    ) {
        Read::Absent => Ok(None),
        Read::Bytes(bytes, _) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| damaged()),
        _ => Err(damaged()),
    }
}

impl Journal {
    fn save(&self, store: &OwnedStore) -> Result<(), Failure> {
        store.write(&serde_json::to_vec(self).map_err(|_| damaged())?)
    }

    fn finish(&mut self, app: &str, row: Value) {
        self.running.retain(|id| id != app);
        self.results.insert(app.into(), row);
    }

    fn payload(&self, platform: Platform) -> Value {
        json!({"results":ordered(platform).into_iter().filter_map(|app| self.results.get(app)).collect::<Vec<_>>()})
    }
}

fn result(
    platform: Platform,
    app: &str,
    status: &str,
    code: &str,
    before: Option<&str>,
    after: Option<&str>,
    manager: Option<&str>,
) -> Value {
    let diagnostic = if matches!(status, "unknown" | "failed") {
        "unknown"
    } else {
        "none"
    };
    json!({
        "appId":app, "platform":platform.name(), "status":status,
        "previousVersion":before, "version":after, "manager":manager,
        "messageCode":code, "updateAttempted":manager.is_some(), "restartedProcesses":0,
        "diagnostics":{"cause":diagnostic}
    })
}

struct Progress<'a, W> {
    id: &'a str,
    out: &'a mut W,
    visible: Option<&'static str>,
    active: Vec<(&'static str, &'static str)>,
}

impl<W: AsyncWrite + Unpin> Progress<'_, W> {
    async fn emit(&mut self, data: Value) -> Result<(), Failure> {
        crate::write_event(
            self.out,
            &Event::Progress {
                protocol: aac_protocol::PROTOCOL_VERSION,
                id: self.id.into(),
                data,
            },
            aac_protocol::MAX_FRAME_BYTES,
        )
        .await
    }

    async fn app(&mut self, app: Option<&'static str>, phase: &'static str) -> Result<(), Failure> {
        if let Some(app) = app {
            if let Some(entry) = self.active.iter_mut().find(|(id, _)| id == &app) {
                entry.1 = phase;
            } else {
                self.active.push((app, phase));
            }
        }
        self.visible = app;
        self.emit(json!({"event":"app","appId":app,"phase":phase}))
            .await
    }

    async fn result(&mut self, app: &str, row: Value) -> Result<(), Failure> {
        self.active.retain(|(id, _)| id != &app);
        self.emit(json!({"event":"result","result":row})).await?;
        if self.visible == Some(app)
            && let Some(&(app, phase)) = self.active.last()
        {
            self.app(Some(app), phase).await?;
        }
        Ok(())
    }
}
