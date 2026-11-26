//! The browser step on the browser computer (the Mac), driven over ssh.
//!
//! Each account has its own plain, persistent Chrome profile (`<profilesDir>/<email>`), started
//! like any Chrome with `open -g -na "Google Chrome"`: no `--remote-debugging-port`, no
//! automation flags, so `navigator.webdriver` stays false and claude.ai's checks pass the way
//! they do for a person. Pages open through a private redirect file, so no link reaches a command
//! line. The page is read and pressed through macOS accessibility (`agent-desktop`): AXPress is a
//! trusted click and needs no focus, and the window stays in the background.
//!
//! Accessibility is granted to ssh sessions on the Mac and denied to launchd jobs, so this runs
//! over ssh. Mail.app is read inside the desktop session instead ([`crate::hostcmd::desktop`]).

use crate::config::{Host, safe_email};
use crate::remote::{Remote, Script, sh_quote};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

static CODE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"[A-Za-z0-9_-]{20,}#[A-Za-z0-9_-]{20,}").expect("static regex")
});
static MAGIC: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"https://claude\.ai/magic-link#[^"'\s<>]+"#).expect("static regex")
});
static CHECK: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)verify you are human|performing security verification|just a moment\.\.\.|hcaptcha|i am human")
        .expect("static regex")
});
static WINDOW_ID: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^w-\d{1,12}$").expect("static regex"));
static REF: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^@[A-Za-z0-9]{1,40}:e\d{1,6}$").expect("static regex"));

/// What the authorize step produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Authorize {
    /// The one-use code the page showed (kept in memory only).
    Code(String),
    /// The profile is signed out of claude.ai.
    SignedOut,
    /// The profile is signed in as another account.
    WrongAccount(Option<String>),
    /// A check only a person may pass is on screen; the window stays open for them.
    Check,
    /// Something else went wrong (a fixed word).
    Failed(&'static str),
}

/// What signing a profile in by mail produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInByMail {
    /// The profile is signed in to claude.ai now.
    SignedIn,
    /// A check only a person may pass is on screen.
    Check,
    /// Something else went wrong (a fixed word).
    Failed(&'static str),
}

/// One page element found through accessibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    /// Its ref for a later action.
    pub reference: String,
    /// Its accessible name.
    pub name: String,
}

/// The waits, shortened in tests.
#[derive(Clone, Copy, Debug)]
pub struct Waits {
    /// Between polls.
    pub poll: Duration,
    /// For the Authorize page to appear.
    pub page: Duration,
    /// For a check to pass by itself before it counts as needing a person.
    pub check_grace: Duration,
    /// For the code page after pressing Authorize.
    pub code: Duration,
    /// For the sign-in mail.
    pub mail: Duration,
}

impl Default for Waits {
    fn default() -> Self {
        Waits {
            poll: Duration::from_millis(1500),
            page: Duration::from_secs(60),
            check_grace: Duration::from_secs(60),
            code: Duration::from_secs(120),
            mail: Duration::from_secs(360),
        }
    }
}

/// Parses `agent-desktop list-windows` for one Chrome process: its usable windows.
pub fn parse_windows(json: &str, pid: u32) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return vec![];
    };
    value["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| {
            w["pid"].as_u64() == Some(u64::from(pid))
                && w["accessible"].as_bool() == Some(true)
                && w["bounds"]["width"].as_f64().unwrap_or(0.0) > 300.0
        })
        .filter_map(|w| w["id"].as_str())
        .filter(|id| WINDOW_ID.is_match(id))
        .map(str::to_owned)
        .collect()
}

/// Parses `agent-desktop find`: the matches.
pub fn parse_find(json: &str) -> Vec<Element> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return vec![];
    };
    value["data"]["matches"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let reference = m["ref_id"].as_str().filter(|r| REF.is_match(r))?;
            Some(Element {
                reference: reference.to_owned(),
                name: m["name"].as_str().unwrap_or("").to_owned(),
            })
        })
        .collect()
}

/// Parses `agent-desktop find` for text: every match's name. Static text has no ref (it cannot
/// be acted on), so it must not be filtered by one.
pub fn parse_names(json: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return vec![];
    };
    value["data"]["matches"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["name"].as_str())
        .map(str::to_owned)
        .collect()
}

