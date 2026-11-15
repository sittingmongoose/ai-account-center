//! R2b: the now-dependent decisions of one Status view, recorded while it is composed.
//!
//! A later request with the same generations re-evaluates these decisions. When every one has the
//! same result, the view's bytes are the same except `generatedAt` (the request's `toISOString`,
//! which is fixed-width and excluded from the revalidated-view identity). A decision that would
//! differ — a rounded age, a stale or due threshold, a "full in" span, or a stored instant that
//! starts or stops resolving as the request's own clock — means the kept bytes must not be served.
use crate::read_context::{age, age_of};
use serde_json::Value;

/// Probes one memo holds at most. Past this the view is composed every time and not kept.
pub const CAP: usize = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lookup {
    Clock,
    Day,
    Iso,
}

#[derive(Clone, Debug, PartialEq)]
enum Probe {
    /// `ageSeconds` of a stored instant (`None` is JSON null).
    Age { at: Option<i64>, null: bool, bits: u64 },
    /// A fractional board mtime, aged as `now as f64 - ms`.
    AgeF { ms_bits: u64, bits: u64 },
    /// `now - at > limit`.
    After { at: i64, limit: i64, hit: bool },
    /// `at <= now` (a reset that has passed).
    Due { at: i64, hit: bool },
    /// `at > now` (a projected full time that is still ahead).
    Ahead { at: i64, hit: bool },
    /// `span((full - now) / 1000)`.
    Span { full: i64, text: String },
    /// A clock, day or ISO word of a stored instant. `hit` means it resolved as the request's own
    /// `now` (the shortcut in `Context::date`).
    Lookup { kind: Lookup, at: i64, hit: bool },
}

impl Probe {
    fn matches(&self, now: i64) -> bool {
        match self {
            Probe::Age { at, null, bits } => age_same(now, *at, *null, *bits),
            Probe::AgeF { ms_bits, bits } => {
                let value = age_of(now as f64 - f64::from_bits(*ms_bits));
                value.as_f64().is_some_and(|n| n.to_bits() == *bits)
            }
            Probe::After { at, limit, hit } => (now - at > *limit) == *hit,
            Probe::Due { at, hit } => (*at <= now) == *hit,
            Probe::Ahead { at, hit } => (*at > now) == *hit,
            Probe::Span { full, text } => super::span((full - now) as f64 / 1000.0) == *text,
            Probe::Lookup { at, hit, .. } => (*at == now) == *hit,
        }
    }
}

fn age_same(now: i64, at: Option<i64>, null: bool, bits: u64) -> bool {
    let value = age(now, at);
    if null {
        value.is_null()
    } else {
        value.as_f64().is_some_and(|n| n.to_bits() == bits)
    }
}

fn age_note(value: &Value) -> (bool, u64) {
    match value.as_f64() {
        Some(n) => (false, n.to_bits()),
        None => (true, 0),
    }
}

/// The decisions recorded for one composition.
#[derive(Default)]
pub(super) struct Recorder {
    recording: bool,
    probes: Vec<Probe>,
    overflow: bool,
    /// An ISO word of the request's own `now` was written somewhere other than `generatedAt`.
    iso_now: bool,
}

impl Recorder {
    pub(super) fn recording() -> Self {
        Self {
            recording: true,
            probes: Vec::new(),
            overflow: false,
            iso_now: false,
        }
    }

    fn note(&mut self, probe: Probe) {
        if !self.recording {
            return;
        }
        if self.probes.len() >= CAP {
            self.overflow = true;
            return;
        }
        self.probes.push(probe);
    }

    pub(super) fn note_age(&mut self, at: Option<i64>, value: &Value) {
        let (null, bits) = age_note(value);
        self.note(Probe::Age { at, null, bits });
    }

    pub(super) fn note_age_f(&mut self, ms: f64, value: &Value) {
        let (_, bits) = age_note(value);
        self.note(Probe::AgeF {
            ms_bits: ms.to_bits(),
            bits,
        });
    }

    pub(super) fn note_after(&mut self, at: i64, limit: i64, hit: bool) {
        self.note(Probe::After { at, limit, hit });
    }

    pub(super) fn note_due(&mut self, at: i64, hit: bool) {
        self.note(Probe::Due { at, hit });
    }

    pub(super) fn note_ahead(&mut self, at: i64, hit: bool) {
        self.note(Probe::Ahead { at, hit });
    }

    pub(super) fn note_span(&mut self, full: i64, text: String) {
        self.note(Probe::Span { full, text });
    }

