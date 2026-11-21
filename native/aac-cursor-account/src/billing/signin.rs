//! Read Cursor-owned credentials in place. The token lives only for this process's read.
use crate::{api, util};
use aac_files::read::{Read, ReadPolicy};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_AUTH: u64 = 64 * 1024;

pub(super) struct Session {
    pub account: String,
    pub cookie: String,
    expires: i64,
}

impl Session {
    fn from_token(token: &str) -> Option<Self> {
        if token.len() > 16_384
            || token.split('.').count() != 3
            || !token.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return None;
        }
        let payload = util::base64url_decode(token.split('.').nth(1)?)?;
        let claims: Value = serde_json::from_slice(&payload).ok()?;
        let expires = claims.get("exp")?.as_i64()?;
        let user = claims.get("sub")?.as_str()?.rsplit('|').next()?.trim();
        if user.is_empty() || user.len() > 512 {
            return None;
        }
        let account = api::account_hash(token)?;
        let mut cookie = String::from("WorkosCursorSessionToken=");
        use std::fmt::Write;
        for byte in format!("{user}::{token}").bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                cookie.push(char::from(byte));
            } else {
                write!(cookie, "%{byte:02X}").ok()?;
            }
        }
        Some(Self {
            account,
            cookie,
            expires,
        })
    }
}

fn desktop_file(config: &Path) -> PathBuf {
    config.join("Cursor/User/globalStorage/state.vscdb")
}

fn paths() -> (Option<PathBuf>, Option<PathBuf>) {
    match env::consts::OS {
        "windows" => {
            let config = env::var_os("APPDATA").map(PathBuf::from);
            (
                config.as_deref().map(desktop_file),
                config.map(|path| path.join("cursor/auth.json")),
            )
        }
        "macos" => (
            env::home_dir().map(|home| desktop_file(&home.join("Library/Application Support"))),
            None,
        ),
        _ => {
            let config = env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| env::home_dir().map(|home| home.join(".config")));
            (
                config.as_deref().map(desktop_file),
                config.map(|path| path.join("cursor/auth.json")),
            )
        }
    }
}

fn desktop(path: &Path) -> Option<String> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    db.busy_timeout(Duration::from_secs(2)).ok()?;
    db.execute_batch("PRAGMA query_only=ON;").ok()?;
    db.query_row(
        "SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken' AND length(value) <= 16384",
        [],
        |row| Ok(std::str::from_utf8(row.get_ref(0)?.as_bytes()?).ok().map(str::to_owned)),
    )
    .ok()?
}

/// Cursor CLI's own sign-in file (another tool's data): a regular file of at most 64 KiB, read
/// through the shared reader, which refuses a link at the name and a FIFO swapped in.
fn cli(path: &Path) -> Option<String> {
    let Read::Bytes(bytes, _) = aac_files::read::read(path, &ReadPolicy::capped(MAX_AUTH)) else {
        return None;
    };
    let auth: Value = serde_json::from_slice(&bytes).ok()?;
    auth.get("accessToken")?.as_str().map(str::to_owned)
}

pub(super) fn load(test_root: Option<&Path>, now: i64) -> Result<Option<Session>, ()> {
    let (desktop_path, cli_path) = match test_root {
        Some(root) => (Some(desktop_file(root)), Some(root.join("auth.json"))),
        None => paths(),
    };
    let tokens = [
        desktop_path.as_deref().and_then(desktop),
        cli_path.as_deref().and_then(cli),
    ];
    let mut expired_or_invalid = false;
    let latest = tokens
        .into_iter()
        .flatten()
        .filter_map(|token| {
            let session = Session::from_token(&token).filter(|session| session.expires > now / 1000);
            expired_or_invalid |= session.is_none();
            session
        })
        .max_by_key(|session| session.expires);
    match latest {
        None if expired_or_invalid => Err(()),
        value => Ok(value),
    }
}
