use crate::{Cause, Failure};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

macro_rules! identifier {
    ($name:ident, $doc:literal, $valid:expr) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);
        impl $name {
            /// Validate without normalizing, so existing private identifiers remain byte-identical.
            pub fn new(value: impl Into<String>) -> Result<Self, Failure> {
                let value = value.into();
                if !($valid)(&value) {
                    return Err(Failure::new(Cause::Refused, "invalid identifier"));
                }
                Ok(Self(value))
            }
            /// Borrow the validated wire spelling.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

fn slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}
fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

identifier!(
    ComputerId,
    "A stable configured computer id; labels and SSH names are separate.",
    slug
);
identifier!(
    ProviderId,
    "A stable provider id, preserved without case folding.",
    slug
);
identifier!(
    ToolId,
    "A stable tool id; adding a tool does not require an enum migration.",
    slug
);
identifier!(
    AccountKey,
    "The full SHA-256 account identity used by today's Analytics history.",
    |s: &str| hex(s, 64)
);
identifier!(
    SessionKey,
    "The published 16-hex session digest; it contains no raw session id.",
    |s: &str| hex(s, 16)
);
identifier!(
    SessionHash,
    "The full SHA-256 companion of a published session key.",
    |s: &str| hex(s, 64)
);
identifier!(
    EventId,
    "A stable nonempty event identifier; control characters and whitespace are refused.",
    |s: &str| !s.is_empty()
        && s.len() <= 160
        && s.chars().all(|c| !c.is_control() && !c.is_whitespace())
);

/// Return a lowercase SHA-256 digest without depending on formatter implementations of
/// a particular digest crate release. Shared by identity and artifact verification.
pub fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    Sha256::digest(bytes.as_ref())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl SessionHash {
    /// Match analyticsSessionHash: SHA-256 of `aac-session-v1:<tool>:<raw-id>` in UTF-8.
    pub fn derive(tool: &ToolId, session_id: &str) -> Self {
        Self(sha256_hex(format!("aac-session-v1:{tool}:{session_id}")))
    }
}
impl SessionKey {
    /// Match analyticsSessionKey: the first 16 lowercase hex characters of the session hash.
    pub fn derive(tool: &ToolId, session_id: &str) -> Self {
        Self(SessionHash::derive(tool, session_id).0[..16].into())
    }
    /// Match publishedSessionKey's legacy shape-only pass-through rule.
    pub fn published(tool: &ToolId, session_id: &str) -> Self {
        if hex(session_id, 16) {
            Self(session_id.into())
        } else {
            Self::derive(tool, session_id)
        }
    }
}
impl AccountKey {
    /// Match accountAnalyticsIdentity's JSON tuple, including null for an absent address.
    /// The address is hashed immediately and is never retained by this type.
    pub fn derive(provider: &ProviderId, id: &str, address: Option<&str>) -> Result<Self, Failure> {
        let value = (provider.as_str(), id, address.map(str::to_lowercase));
        let bytes = serde_json::to_vec(&value)
            .map_err(|_| Failure::new(Cause::Bug, "identity serialization failed"))?;
        Ok(Self(sha256_hex(bytes)))
    }
}

impl EventId {
    /// Match the existing store envelope id, `<store-short-id>-<16-lowercase-hex-sequence>`.
    /// Sequence remains a u64; JavaScript generators use BigInt to avoid precision loss.
    pub fn from_sequence(short: &str, sequence: u64) -> Result<Self, Failure> {
        if !slug(short) {
            return Err(Failure::new(Cause::Refused, "invalid store short id"));
        }
        Self::new(format!("{short}-{sequence:016x}"))
    }
}