    pub(super) fn note_lookup(&mut self, now: i64, kind: LookupKind, at: i64) {
        let hit = at == now;
        if kind == LookupKind::Iso && hit {
            self.iso_now = true;
        }
        self.note(Probe::Lookup {
            kind: match kind {
                LookupKind::Clock => Lookup::Clock,
                LookupKind::Day => Lookup::Day,
                LookupKind::Iso => Lookup::Iso,
            },
            at,
            hit,
        });
    }

    pub(super) fn finish(self) -> FedTrace {
        FedTrace {
            probes: self.probes,
            overflow: self.overflow,
            iso_now: self.iso_now,
        }
    }
}

/// Which word of a stored instant was read. Public so the composer can name it without exposing probes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LookupKind {
    Clock,
    Day,
    Iso,
}

/// The now-dependent decisions of one composed view.
#[derive(Clone, Debug)]
pub struct FedTrace {
    probes: Vec<Probe>,
    overflow: bool,
    iso_now: bool,
}

impl FedTrace {
    /// No now-dependent decision. Every instant matches; the bytes may be kept.
    pub fn stable() -> Self {
        Self {
            probes: Vec::new(),
            overflow: false,
            iso_now: false,
        }
    }

    /// One `now - at > limit` decision, as recorded at `now`.
    pub fn after(now: i64, at: i64, limit: i64) -> Self {
        let mut rec = Recorder::recording();
        rec.note_after(at, limit, now - at > limit);
        rec.finish()
    }

    /// Whether these decisions may be kept. An ISO of `now` written into the body (other than
    /// `generatedAt`) changes every millisecond, so those bytes are not kept.
    pub fn keep(&self) -> bool {
        !self.iso_now && !self.overflow
    }

    /// Whether a request at `now` would make the same decisions. `generatedAt` is not a decision:
    /// it is filled per request.
    pub fn matches(&self, now: i64) -> bool {
        self.probes.iter().all(|probe| probe.matches(now))
    }

    /// Decisions kept (tests).
    pub fn len(&self) -> usize {
        self.probes.len()
    }

    /// No decision kept.
    pub fn is_empty(&self) -> bool {
        self.probes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_age_matches_only_while_the_rounded_second_stays() {
        let mut rec = Recorder::recording();
        // 1_500 - 1_000 = 500 ms, and Math.round(0.5) is 1. 499 ms rounds to 0.
        let now = 1_500;
        let value = age(now, Some(1_000));
        rec.note_age(Some(1_000), &value);
        let trace = rec.finish();
        assert!(trace.keep());
        assert!(trace.matches(now));
        // 499 ms rounds to 0; 500 ms through 1_499 ms round to 1; 1_500 ms rounds to 2.
        assert!(trace.matches(2_499));
        assert!(!trace.matches(1_499), "the rounded age changes");
        assert!(!trace.matches(2_500));
    }

    #[test]
    fn a_threshold_cross_does_not_match_and_a_null_age_stays_null() {
        let trace = FedTrace::after(1_000, 0, 500);
        assert!(trace.matches(1_000));
        assert!(trace.matches(501));
        assert!(!trace.matches(500), "now - at > 500 flips");
        let mut rec = Recorder::recording();
        rec.note_age(None, &json!(null));
        let trace = rec.finish();
        assert!(trace.matches(1) && trace.matches(9_000));
    }

    #[test]
    fn an_iso_of_now_in_the_body_is_not_kept() {
        let mut rec = Recorder::recording();
        rec.note_lookup(50, LookupKind::Iso, 50);
        let trace = rec.finish();
        assert!(!trace.keep());
        assert_eq!(trace.len(), 1);
    }

    #[test]
    fn a_stored_instant_that_becomes_now_does_not_match() {
        let mut rec = Recorder::recording();
        rec.note_lookup(10, LookupKind::Clock, 40);
        let trace = rec.finish();
        assert!(trace.keep());
        assert!(trace.matches(10));
        assert!(trace.matches(11));
        assert!(!trace.matches(40), "the word would switch to the request's clock");
    }

    #[test]
    fn a_span_matches_only_while_its_text_stays() {
        let mut rec = Recorder::recording();
        let text = super::super::span(90.0);
        rec.note_span(90_000, text);
        let trace = rec.finish();
        // 90s and 60s are both "1m"; 59.999s is "59s".
        assert!(trace.matches(0));
        assert!(trace.matches(30_000));
        assert!(!trace.matches(30_001));
    }

    #[test]
    fn past_the_cap_a_trace_is_not_kept() {
        let mut rec = Recorder::recording();
        for i in 0..CAP as i64 + 1 {
            rec.note_due(i, true);
        }
        let trace = rec.finish();
        assert!(!trace.keep());
        assert_eq!(trace.len(), CAP);
    }
}
