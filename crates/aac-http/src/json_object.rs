//! Raw object-envelope gate shared by every object DTO parsed by the service.
use serde::{
    Deserializer,
    de::{self, DeserializeOwned, IgnoredAny, MapAccess, Visitor},
};
use std::{collections::BTreeSet, fmt};

struct ExactKeys<'a>(&'a [&'a str]);
impl<'de> Visitor<'de> for ExactKeys<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON object with exactly the required unique keys")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut seen = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !self.0.contains(&key.as_str()) || !seen.insert(key) {
                return Err(de::Error::custom("unexpected or duplicate object key"));
            }
            map.next_value::<IgnoredAny>()?;
        }
        if seen.len() != self.0.len() {
            return Err(de::Error::custom("missing object key"));
        }
        Ok(())
    }
}

/// Check the original bytes for map shape/exact unique keys before parsing a DTO.
/// Never normalize through Value: that would collapse duplicates and numeric spelling.
pub(crate) fn from_slice<T: DeserializeOwned>(
    bytes: &[u8],
    keys: &[&str],
) -> serde_json::Result<T> {
    let mut raw = serde_json::Deserializer::from_slice(bytes);
    raw.deserialize_map(ExactKeys(keys))?;
    raw.end()?;
    serde_json::from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static DTO_CALLS: AtomicUsize = AtomicUsize::new(0);
    struct Counted;
    impl<'de> Deserialize<'de> for Counted {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            DTO_CALLS.fetch_add(1, Ordering::SeqCst);
            IgnoredAny::deserialize(d)?;
            Ok(Self)
        }
    }
    #[test]
    fn invalid_object_envelopes_never_reach_the_dto_deserializer() {
        for bytes in [
            r#"[90]"#,
            r#"[[90]]"#,
            "90",
            "null",
            "true",
            r#""90""#,
            "{}",
            r#"{"v":90,"extra":0}"#,
            r#"{"v":30,"v":90}"#,
            r#"{"v":30,"\u0076":90}"#,
            r#"{"v":90} []"#,
        ] {
            assert!(
                from_slice::<Counted>(bytes.as_bytes(), &["v"]).is_err(),
                "{bytes}"
            );
        }
        assert_eq!(DTO_CALLS.load(Ordering::SeqCst), 0);
        assert!(from_slice::<Counted>(b" {\"v\":90} \n", &["v"]).is_ok());
        assert_eq!(DTO_CALLS.load(Ordering::SeqCst), 1);
    }
}