/// The account the authorize page says is signed in ("Logged in as <email>").
pub fn signed_in_as(texts: &[String]) -> Option<String> {
    let joined = texts.join(" ");
    let after = joined.split_once("Logged in as")?.1;
    after
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| ".,;:()".contains(c))
                .to_lowercase()
        })
        .find(|w| w.contains('@'))
}

/// The one-use code in a page snapshot.
pub fn code_in(snapshot: &str) -> Option<String> {
    CODE.find(snapshot).map(|m| m.as_str().to_owned())
}

/// The sign-in link in a mail's source (quoted-printable undone).
pub fn magic_link(source: &str) -> Option<String> {
    let flat = source
        .replace("=\r\n", "")
        .replace("=\n", "")
        .replace("=3D", "=")
        .replace("=3d", "=");
    MAGIC.find(&flat).map(|m| m.as_str().replace("&amp;", "&"))
}

/// Whether page text shows a check only a person may pass.
pub fn shows_check(texts: &[String]) -> bool {
    texts.iter().any(|t| CHECK.is_match(t))
}

/// A link that may be opened in a profile: claude.ai pages only, nothing that could escape the
/// redirect file's attribute.
pub fn openable(url: &str) -> bool {
    (url.starts_with("https://claude.ai/") || url.starts_with("https://claude.com/"))
        && url.len() < 8192
        && !url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "\"'<>\\`".contains(c))
}

/// Chrome's cookie time (microseconds since 1601) as Unix milliseconds.
pub fn chrome_time_ms(value: i64) -> Option<i64> {
    (value > 0).then(|| value / 1000 - 11_644_473_600_000)
}

const MAIL_SCRIPT: &str = r#"on run argv
  set toAddr to item 1 of argv
  set cutoff to (date "Thursday, January 1, 1970 at 12:00:00 AM") + ((item 2 of argv) as integer) + (time to GMT)
  tell application "Mail"
    check for new mail
    set best to missing value
    repeat with m in (messages of inbox whose date received > cutoff and sender contains "anthropic")
      if (address of to recipients of m as string) contains toAddr then
        if best is missing value then
          set best to m
        else if date received of m > date received of best then
          set best to m
        end if
      end if
    end repeat
    if best is missing value then return ""
    return source of best
  end tell
end run"#;

/// The browser computer.
pub struct Browser<'a> {
    remote: &'a dyn Remote,
    host: &'a Host,
    profiles: String,
    agent_desktop: String,
    /// The waits.
    pub waits: Waits,
}

impl<'a> Browser<'a> {
    /// A browser driver for this computer and settings.
    pub fn new(
        remote: &'a dyn Remote,
        host: &'a Host,
        profiles_dir: &str,
        agent_desktop: &str,
    ) -> Self {
        let profiles = match profiles_dir.strip_prefix("~/") {
            Some(rest) => format!("\"$HOME\"/{}", sh_quote(rest)),
            None => sh_quote(profiles_dir),
        };
        Browser {
            remote,
            host,
            profiles,
            agent_desktop: sh_quote(agent_desktop),
            waits: Waits::default(),
        }
    }

    fn sh(&self, script: String, stdin: &[u8], secs: u64) -> Option<String> {
        self.remote
            .run(
                self.host,
                &Script::Sh(script),
                stdin,
                Duration::from_secs(secs),
            )
            .ok()
            .filter(|out| out.code == Some(0))
            .map(|out| out.stdout)
    }

    fn profile(&self, email: &str) -> String {
        format!("{}/{}", self.profiles, sh_quote(email))
    }

