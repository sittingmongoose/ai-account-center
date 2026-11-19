//! Codex login identity from a local `auth.json`, never a provider request. Ports
//! `decode-id-token.ts`, `codex-activation-identity.ts` and `codex-login-file.ts` so the
//! sign-in install callbacks verify the workspace and principal binding in Rust while the
//! caller holds its activation lock. Only display-safe claims and the private binding leave
//! this module; raw token bytes stay in the snapshot the caller compares.
use base64::Engine;
use serde_json::Value;
use std::path::Path;

/// The largest `auth.json` the login reader accepts (`codex-login-file.ts` MAX_AUTH_BYTES).
pub const MAX_AUTH_BYTES: usize = 1024 * 1024;

const OPENAI_AUTH_CLAIM: &str = "https://api.openai.com/auth";
const OPENAI_PROFILE_CLAIM: &str = "https://api.openai.com/profile";

/// The private workspace and principal binding (`CodexActivationIdentity`). Never a public DTO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// The ChatGPT workspace account id.
    pub account_id: String,
    /// The login email at the time the binding was read.
    pub email: String,
    /// The known person user id, when the token carries one.
    pub user_id: Option<String>,
    /// The known subject, when present.
    pub subject: Option<String>,
    /// The known issuer, when present.
    pub issuer: Option<String>,
}

/// A complete Codex login identity (`CodexLoginIdentity`): the display claims and the binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The login email; a login without one is not a login.
    pub email: String,
    /// The display account id, when the token carries one.
    pub account_id: Option<String>,
    /// The display plan type, when the token carries one.
    pub plan: Option<String>,
    /// The workspace and principal binding, or `None` when no consistent workspace claim exists.
    pub binding: Option<Binding>,
}

/// The display-safe claims `decodeIdToken` returns; every field is optional.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Display {
    /// The email, preferring the top-level claim over the profile claim.
    pub email: Option<String>,
    /// The plan type string, when present (an empty string is still present).
    pub plan_type: Option<String>,
    /// The account id string, when present.
    pub account_id: Option<String>,
}

/// Node's lenient base64url decode: indifferent padding and non-canonical trailing bits accepted.
/// Encoding is unpadded, the JWT form.
fn b64() -> base64::engine::GeneralPurpose {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    )
}

fn is_segment(part: &str) -> bool {
    !part.is_empty()
        && part.len() % 4 != 1
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn decode_json_segment(part: &str) -> Option<Value> {
    let bytes = b64().decode(part).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    serde_json::from_str(&text).ok()
}

/// Unverified JWT payload claims, or `None` when the token is not a well-formed JWT
/// (`decodeJwtPayload`): three base64url segments whose header and payload are JSON objects.
pub fn decode_jwt_payload(id_token: &str) -> Option<Value> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|part| is_segment(part)) {
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

/// Whether the token is a well-formed JWT (`hasStructurallyValidIdToken`).
pub fn has_structurally_valid_id_token(id_token: &str) -> bool {
    decode_jwt_payload(id_token).is_some()
}

fn claim_str<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value?.get(key)?.as_str()
}

/// The display-safe claims of an id token (`decodeIdToken`); empty on any parse failure.
pub fn decode_id_token(id_token: &str) -> Display {
    let Some(payload) = decode_jwt_payload(id_token) else {
        return Display::default();
    };
    let auth = payload.get(OPENAI_AUTH_CLAIM);
    let profile = payload.get(OPENAI_PROFILE_CLAIM);
    // `payload.email ?? profileClaim?.email`: null or absent falls back to the profile claim.
    let email = match payload.get("email") {
        Some(value) if !value.is_null() => Some(value),
        _ => profile.and_then(|claim| claim.get("email")),
    };
    let mut display = Display::default();
    if let Some(email) = email.and_then(|value| value.as_str())
        && !email.is_empty()
    {
        display.email = Some(email.to_owned());
    }
    if let Some(plan) = claim_str(auth, "chatgpt_plan_type") {
        display.plan_type = Some(plan.to_owned());
    }
    if let Some(account) = claim_str(auth, "chatgpt_account_id") {
        display.account_id = Some(account.to_owned());
    }
    display
}

