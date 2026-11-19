//! Identity cooldown and the wave 67 Claude Desktop cadence for the native coordinator.
use aac_core::{Cause, Failure};
use aac_files::{
    read::{Ancestors, Mode, Owner, Read, ReadPolicy},
    registry::{self, Owners},
};
use aac_views::decline::Decline;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

pub(crate) const ROW: &str = "account-usage-cooldown";
const DAY_MS: f64 = 86_400_000.0;
const MAX_STREAK: u8 = 5;

fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A verified account's gate. The 429 streak survives hold expiry and a dashboard restart.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cooldown {
    /// The next time this identity may be read. Zero is open.
    pub until: f64,
    /// The verified Claude Desktop 429 streak, capped at five.
    pub claude_429_streak: Option<u8>,
}

/// Version 1's on-disk cooldown layout is shared with the TypeScript coordinator.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    version: u32,
    key: String,
    until: f64,
    claude429_streak: Option<serde_json::Value>,
}

/// The identity's last saved gate, preserving an expired streak for the next request.
pub fn persisted(root: &Path, key: &str, now: f64) -> Result<Option<Cooldown>, Decline> {
    if !digest(key) || !now.is_finite() {
        return Err(Decline::Shape);
    }
    let row = registry::row(ROW).map_err(|_| Decline::Internal)?;
    let file = root.join(
        row.file(Some(&format!("cooldown-{key}.json")))
            .map_err(|_| Decline::Shape)?,
    );
    let policy = ReadPolicy::capped(row.max_bytes)
        .mode(Mode::Private)
        .owner(Owner::Mine)
        .ancestors(Ancestors::NoLinkBelow(root));
    match aac_files::read::read(&file, &policy) {
        Read::Absent => Ok(None),
        Read::Bytes(bytes, _) => Ok(decode(&bytes, key, now)),
        _ => Err(Decline::Unsupported),
    }
}

/// Save a hold under the native owner's lease, preserving a longer existing hold.
pub(crate) fn save(owners: &Owners, root: &Path, key: &str, bytes: &[u8]) -> Result<(), Failure> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Hold {
        now: f64,
        until: f64,
        claude429_streak: Option<u8>,
    }
    let refused = || Failure::new(Cause::Refused, "invalid collection cooldown");
    let hold: Hold = crate::json_object::from_slice(bytes, &["now", "until", "claude429Streak"])
        .map_err(|_| refused())?;
    if !digest(key)
        || !hold.now.is_finite()
        || !hold.until.is_finite()
        || !(0.0..=8.64e15).contains(&hold.now)
        || hold.claude429_streak.is_some_and(|n| n > MAX_STREAK)
    {
        return Err(refused());
    }
    let file = format!("cooldown-{key}.json");
    let store = owners.claim(registry::rows()?, ROW, root, Some(&file))?;
    let saved = persisted(root, key, hold.now)
        .map_err(|_| refused())?
        .unwrap_or_default();
    let streak = hold
        .claude429_streak
        .filter(|n| *n != 0 || saved.until <= hold.now);
    let merged = saved.hold(hold.now, hold.until, streak);
    let mut value = json!({"version":1,"key":key,"until":merged.until});
    if let Some(streak) = merged.claude_429_streak {
        value["claude429Streak"] = Value::from(streak);
    }
    store.write_private_cache(&aac_views::js_json::write(&value))
}

fn decode(bytes: &[u8], key: &str, now: f64) -> Option<Cooldown> {
    let record: Record = serde_json::from_slice(bytes).ok()?;
    if record.key != key || record.version != 1 || !(0.0..=now + DAY_MS).contains(&record.until) {
        return None;
    }
    let streak = record.claude429_streak.and_then(|value| {
        let n = value.as_f64()?;
        (n.fract() == 0.0 && (0.0..=f64::from(MAX_STREAK)).contains(&n)).then_some(n as u8)
    });
    Some(Cooldown {
        until: record.until,
        claude_429_streak: streak,
    })
}

impl Cooldown {
    /// Merge the in-process and file holds exactly as the TS reader does.
    pub fn merge(memory: Self, saved: Self, now: f64) -> Self {
        let until = memory.until.max(saved.until);
        Self {
            until: if until > now { until } else { 0.0 },
            claude_429_streak: memory.claude_429_streak.or(saved.claude_429_streak),
        }
    }

    /// Apply a hold without shortening one already held by this provider identity.
    pub fn hold(self, now: f64, requested_until: f64, streak: Option<u8>) -> Self {
        if !now.is_finite() || !requested_until.is_finite() {
            return self;
        }
        Self {
            until: self.until.max(requested_until.clamp(now, now + DAY_MS)),
            claude_429_streak: streak.or(self.claude_429_streak),
        }
    }

    /// Record a Claude Desktop 429 with the original Retry-After floor and profile offset.
    pub fn claude_rate_limited(
        self,
        target_digest: &str,
        now: f64,
        retry_after_seconds: Option<f64>,
    ) -> Result<Self, Decline> {
        if !now.is_finite() || retry_after_seconds.is_some_and(|seconds| !seconds.is_finite()) {
            return Err(Decline::Shape);
        }
        let floor = retry_after_seconds.unwrap_or(60.0).clamp(60.0, 86_400.0);
        let streak = self
            .claude_429_streak
            .unwrap_or(0)
            .saturating_add(1)
            .min(MAX_STREAK);
        let delay = claude_delay_ms(target_digest, floor, streak)?;
        Ok(self.hold(now, now + delay, Some(streak)))
    }

    /// Clear a streak only after a verified, current successful read and an open gate.
    pub fn verified_success(self, now: f64) -> Self {
        if self.until <= now && self.claude_429_streak.is_some_and(|n| n > 0) {
            self.hold(now, now, Some(0))
        } else {
            self
        }
    }
}

/// Wave 67's 120/240/480/960/1800-second backoff, stable digest offset and one-day ceiling.
/// A zero streak is the healthy cadence and still uses the profile offset.
pub fn claude_delay_ms(
    target_digest: &str,
    floor_seconds: f64,
    streak: u8,
) -> Result<f64, Decline> {
    if !digest(target_digest) || !floor_seconds.is_finite() || streak > MAX_STREAK {
        return Err(Decline::Shape);
    }
    let offset =
        u32::from_str_radix(&target_digest[..8], 16).map_err(|_| Decline::Shape)? % 300_000;
    let base = 120_u32
        .saturating_mul(1 << streak.saturating_sub(1))
        .min(1800);
    Ok((floor_seconds.max(f64::from(base)) * 1000.0 + f64::from(offset)).min(DAY_MS))
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &str = "000186a000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn wave_67_floor_offset_and_restart_are_preserved() {
        let now = 1_800_000_000_000.0;
        let hold = Cooldown::default()
            .claude_rate_limited(KEY, now, Some(300.5))
            .expect("delay");
        assert_eq!(hold.until, now + 400_500.0);
        let expired = Cooldown::merge(Cooldown::default(), hold, hold.until + 1.0);
        assert_eq!(expired.until, 0.0);
        assert_eq!(expired.claude_429_streak, Some(1));
        let second = expired
            .claude_rate_limited(KEY, hold.until + 1.0, None)
            .expect("delay");
        assert_eq!(second.until, hold.until + 340_001.0);
        assert_eq!(second.verified_success(now), second);
        assert_eq!(
            second.verified_success(second.until).claude_429_streak,
            Some(0)
        );
        assert_eq!(claude_delay_ms(KEY, 86_400.0, 5), Ok(DAY_MS));
        assert_eq!(claude_delay_ms(KEY, 0.0, 5), Ok(1_900_000.0));
    }
}
