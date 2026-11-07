//! Credential shapes the store refuses to persist (docs/store.md 2.2). The same patterns as the
//! dashboard's log redaction (`src/services/logging/log-redaction.ts`), plus private keys and
//! fine-grained GitHub tokens; matched by hand so the crate takes no regex dependency.

fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// At least `min` bytes from `at` satisfy `class`.
fn run(bytes: &[u8], at: usize, min: usize, class: impl Fn(u8) -> bool) -> bool {
    bytes.len() >= at + min && bytes[at..at + min].iter().all(|&b| class(b))
}

/// Length of the run of `class` bytes from `at`.
fn span(bytes: &[u8], at: usize, class: impl Fn(u8) -> bool) -> usize {
    bytes[at.min(bytes.len())..].iter().take_while(|&&b| class(b)).count()
}

/// Whether `text` holds something shaped like a credential.
pub fn looks_secret(text: &str) -> bool {
    let b = text.as_bytes();
    (0..b.len()).any(|i| secret_at(b, i))
}

fn secret_at(b: &[u8], i: usize) -> bool {
    let rest = &b[i..];
    let after = |prefix: &str| rest.starts_with(prefix.as_bytes()).then(|| i + prefix.len());
    // An authorization scheme followed by a credential.
    for scheme in ["Bearer", "Basic", "Token"] {
        if let Some(at) = after(scheme) {
            let gap = span(b, at, |c| c.is_ascii_whitespace());
            if gap > 0 && span(b, at + gap, |c| !c.is_ascii_whitespace()) >= 8 {
                return true;
            }
        }
    }
    let prefixed: [(&str, usize, fn(u8) -> bool); 9] = [
        ("aacd_", 16, word),
        ("sk-ant-", 16, word),
        ("sk-", 32, word),
        ("glpat-", 18, word),
        ("github_pat_", 20, |c| c.is_ascii_alphanumeric() || c == b'_'),
        ("AIza", 35, word),
        ("AKIA", 16, |c| c.is_ascii_uppercase() || c.is_ascii_digit()),
        ("-----BEGIN", 1, |c| c == b' '),
        ("PRIVATE KEY-----", 0, |_| true),
    ];
    for (prefix, min, class) in prefixed {
        if let Some(at) = after(prefix)
            && run(b, at, min, class)
        {
            return true;
        }
    }
    // Slack tokens: xox[bpoa]-, then 10 or more of [A-Za-z0-9-].
    if rest.len() > 4
        && rest.starts_with(b"xox")
        && b"bpoa".contains(&rest[3])
        && rest[4] == b'-'
        && run(b, i + 5, 10, |c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return true;
    }
    // GitHub tokens: gh[opsu]_, then 36 or more alphanumerics.
    if rest.len() > 3
        && rest.starts_with(b"gh")
        && b"opsu".contains(&rest[2])
        && rest[3] == b'_'
        && run(b, i + 4, 36, |c| c.is_ascii_alphanumeric())
    {
        return true;
    }
    // A JSON web token: eyJ... . eyJ... .
    if let Some(at) = after("eyJ") {
        let first = span(b, at, word);
        let second = at + first + 1;
        if first >= 8
            && b.get(at + first) == Some(&b'.')
            && b[second.min(b.len())..].starts_with(b"eyJ")
            && span(b, second + 3, word) >= 8
            && b.get(second + 3 + span(b, second + 3, word)) == Some(&b'.')
        {
            return true;
        }
    }
    // key=value credentials, plain or URL-encoded.
    for key in [
        "api_key",
        "api-key",
        "apikey",
        "access_token",
        "access-token",
        "refresh_token",
        "refresh-token",
        "secret",
    ] {
        if let Some(at) = after(key) {
            let value = if b[at..].starts_with(b"=") {
                at + 1
            } else if b[at..].starts_with(b"%3D") {
                at + 3
            } else {
                continue;
            };
            let allowed = |c: u8| c.is_ascii_alphanumeric() || b"._~+/=-".contains(&c);
            if run(b, value, 8, allowed) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::looks_secret;

    #[test]
    fn credential_shapes_are_found_and_ordinary_text_is_not() {
        for secret in [
            "Bearer abcdefgh12",
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
            "secret=short",
            "eyJabc.eyJabc.",
        ] {
            assert!(!looks_secret(ordinary), "{ordinary}");
        }
    }
}