fn canonical_id(value: &str) -> bool {
    !value.is_empty() && value.trim() == value
}

fn optional_id(value: &Value) -> bool {
    value.is_null() || value.as_str().is_some_and(canonical_id)
}

/// Decode the workspace and principal binding (`decodeCodexActivationIdentity`). The workspace
/// claim is required; an explicit stored account id must agree with it. `stored_account_id` is
/// the `tokens.account_id` JSON value, or `Value::Null` when absent.
pub fn decode_activation_identity(id_token: &str, stored_account_id: &Value) -> Option<Binding> {
    if !has_structurally_valid_id_token(id_token) {
        return None;
    }
    let payload = decode_jwt_payload(id_token)?;
    let auth = payload.get(OPENAI_AUTH_CLAIM)?;
    if !auth.is_object() {
        return None;
    }
    let null = Value::Null;
    let account_id = auth
        .get("chatgpt_account_id")
        .and_then(|value| value.as_str())
        .filter(|value| canonical_id(value))?;
    if !optional_id(stored_account_id) {
        return None;
    }
    if !stored_account_id.is_null() && stored_account_id.as_str() != Some(account_id) {
        return None;
    }
    let chatgpt_user = auth.get("chatgpt_user_id").unwrap_or(&null);
    let user = auth.get("user_id").unwrap_or(&null);
    let subject = payload.get("sub").unwrap_or(&null);
    let issuer = payload.get("iss").unwrap_or(&null);
    if !optional_id(chatgpt_user)
        || !optional_id(user)
        || !optional_id(subject)
        || !optional_id(issuer)
    {
        return None;
    }
    if !chatgpt_user.is_null() && !user.is_null() && chatgpt_user != user {
        return None;
    }
    let email = decode_id_token(id_token).email?;
    // `claims.chatgpt_user_id ?? claims.user_id`, then truthy fields only.
    let user_id = if !chatgpt_user.is_null() {
        chatgpt_user
    } else {
        user
    };
    let text = |value: &Value| -> Option<String> {
        value
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    Some(Binding {
        account_id: account_id.to_owned(),
        email,
        user_id: text(user_id),
        subject: text(subject),
        issuer: text(issuer),
    })
}

/// Match a known binding to its reread without downgrading known fields
/// (`matchesCodexActivationIdentity`).
pub fn matches_activation_identity(expected: &Binding, actual: &Binding) -> bool {
    if expected.account_id != actual.account_id {
        return false;
    }
    if let Some(user) = &expected.user_id
        && actual.user_id.as_ref() != Some(user)
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

/// Case-insensitive equality of two present strings (`sameText`).
pub fn same_text(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.to_lowercase() == b.to_lowercase(),
        _ => false,
    }
}

/// Whether a new login is a saved profile's own account and person (`sameCodexIdentity`).
/// `meta_email` and `meta_account` are the registry metadata fields, which cover a saved login
/// that can no longer be read.
pub fn same_codex_identity(
    saved: Option<&Identity>,
    meta_email: Option<&str>,
    meta_account: Option<&str>,
    fresh: &Identity,
) -> bool {
    let Some(binding) = &fresh.binding else {
        return false;
    };
    let known_email = saved.map(|s| s.email.as_str()).or(meta_email);
    if let Some(known) = known_email
        && !same_text(Some(known), Some(&fresh.email))
    {
        return false;
    }
    let known_accounts = [
        saved
            .and_then(|s| s.binding.as_ref())
            .map(|b| b.account_id.as_str()),
        saved.and_then(|s| s.account_id.as_deref()),
        meta_account,
    ];
    if known_accounts
        .iter()
        .any(|value| value.is_some_and(|value| value != binding.account_id))
    {
        return false;
    }
    match saved.and_then(|s| s.binding.as_ref()) {
        None => true,
        Some(saved_binding) => matches_activation_identity(saved_binding, binding),
    }
}

/// Read a regular, size-limited, unlinked `auth.json` and decode its login (`readLogin`).
/// `complete` also requires non-empty access and refresh tokens. Returns the identity and the
/// exact bytes, or `None` when the file is not a readable complete login.
pub fn read_login(path: &Path, complete: bool) -> Option<(Identity, Vec<u8>)> {
    let bytes = crate::read_bounded(path, MAX_AUTH_BYTES).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let parsed: Value = serde_json::from_str(&text).ok()?;
    let tokens = parsed.get("tokens")?;
    let token = tokens.get("id_token")?.as_str()?;
    if !has_structurally_valid_id_token(token) {
        return None;
    }
    let display = decode_id_token(token);
    let email = display.email?;
    if complete {
        let present = |key: &str| {
            tokens
                .get(key)
                .and_then(|value| value.as_str())
                .is_some_and(|value| !value.is_empty())
        };
        if !present("access_token") || !present("refresh_token") {
            return None;
        }
    }
    let null = Value::Null;
    let binding = decode_activation_identity(token, tokens.get("account_id").unwrap_or(&null));
    Some((
        Identity {
            email,
            account_id: display.account_id,
            plan: display.plan_type,
            binding,
        },
        bytes,
    ))
}

/// A fresh read for the live-login check: `Ok(Some)` verified, `Ok(None)` absent or unverifiable,
/// `Err` when the hardened read refuses a path TypeScript might still read (a link, foreign owner
/// or oversized file), so the caller declines and TypeScript answers.
fn read_for_live(path: &Path) -> Result<Option<(String, Option<Binding>)>, ()> {
    let bytes = match crate::read_bounded(path, MAX_AUTH_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.cause == aac_core::Cause::NotInstalled => return Ok(None),
        Err(_) => return Err(()),
    };
    let text = String::from_utf8_lossy(&bytes);
    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
        return Ok(None);
    };
    let Some(token) = parsed
        .get("tokens")
        .and_then(|tokens| tokens.get("id_token"))
        .and_then(|token| token.as_str())
    else {
        return Ok(None);
    };
    if !has_structurally_valid_id_token(token) {
        return Ok(None);
    }
    let Some(email) = decode_id_token(token).email else {
        return Ok(None);
    };
    let null = Value::Null;
    let binding =
        decode_activation_identity(token, parsed["tokens"].get("account_id").unwrap_or(&null));
    Ok(Some((email, binding)))
}

