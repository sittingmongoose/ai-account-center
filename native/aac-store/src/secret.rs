//! Credential refusal for usage and capture text (docs/store.md 2.2). There is one detector in
//! AAC: the shared secret policy (`src/shared/secret-policy.json`, `crate::secret_policy`), read
//! by TypeScript and Rust alike. This module keeps no patterns or thresholds of its own.
//!
//! Usage rows carry account ids, labels and pool names, which may name an email address by
//! design (`claude:<profile id or email>`), so usage text is held to every shared rule except
//! the reading of such an address as personal data: the email scan, and the scheme-less
//! userinfo rule when what it read as `user:password@host` is a provider prefix and an address
//! (`claude:person@example.com`). Agent-report text (runs, threads, captures) uses the policy in
//! full.

use crate::secret_policy::find_secret;

/// The shared policy's name for its email scan.
const EMAIL: &str = "an email address";

/// Whether `text` holds something the shared policy refuses as a credential.
pub fn looks_secret(text: &str) -> bool {
    // `find_secret` answers with the first rule that matches, in policy order, and the email
    // scan is the last rule (a test pins that), so an email answer means no credential rule did.
    match find_secret(text) {
        None => false,
        Some(EMAIL) => false,
        Some(_) => !provider_and_address(text),
    }
}

/// `<provider>:<an email address and nothing else>`, an account id the policy would otherwise
/// read as userinfo. The rest is checked again by the same policy: a credential anywhere in it,
/// a URL (`https://...`), or a host that is no email domain (`agent:pw@localhost`) still refuses.
fn provider_and_address(text: &str) -> bool {
    let Some((provider, rest)) = text.split_once(':') else {
        return false;
    };
    !provider.is_empty()
        && provider
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && !rest.contains(':')
        && !rest.chars().any(char::is_whitespace)
        && find_secret(rest) == Some(EMAIL)
}

#[cfg(test)]
mod tests {
    use super::{EMAIL, looks_secret};

    #[test]
    fn the_email_scan_is_the_policys_last_rule() {
        let policy = crate::secret_policy::load_secret_policy();
        let last = policy["patterns"].as_array().unwrap().last().unwrap();
        assert_eq!(last["kind"], EMAIL);
        assert_eq!(last["scan"], "email");
    }

    #[test]
    fn credential_shapes_are_found_and_ordinary_text_is_not() {
        for secret in [
            "Bearer abcdefgh1234567890",
            "Authorization: Token 0123456789abcdef0123",
            "x Basic YWxhZGRpbjpvcGVu",
            "sk-ant-api03-abcdefghijklmnop",
            "key sk-abcdefghijklmnopqrstuvwxyz012345",
            "xoxb-1234567890-abc",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "github_pat_11ABCDEFGHIJKLMNOPQRST",
            "glpat-abcdefghijklmnopqr",
            "AIzaabcdefghijklmnopqrstuvwxyz012345678",
            "AKIAABCDEFGHIJKLMNOP",
            "eyJabcdefgh.eyJabcdefgh.sig",
            "url?access_token=abcdefgh",
            "secret%3Dabcdefgh",
            "aacd_abcdefghijklmnop",
            "-----BEGIN RSA PRIVATE KEY-----",
            // The shapes the old thresholds let through (recheck 1, H2): a 20-character `sk-`
            // key, URL userinfo on a local host and a short Basic credential.
            "vm.sk-abcdefghijklmnopqrstuvwx",
            "https://agent:synthetic-password123@localhost/path",
            "Basic c3ludGhldGljOnBhc3N3b3JkMTIz",
            // An email next to a credential is still a credential.
            "person@example.com sk-abcdefghijklmnopqrstuvwx",
            "claude:person@example.com sk-abcdefghijklmnopqrstuvwx",
            // Userinfo is refused whenever it is more than a provider and an address.
            "agent:synthetic-password123@localhost",
            "https://agent:synthetic-password123@example.com/path",
            "codex:acct:synthetic-password123@example.com",
            "label agent:synthetic-password123@example.com",
        ] {
            assert!(looks_secret(secret), "{secret}");
        }
        for ordinary in [
            "Claude quota on Mac",
            "Codex token quota",
            "Tokens per day",
            "Bearer token",
            "five_hour",
            "[\"seven_day_opus\",\"rate_limit\",null]",
            "claude-opus-4-5-20251101",
            "sk-short",
            "task-runner",
            "Token requests",
            "Token budget for the week",
            "task-0123456789abcdef0123456789abcdef",
            "risk-0123456789abcdef0123456789abcdef",
            "mask_0123456789abcdef0123456789abcdef_ghp_",
            "Basic plan",
            "Bearer of the weekly quota",
            "secret=short",
            "eyJabc.eyJabc.",
            // Usage keeps account ids that may be addresses.
            "codex:person@example.com",
            "claude:person.name+tag@example.co.uk",
            "person@example.com",
        ] {
            assert!(!looks_secret(ordinary), "{ordinary}");
        }
    }
}
