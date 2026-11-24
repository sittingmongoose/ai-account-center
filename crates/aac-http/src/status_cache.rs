//! R2b: the clock-independent bytes of a fed Status view, kept per generation identity.
//!
//! A generation is immutable and is named by its id (the same id with other bytes is refused). The
//! key is the six ids the request names, plus the request's clock and day words. A new id is a
//! different key, so an older generation's bytes are never served as the current ones. Each
//! generation-set keeps at most [`SLOTS`] filled views (one per clock and day words; the oldest
//! goes) and the service keeps at most [`GENERATIONS`] sets.
//!
//! What is filled per request is `generatedAt` only, and only when [`FedTrace`] says every
//! now-dependent decision (ages, stale and due thresholds, spans, stored instants) has the same
//! result. The identity, and so the ETag, does not include `generatedAt`. A different identity is
//! a different tag: the tag is stored with the bytes it was hashed from and is not reused for
//! another key.
use aac_status::read_status::FedTrace;
use std::collections::VecDeque;
use std::sync::Arc;

/// Generation-sets kept at once.
pub(crate) const GENERATIONS: usize = 2;
/// Filled views (clock and day words) one generation-set keeps.
pub(crate) const SLOTS: usize = 2;

/// `Date#toISOString` inside the admitted domain.
pub(crate) const ISO_LEN: usize = 24;

struct Slot {
    clock: String,
    day: String,
    trace: FedTrace,
    bytes: Arc<Vec<u8>>,
    etag: String,
    /// Byte offset of the 24-character `generatedAt` text.
    at: usize,
}

struct Set {
    ids: [String; 6],
    slots: VecDeque<Slot>,
}

/// The kept views.
#[derive(Default)]
pub(crate) struct Cache {
    sets: VecDeque<Set>,
}

static CACHE: std::sync::Mutex<Cache> = std::sync::Mutex::new(Cache {
    sets: VecDeque::new(),
});

/// The kept view for these generations, or nothing when the lock is poisoned or the decisions differ.
pub(crate) fn hit(ids: &[String; 6], clock: &str, day: &str, now: i64) -> Option<Hit> {
    CACHE.lock().ok()?.lookup(ids, clock, day, now)
}

pub(crate) struct Kept {
    pub(crate) ids: [String; 6],
    pub(crate) clock: String,
    pub(crate) day: String,
    pub(crate) trace: FedTrace,
    pub(crate) bytes: Vec<u8>,
    pub(crate) etag: String,
    pub(crate) at: usize,
}

/// Keep a composed view. A trace that must not be kept, or bytes without `generatedAt`, is ignored.
pub(crate) fn remember(kept: Kept) {
    if let Ok(mut cache) = CACHE.lock() {
        cache.store(kept);
    }
}

/// A hit: the bytes with `generatedAt` still from the instant they were composed, and that
/// instant's ETag.
pub(crate) struct Hit {
    pub bytes: Arc<Vec<u8>>,
    pub etag: String,
    pub at: usize,
}

impl Cache {
    /// The kept view when `ids`, the clock and day words, and every recorded decision match.
    pub(crate) fn lookup(
        &mut self,
        ids: &[String; 6],
        clock: &str,
        day: &str,
        now: i64,
    ) -> Option<Hit> {
        let at = self.sets.iter().position(|set| &set.ids == ids)?;
        let set = self.sets.remove(at)?;
        let found = set.slots.iter().rev().find(|slot| {
            slot.clock == clock && slot.day == day && slot.trace.keep() && slot.trace.matches(now)
        });
        let hit = found.map(|slot| Hit {
            bytes: slot.bytes.clone(),
            etag: slot.etag.clone(),
            at: slot.at,
        });
        self.sets.push_back(set);
        hit
    }

    /// Keep `bytes` for this generation identity and these clock and day words. A trace that must
    /// not be kept is ignored. The oldest set, and the oldest slot of this set, go at the bounds.
    fn store(&mut self, kept: Kept) {
        let Kept {
            ids,
            clock,
            day,
            trace,
            bytes,
            etag,
            at,
        } = kept;
        if !trace.keep() || at + ISO_LEN > bytes.len() {
            return;
        }
        let slot = Slot {
            clock,
            day,
            trace,
            bytes: Arc::new(bytes),
            etag,
            at,
        };
        if let Some(pos) = self.sets.iter().position(|set| set.ids == ids) {
            let mut set = self.sets.remove(pos).expect("the set was just found");
            if let Some(old) = set
                .slots
                .iter()
                .position(|kept| kept.clock == slot.clock && kept.day == slot.day)
            {
                set.slots.remove(old);
            }
            if set.slots.len() >= SLOTS {
                set.slots.pop_front();
            }
            set.slots.push_back(slot);
            self.sets.push_back(set);
            return;
        }
        if self.sets.len() >= GENERATIONS {
            self.sets.pop_front();
        }
        let mut slots = VecDeque::new();
        slots.push_back(slot);
        self.sets.push_back(Set { ids, slots });
    }
}

/// Where `generatedAt`'s ISO text sits, when it is `iso` and 24 bytes with a closing quote.
pub(crate) fn generated_at(bytes: &[u8], iso: &str) -> Option<usize> {
    if iso.len() != ISO_LEN {
        return None;
    }
    let needle = format!("\"generatedAt\":\"{iso}\"");
    let pos = memmem(bytes, needle.as_bytes())?;
    let at = pos + "\"generatedAt\":\"".len();
    (bytes.get(at + ISO_LEN) == Some(&b'"') && &bytes[at..at + ISO_LEN] == iso.as_bytes())
        .then_some(at)
}

