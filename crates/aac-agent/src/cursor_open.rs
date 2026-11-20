use aac_core::{Cause, Failure};
use serde_json::{Value, json};

pub(crate) async fn run(args: &Value) -> Result<Value, Failure> {
    if args.as_object().is_none_or(|object| !object.is_empty()) {
        return Err(Failure::new(
            Cause::Refused,
            "Cursor Open takes no arguments",
        ));
    }
    #[cfg(target_os = "macos")]
    {
        Ok(json!({"opened": mac::open().await}))
    }
    #[cfg(not(target_os = "macos"))]
    Ok(json!({"fallback":"python"}))
}

/// Same console guard as the dashboard's fixed SSH command. Launching a
/// process in another login session is never attempted.
#[cfg(any(target_os = "macos", test))]
fn console_user(raw: &str) -> Option<&str> {
    let user = raw.trim_end_matches('\n');
    let allowed = !matches!(user, "" | "root" | "loginwindow")
        && user.len() <= 255
        && !user.starts_with('-')
        && !user.contains(['\n', '\r', '\0']);
    allowed.then_some(user)
}

#[cfg(any(target_os = "macos", test))]
fn same_uid(console: &str, current: &str) -> bool {
    let console = console.trim_end_matches('\n');
    let current = current.trim_end_matches('\n');
    !console.is_empty() && console.bytes().all(|b| b.is_ascii_digit()) && console == current
}

#[cfg(target_os = "macos")]
mod mac {
    use std::process::Stdio;
    use tokio::process::Command;

    async fn output(file: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(file)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8(out.stdout).ok())
            .flatten()
    }

    pub(super) async fn open() -> bool {
        let Some(console) = output("/usr/bin/stat", &["-f", "%Su", "/dev/console"]).await else {
            return false;
        };
        let Some(user) = super::console_user(&console) else {
            return false;
        };
        let Some(console_uid) = output("/usr/bin/id", &["-u", user]).await else {
            return false;
        };
        let Some(current_uid) = output("/usr/bin/id", &["-u"]).await else {
            return false;
        };
        if !super::same_uid(&console_uid, &current_uid) {
            return false;
        }
        Command::new("/usr/bin/open")
            .args(["-a", "Cursor"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status()
            .await
            .is_ok_and(|status| status.success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_same_active_console_user_can_open_cursor() {
        assert_eq!(console_user("alice\n"), Some("alice"));
        for refused in [
            "\n",
            "root\n",
            "loginwindow\n",
            "-option\n",
            "first\nsecond\n",
        ] {
            assert_eq!(console_user(refused), None);
        }
        assert!(same_uid("501\n", "501\n"));
        assert!(!same_uid("502\n", "501\n"));
        assert!(!same_uid("\n", "\n"));
    }
}
