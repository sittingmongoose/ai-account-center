//! One renewal run: read every computer's sign-ins, check earlier renewals held, renew what is
//! due, keep each account's claude.ai browser session alive, and publish.

use crate::browser::{Authorize, Browser, SignInByMail, Waits};
use crate::config::{Config, Host};
use crate::hostcmd::{self, Scan};
use crate::plan::{self, Due, Reading, SignIn, Verification};
use crate::remote::Remote;
use crate::state::State;
use crate::{HOUR_MS, scrub};
use std::time::Duration;

/// What a run needs from its surroundings.
pub struct Deps<'a> {
    /// Runs scripts on the computers.
    pub remote: &'a dyn Remote,
    /// The clock (ms).
    pub now: &'a dyn Fn() -> i64,
    /// A log line (already free of secrets; it is scrubbed again).
    pub log: &'a dyn Fn(&str),
    /// Browser waits (tests shorten them).
    pub waits: Waits,
}

impl Deps<'_> {
    fn say(&self, line: &str) {
        (self.log)(&scrub(line));
    }
}

/// How long a computer's scan may take.
const SCAN_SECS: u64 = 240;
/// How long the login prompt may take to print its link.
const LINK_SECS: u64 = 90;
/// How long the login may take after the code went in.
const FINISH_SECS: u64 = 150;
/// How often a browser session's end is read.
const SESSION_CHECK_MS: i64 = 6 * HOUR_MS;

/// Reads one computer.
pub fn scan_host(cfg: &Config, deps: &Deps, host: &Host) -> Result<Scan, String> {
    let script = hostcmd::scan(host, &cfg.ignore_profiles);
    let out = deps
        .remote
        .run(host, &script, b"", Duration::from_secs(SCAN_SECS))
        .map_err(|e| e.word().to_owned())?;
    if out.stdout.contains("AACNOCLAUDE") {
        return Err("claude_not_found".into());
    }
    if out.code != Some(0) && !out.stdout.contains("AACROW") {
        return Err(format!("exit_{}", out.code.unwrap_or(-1)));
    }
    Ok(hostcmd::parse_scan(&host.name, &out.stdout))
}

fn remember(st: &mut State, reading: &Reading, now: i64) {
    let p = st.profiles.entry(reading.key()).or_default();
    p.email.clone_from(&reading.email);
    p.state = Some(reading.state.word().into());
    p.dir = Some(reading.dir.clone());
    p.default = reading.default;
    p.ends_at = reading.ends_at;
    p.read_at = Some(now);
}

/// Reads every computer; returns each reachable one's scan.
pub fn scan_all(cfg: &Config, st: &mut State, deps: &Deps) -> Vec<(Host, Scan)> {
    let mut out = vec![];
    for host in &cfg.hosts {
        let now = (deps.now)();
        match scan_host(cfg, deps, host) {
            Ok(scan) => {
                for r in &scan.readings {
                    remember(st, r, now);
                }
                let h = st.hosts.entry(host.name.clone()).or_default();
                h.read_at = Some(now);
                h.error = None;
                out.push((host.clone(), scan));
            }
            Err(word) => {
                deps.say(&format!(
                    "{}: could not read its sign-ins ({word})",
                    host.name
                ));
                st.hosts.entry(host.name.clone()).or_default().error = Some(word);
            }
        }
    }
    out
}