    /// Opens a claude.ai page in the account's profile (starting its Chrome when needed).
    pub fn open(&self, email: &str, url: &str) -> bool {
        if !safe_email(email) || !openable(url) {
            return false;
        }
        let script = format!(
            r#"umask 077
P={profiles}; D={profile}; R="$HOME/.ccs/claude-renew/run"
mkdir -p "$D" "$R" && chmod 700 "$P" "$R" || exit 90
find "$R" -maxdepth 1 -name 'open.*' -mmin +10 -exec rm -f {{}} + 2>/dev/null
f=$(mktemp "$R/open.XXXXXX") || exit 90
IFS= read -r u
case "$u" in https://claude.ai/*|https://claude.com/*) ;; *) rm -f "$f"; exit 93 ;; esac
mv "$f" "$f.html" && f="$f.html"
printf '<!doctype html><meta http-equiv="refresh" content="0;url=%s">' "$u" > "$f"
open -g -na 'Google Chrome' --args --user-data-dir="$D" --no-first-run --no-default-browser-check "file://$f" || exit 94
( sleep 60; rm -f "$f" ) </dev/null >/dev/null 2>&1 &
echo AACOPENED
"#,
            profiles = self.profiles,
            profile = self.profile(email)
        );
        let mut input = url.as_bytes().to_vec();
        input.push(b'\n');
        self.sh(script, &input, 30)
            .is_some_and(|out| out.contains("AACOPENED"))
    }

    fn pid_script(&self, email: &str) -> String {
        format!(
            r#"D={profile}
ps -axo pid=,command= | awk -v d="--user-data-dir=$D " 'index($0" ", d) && $0 !~ /--type=/ && /Google Chrome/ {{print $1; exit}}'
"#,
            profile = self.profile(email)
        )
    }

    /// The account's Chrome main process, if it runs.
    pub fn pid(&self, email: &str) -> Option<u32> {
        self.sh(self.pid_script(email), b"", 20)?
            .lines()
            .find_map(|l| l.trim().parse().ok())
    }

    /// The account's usable Chrome windows.
    pub fn windows(&self, email: &str) -> (Option<u32>, Vec<String>) {
        let Some(pid) = self.pid(email) else {
            return (None, vec![]);
        };
        let list = self
            .sh(format!("{} list-windows", self.agent_desktop), b"", 30)
            .unwrap_or_default();
        (Some(pid), parse_windows(&list, pid))
    }

