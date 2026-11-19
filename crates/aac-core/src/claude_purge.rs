//! The dashboard-issued purge permit (M4b exit gate): the `claude-host` verb refuses `purge`
//! without a token the dashboard computed for exactly this host, trash folder and expiry.
//! The key is the shared agent transport policy; the permit is an HMAC-SHA256 over
//! `aac-claude-host-purge\0<host>\0<trashName>\0<expiresMs>`, spelled `<expiresMs>.<hex>`.
//! It binds the action the way the route's confirmation does, for the agent boundary:
//! the confirmation itself stays in the front door's memory and is consumed there first.
use crate::{Cause, Failure};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

fn purge_key() -> Option<&'static [u8; 32]> {
    static KEY: LazyLock<Option<[u8; 32]>> = LazyLock::new(|| {
        let table: serde_json::Value = serde_json::from_str(include_str!(
            "../../../config/tables/agent-transport-policy.json"
        ))
        .ok()?;
        let hex = table["claudeHostPurgeKey"].as_str()?;
        if hex.len() != 64 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let mut key = [0u8; 32];
        for (i, byte) in key.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(key)
    });
    KEY.as_ref()
}

/// HMAC-SHA256, the standard ipad/opad construction over the workspace's sha2.
fn hmac_sha256(key: &[u8; 32], message: &[u8]) -> [u8; 32] {
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..32 {
        ipad[i] ^= key[i];
        opad[i] ^= key[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The permit the dashboard issues for one purge (the TS side computes the same text).
pub fn issue(host: &str, trash_name: &str, expires_ms: u64) -> Option<String> {
    let key = purge_key()?;
    let message = format!("aac-claude-host-purge\0{host}\0{trash_name}\0{expires_ms}");
    Some(format!(
        "{expires_ms}.{}",
        hex(&hmac_sha256(key, message.as_bytes()))
    ))
}

/// Verify a presented permit against this host, trash folder and clock. A missing or
/// malformed token is `ConfirmationInvalid`; a live-shaped but expired one is
/// `ConfirmationExpired`. Nothing about the token is logged.
pub fn verify(
    token: Option<&str>,
    host: &str,
    trash_name: &str,
    now_ms: u64,
) -> Result<(), Failure> {
    let invalid = || {
        Failure::new(
            Cause::ConfirmationInvalid,
            "purge requires the dashboard's permit",
        )
    };
    let Some(token) = token else {
        return Err(invalid());
    };
    let Some((expires, mac)) = token.split_once('.') else {
        return Err(invalid());
    };
    if expires.is_empty()
        || expires.len() > 16
        || !expires.bytes().all(|c| c.is_ascii_digit())
        || mac.len() != 64
        || !mac
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(invalid());
    }
    let Ok(expires_ms) = expires.parse::<u64>() else {
        return Err(invalid());
    };
    let Some(expected) = issue(host, trash_name, expires_ms) else {
        return Err(Failure::new(
            Cause::Bug,
            "purge permit policy is unavailable",
        ));
    };
    if expected != token {
        return Err(invalid());
    }
    if now_ms > expires_ms {
        return Err(Failure::new(
            Cause::ConfirmationExpired,
            "purge permit expired",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_bind_host_trash_and_expiry() {
        let token = issue("windows", "party-20261002T080000Z", 1_000_120_000).expect("key");
        assert!(
            verify(
                Some(&token),
                "windows",
                "party-20261002T080000Z",
                1_000_000_000
            )
            .is_ok()
        );
        // Another host or another folder refuses; the expiry is bound into the MAC.
        assert!(verify(Some(&token), "mac", "party-20261002T080000Z", 1_000_000_000).is_err());
        assert!(
            verify(
                Some(&token),
                "windows",
                "other-20261002T080000Z",
                1_000_000_000
            )
            .is_err()
        );
        let later = issue("windows", "party-20261002T080000Z", 1_000_120_001).expect("key");
        assert!(
            verify(
                Some(&later),
                "windows",
                "party-20261002T080000Z",
                1_000_000_000
            )
            .is_ok()
        );
        // A tampered expiry keeps the old MAC: refused.
        let tampered = format!("1000120001.{}", token.split_once('.').expect("mac").1);
        assert!(
            verify(
                Some(&tampered),
                "windows",
                "party-20261002T080000Z",
                1_000_000_000
            )
            .is_err()
        );
        // Expired.
        assert!(
            verify(
                Some(&token),
                "windows",
                "party-20261002T080000Z",
                1_000_120_001
            )
            .is_err()
        );
        // Missing, malformed, forged.
        assert!(verify(None, "windows", "party-20261002T080000Z", 1_000_000_000).is_err());
        assert!(verify(Some(""), "windows", "party-20261002T080000Z", 1_000_000_000).is_err());
        assert!(
            verify(
                Some("1000120000.zzz"),
                "windows",
                "party-20261002T080000Z",
                1_000_000_000
            )
            .is_err()
        );
        let forged = format!("1000120000.{}", "0".repeat(64));
        assert!(
            verify(
                Some(&forged),
                "windows",
                "party-20261002T080000Z",
                1_000_000_000
            )
            .is_err()
        );
    }
}
