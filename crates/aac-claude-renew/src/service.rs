//! One locked, logged, published run, as the server's schedule and the CLI both start it.

use crate::browser::Waits;
use crate::config::{self, Config};
use crate::localread::LocalRead;
use crate::remote::SshRemote;
use crate::state::State;
use crate::tick::{self, Deps, Report};
use crate::{hostcmd, now_ms, publish, scrub};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One run at a time: a lock file, taken over when older than two hours.
pub struct Lock(PathBuf);

impl Lock {
    /// Takes the lock in this folder, or `None` while another run holds it.
    pub fn take(dir: &Path) -> Option<Lock> {
        let _ = std::fs::create_dir_all(dir);
        let path = dir.join("run.lock");
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", std::process::id());
                    return Some(Lock(path));
                }
                Err(_) => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age.as_secs() > 7200);
                    if !stale {
                        return None;
                    }
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        None
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A logger that appends scrubbed lines to `<dir>/renew.log` (and echoes them when asked).
pub fn logger(dir: &Path, echo: bool) -> impl Fn(&str) + use<> {
    let path = dir.join("renew.log");
    move |line: &str| {
        let stamp = hostcmd::iso(now_ms());
        let text = format!("{} {}\n", &stamp[..19], scrub(line));
        if echo {
            print!("{text}");
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = f.write_all(text.as_bytes());
        }
    }
}

/// What a scheduled run did.
#[derive(Debug, PartialEq, Eq)]
pub enum Ran {
    /// No settings file: renewal is not set up on this server.
    NotSetUp,
    /// The settings refused (a reason without secrets).
    BadSettings(String),
    /// Turned off in the settings.
    Off,
    /// Another run holds the lock.
    Busy,
    /// It ran.
    Done(Report),
}

/// Runs once: the schedule's entry point. `publish_default` is the usage snapshot folder, used
/// when the settings name none.
pub fn run(ccs_dir: &Path, publish_default: Option<PathBuf>, options: RunOptions) -> Ran {
    let dir = config::state_dir(ccs_dir);
    let cfg_path = options
        .config
        .clone()
        .unwrap_or_else(|| dir.join("config.json"));
    if !cfg_path.exists() {
        return Ran::NotSetUp;
    }
    let mut cfg = match Config::load(&cfg_path) {
        Ok(cfg) => cfg,
        Err(e) => return Ran::BadSettings(e.to_string()),
    };
    cfg.step3 = crate::step3::read(ccs_dir);
    let left_out = cfg.place(&options.here);
    if options.scheduled && !cfg.enabled {
        return Ran::Off;
    }
    let Some(lock) = Lock::take(&dir) else {
        return Ran::Busy;
    };
    let state_path = dir.join("state.json");
    let remote = SshRemote::plain();
    let log = logger(&dir, options.echo);
    for name in &left_out {
        log(&format!(
            "{name}: left out, no ssh target and this server is not that computer"
        ));
    }
    let now = now_ms;
    let local = LocalRead::from_process();
    let deps = Deps {
        remote: &remote,
        now: &now,
        log: &log,
        waits: Waits::default(),
        local: local.as_ref(),
    };
    let mut st = State::load(&state_path);
    let only = options.only.as_ref().map(|(h, p)| (h.as_str(), p.as_str()));
    let report = tick::tick(&cfg, &mut st, &deps, only, options.force, options.dry_run);
    if let Err(e) = st.save(&state_path) {
        log(&format!("could not save the state ({e})"));
    }
    let target = options
        .publish
        .clone()
        .or_else(|| cfg.publish_dir.clone())
        .or(publish_default);
    if let Some(out) = target {
        let runner = cfg
            .hosts
            .iter()
            .find(|h| h.ssh.is_none())
            .map_or("server", |h| h.name.as_str())
            .to_owned();
        let doc = publish::document(&cfg, &st, &runner, now_ms());
        if let Err(e) = publish::write(&out, &doc, now_ms()) {
            log(&format!("could not publish ({e})"));
        }
    }
    drop(lock);
    Ran::Done(report)
}

/// How a run is started.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    /// A settings file other than `<ccs>/claude-renew/config.json`.
    pub config: Option<PathBuf>,
    /// A publish folder overriding the settings.
    pub publish: Option<PathBuf>,
    /// Only this `host`, `profile`.
    pub only: Option<(String, String)>,
    /// Renew `only` even when it is not due.
    pub force: bool,
    /// Plan without renewing.
    pub dry_run: bool,
    /// Started by the schedule (honors `enabled: false`).
    pub scheduled: bool,
    /// Echo log lines to stdout.
    pub echo: bool,
    /// Where this server is ([`config::Here`]); the standalone command keeps the settings as
    /// written.
    pub here: config::Here,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_settings_means_not_set_up_and_off_means_off() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(run(dir.path(), None, RunOptions::default()), Ran::NotSetUp);
        let state = config::state_dir(dir.path());
        std::fs::create_dir_all(&state).expect("mkdir");
        std::fs::write(
            state.join("config.json"),
            r#"{"enabled":false,"hosts":[{"name":"mac","os":"mac","ssh":"m"}]}"#,
        )
        .expect("write");
        let scheduled = RunOptions {
            scheduled: true,
            ..RunOptions::default()
        };
        assert_eq!(run(dir.path(), None, scheduled), Ran::Off);
        std::fs::write(state.join("config.json"), "{").expect("write");
        assert!(matches!(
            run(dir.path(), None, RunOptions::default()),
            Ran::BadSettings(_)
        ));
    }

    #[test]
    fn the_lock_is_exclusive_and_released() {
        let dir = tempfile::tempdir().expect("dir");
        let first = Lock::take(dir.path()).expect("first");
        assert!(Lock::take(dir.path()).is_none());
        drop(first);
        assert!(Lock::take(dir.path()).is_some());
    }
}
