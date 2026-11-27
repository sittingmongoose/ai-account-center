//! Claude Code sign-in renewal, run by the AI Account Center server.
//!
//! A Claude Code sign-in has a fixed end (`refreshTokenExpiresAt`). The CLI's own token refresh
//! never moves it: on every refresh the CLI stores `min(old, new)`, and Anthropic documents that
//! using Claude Code does not extend a login. Only a new sign-in through the browser does. So
//! renewal stays a browser step, done the way a person would do it:
//!
//! * **The CLI's own login.** For each due sign-in the server runs `claude auth login --claudeai`
//!   for that profile on its computer ([`hostcmd`]), over plain ssh ([`remote`]). On the Mac the
//!   login runs inside the desktop session through a one-shot launchd job, because only that
//!   session can use the Keychain ([`hostcmd::desktop`]).
//! * **A plain Chrome profile.** The printed link opens in that account's own persistent Chrome
//!   profile on the browser computer, started like any Chrome (`open -g -na "Google Chrome"`),
//!   with no remote debugging and no automation flags. Chrome 158 sets `navigator.webdriver` to
//!   true whenever `--remote-debugging-port` is given, which is why the old runner met a CAPTCHA
//!   every time ([`browser`]).
//! * **An accessibility press.** The server checks the page says "Logged in as <email>", presses
//!   Authorize through macOS accessibility (`agent-desktop`, AXPress: a trusted click), reads the
//!   code the page shows and types it into the login prompt. Accessibility is granted to ssh
//!   sessions on the Mac and denied inside launchd jobs, so the server drives this over ssh.
//! * **Proof by reading back.** Success means the saved end moved later, read again with
//!   `claude auth status --json`; a later reading checks that no old process wrote the old
//!   sign-in back ([`plan`]).
//!
//! What replaces each job of the retired Python runner (`scripts/claude-signin`):
//!
//! | Python | Rust |
//! | --- | --- |
//! | `collect`/`_local-status` (reads credentials over ssh) | [`hostcmd::scan`]: `claude auth status --json` per profile |
//! | `due_reason`, `backoff_ok`, `note_failure`, `note_captcha_wait`, `check_verification` | [`plan`] |
//! | `renew_one` + `Prompt` (pty login) | [`tick::renew_one`] + [`remote::Session`] |
//! | `browser_auth.mjs authorize` (DevTools protocol) | [`browser::Browser::authorize`] (plain Chrome + accessibility) |
//! | `browser_auth.mjs request-link`/`open-link`, `sign_in_browser`, Mail.app reader | [`browser::Browser::sign_in_by_mail`] |
//! | `browser_auth.mjs session` (cookie expiry over DevTools) | [`browser::Browser::session_end`] (cookie store, name and expiry only) |
//! | `browser_sessions` daily visit | [`browser::Browser::visit`] |
//! | `publish` (`claude-signins.json`/`.md`) | [`publish`], same schema, so the dashboard and `aac-usage signins` read it unchanged |
//! | `install`/launchd/self-update, `tick` | [`service::run`], started every 10 minutes by the server (`aac-http`) |
//!
//! Nothing here prints, logs or stores a link, code, token or cookie value: links and codes live
//! in memory for one renewal; the state file and the published files carry times, emails,
//! profile and computer names and fixed words only.

pub mod browser;
pub mod config;
pub mod hostcmd;
pub mod plan;
pub mod publish;
pub mod remote;
pub mod service;
pub mod state;
pub mod step3;
pub mod tick;

/// Milliseconds in an hour.
pub const HOUR_MS: i64 = 3_600_000;

/// Milliseconds since the Unix epoch, now.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Replaces anything shaped like a link, a code or a token before text reaches a log.
pub fn scrub(text: &str) -> String {
    static SECRETISH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"https?://\S+|[A-Za-z0-9_-]{20,}#[A-Za-z0-9_-]{20,}|sk-ant\S*|[A-Za-z0-9_+/=-]{32,}",
        )
        .expect("static regex")
    });
    SECRETISH.replace_all(text, "<hidden>").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_hides_links_codes_and_tokens() {
        let text = "open https://claude.ai/oauth/authorize?x=1 code abcdefghijABCDEFGHIJ1234#abcdefghijABCDEFGHIJ1234 sk-ant-oat01-xyz";
        let out = scrub(text);
        assert!(!out.contains("claude.ai"));
        assert!(!out.contains("abcdefghijABCDEFGHIJ1234"));
        assert!(!out.contains("sk-ant"));
        assert!(out.contains("open <hidden> code <hidden>"));
    }
}
