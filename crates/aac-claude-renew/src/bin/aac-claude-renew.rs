//! `aac-claude-renew`: Claude Code sign-in renewal, run by the AI Account Center server.
//!
//! ```text
//! aac-claude-renew status                         every sign-in and when it ends
//! aac-claude-renew tick [--dry-run]               one run, as the server's schedule starts it
//! aac-claude-renew renew --host H --profile P [--force]
//! aac-claude-renew signin-profile --email E       sign that account's Chrome profile in by mail
//! ```
//!
//! Options: `--ccs <dir>` (default `$CCS_HOME/.ccs` or `~/.ccs`), `--config <file>`
//! (default `<ccs>/claude-renew/config.json`), `--publish <dir>` (overrides `publishDir`).
//! Output is ASCII; it never carries a link, code or token.

use aac_claude_renew::browser::{SignInByMail, Waits};
use aac_claude_renew::config::{self, Config};
use aac_claude_renew::remote::SshRemote;
use aac_claude_renew::service::{self, Lock, Ran, RunOptions};
use aac_claude_renew::state::State;
use aac_claude_renew::tick::{self, Deps};
use aac_claude_renew::{hostcmd, now_ms};
use std::path::PathBuf;

fn usage() -> ! {
    eprintln!(
        "usage: aac-claude-renew [--ccs DIR] [--config FILE] [--publish DIR] status | tick [--dry-run] | renew --host H --profile P [--force] | signin-profile --email E"
    );
    std::process::exit(2)
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn ccs_dir(args: &[String]) -> PathBuf {
    if let Some(dir) = option(args, "--ccs") {
        return PathBuf::from(dir);
    }
    if let Some(home) = std::env::var_os("CCS_HOME") {
        return PathBuf::from(home).join(".ccs");
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(home).join(".ccs")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    const VALUED: [&str; 6] = [
        "--ccs",
        "--config",
        "--publish",
        "--host",
        "--profile",
        "--email",
    ];
    let command = args
        .iter()
        .enumerate()
        .find(|(i, a)| {
            !a.starts_with("--") && (*i == 0 || !VALUED.contains(&args[*i - 1].as_str()))
        })
        .map(|(_, a)| a.clone())
        .unwrap_or_else(|| usage());
    let ccs = ccs_dir(&args);
    let dir = config::state_dir(&ccs);
    let config_file = option(&args, "--config").map(PathBuf::from);
    let cfg_path = config_file
        .clone()
        .unwrap_or_else(|| dir.join("config.json"));
    let cfg = match Config::load(&cfg_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("aac-claude-renew: {e} ({})", cfg_path.display());
            std::process::exit(2);
        }
    };
    let flag = |name: &str| args.iter().any(|a| a == name);
    match command.as_str() {
        "status" => {
            let _ = std::fs::create_dir_all(&dir);
            let remote = SshRemote::with_private_control();
            let log = service::logger(&dir, true);
            let now = now_ms;
            let deps = Deps {
                remote: &remote,
                now: &now,
                log: &log,
                waits: Waits::default(),
            };
            let mut st = State::default();
            for (host, scan) in tick::scan_all(&cfg, &mut st, &deps) {
                for r in scan.readings {
                    println!(
                        "{:<10} {:<12} {:<34} {:<13} ends {}",
                        host.name,
                        r.profile,
                        r.email.unwrap_or_default(),
                        r.state.word(),
                        r.ends_at
                            .map(hostcmd::iso)
                            .unwrap_or_else(|| "unknown".into())
                    );
                }
            }
        }
        "tick" | "renew" => {
            let only = match (option(&args, "--host"), option(&args, "--profile")) {
                (Some(h), Some(p)) => Some((h, p)),
                _ if command == "renew" => usage(),
                _ => None,
            };
            let options = RunOptions {
                config: config_file,
                publish: option(&args, "--publish").map(PathBuf::from),
                only,
                force: flag("--force"),
                dry_run: flag("--dry-run"),
                scheduled: command == "tick" && !flag("--dry-run"),
                echo: true,
            };
            let publish_default = None;
            match service::run(&ccs, publish_default, options) {
                Ran::Done(report) => {
                    for (key, result) in &report.outcomes {
                        println!("{key}: {result}");
                    }
                    if report
                        .outcomes
                        .iter()
                        .any(|(_, r)| r != "renewed" && r != "planned")
                    {
                        std::process::exit(1);
                    }
                }
                Ran::Off => println!("renewal is turned off in the settings (enabled: false)"),
                Ran::Busy => {
                    eprintln!("aac-claude-renew: another run is in progress");
                    std::process::exit(3);
                }
                Ran::NotSetUp | Ran::BadSettings(_) => usage(),
            }
        }
        "signin-profile" => {
            let Some(email) = option(&args, "--email").filter(|e| config::safe_email(e)) else {
                usage()
            };
            let Some(lock) = Lock::take(&dir) else {
                eprintln!("aac-claude-renew: another run is in progress");
                std::process::exit(3);
            };
            let state_path = dir.join("state.json");
            let remote = SshRemote::with_private_control();
            let log = service::logger(&dir, true);
            let now = now_ms;
            let deps = Deps {
                remote: &remote,
                now: &now,
                log: &log,
                waits: Waits::default(),
            };
            let mut st = State::load(&state_path);
            st.accounts
                .entry(email.clone())
                .or_default()
                .link_requested_at = None;
            let result = tick::sign_in_profile(&cfg, &mut st, &deps, &email);
            let _ = st.save(&state_path);
            drop(lock);
            match result {
                SignInByMail::SignedIn => println!("{email}: signed in"),
                SignInByMail::Check => {
                    println!("{email}: a check only a person may pass is on screen");
                    std::process::exit(1);
                }
                SignInByMail::Failed(word) => {
                    println!("{email}: {word}");
                    std::process::exit(1);
                }
            }
        }
        _ => usage(),
    }
}
