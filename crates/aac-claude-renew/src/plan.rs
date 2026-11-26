//! Decisions: which sign-ins are due, how failures back off, and whether a renewed sign-in held.
//! Pure functions over the readings and the saved state, ported from the Python runner.

use crate::HOUR_MS;
use crate::state::ProfileState;

/// A sign-in's state word, as `claude auth status --json` and the saved state describe it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignIn {
    /// Signed in with a known end.
    Ok,
    /// Signed out (the end passed, or the tokens were cleared).
    LoggedOut,
    /// Signed in, but the CLI reports no end.
    NoEndDate,
    /// Never signed in here, or its readers could not tell.
    NotSignedIn,
}

impl SignIn {
    /// The published word.
    pub fn word(self) -> &'static str {
        match self {
            SignIn::Ok => "ok",
            SignIn::LoggedOut => "logged_out",
            SignIn::NoEndDate => "no_end_date",
            SignIn::NotSignedIn => "not_signed_in",
        }
    }
}

/// One reading of one profile on one computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The computer name.
    pub host: String,
    /// The profile name (`default` for `~/.claude`).
    pub profile: String,
    /// Its config folder on that computer.
    pub dir: String,
    /// Whether it is `~/.claude` (no `CLAUDE_CONFIG_DIR`).
    pub default: bool,
    /// The account email, when signed in.
    pub email: Option<String>,
    /// The sign-in's state.
    pub state: SignIn,
    /// When the sign-in ends (ms).
    pub ends_at: Option<i64>,
}

impl Reading {
    /// `host/profile`.
    pub fn key(&self) -> String {
        format!("{}/{}", self.host, self.profile)
    }
}

/// Why a sign-in should be renewed now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    /// Its end is within the renewal window.
    EndsSoon,
    /// It is already signed out.
    LoggedOut,
    /// A renewed sign-in went back to the old one.
    Reverted,
    /// A person asked for it.
    Requested,
}

impl Due {
    /// The log word.
    pub fn word(self) -> &'static str {
        match self {
            Due::EndsSoon => "ends_soon",
            Due::LoggedOut => "logged_out",
            Due::Reverted => "reverted",
            Due::Requested => "requested",
        }
    }
}

/// Why this sign-in should be renewed now, or `None` (`due_reason`).
pub fn due(reading: &Reading, saved: &ProfileState, renew_within_ms: i64, now: i64) -> Option<Due> {
    if matches!(reading.state, SignIn::NotSignedIn | SignIn::NoEndDate) {
        return None;
    }
    // Without an email there is no profile to approve it in.
    reading.email.as_ref()?;
    if saved.reverted_at.unwrap_or(0) > saved.last_renew_at.unwrap_or(0) {
        return Some(Due::Reverted);
    }
    if reading.state == SignIn::LoggedOut {
        return Some(Due::LoggedOut);
    }
    match reading.ends_at {
        Some(end) if end - now < renew_within_ms => Some(Due::EndsSoon),
        _ => None,
    }
}

/// Whether a failed or waiting sign-in may be tried again now (`backoff_ok`).
pub fn may_try(saved: &ProfileState, now: i64) -> bool {
    now >= saved.retry_after.unwrap_or(0)
}

/// Records a failure: 1, 2, 4 hours ..., at most 6 (`note_failure`).
pub fn note_failure(saved: &mut ProfileState, now: i64, reason: &str) {
    saved.failures = saved.failures.saturating_add(1);
    saved.last_result = Some(reason.to_owned());
    saved.last_attempt_at = Some(now);
    let shift = saved.failures.saturating_sub(1).min(10);
    saved.retry_after = Some(now + (6 * HOUR_MS).min(HOUR_MS << shift));
}

/// A check only a person may pass went unsolved: wait instead of reopening claude.ai every run.
/// The wait never passes the point an hour before the sign-in ends (`note_captcha_wait`).
pub fn note_captcha_wait(
    saved: &mut ProfileState,
    now: i64,
    backoff_ms: i64,
    ends_at: Option<i64>,
) {
    saved.last_result = Some("waiting_for_you".into());
    saved.last_attempt_at = Some(now);
    let mut wait = backoff_ms;
    if let Some(end) = ends_at
        && end - HOUR_MS > now
    {
        wait = wait.min((end - HOUR_MS - now).max(HOUR_MS));
    }
    saved.retry_after = Some(now + wait);
}

/// Records a renewal that read back later: when to read it again, and what end to expect.
pub fn note_renewed(
    saved: &mut ProfileState,
    now: i64,
    old_end: Option<i64>,
    new_end: i64,
    verify_after_ms: i64,
    reason: Due,
) {
    saved.last_renew_at = Some(now);
    saved.last_result = Some("renewed".into());
    saved.failures = 0;
    saved.retry_after = None;
    saved.previous_end = old_end;
    saved.expect_end = Some(new_end);
    saved.ends_at = Some(new_end);
    let mut verify = vec![now + verify_after_ms];
    if let Some(old) = old_end
        && old > now
    {
        // A Claude process already running keeps the old sign-in and may write it back when it
        // next refreshes; read once more after the old end.
        verify.push(old + 9 * HOUR_MS);
    }
    verify.sort_unstable();
    verify.dedup();
    saved.verify = verify;
    if reason != Due::Reverted {
        saved.reverts = 0;
    }
}

/// What a later reading says about a renewal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verification {
    /// Nothing to check.
    None,
    /// Still holding; more checks are scheduled.
    Holding,
    /// Every check passed.
    Verified,
    /// It went back to the old sign-in: renew again.
    Reverted,
}