    /// Elements with this role and exact name in a window.
    pub fn find(&self, window: &str, role: &str, name: Option<&str>) -> Vec<Element> {
        if !WINDOW_ID.is_match(window) || !role.bytes().all(|b| b.is_ascii_lowercase()) {
            return vec![];
        }
        let mut cmd = format!(
            "{} find --window-id {window} --role {role}",
            self.agent_desktop
        );
        if let Some(name) = name {
            if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b' ') {
                return vec![];
            }
            cmd.push_str(&format!(" --name {} --exact", sh_quote(name)));
        }
        self.sh(cmd, b"", 40)
            .map(|out| parse_find(&out))
            .unwrap_or_default()
    }

    /// The window's visible text.
    pub fn texts(&self, window: &str) -> Vec<String> {
        if !WINDOW_ID.is_match(window) {
            return vec![];
        }
        self.sh(
            format!(
                "{} find --window-id {window} --role statictext",
                self.agent_desktop
            ),
            b"",
            40,
        )
        .map(|out| parse_names(&out))
        .unwrap_or_default()
    }

    /// Presses an element (AXPress).
    pub fn press(&self, element: &Element) -> bool {
        REF.is_match(&element.reference)
            && self
                .sh(
                    format!(
                        "{} click {}",
                        self.agent_desktop,
                        sh_quote(&element.reference)
                    ),
                    b"",
                    40,
                )
                .is_some_and(|out| out.contains("\"ok\":true"))
    }

    /// Sets a text field's value (the account email only).
    pub fn set_email(&self, element: &Element, email: &str) -> bool {
        REF.is_match(&element.reference)
            && safe_email(email)
            && self
                .sh(
                    format!(
                        "{} set-value {} {}",
                        self.agent_desktop,
                        sh_quote(&element.reference),
                        sh_quote(email)
                    ),
                    b"",
                    40,
                )
                .is_some_and(|out| out.contains("\"ok\":true"))
    }

    fn snapshot_code(&self, window: &str) -> Option<String> {
        if !WINDOW_ID.is_match(window) {
            return None;
        }
        let out = self.sh(
            format!("{} snapshot --window-id {window}", self.agent_desktop),
            b"",
            40,
        )?;
        code_in(&out)
    }

    /// Quits the account's Chrome (cookies stay on disk).
    pub fn quit(&self, email: &str) {
        if let Some(pid) = self.pid(email) {
            let _ = self.sh(format!("kill -TERM {pid}; sleep 3"), b"", 20);
        }
    }

    /// When the account's claude.ai browser session ends: the session cookie's expiry, read from
    /// the profile's cookie store (its name and expiry only, never its value).
    pub fn session_end(&self, email: &str) -> Option<i64> {
        if !safe_email(email) {
            return None;
        }
        let script = format!(
            r#"D={profile}
[ -f "$D/Default/Cookies" ] || exit 0
sqlite3 "file:$D/Default/Cookies?immutable=1" "select max(expires_utc) from cookies where host_key like '%claude.ai' and name='sessionKey'"
"#,
            profile = self.profile(email)
        );
        let out = self.sh(script, b"", 20)?;
        out.trim().parse::<i64>().ok().and_then(chrome_time_ms)
    }

    fn sleep(&self) {
        std::thread::sleep(self.waits.poll);
    }

    /// Opens the authorize link in the account's profile, checks the page names this account,
    /// presses Authorize and reads the code the page shows.
    pub fn authorize(&self, email: &str, url: &str) -> Authorize {
        if !self.open(email, url) {
            return Authorize::Failed("browser_open_failed");
        }
        let start = Instant::now();
        let mut check_since: Option<Instant> = None;
        let mut found: Option<(String, Element)> = None;
        let mut signed_out_seen = 0;
        while found.is_none() && start.elapsed() < self.waits.page {
            self.sleep();
            let (_, windows) = self.windows(email);
            let mut check_now = false;
            for w in &windows {
                if let Some(button) = self.find(w, "button", Some("Authorize")).into_iter().next() {
                    found = Some((w.clone(), button));
                    break;
                }
                if !self
                    .find(w, "button", Some("Continue with email"))
                    .is_empty()
                {
                    signed_out_seen += 1;
                }
                if shows_check(&self.texts(w)) {
                    check_now = true;
                }
            }
            if found.is_some() {
                break;
            }
            if signed_out_seen >= 2 {
                self.quit(email);
                return Authorize::SignedOut;
            }
            if check_now {
                let since = *check_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= self.waits.check_grace {
                    return Authorize::Check;
                }
            } else {
                check_since = None;
            }
        }
        let Some((window, button)) = found else {
            self.quit(email);
            return Authorize::Failed("no_authorize_button");
        };
        // The account line renders a moment after the button.
        let mut who = signed_in_as(&self.texts(&window));
        let start = Instant::now();
        while who.is_none() && start.elapsed() < self.waits.page.min(Duration::from_secs(15)) {
            self.sleep();
            who = signed_in_as(&self.texts(&window));
        }
        if who.as_deref() != Some(email) {
            self.quit(email);
            return Authorize::WrongAccount(who);
        }
        if !self.press(&button) {
            self.quit(email);
            return Authorize::Failed("press_failed");
        }
        let start = Instant::now();
        let mut check_since: Option<Instant> = None;
        while start.elapsed() < self.waits.code {
            self.sleep();
            let (_, windows) = self.windows(email);
            let mut check_now = false;
            for w in &windows {
                if let Some(code) = self.snapshot_code(w) {
                    self.quit(email);
                    return Authorize::Code(code);
                }
                if shows_check(&self.texts(w)) {
                    check_now = true;
                }
            }
            if check_now {
                let since = *check_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= self.waits.check_grace {
                    return Authorize::Check;
                }
            } else {
                check_since = None;
            }
        }
        self.quit(email);
        Authorize::Failed("no_code")
    }

    /// The newest claude.ai sign-in mail's link for this account since `since_secs`, read from
    /// Mail.app inside the desktop session.
    pub fn mail_link(&self, email: &str, since_secs: i64) -> Option<String> {
        if !safe_email(email) {
            return None;
        }
        let inner = format!(
            "exec osascript - {} {} <<'AAC_MAIL_EOF'\n{MAIL_SCRIPT}\nAAC_MAIL_EOF\n",
            sh_quote(email),
            since_secs
        );
        let script = crate::hostcmd::desktop_as(&inner, 150, crate::hostcmd::Parent::Python3);
        let out = self
            .remote
            .run(self.host, &script, b"", Duration::from_secs(180))
            .ok()?;
        magic_link(&out.stdout)
    }

    /// Signs a signed-out profile in to claude.ai: enters the email, asks for the sign-in mail,
    /// reads its link from Mail.app and opens it. A check that does not pass by itself is left on
    /// screen for a person.
    pub fn sign_in_by_mail(&self, email: &str, now_secs: impl Fn() -> i64) -> SignInByMail {
        if !self.open(email, "https://claude.ai/login") {
            return SignInByMail::Failed("browser_open_failed");
        }
        let start = Instant::now();
        let mut asked = false;
        while start.elapsed() < self.waits.page && !asked {
            self.sleep();
            let (_, windows) = self.windows(email);
            for w in &windows {
                if !self.find(w, "button", Some("Authorize")).is_empty() {
                    return SignInByMail::SignedIn;
                }
                let Some(field) = self.find(w, "textfield", Some("Email")).into_iter().next()
                else {
                    if shows_check(&self.texts(w)) && start.elapsed() >= self.waits.check_grace {
                        return SignInByMail::Check;
                    }
                    continue;
                };
                let Some(button) = self
                    .find(w, "button", Some("Continue with email"))
                    .into_iter()
                    .next()
                else {
                    continue;
                };
                if self.set_email(&field, email) && self.press(&button) {
                    asked = true;
                    break;
                }
            }
        }
        if !asked {
            self.quit(email);
            return SignInByMail::Failed("no_email_box");
        }
        let since = now_secs() - 30;
        let start = Instant::now();
        let mut link = None;
        while link.is_none() && start.elapsed() < self.waits.mail {
            std::thread::sleep(self.waits.poll * 4);
            link = self.mail_link(email, since);
        }
        let Some(link) = link else {
            self.quit(email);
            return SignInByMail::Failed("mail_link_missing");
        };
        let opened = self.open(email, &link);
        drop(link);
        if !opened {
            return SignInByMail::Failed("browser_open_failed");
        }
        let start = Instant::now();
        while start.elapsed() < self.waits.page {
            self.sleep();
            let (_, windows) = self.windows(email);
            let checks = windows.iter().any(|w| shows_check(&self.texts(w)));
            if checks && start.elapsed() >= self.waits.check_grace {
                return SignInByMail::Check;
            }
            let login_box = windows
                .iter()
                .any(|w| !self.find(w, "textfield", Some("Email")).is_empty());
            if !checks && !login_box && !windows.is_empty() && start.elapsed() > self.waits.poll * 4
            {
                // Signed in: quit so the session cookie is written to disk.
                self.quit(email);
                return SignInByMail::SignedIn;
            }
        }
        self.quit(email);
        SignInByMail::Failed("not_signed_in_after_link")
    }

    /// Opens claude.ai briefly so the account's browser session keeps sliding; returns the
    /// session's end before and after.
    pub fn visit(&self, email: &str) -> (Option<i64>, Option<i64>) {
        let before = self.session_end(email);
        if before.is_none() || !self.open(email, "https://claude.ai/new") {
            return (before, before);
        }
        std::thread::sleep(self.waits.poll * 6);
        self.quit(email);
        (before, self.session_end(email))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Os;
    use crate::remote::{Output, RemoteError, Session};
    use std::sync::Mutex;

    #[test]
    fn parsers() {
        let windows = r#"{"ok":true,"data":[
          {"id":"w-1","pid":7,"accessible":true,"bounds":{"width":1200.0}},
          {"id":"w-2","pid":7,"accessible":false,"bounds":{"width":1710.0}},
          {"id":"w-3","pid":7,"accessible":true,"bounds":{"width":1.0}},
          {"id":"w-4","pid":8,"accessible":true,"bounds":{"width":1200.0}},
          {"id":"w-5; rm","pid":7,"accessible":true,"bounds":{"width":1200.0}}]}"#;
        assert_eq!(parse_windows(windows, 7), vec!["w-1".to_owned()]);
        let found = r#"{"ok":true,"data":{"matches":[{"name":"Authorize","ref_id":"@abc123:e1"},{"name":"x","ref_id":"bad ref"}]}}"#;
        assert_eq!(
            parse_find(found),
            vec![Element {
                reference: "@abc123:e1".into(),
                name: "Authorize".into()
            }]
        );
        assert_eq!(
            signed_in_as(&[
                "Claude Code would like to connect".into(),
                "Logged in as".into(),
                "Jared@X.party.".into()
            ]),
            Some("jared@x.party".into())
        );
        assert_eq!(signed_in_as(&["nothing".into()]), None);
        let texts = r#"{"ok":true,"data":{"matches":[{"name":"Logged in as ","ref_id":null,"role":"statictext"},{"name":"A@b.com","role":"statictext"}]}}"#;
        assert_eq!(
            parse_names(texts),
            vec!["Logged in as ".to_owned(), "A@b.com".to_owned()]
        );
        assert_eq!(signed_in_as(&parse_names(texts)), Some("a@b.com".into()));
        let code = "abcdefghijABCDEFGHIJ_-12#abcdefghijABCDEFGHIJ_-12";
        assert_eq!(
            code_in(&format!("{{\"value\":\"{code}\"}}")).as_deref(),
            Some(code)
        );
        assert_eq!(
            magic_link("x <a href=3D\"https://claude.ai/magic-link#tok=\nen123&amp;x=1\">")
                .as_deref(),
            Some("https://claude.ai/magic-link#token123&x=1")
        );
        assert!(shows_check(&[
            "Verify you are human by completing the action below.".into()
        ]));
        assert!(!shows_check(&["Logged in as a@b.com".into()]));
        assert!(openable(
            "https://claude.com/cai/oauth/authorize?code=true&x=1"
        ));
        assert!(!openable("https://claude.ai/x\"><script>"));
        assert!(!openable("file:///etc/passwd"));
        assert_eq!(
            chrome_time_ms(13_440_000_000_000_000),
            Some(13_440_000_000_000 - 11_644_473_600_000)
        );
    }

    /// A fake browser computer: answers each script by what it asks for.
    struct Fake {
        log: Mutex<Vec<String>>,
        stage: Mutex<u32>,
        page_email: &'static str,
        signed_out: bool,
        /// Text reads before the account line shows (it renders after the button).
        late_lines: Mutex<u32>,
    }

    impl Remote for Fake {
        fn run(
            &self,
            _: &Host,
            script: &Script,
            stdin: &[u8],
            _: Duration,
        ) -> Result<Output, RemoteError> {
            let Script::Sh(s) = script else {
                return Err(RemoteError::Spawn);
            };
            let ok = |stdout: &str| {
                Ok(Output {
                    code: Some(0),
                    stdout: stdout.into(),
                })
            };
            let mut stage = self.stage.lock().expect("lock");
            if s.contains("open -g -na 'Google Chrome'") {
                assert!(s.contains("--user-data-dir=\"$D\""));
                assert!(
                    !s.contains("remote-debugging"),
                    "never start Chrome with remote debugging"
                );
                assert!(
                    !s.contains("oauth/authorize"),
                    "the link travels on stdin, not in the script"
                );
                self.log
                    .lock()
                    .expect("lock")
                    .push(format!("open {}", String::from_utf8_lossy(stdin).trim()));
                return ok("AACOPENED\n");
            }
            if s.contains("ps -axo") {
                return ok("4242\n");
            }
            if s.contains("list-windows") {
                return ok(
                    r#"{"ok":true,"data":[{"id":"w-9","pid":4242,"accessible":true,"bounds":{"width":1100.0}}]}"#,
                );
            }
            if s.contains("find --window-id w-9 --role button --name 'Authorize'") {
                return if *stage == 0 && !self.signed_out {
                    ok(r#"{"ok":true,"data":{"matches":[{"name":"Authorize","ref_id":"@s1:e4"}]}}"#)
                } else {
                    ok(r#"{"ok":true,"data":{"matches":[]}}"#)
                };
            }
            if s.contains("--name 'Continue with email'") {
                return if self.signed_out {
                    ok(
                        r#"{"ok":true,"data":{"matches":[{"name":"Continue with email","ref_id":"@s1:e5"}]}}"#,
                    )
                } else {
                    ok(r#"{"ok":true,"data":{"matches":[]}}"#)
                };
            }
            if s.contains("--role statictext") {
                let mut late = self.late_lines.lock().expect("lock");
                if *late > 0 {
                    *late -= 1;
                    return ok(
                        r#"{"ok":true,"data":{"matches":[{"name":"Claude Code","ref_id":null}]}}"#,
                    );
                }
                return ok(&format!(
                    r#"{{"ok":true,"data":{{"matches":[{{"name":"Logged in as ","ref_id":null}},{{"name":"{}","ref_id":null}}]}}}}"#,
                    self.page_email
                ));
            }
            if s.contains("click '@s1:e4'") {
                *stage = 1;
                self.log.lock().expect("lock").push("press".into());
                return ok(r#"{"ok":true}"#);
            }
            if s.contains("snapshot --window-id w-9") {
                return ok(if *stage == 1 {
                    r#"{"value":"abcdefghijABCDEFGHIJ1234#abcdefghijABCDEFGHIJ1234"}"#
                } else {
                    "{}"
                });
            }
            if s.contains("kill -TERM 4242") {
                self.log.lock().expect("lock").push("quit".into());
                return ok("");
            }
            ok("")
        }
        fn start(&self, _: &Host, _: &Script, _: bool) -> Result<Box<dyn Session>, RemoteError> {
            Err(RemoteError::Spawn)
        }
    }

    fn mac() -> Host {
        Host {
            name: "mac".into(),
            os: Os::Mac,
            ssh: Some("jared-mac".into()),
            claude: None,
        }
    }

    fn fast(b: &mut Browser) {
        b.waits = Waits {
            poll: Duration::from_millis(1),
            page: Duration::from_millis(200),
            check_grace: Duration::from_millis(50),
            code: Duration::from_millis(200),
            mail: Duration::from_millis(50),
        };
    }

    const URL: &str = "https://claude.com/cai/oauth/authorize?code=true&state=s";

    #[test]
    fn authorize_presses_and_reads_the_code_for_the_right_account() {
        let fake = Fake {
            log: Mutex::new(vec![]),
            stage: Mutex::new(0),
            page_email: "a@b.com",
            signed_out: false,
            late_lines: Mutex::new(0),
        };
        let host = mac();
        let mut b = Browser::new(
            &fake,
            &host,
            "~/.ccs/claude-signin/chrome-plain",
            "/opt/homebrew/bin/agent-desktop",
        );
        fast(&mut b);
        assert_eq!(
            b.authorize("a@b.com", URL),
            Authorize::Code("abcdefghijABCDEFGHIJ1234#abcdefghijABCDEFGHIJ1234".into())
        );
        let log = fake.log.lock().expect("lock").clone();
        assert_eq!(
            log,
            vec![format!("open {URL}"), "press".into(), "quit".into()]
        );
    }

    #[test]
    fn authorize_refuses_another_account_and_reports_signed_out() {
        let host = mac();
        let wrong = Fake {
            log: Mutex::new(vec![]),
            stage: Mutex::new(0),
            page_email: "other@b.com",
            signed_out: false,
            late_lines: Mutex::new(0),
        };
        let mut b = Browser::new(&wrong, &host, "~/p", "/ad");
        fast(&mut b);
        assert_eq!(
            b.authorize("a@b.com", URL),
            Authorize::WrongAccount(Some("other@b.com".into()))
        );
        assert!(
            !wrong
                .log
                .lock()
                .expect("lock")
                .contains(&"press".to_owned())
        );

        let out = Fake {
            log: Mutex::new(vec![]),
            stage: Mutex::new(0),
            page_email: "a@b.com",
            signed_out: true,
            late_lines: Mutex::new(0),
        };
        let mut b = Browser::new(&out, &host, "~/p", "/ad");
        fast(&mut b);
        assert_eq!(b.authorize("a@b.com", URL), Authorize::SignedOut);
        assert_eq!(
            b.authorize("a@b.com", "https://evil.example/x"),
            Authorize::Failed("browser_open_failed")
        );
    }

    #[test]
    fn authorize_waits_for_the_account_line_that_renders_after_the_button() {
        let fake = Fake {
            log: Mutex::new(vec![]),
            stage: Mutex::new(0),
            page_email: "a@b.com",
            signed_out: false,
            late_lines: Mutex::new(3),
        };
        let host = mac();
        let mut b = Browser::new(&fake, &host, "~/p", "/ad");
        fast(&mut b);
        assert_eq!(
            b.authorize("a@b.com", URL),
            Authorize::Code("abcdefghijABCDEFGHIJ1234#abcdefghijABCDEFGHIJ1234".into())
        );
    }
}
