//! The note a saved login's renewal state adds to its Codex dashboard row
//! (`src/web-server/services/codex-renewal-notes.ts`) and the renewal entries
//! map (`codex-renewal-entries.ts`). Messages are fixed strings; only the
//! expiry is formatted.

use crate::jsnum::{iso, parse_date_ms};
use crate::renewal::{CodexProfileRenewalProfileStatus, CodexProfileRenewalStatus};
use crate::types::{CodexRenewalReason, CodexRenewalState};
use serde_json::{Map, Value};
use std::sync::LazyLock;

const HOUR_MS: i64 = 60 * 60_000;
/// A retrying saved login is named on its row once its access token is this
/// close to expiry.
const RETRY_NOTE_WINDOW_MS: i64 = 48 * HOUR_MS;
/// Renewal lead is four days; a login it cannot touch is named from then on.
const STUCK_NOTE_WINDOW_MS: i64 = 96 * HOUR_MS;

/// Skips that AAC cannot clear by itself, so the row says why.
fn stuck_reason(reason: CodexRenewalReason) -> bool {
    matches!(
        reason,
        CodexRenewalReason::SharedFamily
            | CodexRenewalReason::FamilyUnverifiable
            | CodexRenewalReason::Unverifiable
            | CodexRenewalReason::InUse
            | CodexRenewalReason::ProcessScanFailed
            | CodexRenewalReason::LiveUnverifiable
            | CodexRenewalReason::UnsafeFile
    )
}

/// The row note: `needs_sign_in` only when OpenAI rejected the saved login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexRenewalNote {
    /// Set only when OpenAI rejected the saved login.
    pub status: Option<&'static str>,
    /// The fixed message (with the formatted expiry for a retry).
    pub message: String,
}

impl CodexRenewalNote {
    /// The note in the TS key order (status only when present).
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        if let Some(status) = self.status {
            out.insert("status".into(), Value::String(status.into()));
        }
        out.insert("message".into(), Value::String(self.message.clone()));
        Value::Object(out)
    }
}

static ISO_PREFIX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T").expect("static pattern"));

fn iso_time(value: Option<&str>) -> Option<(String, i64)> {
    let value = value?;
    if !ISO_PREFIX.is_match(value) {
        return None;
    }
    let ms = parse_date_ms(value)?;
    Some((iso(ms)?, ms))
}

/// `YYYY-MM-DD HH:MM UTC` for an ISO timestamp.
fn utc_minute(iso_text: &str) -> String {
    format!("{} {} UTC", &iso_text[0..10], &iso_text[11..16])
}

/// `codexRenewalNote`: the note a saved login's renewal state adds to its row,
/// or None to leave the row as it was. `needs_reauth` is the row's flag (the
/// only row field the TS note reads); `now` is the clock the TS reads via
/// Date.now.
pub fn codex_renewal_note(
    needs_reauth: bool,
    entry: &CodexProfileRenewalProfileStatus,
    now: i64,
) -> Option<CodexRenewalNote> {
    if entry.state == CodexRenewalState::Failed {
        return Some(CodexRenewalNote {
            status: Some("needs_sign_in"),
            message: entry.message.to_owned(),
        });
    }
    if needs_reauth
        && (entry.state == CodexRenewalState::Due || entry.state == CodexRenewalState::Renewing)
    {
        return Some(CodexRenewalNote {
            status: None,
            message: "The saved login expired. AAC is renewing it automatically.".into(),
        });
    }
    let (expires_iso, expires_ms) = iso_time(entry.access_expires_at.as_deref())?;
    let until_expiry = expires_ms - now;
    if entry.state == CodexRenewalState::Retrying && until_expiry <= RETRY_NOTE_WINDOW_MS {
        return Some(CodexRenewalNote {
            status: None,
            message: format!(
                "{} The saved login expires {}.",
                entry.message,
                utc_minute(&expires_iso)
            ),
        });
    }
    if entry.state == CodexRenewalState::Skipped
        && stuck_reason(entry.reason)
        && until_expiry <= STUCK_NOTE_WINDOW_MS
    {
        return Some(CodexRenewalNote {
            status: None,
            message: entry.message.to_owned(),
        });
    }
    None
}

/// `readCodexRenewalEntries`: renewal status keyed by profile name; a read that
/// fails yields no entries, so every caller keeps today's output. Map
/// semantics: a repeated name keeps its first position and the last value.
pub fn codex_renewal_entries(
    status: Result<CodexProfileRenewalStatus, ()>,
) -> Vec<(String, CodexProfileRenewalProfileStatus)> {
    let Ok(status) = status else {
        return Vec::new();
    };
    let mut entries: Vec<(String, CodexProfileRenewalProfileStatus)> = Vec::new();
    for profile in status.profiles {
        match entries.iter_mut().find(|(name, _)| *name == profile.name) {
            Some(entry) => entry.1 = profile,
            None => entries.push((profile.name.clone(), profile)),
        }
    }
    entries
}

