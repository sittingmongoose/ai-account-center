use super::*;
#[test]
fn chromium_duplicates_terminator_and_empty_values() {
    let args = [
        "ChatGPT",
        " --type=renderer ",
        "-type=",
        "--user-data-dir=Work One",
        "--",
        "--type=ignored",
    ]
    .map(String::from);
    let got = chromium_switches(&args);
    assert_eq!(got.get("type").map(String::as_str), Some(""));
    assert_eq!(
        got.get("user-data-dir").map(String::as_str),
        Some("Work One")
    );
}

fn process(pid: u32, args: &[&str]) -> Process {
    Process {
        pid,
        ppid: 1,
        start_time: format!("tick-{pid}"),
        state: "S".into(),
        args: args.iter().map(|s| (*s).into()).collect(),
        exe: "/opt/codex".into(),
        cwd: "/work".into(),
        env: BTreeMap::from([("HOME".into(), "/fixture".into())]),
        unit: None,
    }
}
fn daemon(pid: u32) -> Process {
    process(pid, &["codex", "app-server", "--listen", "unix://"])
}
fn desktop(pid: u32) -> Process {
    Process {
        exe: "/usr/lib/chatgpt/ChatGPT".into(),
        args: vec!["ChatGPT".into()],
        env: BTreeMap::from([
            ("HOME".into(), "/fixture".into()),
            ("DISPLAY".into(), ":4".into()),
        ]),
        ..process(pid, &[])
    }
}
#[test]
fn desktop_rewritten_children_and_node_mode() {
    let mut p = desktop(10);
    assert!(p.desktop());
    p.args = vec!["ChatGPT --type=renderer --user-data-dir=Work One".into()];
    assert!(!p.desktop());
    assert_eq!(chromium_switches(&p.argv())["user-data-dir"], "Work One");
    p.args = vec!["ChatGPT -- --type=renderer".into()];
    assert!(p.desktop());
    p.env.insert("ELECTRON_RUN_AS_NODE".into(), "0".into());
    assert!(!p.desktop());
}
#[test]
#[cfg_attr(windows, ignore = "the Linux Codex runtime joins POSIX folders")]
fn desktop_recovers_child_display_and_relative_folder_before_terminator() {
    let mut p = desktop(10);
    p.env.clear();
    p.args.push("--".into());
    let mut child = process(11, &["codex", "app-server", "--user-data-dir=Work One"]);
    child.ppid = 10;
    child.cwd = "/other".into();
    child.env.insert("DISPLAY".into(), ":9".into());
    child.env.insert("CODEX_SECRET".into(), "private".into());
    let launch = desktop_launcher(&p, &[p.clone(), child], Path::new("/fixture")).expect("recover");
    assert_eq!(launch.env["DISPLAY"], ":9");
    assert!(!launch.env.contains_key("CODEX_SECRET"));
    assert_eq!(
        launch.args,
        vec!["ChatGPT", "--user-data-dir=/other/Work One", "--"]
    );
    assert!(desktop_launcher(&p, &[p.clone()], Path::new("/fixture")).is_err());
}
#[test]
fn identities_homes_and_cgroups() {
    let mut p = daemon(1);
    let original = p.fingerprint();
    p.env.insert("TOKEN".into(), "not hashed".into());
    assert_eq!(p.fingerprint(), original);
    p.args.push("--new".into());
    assert_ne!(p.fingerprint(), original);
    p.env
        .insert("CODEX_HOME".into(), "../fixture/.codex".into());
    assert!(p.on_home(Path::new("/fixture/.codex")));
    let mut reused = p.clone();
    reused.start_time = "later".into();
    assert!(!p.same(&reused));
    assert_eq!(
        systemd_unit_of("0::/user.slice/app.slice/aac.service\n"),
        Some("aac.service".into())
    );
    assert_eq!(systemd_unit_of("1:cpu:/other.service"), None);
}

