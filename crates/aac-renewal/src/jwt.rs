//! Unverified JWT claim reads for renewal fingerprints and identity guards
//! (`src/codex-auth/decode-id-token.ts`, `codex-activation-identity.ts`).
//! Local consistency only: no signature check, no provider request. Claim text
//! never leaves this module except as the display-safe fields below.

use base64::Engine;
use serde_json::Value;

/// The OpenAI auth claim URI.
pub const OPENAI_AUTH_CLAIM: &str = "https://api.openai.com/auth";
const OPENAI_PROFILE_CLAIM: &str = "https://api.openai.com/profile";

/// Display-safe fields decoded from an id token.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexAccountIdentity {
    /// Top-level email, else the profile claim's.
    pub email: Option<String>,
    /// `chatgpt_plan_type` when it is a string.
    pub plan_type: Option<String>,
    /// `chatgpt_account_id` when it is a string.
    pub account_id: Option<String>,
}

fn is_base64url_segment(part: &str) -> bool {
    !part.is_empty()
        && part.len() % 4 != 1
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn decode_json_segment(part: &str) -> Option<Value> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part)
        .ok()?;
    // Node's Buffer.toString('utf8') replaces invalid sequences; JSON.parse then
    // reads the replacement text. Match that leniency.
    let text = String::from_utf8_lossy(&bytes);
    serde_json::from_str(&text).ok()
}

/// `decodeJwtPayload`: three base64url segments whose header and payload both
/// decode to JSON objects; null on any failure.
pub fn decode_jwt_payload(id_token: &str) -> Option<Value> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|p| is_base64url_segment(p)) {
        return None;
    }
    let header = decode_json_segment(parts[0])?;
    if !header.is_object() {
        return None;
    }
    let payload = decode_json_segment(parts[1])?;
    if !payload.is_object() {
        return None;
    }
    Some(payload)
}

/// `hasStructurallyValidIdToken`.
pub fn has_structurally_valid_id_token(id_token: &str) -> bool {
    decode_jwt_payload(id_token).is_some()
}

