//! Pure reset behavior supplied by each supported provider.
use crate::reset::{Grant, Result, Snapshot};
use serde_json::{Value, json};

pub trait Rules: Sync {
    fn kind(&self, g: &Grant) -> Option<(&'static str, &'static str)>;
    fn grant_id_refusal(&self, _g: &Grant) -> Option<&'static str> {
        None
    }
    fn identity_refusal(&self, _s: &Snapshot) -> Option<&'static str> {
        None
    }
    fn unusable_reason(&self) -> &'static str {
        "grant-unavailable"
    }
    fn blocked(&self, s: &Snapshot, g: &Grant) -> Option<&'static str>;
    fn write(&self, s: &Snapshot, g: &Grant, attempt: &str) -> Result<(&'static str, Value)>;
    fn verify(&self, before: &Snapshot, g: &Grant, after: &Snapshot, provider_said_spent: bool) -> bool;
    fn unspent(&self, _before: &Snapshot, _g: &Grant, _after: &Snapshot, _since_send_ms: i64) -> bool {
        false
    }
}

pub fn rules(provider: &str) -> Option<&'static dyn Rules> {
    match provider {
        "claude" => Some(&Claude),
        "codex" => Some(&Codex),
        "zai" => Some(&crate::reset_zai::Zai),
        _ => None,
    }
}

struct Claude;
impl Rules for Claude {
    fn kind(&self, g: &Grant) -> Option<(&'static str, &'static str)> {
        match g.kind.as_str() {
            "session" => Some(("5-hour reset", "Refills your 5-hour allowance.")),
            "weekly" => Some(("Weekly reset", "Refills your weekly allowance.")),
            "full" => Some(("Full reset", "Refills your 5-hour and weekly allowances.")),
            "fable" => Some((
                "Fable reset",
                "Refills the Fable limit and restores the portion of your weekly allowance used by Fable.",
            )),
            _ => None,
        }
    }
    fn grant_id_refusal(&self, g: &Grant) -> Option<&'static str> {
        if g.id.as_ref().is_some_and(|id| {
            id.len() > 40
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        }) {
            return Some("unsupported-grant-id");
        }
        None
    }
    fn identity_refusal(&self, s: &Snapshot) -> Option<&'static str> {
        s.identity.organization.is_empty().then_some("identity-unconfirmed")
    }
    fn blocked(&self, s: &Snapshot, g: &Grant) -> Option<&'static str> {
        let permits_unobserved = matches!(g.kind.as_str(), "full" | "fable");
        if (permits_unobserved || g.kind == "weekly") && s.windows.get("seven_day") == Some(&0.0) {
            return Some("no-weekly-usage");
        }
        if g.kind == "fable" && s.windows.get("seven_day_overage_included") == Some(&0.0) {
            return Some("no-fable-usage");
        }
        let missing_required_window = if permits_unobserved {
            !s.windows.contains_key("seven_day")
        } else {
            g.clears.iter().any(|k| !s.windows.contains_key(k))
        };
        if g.clears.is_empty() || missing_required_window {
            return Some("refill-windows-unconfirmed");
        }
        None
    }
    fn write(&self, _s: &Snapshot, g: &Grant, key: &str) -> Result<(&'static str, Value)> {
        let id = g.id.as_deref().ok_or("exact-grant-unavailable")?;
        if id.len() > 40
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        {
            return Err("exact-grant-unavailable");
        }
        Ok((
            "claude.reset_rate_limits",
            json!({"program":"cedar_ember","grant_id":id,"request_id":key}),
        ))
    }
    fn verify(&self, before: &Snapshot, g: &Grant, after: &Snapshot, _provider_said_spent: bool) -> bool {
        let Some(new) = after.grants.iter().find(|n| n.id == g.id) else {
            return false;
        };
        let spent = new.left.checked_add(1) == Some(g.left);
        if matches!(g.kind.as_str(), "full" | "fable") {
            let readings = |key: &str| before.windows.get(key).zip(after.windows.get(key));
            if !spent || !readings("seven_day").is_some_and(|(b, a)| a < b) {
                return false;
            }
            if g.kind == "fable" {
                // Scoped Fable and overall weekly percentages describe different allowances.
                return readings("seven_day_overage_included").is_none_or(|(b, a)| a < b);
            }
            // A limit absent from either read cannot supply refill evidence. Already-empty limits can stay empty.
            return g
                .clears
                .iter()
                .all(|key| readings(key).is_none_or(|(b, a)| a < b || *b == 0.0 && *a == 0.0));
        }
        spent
            && !g.clears.is_empty()
            && g.clears
                .iter()
                .all(|k| match (before.windows.get(k), after.windows.get(k)) {
                    (Some(b), Some(a)) => a <= b,
                    _ => false,
                })
            && g.clears
                .iter()
                .any(|k| match (before.windows.get(k), after.windows.get(k)) {
                    (Some(b), Some(a)) => a < b,
                    _ => false,
                })
    }
}

struct Codex;
impl Rules for Codex {
    fn kind(&self, g: &Grant) -> Option<(&'static str, &'static str)> {
        (g.kind == "banked").then_some(("Banked reset", "Refills the eligible Codex usage windows."))
    }
    fn unusable_reason(&self) -> &'static str {
        "redeeming"
    }
    fn blocked(&self, s: &Snapshot, g: &Grant) -> Option<&'static str> {
        (g.clears.is_empty() || g.clears.iter().any(|k| !s.windows.contains_key(k)))
            .then_some("refill-windows-unconfirmed")
    }
    fn write(&self, _s: &Snapshot, g: &Grant, key: &str) -> Result<(&'static str, Value)> {
        let id = g.id.as_deref().ok_or("exact-grant-unavailable")?;
        Ok((
            "account/rateLimitResetCredit/consume",
            json!({"creditId":id,"idempotencyKey":key}),
        ))
    }
    fn verify(&self, _before: &Snapshot, g: &Grant, after: &Snapshot, provider_said_spent: bool) -> bool {
        // A missing grant alone is not proof: Claude needs an explicit chosen-grant decrement. OpenAI, though,
        // removes a redeemed Codex credit from its list (seen live on 2026-11-16: the claim refilled the weekly
        // window and the credit vanished, and the account then stayed locked as unconfirmed). So for Codex, a
        // credit OpenAI itself answered `reset` or `alreadyRedeemed` for, gone from a newer read of the same
        // identity, was spent by this attempt.
        let Some(new) = after.grants.iter().find(|n| n.id == g.id) else {
            return provider_said_spent;
        };
        let spent = new.left.checked_add(1) == Some(g.left);
        // OpenAI chooses which limits a credit refills, and a credit can be used at any usage, so no window
        // movement is required: the exact credit `redeemed` is the proof. Windows that fell are reported as
        // evidence by the receipt. A `redeeming` credit (same count, not usable) is still in progress; it counts
        // only when OpenAI itself answered `reset` or `alreadyRedeemed` for this attempt.
        let redeeming = new.left == g.left && !new.usable;
        spent || (provider_said_spent && redeeming)
    }
}