/// Checks a later reading against the end a renewal produced (`check_verification`).
pub fn verify(saved: &mut ProfileState, reading: &Reading, now: i64) -> Verification {
    let Some(expect) = saved.expect_end else {
        return Verification::None;
    };
    let current = reading.ends_at.unwrap_or(0);
    if reading.state == SignIn::LoggedOut || current < expect - 60_000 {
        let mut result = Verification::None;
        if saved.reverted_at.unwrap_or(0) < saved.last_renew_at.unwrap_or(0) {
            saved.reverted_at = Some(now);
            saved.reverts = saved.reverts.saturating_add(1);
            result = Verification::Reverted;
        }
        saved.expect_end = None;
        saved.verify.clear();
        return result;
    }
    saved.verify.retain(|t| *t > now);
    if saved.verify.is_empty() {
        saved.expect_end = None;
        saved.verified_at = Some(now);
        Verification::Verified
    } else {
        Verification::Holding
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn reading(state: SignIn, ends_in_hours: Option<i64>) -> Reading {
        Reading {
            host: "windows".into(),
            profile: "party".into(),
            dir: "C:\\Users\\x\\.claude-t3\\party".into(),
            default: false,
            email: Some("a@b.com".into()),
            state,
            ends_at: ends_in_hours.map(|h| NOW + h * HOUR_MS),
        }
    }

    #[test]
    fn due_within_the_window_only() {
        let saved = ProfileState::default();
        let window = 24 * HOUR_MS;
        assert_eq!(
            due(&reading(SignIn::Ok, Some(23)), &saved, window, NOW),
            Some(Due::EndsSoon)
        );
        assert_eq!(
            due(&reading(SignIn::Ok, Some(25)), &saved, window, NOW),
            None
        );
        assert_eq!(
            due(&reading(SignIn::LoggedOut, None), &saved, window, NOW),
            Some(Due::LoggedOut)
        );
        assert_eq!(
            due(&reading(SignIn::NotSignedIn, Some(1)), &saved, window, NOW),
            None
        );
        assert_eq!(
            due(&reading(SignIn::NoEndDate, None), &saved, window, NOW),
            None
        );
        let mut no_email = reading(SignIn::Ok, Some(1));
        no_email.email = None;
        assert_eq!(due(&no_email, &saved, window, NOW), None);
    }

    #[test]
    fn reverted_beats_the_window() {
        let saved = ProfileState {
            last_renew_at: Some(NOW - 10),
            reverted_at: Some(NOW - 5),
            ..ProfileState::default()
        };
        assert_eq!(
            due(&reading(SignIn::Ok, Some(500)), &saved, 24 * HOUR_MS, NOW),
            Some(Due::Reverted)
        );
    }

    #[test]
    fn failures_back_off_1_2_4_then_6_hours() {
        let mut saved = ProfileState::default();
        let mut waits = vec![];
        for _ in 0..5 {
            note_failure(&mut saved, NOW, "login_failed");
            waits.push((saved.retry_after.unwrap_or(0) - NOW) / HOUR_MS);
        }
        assert_eq!(waits, vec![1, 2, 4, 6, 6]);
        assert!(!may_try(&saved, NOW + 5 * HOUR_MS));
        assert!(may_try(&saved, NOW + 6 * HOUR_MS));
    }

    #[test]
    fn captcha_wait_never_passes_an_hour_before_the_end() {
        let mut saved = ProfileState::default();
        note_captcha_wait(&mut saved, NOW, 12 * HOUR_MS, Some(NOW + 5 * HOUR_MS));
        assert_eq!(saved.retry_after, Some(NOW + 4 * HOUR_MS));
        note_captcha_wait(&mut saved, NOW, 12 * HOUR_MS, Some(NOW + 30 * HOUR_MS));
        assert_eq!(saved.retry_after, Some(NOW + 12 * HOUR_MS));
        note_captcha_wait(&mut saved, NOW, 12 * HOUR_MS, Some(NOW + HOUR_MS / 2));
        assert_eq!(saved.retry_after, Some(NOW + 12 * HOUR_MS));
        assert_eq!(saved.last_result.as_deref(), Some("waiting_for_you"));
    }

    #[test]
    fn renewal_then_verification_passes_or_reverts() {
        let mut saved = ProfileState::default();
        let old = NOW + 20 * HOUR_MS;
        let new = NOW + 30 * 24 * HOUR_MS;
        note_renewed(&mut saved, NOW, Some(old), new, 9 * HOUR_MS, Due::EndsSoon);
        assert_eq!(saved.verify, vec![NOW + 9 * HOUR_MS, old + 9 * HOUR_MS]);
        let mut held = reading(SignIn::Ok, None);
        held.ends_at = Some(new);
        assert_eq!(
            verify(&mut saved, &held, NOW + 10 * HOUR_MS),
            Verification::Holding
        );
        assert_eq!(
            verify(&mut saved, &held, old + 10 * HOUR_MS),
            Verification::Verified
        );
        assert_eq!(saved.expect_end, None);

        let mut again = ProfileState::default();
        note_renewed(&mut again, NOW, Some(old), new, 9 * HOUR_MS, Due::EndsSoon);
        let mut back = reading(SignIn::Ok, None);
        back.ends_at = Some(old);
        assert_eq!(
            verify(&mut again, &back, NOW + 10 * HOUR_MS),
            Verification::Reverted
        );
        assert_eq!(again.reverts, 1);
        assert_eq!(
            due(&back, &again, 24 * HOUR_MS, NOW + 10 * HOUR_MS),
            Some(Due::Reverted)
        );
    }
}