struct Fake {
    all: Vec<Process>,
    time: u64,
    busy: bool,
    signals: Vec<u32>,
    fail_signal: Option<u32>,
    wrong_desktop: bool,
    locked: bool,
    launched: usize,
}
impl Fake {
    fn new(all: Vec<Process>) -> Self {
        Self {
            all,
            time: 0,
            busy: false,
            signals: vec![],
            fail_signal: None,
            wrong_desktop: false,
            locked: false,
            launched: 0,
        }
    }
}
impl Backend for Fake {
    async fn scan(&mut self) -> Result<Vec<Process>> {
        Ok(self.all.clone())
    }
    async fn lock(&mut self) -> Result<()> {
        self.locked = true;
        Ok(())
    }
    async fn release(&mut self) {
        self.locked = false;
    }
    async fn assert_idle(&mut self, _: &[Process]) -> Result<()> {
        if self.busy { Err(Error::Busy) } else { Ok(()) }
    }
    async fn signal(&mut self, p: &Process, _: bool) -> Result<()> {
        if self.fail_signal == Some(p.pid) {
            self.fail_signal = None;
            return Err(Error::RestartFailed);
        }
        self.signals.push(p.pid);
        let children = descendants(p, &self.all);
        self.all.retain(|n| !children.iter().any(|c| c.same(n)));
        Ok(())
    }
    async fn launch(&mut self, p: &Process, is_desktop: bool) -> Result<Launch> {
        assert!(!self.locked);
        self.launched += 1;
        let mut p = p.clone();
        p.pid += 100;
        p.start_time = "new".into();
        let identity = Identity {
            pid: p.pid,
            start_time: p.start_time.clone(),
        };
        if is_desktop {
            let mut child = process(p.pid + 1, &["codex", "app-server"]);
            child.ppid = p.pid;
            self.all.push(child);
        }
        self.all.push(p);
        Ok(Launch {
            identity: if self.wrong_desktop {
                None
            } else {
                Some(identity)
            },
            unit: None,
        })
    }
    async fn verify_daemon(&mut self, _: &Process) -> Result<()> {
        if self.all.iter().any(Process::daemon) {
            Ok(())
        } else {
            Err(Error::RestartFailed)
        }
    }
    async fn remove_socket(&mut self) -> Result<()> {
        Ok(())
    }
    fn now(&self) -> u64 {
        self.time
    }
    async fn sleep(&mut self, d: std::time::Duration) {
        self.time += d.as_millis() as u64;
    }
}
fn runtime(all: Vec<Process>) -> Runtime<Fake> {
    Runtime::new("/fixture/.codex".into(), "/fixture".into(), Fake::new(all))
}
#[tokio::test]
async fn busy_and_hook_failure_never_signal() {
    let mut r = runtime(vec![daemon(10)]);
    r.backend.busy = true;
    assert_eq!(r.stop(|_| Ok(())).await, Err(Error::Busy));
    assert!(r.backend.signals.is_empty());
    assert!(!r.backend.locked);
    r.backend.busy = false;
    assert_eq!(
        r.stop(|_| Err(Error::RestartFailed)).await,
        Err(Error::RestartFailed)
    );
    assert!(r.backend.signals.is_empty());
}
#[tokio::test]
async fn stop_start_records_obligations_and_verifies_own_launch() {
    let d = desktop(20);
    let mut child = process(21, &["codex", "app-server"]);
    child.ppid = 20;
    let mut r = runtime(vec![daemon(10), d, child]);
    let mut hook = false;
    r.stop(|p| {
        assert_eq!(
            p,
            RestartPlan {
                daemon: true,
                desktops: 1
            }
        );
        hook = true;
        Ok(())
    })
    .await
    .expect("stop");
    assert!(hook);
    assert!(r.backend.locked);
    assert_eq!(
        r.retired_plan().await.expect("retired"),
        RestartPlan {
            daemon: true,
            desktops: 1
        }
    );
    r.start().await.expect("start");
    assert!(!r.backend.locked);
    assert_eq!(r.backend.launched, 2);
    assert_eq!(r.retired_plan().await.expect("retired").desktops, 0);
    assert!(r.verify_running_daemon().await.expect("verify").is_some());
    assert!(r.restart_state().await.expect("state").daemon.is_some());
}
#[tokio::test]
async fn unowned_stdio_refused_owned_browser_allowed_and_fresh_cli_ignored() {
    let mut r = runtime(vec![daemon(10), process(30, &["codex", "app-server"])]);
    assert_eq!(r.stop(|_| Ok(())).await, Err(Error::Busy));
    assert!(r.backend.signals.is_empty());
    let mut helper = process(12, &["codex", "app-server", "stdio://"]);
    helper.ppid = 11;
    let mut node = process(11, &["node_repl"]);
    node.exe = "/usr/lib/chatgpt/resources/cua_node/bin/node_repl".into();
    node.ppid = 10;
    let mut r = runtime(vec![
        daemon(10),
        node,
        helper,
        process(30, &["codex", "resume"]),
    ]);
    r.stop(|_| Ok(())).await.expect("owned");
    assert!(r.backend.all.iter().any(|p| p.pid == 30));
}
#[tokio::test]
async fn unrelated_desktop_does_not_satisfy_restart_readiness() {
    let mut child = process(21, &["codex", "app-server"]);
    child.ppid = 20;
    let mut r = runtime(vec![desktop(20), child]);
    r.backend.wrong_desktop = true;
    r.stop(|_| Ok(())).await.expect("stop");
    assert_eq!(r.start().await, Err(Error::RestartFailed));
    assert_eq!(r.retired_plan().await.expect("retired").desktops, 1);
}
#[tokio::test]
async fn partial_stop_restores_closed_desktop_and_releases_locks() {
    let mut child = process(21, &["codex", "app-server"]);
    child.ppid = 20;
    let mut r = runtime(vec![daemon(10), desktop(20), child]);
    r.backend.fail_signal = Some(10);
    assert_eq!(r.stop(|_| Ok(())).await, Err(Error::RestartFailed));
    assert!(!r.backend.locked);
    assert!(r.backend.all.iter().any(Process::desktop));
}
#[cfg(target_os = "linux")]
#[test]
fn rollout_last_lifecycle_and_large_incomplete_tail() {
    use std::io::Write;
    let mut file = tempfile::tempfile().expect("temp");
    file.write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n")
        .expect("write");
    assert_eq!(rollout_lifecycle(&mut file), Ok("busy"));
    file.write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"turn_aborted\"}}\ntruncated")
        .expect("write");
    assert_eq!(rollout_lifecycle(&mut file), Ok("idle"));
    let mut file = tempfile::tempfile().expect("temp");
    file.write_all(&vec![b'x'; 1024 * 1024 + 1]).expect("tail");
    assert_eq!(rollout_lifecycle(&mut file), Ok("unknown"));
}

#[test]
fn generated_typescript_oracle_matches_all_512_cases() {
    let table: serde_json::Value =
        serde_json::from_str(include_str!("oracle.json")).expect("oracle");
    let cases = table["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 512);
    for (i, c) in cases.iter().enumerate() {
        let args: Vec<String> = serde_json::from_value(c["args"].clone()).expect("args");
        let switches: BTreeMap<String, String> =
            serde_json::from_value(c["switches"].clone()).expect("switches");
        let env = serde_json::from_value(c["env"].clone()).expect("env");
        let p = Process {
            args: args.clone(),
            exe: "/usr/lib/chatgpt/ChatGPT".into(),
            env,
            ..process(10, &[])
        };
        assert_eq!(chromium_switches(&args), switches, "case {i}");
        assert_eq!(
            p.desktop(),
            c["desktop"].as_bool().expect("desktop"),
            "case {i}"
        );
        assert_eq!(
            p.fingerprint(),
            c["fingerprint"].as_str().expect("fingerprint"),
            "case {i}"
        );
    }
}