/// Renews one sign-in through the CLI's own login and the account's plain Chrome profile.
/// Returns `renewed`, `signed_out`, `waiting_for_you` or a failure word; on `renewed` the state
/// is updated with the new end.
pub fn renew_one(
    cfg: &Config,
    st: &mut State,
    deps: &Deps,
    host: &Host,
    claude: &str,
    reading: &Reading,
    reason: Due,
) -> String {
    let Some(email) = reading.email.clone() else {
        return "no_email".into();
    };
    let Some(browser_host) = cfg.host(&cfg.browser_host) else {
        return "no_browser_host".into();
    };
    let Some(script) = hostcmd::login(host, claude, &reading.dir, reading.default) else {
        return "unsafe_profile".into();
    };
    let mut session = match deps.remote.start(host, &script, false) {
        Ok(s) => s,
        Err(e) => return format!("login_{}", e.word()),
    };
    let mut url = None;
    let start = std::time::Instant::now();
    while url.is_none() && start.elapsed() < Duration::from_secs(LINK_SECS) {
        if !session.read(Duration::from_millis(500)) {
            break;
        }
        url = hostcmd::login_url(&session.text());
    }
    let Some(link) = url else {
        return "no_login_link".into();
    };
    let mut browser = Browser::new(
        deps.remote,
        browser_host,
        &cfg.profiles_dir,
        &cfg.agent_desktop,
    );
    browser.waits = deps.waits;
    let outcome = browser.authorize(&email, &link);
    drop(link);
    match outcome {
        Authorize::Code(code) => {
            let sent = session.write_line(&code).is_ok();
            drop(code);
            if !sent {
                return "code_not_sent".into();
            }
            session.close_input();
            let rc = session.wait(Duration::from_secs(FINISH_SECS));
            if !hostcmd::login_succeeded(&session.text(), rc) {
                return "login_failed".into();
            }
        }
        Authorize::SignedOut => return "signed_out".into(),
        Authorize::Check => return "waiting_for_you".into(),
        Authorize::WrongAccount(who) => {
            let now = (deps.now)();
            st.need_person(
                &email,
                "wrong_account",
                &format!(
                    "The Chrome profile kept for {email} on the Mac is signed in as {}. Sign it in to claude.ai as {email}.",
                    who.as_deref().unwrap_or("another account")
                ),
                now,
            );
            return "wrong_account".into();
        }
        Authorize::Failed(word) => return format!("browser_{word}"),
    }
    drop(session);
    // Proof by reading back: the saved end must have moved later, on the same account.
    let after = match scan_host(cfg, deps, host) {
        Ok(scan) => scan
            .readings
            .into_iter()
            .find(|r| r.profile == reading.profile),
        Err(_) => None,
    };
    let Some(after) = after else {
        return "profile_missing_after".into();
    };
    if after.email.is_some() && after.email != reading.email {
        return "wrong_account_saved".into();
    }
    let moved = after.state == SignIn::Ok
        && after.ends_at.unwrap_or(0) > reading.ends_at.unwrap_or(0) + 60_000;
    if !moved {
        return "end_not_moved".into();
    }
    let now = (deps.now)();
    remember(st, &after, now);
    let saved = st.profiles.entry(reading.key()).or_default();
    plan::note_renewed(
        saved,
        now,
        reading.ends_at,
        after.ends_at.unwrap_or(0),
        Config::ms(cfg.verify_after_hours),
        reason,
    );
    st.clear_need(&email);
    st.event(now, "renewed", Some(&reading.key()), Some(&email), None);
    deps.say(&format!(
        "{}: renewed; the sign-in now ends {}",
        reading.key(),
        after.ends_at.map(hostcmd::iso).unwrap_or_default()
    ));
    "renewed".into()
}

/// What one run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// `host/profile` and its result word, for each attempted sign-in.
    pub outcomes: Vec<(String, String)>,
}