fn non_empty_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// `decodeIdToken`: email (top-level first, then the profile claim), plan type
/// and account id; empty on any parse failure.
pub fn decode_id_token(id_token: &str) -> CodexAccountIdentity {
    let Some(payload) = decode_jwt_payload(id_token) else {
        return CodexAccountIdentity::default();
    };
    let auth = payload.get(OPENAI_AUTH_CLAIM).and_then(Value::as_object);
    let profile = payload.get(OPENAI_PROFILE_CLAIM).and_then(Value::as_object);
    // `payload.email ?? profileClaim?.email`: only null/undefined fall through.
    let email_value = match payload.get("email") {
        Some(Value::Null) | None => profile.and_then(|p| p.get("email")),
        other => other,
    };
    CodexAccountIdentity {
        email: non_empty_string(email_value),
        plan_type: auth
            .and_then(|a| a.get("chatgpt_plan_type"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        account_id: auth
            .and_then(|a| a.get("chatgpt_account_id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

/// Private, local auth-file binding. Never part of a public DTO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexActivationIdentity {
    /// The workspace claim (required).
    pub account_id: String,
    /// The decoded email (required).
    pub email: String,
    /// `chatgpt_user_id` or `user_id`, when present.
    pub user_id: Option<String>,
    /// The JWT subject, when present.
    pub subject: Option<String>,
    /// The JWT issuer, when present.
    pub issuer: Option<String>,
}

fn canonical_id(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) if !s.is_empty() && s.trim() == s => Some(s.clone()),
        _ => None,
    }
}

fn optional_id(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        other => canonical_id(other).is_some(),
    }
}

/// `decodeCodexActivationIdentity`: the workspace claim is required, an explicit
/// stored account id must agree, and the person fields must be locally consistent.
pub fn decode_codex_activation_identity(
    id_token: &str,
    stored_account_id: &Value,
) -> Option<CodexActivationIdentity> {
    let payload = decode_jwt_payload(id_token)?;
    let auth = payload.get(OPENAI_AUTH_CLAIM)?.as_object()?;
    let account_id = canonical_id(auth.get("chatgpt_account_id"))?;
    if !optional_id(Some(stored_account_id)) {
        return None;
    }
    if !stored_account_id.is_null() && stored_account_id != &Value::String(account_id.clone()) {
        return None;
    }
    if !optional_id(auth.get("chatgpt_user_id"))
        || !optional_id(auth.get("user_id"))
        || !optional_id(payload.get("sub"))
        || !optional_id(payload.get("iss"))
    {
        return None;
    }
    let chatgpt_user = auth.get("chatgpt_user_id").filter(|v| !v.is_null());
    let user = auth.get("user_id").filter(|v| !v.is_null());
    if let (Some(a), Some(b)) = (chatgpt_user, user)
        && a != b
    {
        return None;
    }
    let email = decode_id_token(id_token).email?;
    let user_id = chatgpt_user
        .or(user)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let subject = payload
        .get("sub")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let issuer = payload
        .get("iss")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Some(CodexActivationIdentity {
        account_id,
        email,
        user_id,
        subject,
        issuer,
    })
}

/// `matchesCodexActivationIdentity`: known person fields must survive and agree;
/// legacy bindings still require the exact email.
pub fn matches_codex_activation_identity(
    expected: &CodexActivationIdentity,
    actual: &CodexActivationIdentity,
) -> bool {
    if expected.account_id != actual.account_id {
        return false;
    }
    if let Some(user_id) = &expected.user_id
        && actual.user_id.as_ref() != Some(user_id)
    {
        return false;
    }
    if let Some(subject) = &expected.subject
        && actual.subject.as_ref() != Some(subject)
    {
        return false;
    }
    if let Some(issuer) = &expected.issuer
        && actual.issuer.as_ref() != Some(issuer)
    {
        return false;
    }
    if expected.user_id.is_some() || (expected.subject.is_some() && expected.issuer.is_some()) {
        return true;
    }
    expected.email == actual.email
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn jwt(payload: &Value) -> String {
        let header = serde_json::json!({"alg": "RS256", "typ": "JWT"});
        format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(payload.to_string()),
            URL_SAFE_NO_PAD.encode("FAKE-SIG-test")
        )
    }

    #[test]
    fn structural_checks_match_the_ts_decoder() {
        assert!(decode_jwt_payload("abc").is_none());
        assert!(decode_jwt_payload("a.b").is_none());
        // A payload that is not an object fails.
        let array_payload = format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode("{\"alg\":\"RS256\"}"),
            URL_SAFE_NO_PAD.encode("[1]"),
            URL_SAFE_NO_PAD.encode("sig")
        );
        assert!(decode_jwt_payload(&array_payload).is_none());
        let token = jwt(&serde_json::json!({"email": "a@example.test"}));
        assert_eq!(
            decode_id_token(&token).email.as_deref(),
            Some("a@example.test")
        );
    }

    #[test]
    fn email_falls_through_only_on_null() {
        let token = jwt(&serde_json::json!({
            "email": null,
            "https://api.openai.com/profile": {"email": "profile@example.test"}
        }));
        assert_eq!(
            decode_id_token(&token).email.as_deref(),
            Some("profile@example.test")
        );
        let token = jwt(&serde_json::json!({
            "email": 42,
            "https://api.openai.com/profile": {"email": "profile@example.test"}
        }));
        assert_eq!(decode_id_token(&token).email, None);
    }

    #[test]
    fn binding_requires_workspace_and_consistent_person() {
        let token = jwt(&serde_json::json!({
            "email": "a@example.test",
            "sub": "user-a",
            "iss": "https://auth.openai.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-a",
                "chatgpt_user_id": "user-a"
            }
        }));
        let binding = decode_codex_activation_identity(&token, &Value::String("acct-a".into()))
            .expect("bind");
        assert_eq!(binding.account_id, "acct-a");
        assert_eq!(binding.user_id.as_deref(), Some("user-a"));
        assert!(
            decode_codex_activation_identity(&token, &Value::String("acct-b".into())).is_none()
        );
        assert!(decode_codex_activation_identity(&token, &Value::Null).is_some());
        let mismatch = jwt(&serde_json::json!({
            "email": "a@example.test",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-a",
                "chatgpt_user_id": "user-a",
                "user_id": "user-b"
            }
        }));
        assert!(decode_codex_activation_identity(&mismatch, &Value::Null).is_none());
    }

    #[test]
    fn matching_keeps_known_person_fields() {
        let base = CodexActivationIdentity {
            account_id: "acct-a".into(),
            email: "a@example.test".into(),
            user_id: Some("user-a".into()),
            subject: None,
            issuer: None,
        };
        let mut renamed = base.clone();
        renamed.email = "new@example.test".into();
        assert!(matches_codex_activation_identity(&base, &renamed));
        let mut other_user = base.clone();
        other_user.user_id = Some("user-b".into());
        assert!(!matches_codex_activation_identity(&base, &other_user));
        let legacy = CodexActivationIdentity {
            user_id: None,
            ..base.clone()
        };
        assert!(!matches_codex_activation_identity(&legacy, &renamed));
        assert!(matches_codex_activation_identity(&legacy, &legacy));
    }
}
