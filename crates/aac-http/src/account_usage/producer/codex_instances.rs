//! Central Logins step 3: with the switch on (`logins-step3.json` names the Codex accounts), each
//! saved Codex profile is read only through a login a tool owns on this computer (a T3 instance's
//! own home, or the computer's own `~/.codex`) whose identity is the one the profile's saved copy
//! proved before it was quarantined. Never a saved copy, never the local session files, never
//! another account's login: when no such login answers, the row is unavailable and says why.
use super::{inventory_complete, rows, valid_candidate};
use aac_files::login_rollback::{CodexPin, Switch};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// How the Codex rows are read on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    /// No switch (or a switch without Codex pins): AAC's saved copies, as before step 3.
    Saved,
    /// Through tool-owned logins with these identities.
    Instance(BTreeMap<String, CodexPin>),
    /// The switch is there but cannot be read: every row is unavailable, nothing is read.
    Damaged,
}

/// The mode from the switch read (`None`: the read itself did not finish).
pub(super) fn mode(read: Option<Result<Option<Switch>, aac_core::Failure>>) -> Mode {
    match read {
        Some(Ok(None)) => Mode::Saved,
        Some(Ok(Some(switch))) => switch.codex.map_or(Mode::Saved, Mode::Instance),
        _ => Mode::Damaged,
    }
}

/// Why a row has no reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unavailable {
    Damaged,
    Unpinned,
    NotListed,
    NoLogin,
}

impl Unavailable {
    fn message(self) -> &'static str {
        match self {
            Unavailable::Damaged => {
                "The Central Logins switch file cannot be read, so no Codex login is read."
            }
            Unavailable::Unpinned => {
                "No account was recorded for this profile when its saved copy was retired, so no login is read for it."
            }
            Unavailable::NotListed => "The Codex logins on this computer could not be listed.",
            Unavailable::NoLogin => {
                "No T3 Codex instance on this computer is signed in to this account."
            }
        }
    }
}

fn selectors(row: &Value) -> impl Iterator<Item = &str> {
    row["selectors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

/// A login a tool owns: a T3 home or the computer's own `~/.codex`, and never a place that is
/// also an AAC saved copy (`codex-profile:<name>` other than the bare default).
fn tool_owned(row: &Value) -> bool {
    selectors(row).any(|s| s == "t3" || s == "discover")
        && selectors(row).all(|s| !s.starts_with("codex-profile:") || s == "codex-profile:default")
}

/// The tool-owned login for this pin: same identity kind and key, else none. Several T3
/// instances signed in to one account read the same quota; the lowest locator is used.
pub(super) fn own_login<'a>(
    inventory: &'a Option<Value>,
    pin: &CodexPin,
) -> Result<&'a Value, Unavailable> {
    if !inventory_complete(inventory, "t3") || !inventory_complete(inventory, "discover") {
        return Err(Unavailable::NotListed);
    }
    inventory
        .as_ref()
        .and_then(|raw| raw["candidates"].as_array())
        .into_iter()
        .flatten()
        .filter(|row| {
            tool_owned(row)
                && valid_candidate(row, "codex")
                && row["identity"]["kind"] == "codex-principal-v1"
                && row["identity"]["key"] == pin.identity.as_str()
        })
        .min_by(|a, b| a["locator"].as_str().cmp(&b["locator"].as_str()))
        .ok_or(Unavailable::NoLogin)
}

/// The profile Codex itself is using: the one whose pinned account is the computer's own
/// `~/.codex` login (the latest used, when several are).
pub(super) fn active<'a>(
    inventory: &Option<Value>,
    pins: &BTreeMap<String, CodexPin>,
    profiles: impl Iterator<Item = (&'a str, &'a str)>,
) -> Option<String> {
    let own = inventory
        .as_ref()?
        .get("candidates")?
        .as_array()?
        .iter()
        .find(|row| selectors(row).any(|s| s == "discover") && valid_candidate(row, "codex"))?;
    profiles
        .filter(|(name, _)| {
            pins.get(*name)
                .is_some_and(|pin| own["identity"]["key"] == pin.identity.as_str())
        })
        .max_by(|a, b| a.1.cmp(b.1))
        .map(|(name, _)| name.to_owned())
}

const SOURCE: &str = "Codex T3 login on Ubuntu";

/// The row of a profile that has no reading, with the pinned email when there is one.
pub(super) fn unavailable_row(name: &str, why: Unavailable, pin: Option<&CodexPin>) -> Value {
    let shown = json!({"email": pin.map(|p| p.email.as_str()), "plan": null});
    let mut row = rows::codex(name, false, false, Some(&shown), None, None);
    row["source"] = json!(SOURCE);
    row["message"] = json!(why.message());
    row
}

