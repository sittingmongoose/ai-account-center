//! Credential refusal for usage and capture text (docs/store.md 2.2). There is one detector in
//! AAC: the shared secret policy (`src/shared/secret-policy.json`, `crate::secret_policy`), read
//! by TypeScript and Rust alike. This module keeps no patterns or thresholds of its own.
//!
//! Usage text is held to the whole policy except its email scan: an address is personal data
//! that usage rows have always kept, not a credential. One field has one exception more: a usage
//! account id may be exactly `<provider>:<email address>` (a Claude account without a profile id
//! is `claude:<email>`), which the policy's scheme-less userinfo rule reads as
//! `user:password@host`. Exactly that shape is allowed, nothing near it (`account_id_looks_secret`).
//! Agent-report text (runs, threads, captures) uses the policy in full.

use crate::secret_policy::find_secret;

/// The shared policy's name for its email scan.
const EMAIL: &str = "an email address";
/// The shared policy's name for its userinfo rules.
const USERINFO: &str = "URL userinfo";

/// The providers a usage account id may start with: `DashboardProvider` in
/// `src/web-server/services/account-dashboard-types.ts`.
pub const ACCOUNT_PROVIDERS: [&str; 9] = [
    "codex",
    "claude",
    "antigravity",
    "muse",
    "cursor",
    "kimi-code",
    "qwen",
    "zai",
    "opencode-go",
];

/// Top-level names of hosts inside a network, never an email domain.
const INTERNAL_TLDS: [&str; 10] = [
    "localhost",
    "localdomain",
    "local",
    "internal",
    "intranet",
    "lan",
    "home",
    "corp",
    "private",
    "arpa",
];

/// Whether usage text holds something the shared policy refuses as a credential.
pub fn looks_secret(text: &str) -> bool {
    // `find_secret` answers with the first rule that matches, in policy order, and the email
    // scan is the last rule (a test pins that), so an email answer means no credential rule did.
    find_secret(text).is_some_and(|kind| kind != EMAIL)
}

/// Whether a usage account id holds something the shared policy refuses as a credential. As
/// `looks_secret`, except that an id that is exactly `<provider>:<local>@<domain>` is not read as
/// userinfo; see `provider_and_address` for "exactly".
pub fn account_id_looks_secret(id: &str) -> bool {
    match find_secret(id) {
        None | Some(EMAIL) => false,
        // The userinfo rules come after every other credential rule, so this answer means no
        // other rule found anything in the whole id.
        Some(USERINFO) => !provider_and_address(id),
        Some(_) => true,
    }
}

/// `<provider>:<local>@<domain>` and nothing else: the whole text, exactly one `:` and one `@`,
/// a provider from `ACCOUNT_PROVIDERS`, a local part of ordinary email characters, and a dotted
/// DNS name for the domain (no localhost or internal name, no IP address, no port, no path). The
/// address alone is checked again by the policy, and must be found as an email and nothing else.
fn provider_and_address(id: &str) -> bool {
    if id.len() > 128 || id.matches(':').count() != 1 || id.matches('@').count() != 1 {
        return false;
    }
    let Some((provider, address)) = id.split_once(':') else {
        return false;
    };
    let Some((local, domain)) = address.split_once('@') else {
        return false;
    };
    ACCOUNT_PROVIDERS.contains(&provider)
        && local_part(local)
        && dns_name(domain)
        && find_secret(address) == Some(EMAIL)
}

/// Letters, digits and `._+-`; at most 64; no leading, trailing or doubled dot. No `%`: an
/// encoded character is not an ordinary address.
fn local_part(local: &str) -> bool {
    !local.is_empty()
        && local.len() <= 64
        && local.bytes().all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
}

/// Two or more labels of letters, digits and inner hyphens; a top-level name of two or more
/// letters (so no IP address) that names no internal network; no label `localhost`.
fn dns_name(domain: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    let Some(top) = labels.last() else {
        return false;
    };
    domain.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
                && !label.eq_ignore_ascii_case("localhost")
        })
        && top.len() >= 2
        && top.bytes().all(|b| b.is_ascii_alphabetic())
        && !INTERNAL_TLDS.iter().any(|t| top.eq_ignore_ascii_case(t))
}

#[cfg(test)]
mod tests {
    use super::{EMAIL, USERINFO, account_id_looks_secret, looks_secret};
    use serde_json::Value as Json;

    #[test]
    fn the_email_scan_is_the_policys_last_rule_and_userinfo_comes_just_before_it() {
        let policy = crate::secret_policy::load_secret_policy();
        let kinds: Vec<&str> = policy["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds.last(), Some(&EMAIL));
        assert_eq!(policy["patterns"].as_array().unwrap().last().unwrap()["scan"], "email");
        // Every userinfo rule follows every other credential rule, so a userinfo answer for a
        // whole id means nothing else matched in it.
        let first_userinfo = kinds.iter().position(|k| *k == USERINFO).unwrap();
        assert!(
            kinds[first_userinfo..].iter().all(|k| *k == USERINFO || *k == EMAIL),
            "{kinds:?}"
        );
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
            // The shapes the old thresholds let through (recheck 1, H2).
            "vm.sk-abcdefghijklmnopqrstuvwx",
            "https://agent:synthetic-password123@localhost/path",
            "Basic c3ludGhldGljOnBhc3N3b3JkMTIz",
            "person@example.com sk-abcdefghijklmnopqrstuvwx",
            // Outside the account id field, a provider and an address is read as userinfo too.
            "claude:person@example.com",
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
            // An address alone is personal data usage rows keep, not a credential.
            "person@example.com",
        ] {
            assert!(!looks_secret(ordinary), "{ordinary}");
        }
    }

    /// The shared table (also run by TypeScript through the module, and end to end in
    /// `tests/store_usage.rs`): the reviewer's witness and every near miss.
    #[test]
    fn account_ids_follow_the_shared_table() {
        let cases: Vec<Json> =
            serde_json::from_str(include_str!("../../../tests/fixtures/usage-account-id-cases.json")).unwrap();
        assert!(cases.len() >= 40, "{}", cases.len());
        for case in &cases {
            let id = case["id"].as_str().unwrap();
            let accepted = case["accepted"].as_bool().unwrap();
            assert_eq!(!account_id_looks_secret(id), accepted, "{}: {id:?}", case["name"]);
            // The exception only ever narrows the policy's userinfo reading: an id the general
            // rule accepts is accepted here too.
            if !looks_secret(id) {
                assert!(accepted, "{}: {id:?}", case["name"]);
            }
        }
    }
}