/// A copy of `bytes` with the 24-byte `generatedAt` at `at` replaced by `iso`.
pub(crate) fn fill(bytes: &[u8], at: usize, iso: &str) -> Option<Vec<u8>> {
    if iso.len() != ISO_LEN || at + ISO_LEN > bytes.len() {
        return None;
    }
    let mut out = bytes.to_vec();
    out[at..at + ISO_LEN].copy_from_slice(iso.as_bytes());
    Some(out)
}

fn memmem(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: u8) -> [String; 6] {
        std::array::from_fn(|i| format!("{n}-{i}"))
    }

    fn bytes(mark: &str) -> Vec<u8> {
        format!(r#"{{"version":1,"generatedAt":"2026-11-14T12:00:00.500Z","mark":"{mark}"}}"#)
            .into_bytes()
    }

    fn put(
        cache: &mut Cache,
        ids: [String; 6],
        clock: &str,
        trace: FedTrace,
        raw: Vec<u8>,
        etag: &str,
        at: usize,
    ) {
        cache.store(Kept {
            ids,
            clock: clock.to_owned(),
            day: "Nov 14".to_owned(),
            trace,
            bytes: raw,
            etag: etag.to_owned(),
            at,
        });
    }

    #[test]
    fn a_kept_view_is_served_only_for_its_own_generations_and_thresholds() {
        let mut cache = Cache::default();
        let iso = "2026-11-14T12:00:00.500Z";
        let first = bytes("one");
        let at = generated_at(&first, iso).expect("generatedAt");
        put(
            &mut cache,
            ids(1),
            "12:00 PM",
            FedTrace::after(1_000, 0, 500),
            first.clone(),
            "etag-one",
            at,
        );
        put(
            &mut cache,
            ids(2),
            "12:00 PM",
            FedTrace::stable(),
            bytes("two"),
            "etag-two",
            at,
        );
        // Another generation, the same clock: not this view.
        assert!(cache.lookup(&ids(3), "12:00 PM", "Nov 14", 1_000).is_none());
        let other = cache
            .lookup(&ids(2), "12:00 PM", "Nov 14", 1_000)
            .expect("two");
        assert_eq!(other.etag, "etag-two");
        assert!(other.bytes.windows(3).any(|w| w == b"two"));
        assert_ne!(other.etag, "etag-one");
        // The same generations, across the threshold: not the kept bytes.
        assert!(cache.lookup(&ids(1), "12:00 PM", "Nov 14", 500).is_none());
        let hit = cache
            .lookup(&ids(1), "12:00 PM", "Nov 14", 1_000)
            .expect("one");
        assert_eq!(hit.etag, "etag-one");
        assert_eq!(hit.bytes.as_slice(), first.as_slice());
        // A different page clock is a different entry.
        assert!(cache.lookup(&ids(1), "12:01 PM", "Nov 14", 1_000).is_none());
    }

    #[test]
    fn generated_at_is_the_only_bytes_a_hit_rewrites() {
        let raw = bytes("one");
        let iso = "2026-11-14T12:00:00.500Z";
        let at = generated_at(&raw, iso).expect("offset");
        let next = "2026-11-14T12:00:00.501Z";
        let filled = fill(&raw, at, next).expect("filled");
        assert!(filled.windows(next.len()).any(|w| w == next.as_bytes()));
        assert!(!filled.windows(iso.len()).any(|w| w == iso.as_bytes()));
        assert!(filled.windows(3).any(|w| w == b"one"));
        assert!(fill(&raw, at, "short").is_none());
        assert!(generated_at(&raw, "2026-11-14T12:00:00.999Z").is_none());
    }

    #[test]
    fn a_generation_keeps_at_most_its_slots_and_the_cache_at_most_its_generations() {
        let mut cache = Cache::default();
        let raw = bytes("x");
        let at = generated_at(&raw, "2026-11-14T12:00:00.500Z").expect("offset");
        for n in 0..GENERATIONS + 3 {
            put(
                &mut cache,
                ids(n as u8),
                "12:00 PM",
                FedTrace::stable(),
                raw.clone(),
                &format!("e{n}"),
                at,
            );
        }
        assert!(cache.sets.len() <= GENERATIONS);
        assert!(cache.lookup(&ids(0), "12:00 PM", "Nov 14", 1).is_none());
        assert!(
            cache
                .lookup(&ids((GENERATIONS + 2) as u8), "12:00 PM", "Nov 14", 1)
                .is_some()
        );
        for minute in 0..=SLOTS {
            put(
                &mut cache,
                ids(GENERATIONS as u8 + 2),
                &format!("12:0{minute} PM"),
                FedTrace::stable(),
                raw.clone(),
                &format!("m{minute}"),
                at,
            );
        }
        let set = cache
            .sets
            .iter()
            .find(|set| set.ids == ids(GENERATIONS as u8 + 2))
            .expect("the set");
        assert!(set.slots.len() <= SLOTS);
        assert!(
            cache
                .lookup(&ids(GENERATIONS as u8 + 2), "12:00 PM", "Nov 14", 1)
                .is_none(),
            "the oldest clock words went"
        );
        assert!(
            cache
                .lookup(
                    &ids(GENERATIONS as u8 + 2),
                    &format!("12:0{SLOTS} PM"),
                    "Nov 14",
                    1
                )
                .is_some()
        );
    }
}
