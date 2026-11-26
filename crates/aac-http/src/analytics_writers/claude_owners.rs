//! A Claude mirror owner map (log name -> owning computer), held compactly.
//!
//! The activity pass keeps three of these maps resident (live owners, historical owners and
//! published mirrors), each with an entry for every Claude log ever seen. Log names are
//! SHA-256 hex digests: as `BTreeMap<String, String>` an entry cost about 150 bytes (two heap
//! strings plus the tree slot). Here a digest name is its 32 bytes and the computer is an index
//! into the map's short host list, about 40 bytes an entry. Any other name spelling keeps its
//! text. The JSON form (`{"<name>": "<host>"}`, sorted by name) is unchanged.
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, Serializer};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

type HostId = u16;

#[derive(Clone, Default)]
pub(crate) struct Owners {
    digests: BTreeMap<[u8; 32], HostId>,
    names: BTreeMap<Box<str>, HostId>,
    hosts: Vec<Box<str>>,
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// A lowercase 64-hex name as its 32 bytes.
fn digest_of(name: &str) -> Option<[u8; 32]> {
    let bytes = name.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(bytes.as_chunks::<2>().0) {
        *slot = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}

/// A digest's name, written into `buffer`.
fn spell<'a>(digest: &[u8; 32], buffer: &'a mut [u8; 64]) -> &'a str {
    for (pair, byte) in buffer.as_chunks_mut::<2>().0.iter_mut().zip(digest) {
        pair[0] = HEX[usize::from(byte >> 4)];
        pair[1] = HEX[usize::from(byte & 15)];
    }
    std::str::from_utf8(buffer).expect("hex digits are ASCII")
}

impl Owners {
    pub(crate) fn new() -> Self {
        Owners::default()
    }

    fn host_id(&mut self, host: &str) -> HostId {
        if let Some(position) = self.hosts.iter().position(|held| &**held == host) {
            return position as HostId;
        }
        self.hosts.push(Box::from(host));
        HostId::try_from(self.hosts.len() - 1).expect("fewer than 65,536 computers")
    }

    fn host(&self, id: HostId) -> &str {
        &self.hosts[usize::from(id)]
    }

    pub(crate) fn len(&self) -> usize {
        self.digests.len() + self.names.len()
    }

    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        let id = match digest_of(name) {
            Some(digest) => self.digests.get(&digest),
            None => self.names.get(name),
        };
        id.map(|id| self.host(*id))
    }

    pub(crate) fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Set the name's owner.
    pub(crate) fn insert(&mut self, name: &str, host: &str) {
        let id = self.host_id(host);
        match digest_of(name) {
            Some(digest) => {
                self.digests.insert(digest, id);
            }
            None => {
                self.names.insert(Box::from(name), id);
            }
        }
    }

    /// Set the name's owner unless it has one. Returns whether it set it.
    pub(crate) fn insert_absent(&mut self, name: &str, host: &str) -> bool {
        if self.contains_key(name) {
            return false;
        }
        self.insert(name, host);
        true
    }

    /// Every (name, owner), digest names first.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (Cow<'_, str>, &str)> {
        let digests = self.digests.iter().map(|(digest, id)| {
            let mut buffer = [0u8; 64];
            (
                Cow::Owned(spell(digest, &mut buffer).to_owned()),
                self.host(*id),
            )
        });
        let names = self
            .names
            .iter()
            .map(|(name, id)| (Cow::Borrowed(&**name), self.host(*id)));
        digests.chain(names)
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &str> {
        self.digests
            .values()
            .chain(self.names.values())
            .map(|id| self.host(*id))
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&str, &str) -> bool) {
        let hosts = &self.hosts;
        self.digests.retain(|digest, id| {
            let mut buffer = [0u8; 64];
            keep(spell(digest, &mut buffer), &hosts[usize::from(*id)])
        });
        self.names
            .retain(|name, id| keep(name, &hosts[usize::from(*id)]));
    }

    /// Set every owner `other` names (its owner wins).
    pub(crate) fn extend_from(&mut self, other: &Owners) {
        let ids: Vec<HostId> = other.hosts.iter().map(|host| self.host_id(host)).collect();
        for (digest, id) in &other.digests {
            self.digests.insert(*digest, ids[usize::from(*id)]);
        }
        for (name, id) in &other.names {
            self.names.insert(name.clone(), ids[usize::from(*id)]);
        }
    }
}

impl PartialEq for Owners {
    fn eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        if self.hosts == other.hosts {
            return self.digests == other.digests && self.names == other.names;
        }
        self.digests.iter().all(|(digest, id)| {
            other.digests.get(digest).map(|o| other.host(*o)) == Some(self.host(*id))
        }) && self
            .names
            .iter()
            .all(|(name, id)| other.names.get(name).map(|o| other.host(*o)) == Some(self.host(*id)))
    }
}

impl fmt::Debug for Owners {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl Serialize for Owners {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut entries: Vec<_> = self.iter().collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (name, host) in entries {
            map.serialize_entry(&*name, host)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Owners {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OwnersVisitor;
        impl<'de> Visitor<'de> for OwnersVisitor {
            type Value = Owners;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a map of Claude log names to computers")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Owners, A::Error> {
                let mut owners = Owners::new();
                while let Some((name, host)) = map.next_entry::<Cow<'de, str>, Cow<'de, str>>()? {
                    owners.insert(&name, &host);
                }
                Ok(owners)
            }
        }
        deserializer.deserialize_map(OwnersVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(n: u8) -> String {
        aac_core::sha256_hex([n])
    }

    #[test]
    fn owners_round_trip_the_sorted_json_object() {
        let mut owners = Owners::new();
        owners.insert(&digest(2), "nas1");
        owners.insert("same-basename-digest", "mac");
        owners.insert(&digest(1), "ubuntu");
        let mut plain = BTreeMap::new();
        plain.insert(digest(2), "nas1".to_owned());
        plain.insert("same-basename-digest".to_owned(), "mac".to_owned());
        plain.insert(digest(1), "ubuntu".to_owned());
        let bytes = serde_json::to_vec(&owners).expect("encode");
        assert_eq!(bytes, serde_json::to_vec(&plain).expect("plain"));
        let back: Owners = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(back, owners);
        assert_eq!(back.get(&digest(1)), Some("ubuntu"));
        assert_eq!(back.get("same-basename-digest"), Some("mac"));
        assert_eq!(back.get(&digest(3)), None);
        assert_eq!(back.len(), 3);
    }

    #[test]
    fn owners_compare_by_spelled_entries_whatever_the_host_order() {
        let mut a = Owners::new();
        a.insert(&digest(1), "mac");
        a.insert(&digest(2), "nas1");
        let mut b = Owners::new();
        b.insert(&digest(2), "nas1");
        b.insert(&digest(1), "mac");
        assert_eq!(a, b);
        b.insert(&digest(1), "nas1");
        assert_ne!(a, b);
        assert!(!a.insert_absent(&digest(1), "odroid"));
        assert!(a.insert_absent(&digest(9), "odroid"));
        let mut c = Owners::new();
        c.extend_from(&a);
        assert_eq!(c, a);
        c.retain(|name, host| name != digest(9) && host != "nas1");
        assert_eq!(c.len(), 1);
        assert_eq!(
            c.iter().next().map(|(name, _)| name.into_owned()),
            Some(digest(1))
        );
        // An uppercase spelling is another name, as the string map held it.
        c.insert(&digest(1).to_uppercase(), "mac");
        assert_eq!(c.len(), 2);
    }
}