fn is_live_login(live: &(String, Option<Binding>), saved: &(String, Option<Binding>)) -> bool {
    match (&live.1, &saved.1) {
        (Some(live), Some(saved)) => {
            matches_activation_identity(live, saved) || matches_activation_identity(saved, live)
        }
        _ => live.0 == saved.0,
    }
}
/// Whether a saved profile holds the live native login (`savedLoginIsLive`): the workspace and
/// principal match in either direction, the email alone only when a binding cannot be read.
/// `None` when a read refuses a path TypeScript may still read, so the caller declines.
pub fn saved_login_is_live_checked(native_auth: &Path, profile_auth: &Path) -> Option<bool> {
    let live = read_for_live(native_auth).ok()?;
    let Some(live) = live else {
        return Some(false);
    };
    let saved = read_for_live(profile_auth).ok()?;
    let Some(saved) = saved else {
        return Some(false);
    };
    Some(is_live_login(&live, &saved))
}

/// `savedLoginIsLive` as a boolean; a refused read answers false. Callers that must match
/// TypeScript use [`saved_login_is_live_checked`] and decline instead.
pub fn saved_login_is_live(native_auth: &Path, profile_auth: &Path) -> bool {
    saved_login_is_live_checked(native_auth, profile_auth).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn part(value: &Value) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
    }

    fn token(email: &str, account: Option<&str>, plan: &str, principal: &Value) -> String {
        let mut auth = json!({"chatgpt_plan_type": plan});
        if let Some(account) = account {
            auth["chatgpt_account_id"] = json!(account);
        }
        if let Some(user) = principal.get("userId") {
            auth["chatgpt_user_id"] = user.clone();
        }
        let mut payload = json!({"email": email, "https://api.openai.com/auth": auth});
        if let Some(sub) = principal.get("sub") {
            payload["sub"] = sub.clone();
        }
        if let Some(iss) = principal.get("iss") {
            payload["iss"] = iss.clone();
        }
        format!("{}.{}.sig", part(&json!({"alg": "none"})), part(&payload))
    }

    #[test]
    fn decodes_display_claims() {
        let id = token("a@b.c", Some("acct-1"), "pro", &json!({}));
        let display = decode_id_token(&id);
        assert_eq!(display.email.as_deref(), Some("a@b.c"));
        assert_eq!(display.account_id.as_deref(), Some("acct-1"));
        assert_eq!(display.plan_type.as_deref(), Some("pro"));
        assert!(has_structurally_valid_id_token(&id));
    }

    #[test]
    fn rejects_malformed_tokens() {
        assert!(!has_structurally_valid_id_token("abc"));
        assert!(!has_structurally_valid_id_token("a.b"));
        assert!(!has_structurally_valid_id_token("a.b.c.d"));
        // A length-1 mod 4 segment is not base64url.
        assert!(!has_structurally_valid_id_token("aaaaa.bbbb.sig"));
        assert_eq!(decode_id_token("not-a-token"), Display::default());
    }

    #[test]
    fn email_falls_back_to_profile_claim() {
        let payload = json!({"https://api.openai.com/profile": {"email": "p@b.c"}});
        let id = format!("{}.{}.sig", part(&json!({"alg": "none"})), part(&payload));
        assert_eq!(decode_id_token(&id).email.as_deref(), Some("p@b.c"));
    }

    #[test]
    fn binding_requires_workspace_and_agreement() {
        let id = token("a@b.c", Some("acct-1"), "pro", &json!({"userId": "user-1"}));
        let binding = decode_activation_identity(&id, &json!("acct-1")).unwrap();
        assert_eq!(binding.account_id, "acct-1");
        assert_eq!(binding.user_id.as_deref(), Some("user-1"));
        // A disagreeing stored account id refuses the binding.
        assert!(decode_activation_identity(&id, &json!("other")).is_none());
        // No workspace claim: no binding.
        let no_account = token("a@b.c", None, "pro", &json!({}));
        assert!(decode_activation_identity(&no_account, &Value::Null).is_none());
    }

    #[test]
    fn identity_match_keeps_known_person() {
        let saved = decode_activation_identity(
            &token("a@b.c", Some("acct-1"), "pro", &json!({"userId": "user-1"})),
            &Value::Null,
        )
        .unwrap();
        let same = decode_activation_identity(
            &token("a@b.c", Some("acct-1"), "pro", &json!({"userId": "user-1"})),
            &Value::Null,
        )
        .unwrap();
        let other_person = decode_activation_identity(
            &token("a@b.c", Some("acct-1"), "pro", &json!({"userId": "user-2"})),
            &Value::Null,
        )
        .unwrap();
        assert!(matches_activation_identity(&saved, &same));
        assert!(!matches_activation_identity(&saved, &other_person));
    }

    #[test]
    fn same_identity_uses_metadata_when_saved_unreadable() {
        let fresh = Identity {
            email: "a@b.c".into(),
            account_id: Some("acct-1".into()),
            plan: Some("pro".into()),
            binding: decode_activation_identity(
                &token("a@b.c", Some("acct-1"), "pro", &json!({})),
                &Value::Null,
            ),
        };
        assert!(same_codex_identity(
            None,
            Some("A@B.C"),
            Some("acct-1"),
            &fresh
        ));
        assert!(!same_codex_identity(None, Some("other@b.c"), None, &fresh));
        assert!(!same_codex_identity(None, None, Some("acct-2"), &fresh));
        let no_binding = Identity {
            binding: None,
            ..fresh.clone()
        };
        assert!(!same_codex_identity(None, None, None, &no_binding));
    }
}
