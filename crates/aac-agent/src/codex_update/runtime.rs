use super::*;
use std::time::Duration;

/// Restart obligations, without paths or credentials.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct RestartPlan {
    /// Whether a shared daemon was closed.
    pub daemon: bool,
    /// Number of desktop instances closed.
    pub desktops: usize,
}
/// Kernel process identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Kernel pid.
    pub pid: u32,
    /// Kernel start ticks.
    pub start_time: String,
}
/// Current daemon/proxy state.
#[derive(Default, Debug)]
pub struct RestartState {
    /// Shared daemon currently present.
    pub daemon: Option<Identity>,
    /// A proxy could respawn the shared daemon.
    pub proxy: bool,
}
/// Launch attribution; absence of a start time never proves readiness.
#[derive(Default)]
pub struct Launch {
    /// Started process identity.
    pub identity: Option<Identity>,
    /// Transient systemd unit started by this launch.
    pub unit: Option<String>,
}
impl Launch {
    fn owns(&self, main: &Process, all: &[Process]) -> bool {
        if self.unit.as_ref().is_some_and(|u| {
            main.unit
                .as_ref()
                .is_some_and(|s| s == u || s == &format!("{u}.service"))
        }) {
            return true;
        }
        let Some(id) = &self.identity else {
            return false;
        };
        let mut entry = Some(main);
        for _ in 0..64 {
            let Some(p) = entry else {
                return false;
            };
            if p.pid == id.pid {
                return p.start_time == id.start_time;
            }
            entry = all.iter().find(|n| n.pid == p.ppid);
        }
        false
    }
}
/// Host operations. Test implementations can exercise the complete transaction without real processes.
#[allow(async_fn_in_trait)]
pub trait Backend {
    /// Capture same-user ancestry and relevant private context.
    async fn scan(&mut self) -> Result<Vec<Process>>;
    /// Acquire both legacy and native startup locks, retaining them until release.
    async fn lock(&mut self) -> Result<()>;
    /// Release retained locks. Must also release on drop.
    async fn release(&mut self);
    /// Fail closed unless all shared-daemon and desktop tasks are idle.
    async fn assert_idle(&mut self, processes: &[Process]) -> Result<()>;
    /// Recheck pid, start ticks and fingerprint immediately before signaling.
    async fn signal(&mut self, process: &Process, kill: bool) -> Result<()>;
    /// Relaunch captured context using the updated executable, outside the dashboard cgroup.
    async fn launch(&mut self, process: &Process, desktop: bool) -> Result<Launch>;
    /// Bounded readiness check against the daemon socket.
    async fn verify_daemon(&mut self, process: &Process) -> Result<()>;
    /// Remove only the retired Unix control socket.
    async fn remove_socket(&mut self) -> Result<()>;
    /// Monotonic clock in milliseconds.
    fn now(&self) -> u64;
    /// Yield between bounded checks.
    async fn sleep(&mut self, duration: Duration);
}
/// In-memory update transaction; it never changes account credentials.
pub struct Runtime<B: Backend> {
    pub(super) backend: B,
    home: PathBuf,
    user_home: PathBuf,
    launchers: Vec<Process>,
    relaunched: BTreeMap<u32, Process>,
    locked: bool,
    stopped: bool,
}
impl<B: Backend> Runtime<B> {
    /// Build a transaction for this user's Codex home and restart backend.
    pub fn new(home: PathBuf, user_home: PathBuf, backend: B) -> Self {
        Self {
            backend,
            home,
            user_home,
            launchers: vec![],
            relaunched: BTreeMap::new(),
            locked: false,
            stopped: false,
        }
    }
    async fn scan(&mut self) -> Result<Vec<Process>> {
        Ok(self
            .backend
            .scan()
            .await?
            .into_iter()
            .filter(Process::alive)
            .collect())
    }
    /// Release startup locks even after a failed operation.
    pub async fn dispose(&mut self) {
        self.backend.release().await;
        self.locked = false;
    }
    /// The targets this transaction planned to close.
    pub fn stop_plan(&self) -> RestartPlan {
        plan(&self.launchers)
    }
    /// The targets closed and not observed back after our own relaunch.
    pub async fn retired_plan(&mut self) -> Result<RestartPlan> {
        let all = self.scan().await?;
        Ok(plan(
            &self
                .launchers
                .iter()
                .filter(|p| !all.iter().any(|n| n.same(p)) && !self.relaunched.contains_key(&p.pid))
                .cloned()
                .collect::<Vec<_>>(),
        ))
    }
    /// Current daemon identity and proxy presence on this home.
    pub async fn restart_state(&mut self) -> Result<RestartState> {
        let all = self.scan().await?;
        Ok(RestartState {
            daemon: all
                .iter()
                .find(|p| p.daemon() && !p.proxy() && p.on_home(&self.home))
                .map(identity),
            proxy: all
                .iter()
                .any(|p| p.codex() && p.proxy() && p.on_home(&self.home)),
        })
    }
    /// Check the daemon found now and return exactly the identity checked.
    pub async fn verify_running_daemon(&mut self) -> Result<Option<Identity>> {
        let all = self.scan().await?;
        let Some(p) = all
            .iter()
            .find(|p| p.daemon() && !p.proxy() && p.on_home(&self.home))
        else {
            return Ok(None);
        };
        Ok(if self.backend.verify_daemon(p).await.is_ok() {
            Some(identity(p))
        } else {
            None
        })
    }
    async fn wait_exit(&mut self, targets: &[Process], millis: u64) -> Result<bool> {
        let end = self.backend.now().saturating_add(millis);
        loop {
            let all = self.scan().await?;
            if !targets.iter().any(|p| all.iter().any(|n| n.same(p))) {
                return Ok(true);
            }
            self.backend.sleep(Duration::from_millis(100)).await;
            if self.backend.now() >= end {
                return Ok(false);
            }
        }
    }
    async fn retire(&mut self, roots: &[Process], all: &[Process]) -> Result<()> {
        let targets: Vec<_> = roots.iter().flat_map(|p| descendants(p, all)).collect();
        for p in roots {
            self.backend.signal(p, false).await?;
        }
        if self.wait_exit(&targets, 10_000).await? {
            return Ok(());
        }
        for (kill, ms) in [(false, 2000), (true, 5000)] {
            let current = self.scan().await?;
            for p in &targets {
                if current.iter().any(|n| n.same(p)) {
                    self.backend.signal(p, kill).await?;
                }
            }
            if self.wait_exit(&targets, ms).await? {
                return Ok(());
            }
        }
        Err(Error::RestartFailed)
    }
    /// Stop idle shared daemons and owned desktops under startup locks.
    /// Fresh terminal CLIs are intentionally left to the terminal updater.
    /// The hook records obligations under the lock, before any signal; a failure closes nothing.
    pub async fn stop(
        &mut self,
        mut before_stop: impl FnMut(RestartPlan) -> Result<()>,
    ) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.backend.lock().await?;
        self.locked = true;
        let mut mutated = false;
        let result = async {
            let all = self.scan().await?;
            let writers: Vec<_> = all
                .iter()
                .filter(|p| p.codex() && p.on_home(&self.home))
                .cloned()
                .collect();
            let desktops: Vec<_> = all
                .iter()
                .filter(|p| {
                    p.desktop()
                        && descendants(p, &all)
                            .iter()
                            .any(|n| writers.iter().any(|w| w.same(n)))
                })
                .cloned()
                .collect();
            let children: Vec<_> = desktops.iter().flat_map(|p| descendants(p, &all)).collect();
            let daemons: Vec<_> = writers.iter().filter(|p| p.daemon()).cloned().collect();
            for p in writers
                .iter()
                .filter(|p| !p.proxy() && !p.daemon() && !children.iter().any(|n| n.same(p)))
            {
                // The update bridge excludes fresh terminal CLIs. Unowned stdio servers remain busy.
                if !p.args.iter().any(|a| a == "app-server") {
                    continue;
                }
                let owned = p.args.iter().any(|a| a == "stdio://")
                    && all.iter().any(|n| {
                        n.pid == p.ppid
                            && n.exe == "/usr/lib/chatgpt/resources/cua_node/bin/node_repl"
                    })
                    && daemons
                        .iter()
                        .any(|d| descendants(d, &all).iter().any(|n| n.same(p)));
                if !owned {
                    return Err(Error::Busy);
                }
            }
            self.backend.assert_idle(&all).await?;
            if self.launchers.is_empty() {
                self.launchers = daemons.clone();
                for p in &desktops {
                    self.launchers
                        .push(desktop_launcher(p, &all, &self.user_home)?);
                }
            }
            before_stop(self.stop_plan())?;
            mutated = true;
            self.retire(&desktops, &all).await?;
            self.relaunched
                .retain(|_, p| !desktops.iter().any(|d| d.same(p)));
            self.retire(&daemons, &all).await?;
            if self.scan().await?.iter().any(|p| {
                p.codex()
                    && p.on_home(&self.home)
                    && !p.proxy()
                    && !p.daemon()
                    && p.args.iter().any(|a| a == "app-server")
            }) {
                return Err(Error::RestartFailed);
            }
            if !self
                .scan()
                .await?
                .iter()
                .any(|p| p.daemon() && p.on_home(&self.home))
            {
                self.backend.remove_socket().await?;
            }
            self.stopped = true;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            if mutated {
                let restored = self.start().await;
                if restored.is_err() {
                    self.dispose().await;
                    return Err(Error::RestartFailed);
                }
            } else {
                self.dispose().await;
            }
            return Err(error);
        }
        Ok(())
    }
    /// Start captured processes and verify our own desktop launches and the shared daemon.
    pub async fn start(&mut self) -> Result<()> {
        if !self.locked {
            return Ok(());
        }
        self.stopped = false;
        let all = self.scan().await?;
        let early: Vec<_> = all
            .iter()
            .filter(|p| p.daemon() && p.on_home(&self.home))
            .cloned()
            .collect();
        self.retire(&early, &all).await?;
        if !self
            .scan()
            .await?
            .iter()
            .any(|p| p.daemon() && p.on_home(&self.home))
        {
            self.backend.remove_socket().await?;
        }
        self.dispose().await; // Codex cannot bind its socket while either lock is held.
        for p in self.launchers.clone().iter().filter(|p| p.daemon()) {
            if !self
                .scan()
                .await?
                .iter()
                .any(|p| p.daemon() && p.on_home(&self.home))
            {
                self.backend.launch(p, false).await?;
            }
            self.backend.verify_daemon(p).await?;
        }
        for p in self.launchers.clone().iter().filter(|p| p.desktop()) {
            let all = self.scan().await?;
            if all.iter().any(|n| n.same(p))
                || self
                    .relaunched
                    .get(&p.pid)
                    .is_some_and(|r| all.iter().any(|n| n.same(r)))
            {
                continue;
            }
            self.relaunched.remove(&p.pid);
            let launch = self.backend.launch(p, true).await?;
            let end = self.backend.now().saturating_add(30_000);
            loop {
                let all = self.scan().await?;
                if let Some(main) = all.iter().find(|n| {
                    n.desktop()
                        && launch.owns(n, &all)
                        && descendants(n, &all).iter().any(|c| {
                            c.codex()
                                && c.on_home(&self.home)
                                && c.args.iter().any(|s| s == "app-server")
                                && !c.proxy()
                        })
                }) {
                    self.relaunched.insert(p.pid, main.clone());
                    break;
                }
                self.backend.sleep(Duration::from_millis(250)).await;
                if self.backend.now() >= end {
                    return Err(Error::RestartFailed);
                }
            }
        }
        Ok(())
    }
}
fn plan(targets: &[Process]) -> RestartPlan {
    RestartPlan {
        daemon: targets.iter().any(Process::daemon),
        desktops: targets.iter().filter(|p| p.desktop()).count(),
    }
}
fn identity(p: &Process) -> Identity {
    Identity {
        pid: p.pid,
        start_time: p.start_time.clone(),
    }
}