/// The row read through a tool-owned login. A rejected login says so; nothing renews it here.
pub(super) fn read_row(
    name: &str,
    active: bool,
    login: &Value,
    sample: Option<&Value>,
    failure: Option<&str>,
) -> Value {
    let refused = failure == Some("needs_sign_in")
        || sample.is_some_and(|reading| reading["status"] == "needs_sign_in");
    let mut row = rows::codex(name, active, refused, Some(login), sample, None);
    row["source"] = json!(SOURCE);
    if refused {
        row["message"] =
            json!("OpenAI refused this T3 instance's Codex login; sign in again in that instance.");
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn login(selectors: &[&str], identity: char, locator: char, email: &str) -> Value {
        json!({"locator":key(locator),"selectors":selectors,
            "identity":{"kind":"codex-principal-v1","key":key(identity)},
            "revision":key('c'),"email":email,"plan":"plus"})
    }

    fn pin(identity: char) -> CodexPin {
        CodexPin {
            identity: key(identity),
            email: "work@example.test".into(),
        }
    }

    fn inventory(rows: Vec<Value>) -> Option<Value> {
        Some(json!({"version":1,"action":"inventory","candidates":rows}))
    }

    /// The instance-login read: the T3 instance's own login with the pinned identity is the one
    /// read, never the saved copy of the same account that is still listed.
    #[test]
    fn reads_the_t3_instance_login_with_the_pinned_identity() {
        let found = inventory(vec![
            login(&["codex-profile:work"], 'a', '1', "work@example.test"),
            login(&["t3"], 'a', '7', "work@example.test"),
            login(&["t3"], 'a', '5', "work@example.test"),
        ]);
        let chosen = own_login(&found, &pin('a')).expect("the T3 login");
        assert_eq!(
            chosen["locator"],
            key('5'),
            "lowest locator of the T3 logins"
        );
        assert_eq!(chosen["selectors"], json!(["t3"]));
    }

    /// Never a saved copy, even one listed under a T3 home's selector, and never the local
    /// session files: with only the saved copy, the row is unavailable with the reason.
    #[test]
    fn a_saved_copy_is_never_used_and_no_login_is_unavailable() {
        let saved = inventory(vec![
            login(&["codex-profile:work"], 'a', '1', "work@example.test"),
            login(&["codex-profile:work", "t3"], 'a', '2', "work@example.test"),
        ]);
        assert_eq!(
            own_login(&saved, &pin('a')),
            Err(Unavailable::NoLogin),
            "a saved copy is never read"
        );
        let row = unavailable_row("work", Unavailable::NoLogin, Some(&pin('a')));
        assert_eq!(row["status"], "unavailable");
        assert_eq!(row["windows"], json!([]));
        assert_eq!(row["email"], "work@example.test");
        assert_eq!(row["source"], SOURCE);
        assert_eq!(
            row["message"],
            "No T3 Codex instance on this computer is signed in to this account."
        );
        assert!(row["fetchedAt"].is_null(), "no invented reading");
    }

    /// Another account's login (same email, another workspace or principal) is refused.
    #[test]
    fn another_accounts_login_is_refused() {
        let other = inventory(vec![login(&["t3"], 'b', '1', "work@example.test")]);
        assert_eq!(own_login(&other, &pin('a')), Err(Unavailable::NoLogin));
        let mut wrong_kind = login(&["t3"], 'a', '1', "work@example.test");
        wrong_kind["identity"]["kind"] = json!("claude-account-org-v1");
        assert_eq!(
            own_login(&inventory(vec![wrong_kind]), &pin('a')),
            Err(Unavailable::NoLogin)
        );
    }

    /// The computer's own `~/.codex` is tool-owned too, and marks the active profile.
    #[test]
    fn the_computers_own_codex_login_counts_and_marks_active() {
        let found = inventory(vec![login(
            &["codex-profile:default", "discover"],
            'a',
            '1',
            "work@example.test",
        )]);
        assert!(own_login(&found, &pin('a')).is_ok());
        let pins = BTreeMap::from([
            ("work".to_owned(), pin('a')),
            ("gmail".to_owned(), pin('b')),
        ]);
        let profiles = [("work", "2026-11-01"), ("gmail", "2026-11-20")];
        assert_eq!(
            active(&found, &pins, profiles.into_iter()),
            Some("work".into())
        );
    }

    /// An unlisted inventory, a damaged switch and an unpinned profile read nothing.
    #[test]
    fn unlisted_damaged_or_unpinned_read_nothing() {
        assert_eq!(own_login(&None, &pin('a')), Err(Unavailable::NotListed));
        let partial = Some(json!({"version":1,"action":"inventory","candidates":[],
            "incompleteSelectors":["t3"]}));
        assert_eq!(own_login(&partial, &pin('a')), Err(Unavailable::NotListed));
        let failed = aac_core::Failure::new(aac_core::Cause::Damaged, "bad");
        assert_eq!(mode(Some(Err(failed))), Mode::Damaged);
        assert_eq!(mode(None), Mode::Damaged);
        assert_eq!(mode(Some(Ok(None))), Mode::Saved);
        let row = unavailable_row("work", Unavailable::Unpinned, None);
        assert_eq!(row["status"], "unavailable");
        assert!(row["email"].is_null());
    }

    /// A rejected T3 login says so; nothing renews it.
    #[test]
    fn a_rejected_instance_login_says_so() {
        let chosen = login(&["t3"], 'a', '1', "work@example.test");
        let row = read_row("work", false, &chosen, None, Some("needs_sign_in"));
        assert_eq!(row["status"], "needs_sign_in");
        assert!(
            row["message"]
                .as_str()
                .unwrap_or_default()
                .contains("T3 instance")
        );
        let sample = json!({"status":"ok","windows":[{"key":"five_hour","label":"5h","usedPercent":24.0,
            "remainingPercent":76.0,"resetAt":"2026-11-27T20:00:00.000Z","windowMinutes":300}],
            "fetchedAt":"2026-11-27T18:00:00.000Z","sampledAt":"2026-11-27T18:00:00.000Z"});
        let row = read_row("work", true, &chosen, Some(&sample), None);
        assert_eq!(row["status"], "ok");
        assert_eq!(row["isActive"], true);
        assert_eq!(row["source"], SOURCE);
    }
}
