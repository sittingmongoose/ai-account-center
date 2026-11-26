//! `ai-account-center antigravity ...`: terminal sign-in delegation and retired-verb guard.
//!
//! The managed Antigravity switching runtime (`status`, `runtime refresh`, `recover`) was
//! removed with account switching. `signin <profile>` delegates to the bundled TS sign-in
//! driver; `activate` keeps its retired-command message.

use std::path::Path;

fn out(text: &str) {
    print!("{text}");
}

fn err(text: &str) {
    eprint!("{text}");
}

pub fn help() -> &'static str {
    "AI Account Center Antigravity profiles

Usage
  ai-account-center antigravity signin <profile>

Commands
  signin <profile>   Add a new saved profile, or sign in again to a saved one

Notes
  Runs on Ubuntu in an interactive terminal (over SSH is fine). The official
  Antigravity CLI starts in a private sign-in home: choose Google OAuth, open the
  link in a browser, sign in to the Google account for this profile and paste
  the code back. History and settings are not touched. Sign in again keeps the
  same Google account; when the profile is the live Antigravity login, the live
  login is renewed too. Remove profiles in the dashboard. AI Account Center no
  longer switches the live login (activate is retired); it reads each profile's
  usage.
"
}

pub fn run(args: &[String], ccs: &Path) -> i32 {
    let Some((first, rest)) = args.split_first() else {
        out(help());
        return 0;
    };
    match first.as_str() {
        "help" | "--help" | "-h" => {
            out(help());
            0
        }
        "activate" => {
            out("[X] antigravity activate is retired in AI Account Center.\n");
            out(
                "    Account switching was removed; AI Account Center only reads usage and limits.\n",
            );
            1
        }
        "signin" => {
            if rest.len() != 1 || rest[0].starts_with('-') {
                out("[X] Usage: ai-account-center antigravity signin <profile>\n");
                return 1;
            }
            let mut full = vec!["antigravity".to_string(), "signin".to_string()];
            full.extend(rest.iter().cloned());
            match super::antigravity_signin(&full, ccs) {
                Ok(code) => code,
                Err(message) => {
                    err(&format!("[X] {message}\n"));
                    1
                }
            }
        }
        other => {
            out(&format!(
                "[X] Unknown antigravity command: {other}. Run: ai-account-center antigravity help\n"
            ));
            1
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn retired_runtime_verbs_are_rejected_and_omitted_from_help() {
        let dir = tempfile::tempdir().unwrap();
        let text = help();
        assert!(text.contains("ai-account-center antigravity signin <profile>"));
        assert!(!text.contains("antigravity status"));
        assert!(!text.contains("antigravity runtime refresh"));
        assert!(!text.contains("recover"));
        assert_eq!(run(&["status".into()], dir.path()), 1);
        assert_eq!(run(&["recover".into()], dir.path()), 1);
        assert_eq!(run(&["runtime".into(), "refresh".into()], dir.path()), 1);
        assert_eq!(run(&["activate".into()], dir.path()), 1);
    }
}