/// The entries as the dashboard reads them: name -> status JSON.
pub fn entries_to_json(entries: &[(String, CodexProfileRenewalProfileStatus)]) -> Value {
    let mut out = Map::new();
    for (name, profile) in entries {
        out.insert(name.clone(), profile.to_json());
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        state: CodexRenewalState,
        reason: CodexRenewalReason,
        expires: Option<&str>,
    ) -> CodexProfileRenewalProfileStatus {
        CodexProfileRenewalProfileStatus {
            name: "gmail".into(),
            state,
            reason,
            message: reason.message(),
            last_renewed_at: None,
            last_attempt_at: None,
            last_outcome: None,
            next_attempt_at: None,
            access_expires_at: expires.map(str::to_owned),
        }
    }

    // tests/unit/web-server/account-dashboard-codex-renewal.test.ts clock.
    const NOW: i64 = 1_790_856_000_000; // 2026-10-01T12:00:00Z

    #[test]
    fn failed_rows_need_sign_in() {
        let note = codex_renewal_note(
            false,
            &entry(CodexRenewalState::Failed, CodexRenewalReason::Dead, None),
            NOW,
        )
        .expect("note");
        assert_eq!(note.status, Some("needs_sign_in"));
        assert_eq!(note.message, CodexRenewalReason::Dead.message());
    }

    #[test]
    fn retry_names_the_expiry_within_48_hours() {
        let soon = entry(
            CodexRenewalState::Retrying,
            CodexRenewalReason::Transient,
            Some("2026-10-02T08:00:00Z"),
        );
        let note = codex_renewal_note(false, &soon, NOW).expect("note");
        assert_eq!(
            note.message,
            format!(
                "{} The saved login expires 2026-10-02 08:00 UTC.",
                CodexRenewalReason::Transient.message()
            )
        );
        let far = entry(
            CodexRenewalState::Retrying,
            CodexRenewalReason::Transient,
            Some("2026-10-03T12:01:00Z"),
        );
        assert!(codex_renewal_note(false, &far, NOW).is_none());
        let past = entry(
            CodexRenewalState::Retrying,
            CodexRenewalReason::Transient,
            Some("2026-09-30T00:00:00Z"),
        );
        assert!(codex_renewal_note(false, &past, NOW).is_some());
    }

    #[test]
    fn stuck_skips_name_the_reason_within_four_days() {
        for reason in [
            CodexRenewalReason::SharedFamily,
            CodexRenewalReason::FamilyUnverifiable,
            CodexRenewalReason::Unverifiable,
            CodexRenewalReason::InUse,
            CodexRenewalReason::ProcessScanFailed,
            CodexRenewalReason::LiveUnverifiable,
            CodexRenewalReason::UnsafeFile,
        ] {
            let near = entry(
                CodexRenewalState::Skipped,
                reason,
                Some("2026-10-04T12:00:00Z"),
            );
            assert_eq!(
                codex_renewal_note(false, &near, NOW).expect("note").message,
                reason.message()
            );
        }
        let far = entry(
            CodexRenewalState::Skipped,
            CodexRenewalReason::SharedFamily,
            Some("2026-10-06T12:00:00Z"),
        );
        assert!(codex_renewal_note(false, &far, NOW).is_none());
        for reason in [
            CodexRenewalReason::ActivationBusy,
            CodexRenewalReason::Changed,
            CodexRenewalReason::Disabled,
        ] {
            let near = entry(
                CodexRenewalState::Skipped,
                reason,
                Some("2026-10-02T12:00:00Z"),
            );
            assert!(codex_renewal_note(false, &near, NOW).is_none());
        }
    }

    #[test]
    fn needs_reauth_with_due_or_renewing_says_renewal_is_under_way() {
        for state in [CodexRenewalState::Due, CodexRenewalState::Renewing] {
            let note = codex_renewal_note(true, &entry(state, CodexRenewalReason::Due, None), NOW)
                .expect("note");
            assert_eq!(
                note.message,
                "The saved login expired. AAC is renewing it automatically."
            );
        }
        let fresh = entry(CodexRenewalState::Ok, CodexRenewalReason::Fresh, None);
        assert!(codex_renewal_note(true, &fresh, NOW).is_none());
    }
}
