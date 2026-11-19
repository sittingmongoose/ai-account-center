use aac_core::{Cause, Failure};
use serde_json::{Value, json};

fn home_id(args: &Value) -> Result<Option<&str>, Failure> {
    let invalid = || Failure::new(Cause::Refused, "invalid Muse identity request");
    let object = args
        .as_object()
        .filter(|v| v.len() == 1)
        .ok_or_else(invalid)?;
    match object.get("homeId") {
        Some(Value::Null) => Ok(None),
        Some(Value::String(id))
            if id.len() == 8
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            Ok(Some(id))
        }
        _ => Err(invalid()),
    }
}

/// Read the fixed Muse auth file on the selected Mac. The reply contains only the login email.
pub(crate) fn run(args: &Value) -> Result<Value, Failure> {
    let slot = home_id(args)?;
    #[cfg(target_os = "macos")]
    {
        use std::path::PathBuf;

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| Failure::new(Cause::NotInstalled, "Muse home unavailable"))?;
        let config = match slot {
            Some(id) => home.join(".ccs/muse-homes").join(id),
            None => std::env::var_os("XDG_CONFIG_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config")),
        };
        let bytes = aac_files::read_bounded(&config.join("muse/auth.json"), 256 * 1024)?;
        Ok(json!({"email": email(&bytes)}))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = slot;
        Ok(json!({"fallback":"python"}))
    }
}

#[cfg(any(target_os = "macos", test))]
fn email(bytes: &[u8]) -> Option<String> {
    let auth = serde_json::from_slice::<Value>(bytes).ok()?;
    let email = auth["providers"]["meta"]["user_email"]
        .as_str()?
        .trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{feff}');
    let (local, domain) = email.split_once('@')?;
    if local.is_empty()
        || domain.contains('@')
        || !domain
            .rsplit_once('.')
            .is_some_and(|(name, suffix)| !name.is_empty() && !suffix.is_empty())
        || email.encode_utf16().count() > 254
        || email
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\u{feff}')
    {
        return None;
    }
    Some(email.into())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn only_a_fixed_home_selector_can_choose_the_auth_file() {
        assert_eq!(home_id(&json!({"homeId":null})).unwrap(), None);
        assert_eq!(
            home_id(&json!({"homeId":"9f2c41d0"})).unwrap(),
            Some("9f2c41d0")
        );
        for args in [
            json!({}),
            json!({"homeId":"../../.."}),
            json!({"homeId":null,"path":"/arbitrary"}),
        ] {
            assert!(home_id(&args).is_err());
        }
    }

    #[test]
    fn returns_only_a_valid_meta_email() {
        for (value, expected) in [
            (" fixture@example.test\n", Some("fixture@example.test")),
            ("invalid", None),
            ("with space@example.test", None),
            ("two@@example.test", None),
        ] {
            let bytes =
                json!({"providers":{"meta":{"user_email":value}},"email":"outside@example.test"});
            assert_eq!(email(bytes.to_string().as_bytes()).as_deref(), expected);
        }
        assert_eq!(email(b"{}"), None);
        assert_eq!(email(b"broken"), None);
    }
}