/// One full run. `only` limits it to one `host/profile`; `force` renews that one even when it is
/// not due; `dry_run` plans without renewing.
pub fn tick(
    cfg: &Config,
    st: &mut State,
    deps: &Deps,
    only: Option<(&str, &str)>,
    force: bool,
    dry_run: bool,
) -> Report {
    let scans = scan_all(cfg, st, deps);
    let now = (deps.now)();
    // Later readings of earlier renewals.
    for (_, scan) in &scans {
        for r in &scan.readings {
            let saved = st.profiles.entry(r.key()).or_default();
            if plan::verify(saved, r, now) == Verification::Reverted {
                let key = r.key();
                st.event(now, "reverted", Some(&key), r.email.as_deref(), None);
                deps.say(&format!(
                    "{key}: the renewed sign-in went back; renewing again"
                ));
            }
        }
    }
    let skip: std::collections::HashSet<&str> = cfg.skip.iter().map(String::as_str).collect();
    let within = Config::ms(cfg.renew_within_hours);
    let mut due: Vec<(Host, String, Reading, Due)> = vec![];
    for (host, scan) in &scans {
        let Some(claude) = scan.claude.clone() else {
            continue;
        };
        for r in &scan.readings {
            if let Some((h, p)) = only
                && (h != host.name || p != r.profile)
            {
                continue;
            }
            let saved = st.profiles.get(&r.key()).cloned().unwrap_or_default();
            let reason = if force && only.is_some() && r.email.is_some() {
                Some(Due::Requested)
            } else {
                plan::due(r, &saved, within, now)
            };
            let Some(reason) = reason else {
                continue;
            };
            if reason != Due::Requested
                && (skip.contains(r.key().as_str()) || !plan::may_try(&saved, now))
            {
                continue;
            }
            due.push((host.clone(), claude.clone(), r.clone(), reason));
        }
    }
    due.sort_by(|a, b| (a.2.email.as_deref(), &a.0.name).cmp(&(b.2.email.as_deref(), &b.0.name)));
    let mut report = Report::default();
    if !due.is_empty() {
        deps.say(&format!(
            "renewing {} sign-in(s): {}",
            due.len(),
            due.iter()
                .map(|d| format!("{} ({})", d.2.key(), d.3.word()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if dry_run {
        report.outcomes = due
            .iter()
            .map(|d| (d.2.key(), "planned".to_owned()))
            .collect();
        return report;
    }
    let mut blocked: std::collections::HashSet<String> = std::collections::HashSet::new();
    let backoff = Config::ms(cfg.captcha_backoff_hours);
    for (host, claude, reading, reason) in due {
        let key = reading.key();
        let email = reading.email.clone().unwrap_or_default();
        if blocked.contains(&email) {
            let saved = st.profiles.entry(key.clone()).or_default();
            plan::note_captcha_wait(saved, (deps.now)(), backoff, reading.ends_at);
            report.outcomes.push((key, "waiting_for_you".into()));
            continue;
        }
        deps.say(&format!("{key} ({email}): renewing ({})", reason.word()));
        let mut result = renew_one(cfg, st, deps, &host, &claude, &reading, reason);
        if result == "signed_out" {
            result = match sign_in_profile(cfg, st, deps, &email) {
                SignInByMail::SignedIn => {
                    renew_one(cfg, st, deps, &host, &claude, &reading, reason)
                }
                SignInByMail::Check => "waiting_for_you".into(),
                SignInByMail::Failed(word) => format!("signin_{word}"),
            };
        }
        let now = (deps.now)();
        match result.as_str() {
            "renewed" => {}
            "waiting_for_you" => {
                blocked.insert(email.clone());
                st.need_person(
                    &email,
                    "captcha",
                    &format!("Solve the check in the Chrome window for {email} on the Mac; renewal carries on by itself after."),
                    now,
                );
                let saved = st.profiles.entry(key.clone()).or_default();
                plan::note_captcha_wait(saved, now, backoff, reading.ends_at);
            }
            word => {
                let saved = st.profiles.entry(key.clone()).or_default();
                plan::note_failure(saved, now, word);
                let failures = saved.failures;
                st.event(now, "renew_failed", Some(&key), Some(&email), Some(word));
                deps.say(&format!("{key}: renewal failed ({word})"));
                if failures >= 3 && reading.ends_at.unwrap_or(0) - now < 24 * HOUR_MS {
                    st.need_person(
                        &email,
                        "renewal_failing",
                        &format!(
                            "Renewing {key} keeps failing ({word}). Run `aac-claude-renew renew --host {} --profile {}` and watch, or sign in there by hand.",
                            host.name, reading.profile
                        ),
                        now,
                    );
                }
            }
        }
        report.outcomes.push((key, result));
    }
    if only.is_none() {
        keep_sessions(cfg, st, deps, &blocked);
    }
    st.last_tick_at = Some((deps.now)());
    report
}

/// Signs an account's plain profile in to claude.ai by mail.
pub fn sign_in_profile(cfg: &Config, st: &mut State, deps: &Deps, email: &str) -> SignInByMail {
    let Some(browser_host) = cfg.host(&cfg.browser_host) else {
        return SignInByMail::Failed("no_browser_host");
    };
    let now = (deps.now)();
    let acc = st.accounts.entry(email.to_owned()).or_default();
    // One link per account per 50 minutes: a person may still be finishing the last one.
    if acc.link_requested_at.is_some_and(|t| now - t < 50 * 60_000) {
        return SignInByMail::Failed("link_recently_requested");
    }
    acc.link_requested_at = Some(now);
    deps.say(&format!(
        "{email}: its Chrome profile is signed out of claude.ai; asking for a sign-in link"
    ));
    let mut browser = Browser::new(
        deps.remote,
        browser_host,
        &cfg.profiles_dir,
        &cfg.agent_desktop,
    );
    browser.waits = deps.waits;
    let clock = deps.now;
    let result = browser.sign_in_by_mail(email, || clock() / 1000);
    let now = (deps.now)();
    match &result {
        SignInByMail::SignedIn => {
            st.clear_need(email);
            st.event(now, "browser_signed_in", None, Some(email), None);
            deps.say(&format!(
                "{email}: Chrome profile signed in to claude.ai again"
            ));
        }
        SignInByMail::Check => {}
        SignInByMail::Failed(word) => {
            st.need_person(
                email,
                "browser_signin_failed",
                &format!(
                    "Sign in to claude.ai as {email} in its Chrome profile on the Mac ({word})."
                ),
                now,
            );
        }
    }
    result
}

/// Reads each account's browser session end every 6 hours and opens claude.ai once a day so the
/// session keeps sliding.
pub fn keep_sessions(
    cfg: &Config,
    st: &mut State,
    deps: &Deps,
    skip: &std::collections::HashSet<String>,
) {
    let Some(browser_host) = cfg.host(&cfg.browser_host) else {
        return;
    };
    let mut browser = Browser::new(
        deps.remote,
        browser_host,
        &cfg.profiles_dir,
        &cfg.agent_desktop,
    );
    browser.waits = deps.waits;
    let emails: std::collections::BTreeSet<String> = st
        .profiles
        .values()
        .filter_map(|p| p.email.clone())
        .collect();
    for email in emails {
        if skip.contains(&email) {
            continue;
        }
        let now = (deps.now)();
        let acc = st.accounts.entry(email.clone()).or_default();
        let visit_due = acc
            .visited_at
            .is_none_or(|t| now - t >= Config::ms(cfg.visit_every_hours));
        let check_due = acc
            .session_checked_at
            .is_none_or(|t| now - t >= SESSION_CHECK_MS);
        if !visit_due && !check_due {
            continue;
        }
        if visit_due {
            let (before, after) = browser.visit(&email);
            let acc = st.accounts.entry(email.clone()).or_default();
            acc.session_ends_at = after;
            acc.session_checked_at = Some(now);
            if before.is_some() {
                acc.visited_at = Some(now);
                acc.visit_extends = Some(after.unwrap_or(0) > before.unwrap_or(0));
            }
        } else {
            let end = browser.session_end(&email);
            let acc = st.accounts.entry(email.clone()).or_default();
            acc.session_ends_at = end;
            acc.session_checked_at = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{Output, RemoteError, Script, Session};
    use std::sync::{Arc, Mutex};

    const NOW: i64 = 1_800_000_000_000;
    const CODE: &str = "abcdefghijABCDEFGHIJ1234#abcdefghijABCDEFGHIJ1234";

    #[derive(Default)]
    struct World {
        renewed: bool,
        code_seen: Option<String>,
        logins: u32,
        presses: u32,
    }

    struct Fake(Arc<Mutex<World>>);

    fn status(ends: i64) -> String {
        format!(
            r#"{{"loggedIn":true,"authMethod":"claude.ai","loginExpiresAt":"{}","email":"p@x.party"}}"#,
            hostcmd::iso(ends)
        )
    }

    impl Remote for Fake {
        fn run(
            &self,
            host: &Host,
            script: &Script,
            _: &[u8],
            _: Duration,
        ) -> Result<Output, RemoteError> {
            let ok = |s: String| {
                Ok(Output {
                    code: Some(0),
                    stdout: s,
                })
            };
            let w = self.0.lock().expect("lock");
            let text = match script {
                Script::Sh(s) | Script::Cmd(s) | Script::Pwsh(s) => s.clone(),
            };
            if text.contains("auth status --json") {
                if host.name != "windows" {
                    return ok("AACCLAUDE\t/u/claude\n".into());
                }
                let ends = if w.renewed {
                    NOW + 30 * 24 * HOUR_MS
                } else {
                    NOW + 3 * HOUR_MS
                };
                return ok(format!(
                    "AACCLAUDE\tC:\\Users\\s\\.local\\bin\\claude.exe\nAACROW\tparty\tC:\\Users\\s\\.claude-t3\\party\t0\t{}\n",
                    status(ends)
                ));
            }
            drop(w);
            if text.contains("open -g -na") {
                return ok("AACOPENED\n".into());
            }
            if text.contains("ps -axo") {
                return ok("77\n".into());
            }
            if text.contains("list-windows") {
                return ok(r#"{"data":[{"id":"w-1","pid":77,"accessible":true,"bounds":{"width":900.0}}]}"#.into());
            }
            if text.contains("--name 'Authorize'") {
                return ok(
                    r#"{"data":{"matches":[{"name":"Authorize","ref_id":"@a:e1"}]}}"#.into(),
                );
            }
            if text.contains("--role statictext") {
                return ok(
                    r#"{"data":{"matches":[{"name":"Logged in as ","ref_id":null},{"name":"p@x.party","ref_id":null}]}}"#
                        .into(),
                );
            }
            if text.contains("click '@a:e1'") {
                self.0.lock().expect("lock").presses += 1;
                return ok(r#"{"ok":true}"#.into());
            }
            if text.contains("snapshot --window-id w-1") {
                return ok(format!("{{\"value\":\"{CODE}\"}}"));
            }
            if text.contains("sqlite3") {
                return ok(format!(
                    "{}\n",
                    (NOW + 20 * 24 * HOUR_MS + 11_644_473_600_000) * 1000
                ));
            }
            ok(String::new())
        }

        fn start(
            &self,
            host: &Host,
            script: &Script,
            tty: bool,
        ) -> Result<Box<dyn Session>, RemoteError> {
            assert!(!tty);
            assert_eq!(host.name, "windows");
            let Script::Cmd(line) = script else {
                panic!("windows login runs through cmd");
            };
            assert!(line.contains("auth login --claudeai"));
            self.0.lock().expect("lock").logins += 1;
            Ok(Box::new(FakeLogin {
                world: self.0.clone(),
                text: "Opening browser to sign in…\nIf the browser didn't open, visit:\nhttps://claude.com/cai/oauth/authorize?code=true&state=s\nPaste code here if prompted > ".into(),
            }))
        }
    }

    struct FakeLogin {
        world: Arc<Mutex<World>>,
        text: String,
    }

    impl Session for FakeLogin {
        fn write_line(&mut self, line: &str) -> std::io::Result<()> {
            let mut w = self.world.lock().expect("lock");
            w.code_seen = Some(line.to_owned());
            if line == CODE {
                w.renewed = true;
                self.text.push_str("Login successful.\n");
            }
            Ok(())
        }
        fn close_input(&mut self) {}
        fn read(&mut self, _: Duration) -> bool {
            true
        }
        fn text(&self) -> String {
            self.text.clone()
        }
        fn wait(&mut self, _: Duration) -> Option<i32> {
            Some(0)
        }
    }

    fn cfg() -> Config {
        Config::parse(
            r#"{"hosts":[{"name":"mac","os":"mac","ssh":"test-mac"},{"name":"windows","os":"windows","ssh":"test-windows"}]}"#,
        )
        .expect("cfg")
    }

    fn fast() -> Waits {
        Waits {
            poll: Duration::from_millis(1),
            page: Duration::from_millis(300),
            check_grace: Duration::from_millis(50),
            code: Duration::from_millis(300),
            mail: Duration::from_millis(50),
        }
    }

    #[test]
    fn a_due_sign_in_is_renewed_and_read_back() {
        let world = Arc::new(Mutex::new(World::default()));
        let fake = Fake(world.clone());
        let lines = Mutex::new(vec![]);
        let log = |l: &str| lines.lock().expect("lock").push(l.to_owned());
        let now = || NOW;
        let deps = Deps {
            remote: &fake,
            now: &now,
            log: &log,
            waits: fast(),
        };
        let mut st = State::default();
        let report = tick(&cfg(), &mut st, &deps, None, false, false);
        assert_eq!(
            report.outcomes,
            vec![("windows/party".to_owned(), "renewed".to_owned())]
        );
        let w = world.lock().expect("lock");
        assert_eq!((w.logins, w.presses), (1, 1));
        assert_eq!(w.code_seen.as_deref(), Some(CODE));
        let saved = &st.profiles["windows/party"];
        assert_eq!(saved.ends_at, Some(NOW + 30 * 24 * HOUR_MS));
        assert_eq!(saved.previous_end, Some(NOW + 3 * HOUR_MS));
        assert_eq!(saved.last_result.as_deref(), Some("renewed"));
        assert_eq!(saved.verify, vec![NOW + 9 * HOUR_MS, NOW + 12 * HOUR_MS]);
        let logged = lines.lock().expect("lock").join("\n");
        assert!(!logged.contains(CODE), "the code never reaches the log");
        assert!(!logged.contains("https://"), "links never reach the log");
        assert!(logged.contains("windows/party: renewed"));
        assert_eq!(
            st.accounts["p@x.party"].session_ends_at,
            Some(NOW + 20 * 24 * HOUR_MS)
        );
    }

    #[test]
    fn not_due_means_no_login_and_dry_run_only_plans() {
        let world = Arc::new(Mutex::new(World {
            renewed: true,
            ..World::default()
        }));
        let fake = Fake(world.clone());
        let log = |_: &str| {};
        let now = || NOW;
        let deps = Deps {
            remote: &fake,
            now: &now,
            log: &log,
            waits: fast(),
        };
        let mut st = State::default();
        assert_eq!(
            tick(&cfg(), &mut st, &deps, None, false, false),
            Report::default()
        );
        assert_eq!(world.lock().expect("lock").logins, 0);
        let planned = tick(
            &cfg(),
            &mut st,
            &deps,
            Some(("windows", "party")),
            true,
            true,
        );
        assert_eq!(
            planned.outcomes,
            vec![("windows/party".to_owned(), "planned".to_owned())]
        );
        assert_eq!(world.lock().expect("lock").logins, 0);
    }
}
