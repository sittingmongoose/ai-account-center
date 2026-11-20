//! Token handling for one renewal (`src/codex-auth/codex-renewal-tokens.ts`):
//! account checks on the new tokens and the merged file. Every other key of
//! the saved file is preserved, in its original order.

use crate::family::{CodexLoginFingerprint, fingerprint_codex_login};
use crate::jwt::{
    decode_codex_activation_identity, decode_id_token, has_structurally_valid_id_token,
    matches_codex_activation_identity,
};
use crate::planner::CodexRenewalProfileSnapshot;
use crate::refresh::CodexRefreshedTokens;
use crate::types::CodexRenewalReason;
use serde_json::{Map, Value};

fn intersects(
    left: &std::collections::BTreeSet<String>,
    right: &std::collections::BTreeSet<String>,
) -> bool {
    left.iter().any(|value| right.contains(value))
}

fn auth_time_intersects(left: &[f64], right: &[f64]) -> bool {
    left.iter().any(|value| right.contains(value))
}

/// Whether the renewed tokens kept a session id or sign-in time (log only).
pub fn family_markers_kept(before: &CodexLoginFingerprint, after: &CodexLoginFingerprint) -> bool {
    intersects(&before.session_ids, &after.session_ids)
        || auth_time_intersects(&before.auth_times, &after.auth_times)
}

/// `mergeRefreshedTokens`: the new tokens into the parsed file, every other key
/// preserved; `last_refresh` stamped with the renewal time. The bytes are the
/// exact `JSON.stringify(merged, null, 2)` the TS writes.
pub fn merge_refreshed_tokens(parsed: &Value, tokens: &CodexRefreshedTokens, now: i64) -> Vec<u8> {
    let mut merged = parsed.as_object().cloned().unwrap_or_default();
    let saved = merged
        .get("tokens")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut new_tokens = saved;
    if let Some(id_token) = &tokens.id_token {
        new_tokens.insert("id_token".into(), Value::String(id_token.clone()));
    }
    new_tokens.insert(
        "access_token".into(),
        Value::String(tokens.access_token.clone()),
    );
    if let Some(refresh_token) = &tokens.refresh_token {
        new_tokens.insert("refresh_token".into(), Value::String(refresh_token.clone()));
    }
    merged.insert("tokens".into(), Value::Object(new_tokens));
    merged.insert(
        "last_refresh".into(),
        Value::String(crate::jsnum::iso(now).unwrap_or_default()),
    );
    serde_json::to_string_pretty(&Value::Object(merged))
        .unwrap_or_default()
        .into_bytes()
}

/// `checkRefreshedIdentity`: the new login must stay the saved account — same
/// binding, same access-token workspace, and an expiry still in the future.
pub fn check_refreshed_identity(
    profile: &CodexRenewalProfileSnapshot,
    tokens: &CodexRefreshedTokens,
    now: i64,
) -> (Option<CodexRenewalReason>, Option<CodexLoginFingerprint>) {
    let mismatch = (Some(CodexRenewalReason::IdentityMismatch), None);
    let saved = profile
        .parsed
        .as_ref()
        .and_then(|p| p.get("tokens"))
        .and_then(Value::as_object);
    let Some(before) = &profile.fingerprint else {
        return mismatch;
    };
    let Some(binding) = &before.binding else {
        return mismatch;
    };
    if let Some(id_token) = &tokens.id_token {
        let stored_account = saved
            .and_then(|s| s.get("account_id"))
            .cloned()
            .unwrap_or(Value::Null);
        let fresh = decode_codex_activation_identity(id_token, &stored_account);
        if !has_structurally_valid_id_token(id_token)
            || decode_id_token(id_token).email.is_none()
            || fresh.is_none()
            || !matches_codex_activation_identity(binding, fresh.as_ref().expect("checked"))
        {
            return mismatch;
        }
    }
    let mut token_object = Map::new();
    let id_value = tokens
        .id_token
        .clone()
        .map(Value::String)
        .or_else(|| saved.and_then(|s| s.get("id_token").cloned()))
        .unwrap_or(Value::Null);
    token_object.insert("id_token".into(), id_value);
    token_object.insert(
        "access_token".into(),
        Value::String(tokens.access_token.clone()),
    );
    token_object.insert(
        "account_id".into(),
        saved
            .and_then(|s| s.get("account_id").cloned())
            .unwrap_or(Value::Null),
    );
    let mut wrapper = Map::new();
    wrapper.insert("tokens".into(), Value::Object(token_object));
    let Some(after) = fingerprint_codex_login(&Value::Object(wrapper)) else {
        return (Some(CodexRenewalReason::InvalidResponse), None);
    };
    if let Some(access_account) = &after.access_account_id
        && access_account != &binding.account_id
    {
        return mismatch;
    }
    if let Some(expiry) = after.access_expires_at
        && expiry <= now
    {
        return (Some(CodexRenewalReason::InvalidResponse), None);
    }
    (None, Some(after))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_preserves_key_order_and_extra_keys() {
        let parsed = json!({
            "OPENAI_API_KEY": null,
            "auth_mode": "chatgpt",
            "last_refresh": "2026-10-05T12:00:00.000Z",
            "tokens": {"id_token": "old-id", "access_token": "old-access", "refresh_token": "old-rt", "account_id": "acct-a"},
            "unknown_extra": {"kept": true}
        });
        let tokens = CodexRefreshedTokens {
            id_token: None,
            access_token: "new-access".into(),
            refresh_token: Some("new-rt".into()),
        };
        let bytes = merge_refreshed_tokens(&parsed, &tokens, 1_791_460_800_000);
        let text = String::from_utf8(bytes).expect("utf8");
        let merged: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(merged["tokens"]["id_token"], "old-id");
        assert_eq!(merged["tokens"]["access_token"], "new-access");
        assert_eq!(merged["tokens"]["refresh_token"], "new-rt");
        assert_eq!(merged["unknown_extra"]["kept"], true);
        assert_eq!(merged["last_refresh"], "2026-10-08T12:00:00.000Z");
        let keys: Vec<&String> = merged.as_object().expect("object").keys().collect();
        assert_eq!(
            keys,
            vec![
                "OPENAI_API_KEY",
                "auth_mode",
                "last_refresh",
                "tokens",
                "unknown_extra"
            ]
        );
        assert!(text.starts_with("{\n  \"OPENAI_API_KEY\""));
    }
}
