//! Direct compact Status composer over typed share snapshots (`CompactAgentReportsState`,
//! `CompactRegistrySnapshot`, and `Zone`-backed presentation facts).
//!
//! Eliminates:
//! - `aac_status::read_status::ReportView` + its `serialized` JSON string cache (~10+ MiB)
//! - `aac_status::produce::facts::Facts` + `Presentation` retained in `status-facts` (~8-10 MiB)
//! - Retained `serde_json::Value` DOM copies of reports and pm-mail registry rows (~35+ MiB)
//!
//! Produces byte-for-byte identical Status JSON, ETag, and `status_cache` invalidation decisions
//! as `aac_status::read_status::compose_with_reports`.

use crate::compact_share::registry::{CompactRegistryRow, CompactRegistrySnapshot};
use crate::compact_share::reports::{
    CardState, CompactAgentReportsState, CompactBlockedOn, CompactProgress, CompactSeenReport,
    ThreadState, compact_cleared_writers,
};
use aac_num::intl::{ASCII_ORDER, Zone};
use aac_num::{ecma, view};
use aac_status::read_context::{
    Context, Date, Presentation, age, age_of, array, js_whitespace, number, plural, rows, s,
};
use aac_status::read_status::Unproven;
use serde_json::{Value as V, json};
use std::cell::{Cell, OnceCell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

static NULL: V = V::Null;

/// Compact owned replacement for `aac_status::read_status::ReportView`.
/// Holds only the shared `Arc<CompactAgentReportsState>` and the cleared-blocker writer set
/// (no `metadata: Value`, no `serialized: HashMap`).
#[derive(Debug)]
pub(crate) struct OwnedReportsView {
    pub(crate) state: Arc<CompactAgentReportsState>,
    pub(crate) cleared: Arc<HashSet<Box<str>>>,
}

impl OwnedReportsView {
    pub(crate) fn new(state: Arc<CompactAgentReportsState>, closed: &HashSet<String>) -> Self {
        let cleared = compact_cleared_writers(&state.threads, &state.cards, closed);
        Self {
            state,
            cleared: Arc::new(cleared),
        }
    }

    pub(crate) fn blocker_is_cleared(&self, writer: &str) -> bool {
        self.cleared.contains(writer)
    }
}

/// Registry input in `status_fed::CollectedView`: either compact typed rows (owned path)
/// or wire JSON (external PUT path).
#[derive(Debug, Clone)]
pub(crate) enum RegistrySource {
    Wire(Arc<V>),
    Compact(Arc<CompactRegistrySnapshot>),
}

impl RegistrySource {
    pub(crate) fn proto_unsafe(&self) -> bool {
        match self {
            Self::Wire(value) => aac_status::read_context::proto_named(value),
            Self::Compact(snap) => snap.proto_unsafe,
        }
    }
}

/// Presentation facts source in `status_fed::Part`: either wire `Presentation` (external PUT or
/// test override) or the display `Zone` directly (owned path, ~200 bytes instead of ~8-10 MiB).
#[derive(Clone)]
pub(crate) enum FactsSource {
    Wire(Arc<Presentation>),
    OwnedZone(Zone),
}

impl std::fmt::Debug for FactsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wire(_) => f.write_str("FactsSource::Wire(..)"),
            Self::OwnedZone(z) => f.debug_tuple("FactsSource::OwnedZone").field(z).finish(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Zero-allocation total collation matching `aac_num::intl::collate`

const NON_ASCII_PRIMARY: u32 = 256;

const ASCII_WEIGHTS: [Option<(u32, u32)>; 128] = {
    let order = ASCII_ORDER.as_bytes();
    let mut digits_end = 0u32;
    let mut i = 0;
    while i < order.len() {
        if order[i] == b'a' {
            digits_end = i as u32;
            break;
        }
        i += 1;
    }
    let mut table: [Option<(u32, u32)>; 128] = [None; 128];
    let mut at = 0;
    while at < order.len() {
        let c = order[at];
        if (c as usize) < 128 {
            let entry = if c.is_ascii_alphabetic() {
                (
                    digits_end + (at as u32 - digits_end) / 2,
                    if c.is_ascii_uppercase() { 1 } else { 0 },
                )
            } else {
                (at as u32, 0)
            };
            table[c as usize] = Some(entry);
        }
        at += 1;
    }
    table
};

#[inline]
fn char_weight(c: char) -> (u32, u32) {
    if ('\u{20}'..='\u{7e}').contains(&c) {
        ASCII_WEIGHTS[c as usize].unwrap_or((NON_ASCII_PRIMARY + c as u32, 0))
    } else {
        (NON_ASCII_PRIMARY + c as u32, 0)
    }
}

/// Zero-allocation collation strictly identical to `aac_num::intl::collate`.
pub(crate) fn collate_fast(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let mut ca = a.chars();
    let mut cb = b.chars();
    let mut first_tert: Option<Ordering> = None;
    loop {
        match (ca.next(), cb.next()) {
            (Some(xa), Some(xb)) => {
                let (pa, ta) = char_weight(xa);
                let (pb, tb) = char_weight(xb);
                if pa != pb {
                    return pa.cmp(&pb);
                }
                if first_tert.is_none() && ta != tb {
                    first_tert = Some(ta.cmp(&tb));
                }
            }
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, None) => return first_tert.unwrap_or(Ordering::Equal),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Now-dependent decision trace (mirrors `aac_status::read_status_trace`)

const TRACE_CAP: usize = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LookupKind {
    Clock,
    Day,
    Iso,
}

#[derive(Clone, Debug, PartialEq)]
enum Probe {
    Age {
        at: Option<i64>,
        null: bool,
        bits: u64,
    },
    AgeF {
        ms_bits: u64,
        bits: u64,
    },
    After {
        at: i64,
        limit: i64,
        hit: bool,
    },
    Due {
        at: i64,
        hit: bool,
    },
    Ahead {
        at: i64,
        hit: bool,
    },
    Span {
        full: i64,
        text: String,
    },
    Lookup {
        kind: LookupKind,
        at: i64,
        hit: bool,
    },
}

impl Probe {
    fn matches(&self, now: i64) -> bool {
        match self {
            Probe::Age { at, null, bits } => {
                let value = age(now, *at);
                if *null {
                    value.is_null()
                } else {
                    value.as_f64().is_some_and(|n| n.to_bits() == *bits)
                }
            }
            Probe::AgeF { ms_bits, bits } => {
                let value = age_of(now as f64 - f64::from_bits(*ms_bits));
                value.as_f64().is_some_and(|n| n.to_bits() == *bits)
            }
            Probe::After { at, limit, hit } => (now - at > *limit) == *hit,
            Probe::Due { at, hit } => (*at <= now) == *hit,
            Probe::Ahead { at, hit } => (*at > now) == *hit,
            Probe::Span { full, text } => span((full - now) as f64 / 1000.0) == *text,
            Probe::Lookup { at, hit, .. } => (*at == now) == *hit,
        }
    }
}

fn age_note(value: &V) -> (bool, u64) {
    match value.as_f64() {
        Some(n) => (false, n.to_bits()),
        None => (true, 0),
    }
}

#[derive(Default)]
struct TraceRecorder {
    recording: bool,
    probes: Vec<Probe>,
    overflow: bool,
    iso_now: bool,
}

impl TraceRecorder {
    fn recording() -> Self {
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
        if self.probes.len() >= TRACE_CAP {
            self.overflow = true;
            return;
        }
        self.probes.push(probe);
    }

    fn note_age(&mut self, at: Option<i64>, value: &V) {
        let (null, bits) = age_note(value);
        self.note(Probe::Age { at, null, bits });
    }

    fn note_age_f(&mut self, ms: f64, value: &V) {
        let (_, bits) = age_note(value);
        self.note(Probe::AgeF {
            ms_bits: ms.to_bits(),
            bits,
        });
    }

    fn note_after(&mut self, at: i64, limit: i64, hit: bool) {
        self.note(Probe::After { at, limit, hit });
    }

    fn note_due(&mut self, at: i64, hit: bool) {
        self.note(Probe::Due { at, hit });
    }

    fn note_ahead(&mut self, at: i64, hit: bool) {
        self.note(Probe::Ahead { at, hit });
    }

    fn note_span(&mut self, full: i64, text: String) {
        self.note(Probe::Span { full, text });
    }

    fn note_lookup(&mut self, now: i64, kind: LookupKind, at: i64) {
        let hit = at == now;
        if kind == LookupKind::Iso && hit {
            self.iso_now = true;
        }
        self.note(Probe::Lookup { kind, at, hit });
    }

    fn finish(self) -> CompactFedTrace {
        CompactFedTrace {
            probes: self.probes,
            overflow: self.overflow,
            iso_now: self.iso_now,
        }
    }
}

/// Compact composer decision trace (same invalidation rules as `aac_status::read_status::FedTrace`).
#[derive(Clone, Debug)]
pub(crate) struct CompactFedTrace {
    probes: Vec<Probe>,
    overflow: bool,
    iso_now: bool,
}

impl CompactFedTrace {
    pub(crate) fn keep(&self) -> bool {
        !self.iso_now && !self.overflow
    }

    pub(crate) fn matches(&self, now: i64) -> bool {
        self.probes.iter().all(|probe| probe.matches(now))
    }
}

// ---------------------------------------------------------------------------------------------
// Presentation facts context supporting both `OwnedZone(Zone)` and `Wire(Arc<Presentation>)`

#[derive(Clone, Debug)]
pub(crate) struct DateTriple {
    pub(crate) iso: String,
    pub(crate) clock: String,
    pub(crate) day: String,
}

impl DateTriple {
    pub(crate) fn of_now(ms: i64, clock: String, day: String) -> Option<Self> {
        Some(Self {
            iso: ecma::to_iso_string(ms)?,
            clock,
            day,
        })
    }
}

enum FactsBackend<'a> {
    OwnedZone(&'a Zone),
    Wire(Context),
}

struct FactsCtx<'a> {
    backend: FactsBackend<'a>,
    now: (i64, DateTriple),
    scan_dates: [Option<(i64, DateTriple)>; 2],
    missing: Cell<bool>,
}

impl<'a> FactsCtx<'a> {
    fn new(
        facts: &'a FactsSource,
        now_ms: i64,
        clock: String,
        day: String,
        scan_dates: [Option<(i64, DateTriple)>; 2],
    ) -> Option<Self> {
        let now_triple = DateTriple::of_now(now_ms, clock.clone(), day.clone())?;
        let backend = match facts {
            FactsSource::OwnedZone(zone) => FactsBackend::OwnedZone(zone),
            FactsSource::Wire(pres) => {
                let date = Date::of_now(now_ms, clock, day)?;
                FactsBackend::Wire(Context::shared(Arc::clone(pres), Some((now_ms, date))))
            }
        };
        Some(Self {
            backend,
            now: (now_ms, now_triple),
            scan_dates,
            missing: Cell::new(false),
        })
    }

    fn valid(&self) -> bool {
        if self.missing.get() {
            return false;
        }
        match &self.backend {
            FactsBackend::OwnedZone(_) => true,
            FactsBackend::Wire(ctx) => ctx.valid(),
        }
    }

    fn mark_missing(&self) {
        self.missing.set(true);
    }

    fn is_wire(&self) -> bool {
        matches!(self.backend, FactsBackend::Wire(_))
    }

    fn epoch(&self, value: &V) -> Option<i64> {
        match &self.backend {
            FactsBackend::Wire(ctx) => ctx.epoch(value),
            FactsBackend::OwnedZone(_) => {
                if value.is_null() {
                    return None;
                }
                if let Some(ms) = value
                    .as_i64()
                    .filter(|ms| ms.unsigned_abs() <= ecma::MAX_SAFE_INTEGER as u64)
                {
                    return Some(ms);
                }
                if let Some(text) = value.as_str()
                    && ecma::iso_prefixed(text)
                {
                    return ecma::parse_instant(text);
                }
                self.missing.set(true);
                None
            }
        }
    }

    fn epoch_u64(&self, ms: u64) -> Option<i64> {
        if ms <= ecma::MAX_SAFE_INTEGER as u64 {
            Some(ms as i64)
        } else {
            self.missing.set(true);
            None
        }
    }

    fn epoch_i64(&self, ms: i64) -> Option<i64> {
        if ms.unsigned_abs() <= ecma::MAX_SAFE_INTEGER as u64 {
            Some(ms)
        } else {
            self.missing.set(true);
            None
        }
    }

    fn lookup_special(&self, ms: i64) -> Option<&DateTriple> {
        if self.now.0 == ms {
            return Some(&self.now.1);
        }
        self.scan_dates
            .iter()
            .flatten()
            .find(|(at, _)| *at == ms)
            .map(|(_, d)| d)
    }

    fn iso(&self, ms: Option<i64>) -> V {
        let Some(m) = ms else {
            return V::Null;
        };
        if let Some(d) = self.lookup_special(m) {
            return json!(d.iso);
        }
        match &self.backend {
            FactsBackend::Wire(ctx) => ctx.iso(Some(m)),
            FactsBackend::OwnedZone(zone) => {
                // In Facts::build, a date exists iff both toISOString and zone.words succeed.
                if let (Some(iso), Some(_)) = (ecma::to_iso_string(m), zone.words(m)) {
                    json!(iso)
                } else {
                    self.missing.set(true);
                    V::Null
                }
            }
        }
    }

    fn clock(&self, ms: i64) -> String {
        if let Some(d) = self.lookup_special(ms) {
            return d.clock.clone();
        }
        match &self.backend {
            FactsBackend::Wire(ctx) => ctx.clock(ms),
            FactsBackend::OwnedZone(zone) => {
                if let (Some(_), Some((clock, _))) = (ecma::to_iso_string(ms), zone.words(ms)) {
                    clock
                } else {
                    self.missing.set(true);
                    String::new()
                }
            }
        }
    }

    fn day(&self, ms: i64) -> String {
        if let Some(d) = self.lookup_special(ms) {
            return d.day.clone();
        }
        match &self.backend {
            FactsBackend::Wire(ctx) => ctx.day(ms),
            FactsBackend::OwnedZone(zone) => {
                if let (Some(_), Some((_, day))) = (ecma::to_iso_string(ms), zone.words(ms)) {
                    day
                } else {
                    self.missing.set(true);
                    String::new()
                }
            }
        }
    }

    fn compare(&self, a: &str, b: &str) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        match &self.backend {
            FactsBackend::Wire(ctx) => ctx.compare(a, b),
            FactsBackend::OwnedZone(_) => collate_fast(a, b),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Composer inputs (supports compact owned reports/registry AND wire fallback transparently)

pub(crate) enum ReportsInput<'a> {
    Compact(&'a OwnedReportsView),
    Wire(&'a V),
}

pub(crate) enum RegistryInput<'a> {
    Compact(&'a CompactRegistrySnapshot),
    Wire(&'a V),
}

struct Parts<'a> {
    entries: Vec<(&'a str, &'a V)>,
}

impl<'a> Parts<'a> {
    fn get(&self, key: &str) -> Option<&'a V> {
        self.entries
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
    }

    fn index(&self, key: &str) -> &'a V {
        self.get(key).unwrap_or(&NULL)
    }
}

#[derive(Default)]
struct OwnerCards {
    strings: HashSet<String>,
    other: Vec<V>,
}

impl OwnerCards {
    fn has_str(&self, writer: &str) -> bool {
        self.strings.contains(writer)
    }

    fn has_val(&self, writer: &V) -> bool {
        match writer.as_str() {
            Some(text) => self.strings.contains(text),
            None => self.other.contains(writer),
        }
    }
}

struct Inputs<'a> {
    reports: ReportsInput<'a>,
    registry: RegistryInput<'a>,
    scan_stamps: [Option<i64>; 2],
    parts: Parts<'a>,
    ctx: FactsCtx<'a>,
    now: i64,
    owner_cards: OnceCell<OwnerCards>,
    trace: RefCell<TraceRecorder>,
}

impl<'a> Inputs<'a> {
    fn at(&self, v: &V) -> Option<i64> {
        self.ctx.epoch(v)
    }

    fn seen_compact(&self, row: &CompactSeenReport) -> Option<i64> {
        let first = self.ctx.epoch_u64(row.first_seen_at)?;
        let written = self.ctx.epoch_i64(row.written_at_ms);
        Some(written.map_or(first, |at| at.min(first)))
    }

    fn seen_wire(&self, row: &V) -> Option<i64> {
        let first = self.at(&row["firstSeenAt"])?;
        let written = self.at(&row["writtenAtMs"]);
        Some(written.map_or(first, |at| at.min(first)))
    }

    fn report_stamp(&self) -> Option<i64> {
        self.scan_stamps[0].or_else(|| match self.reports {
            ReportsInput::Compact(r) => r.state.scanned_at.and_then(|v| self.ctx.epoch_u64(v)),
            ReportsInput::Wire(v) => self.at(&v["scannedAt"]),
        })
    }

    fn registry_stamp(&self) -> Option<i64> {
        self.scan_stamps[1].or_else(|| match self.registry {
            RegistryInput::Compact(reg) => reg.as_of.and_then(|v| self.ctx.epoch_u64(v)),
            RegistryInput::Wire(v) => self.at(&v["asOf"]),
        })
    }

    fn clock(&self, at: i64) -> String {
        self.trace
            .borrow_mut()
            .note_lookup(self.now, LookupKind::Clock, at);
        self.ctx.clock(at)
    }

    fn day(&self, at: i64) -> String {
        self.trace
            .borrow_mut()
            .note_lookup(self.now, LookupKind::Day, at);
        self.ctx.day(at)
    }

    fn iso(&self, at: Option<i64>) -> V {
        if let Some(ms) = at {
            self.trace
                .borrow_mut()
                .note_lookup(self.now, LookupKind::Iso, ms);
        }
        self.ctx.iso(at)
    }

    fn generated_at(&self) -> V {
        self.ctx.iso(Some(self.now))
    }

    fn clock_or_now(&self, at: Option<i64>) -> String {
        match at {
            Some(t) => self.clock(t),
            None => self.ctx.clock(self.now),
        }
    }

    fn age(&self, at: Option<i64>) -> V {
        let value = age(self.now, at);
        self.trace.borrow_mut().note_age(at, &value);
        value
    }

    fn age_f(&self, ms: f64) -> V {
        let value = age_of(self.now as f64 - ms);
        self.trace.borrow_mut().note_age_f(ms, &value);
        value
    }

    fn after(&self, at: i64, limit: i64) -> bool {
        let hit = self.now - at > limit;
        self.trace.borrow_mut().note_after(at, limit, hit);
        hit
    }

    fn due(&self, at: i64) -> bool {
        let hit = at <= self.now;
        self.trace.borrow_mut().note_due(at, hit);
        hit
    }

    fn ahead(&self, at: i64) -> bool {
        let hit = at > self.now;
        self.trace.borrow_mut().note_ahead(at, hit);
        hit
    }

    fn span_until(&self, full: i64) -> String {
        let text = span((full - self.now) as f64 / 1000.0);
        self.trace.borrow_mut().note_span(full, text.clone());
        text
    }

    fn owner_cards(&self) -> &OwnerCards {
        self.owner_cards.get_or_init(|| {
            let mut found = OwnerCards::default();
            let answers = rows(self.parts.index("answers"));
            match self.reports {
                ReportsInput::Compact(owned) => {
                    for row in &owned.state.cards {
                        let Some(card) = row.card() else { continue };
                        if is_open_compact(row, card, answers)
                            && ["question", "judgement", "action"].contains(&card.kind.as_ref())
                        {
                            found.strings.insert(row.writer.to_string());
                        }
                    }
                }
                ReportsInput::Wire(wire) => {
                    for c in rows(&wire["cards"]) {
                        if is_open_wire(c, answers)
                            && ["question", "judgement", "action"].contains(&s(&c["doc"]["kind"]))
                        {
                            match &c["writer"] {
                                V::String(text) => {
                                    found.strings.insert(text.clone());
                                }
                                other => found.other.push(other.clone()),
                            }
                        }
                    }
                }
            }
            found
        })
    }

    fn stale_thread_compact(
        &self,
        row: &CompactSeenReport,
        owned: &OwnedReportsView,
    ) -> Option<bool> {
        if row.problem.is_some() {
            return Some(true);
        }
        let thread = row.thread()?;
        let blocker_is_owner = !owned.blocker_is_cleared(&row.writer)
            && thread
                .blocked_on
                .as_deref()
                .is_some_and(|b| b.kind.as_ref() == "owner");
        if blocker_is_owner || self.owner_cards().has_str(&row.writer) {
            return Some(false);
        }
        let seen = self.seen_compact(row)?;
        match thread.ttl_seconds {
            None => Some(
                !matches!(thread.state, ThreadState::Blocked | ThreadState::Waiting)
                    && self.after(seen, 86_400_000),
            ),
            Some(ttl) => {
                let ttl_i64 = i64::try_from(ttl).ok()?;
                Some(self.after(seen, ttl_i64 * 1000))
            }
        }
    }

    fn stale_thread_wire(&self, row: &V) -> Option<bool> {
        if !row["problem"].is_null() {
            return Some(true);
        }
        let doc = &row["doc"];
        let owner =
            doc["blockedOn"]["kind"] == "owner" || self.owner_cards().has_val(&row["writer"]);
        if owner {
            return Some(false);
        }
        let seen = self.seen_wire(row)?;
        if doc["ttlSeconds"].is_null() {
            Some(
                !["blocked", "waiting"].contains(&s(&doc["state"])) && self.after(seen, 86_400_000),
            )
        } else {
            Some(self.after(seen, doc["ttlSeconds"].as_i64()? * 1000))
        }
    }

    fn board_compact(&self, given: Option<&str>) -> V {
        let Some(given) = given.filter(|g| !g.is_empty()) else {
            return V::Null;
        };
        let mtime = match self.reports {
            ReportsInput::Compact(owned) => owned.state.board_mtime(given),
            ReportsInput::Wire(wire) => {
                return self.board_wire(&json!(given), wire);
            }
        };
        let age = match mtime {
            Some(ms) if ms.fract() != 0.0 && ms.is_finite() && ms.abs() <= 8.64e15 => {
                self.age_f(ms)
            }
            Some(ms) if ms.is_finite() && ms.abs() <= ecma::MAX_SAFE_INTEGER as f64 => {
                let int_ms = ms as i64;
                self.age(self.ctx.epoch_i64(int_ms))
            }
            Some(_) => {
                self.ctx.mark_missing();
                self.age(None)
            }
            None => self.age(None),
        };
        json!({"path": given, "ageSeconds": age})
    }

    fn board_wire(&self, given: &V, wire_reports: &V) -> V {
        if s(given).is_empty() {
            return V::Null;
        }
        let mtime = &wire_reports["boards"][s(given)];
        let age = match mtime.as_f64() {
            Some(ms) if !mtime.is_i64() && ms.is_finite() && ms.abs() <= 8.64e15 => self.age_f(ms),
            _ => self.age(self.at(mtime)),
        };
        json!({"path": given, "ageSeconds": age})
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers matching `read_status.rs`

fn sentence(text: &str) -> String {
    let t = text.trim_matches(js_whitespace);
    if t.ends_with(['.', '!', '?']) {
        t.to_owned()
    } else {
        format!("{t}.")
    }
}

fn trim_period(text: &str) -> &str {
    text.strip_suffix('.').unwrap_or(text)
}

fn round1(v: &V) -> V {
    number(v)
        .map(|n| json!(ecma::math_round(n * 10.0) / 10.0))
        .unwrap_or(V::Null)
}

fn gb(v: &V) -> V {
    number(v)
        .map(|n| json!(ecma::math_round((n / 1_073_741_824.0) * 10.0) / 10.0))
        .unwrap_or(V::Null)
}

fn span(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "now".into();
    }
    let d = (seconds / 86400.0).floor();
    let h = ((seconds % 86400.0) / 3600.0).floor();
    let m = ((seconds % 3600.0) / 60.0).floor();
    if d > 0.0 {
        format!("{d}d {h}h")
    } else if h > 0.0 {
        format!("{h}h {m}m")
    } else if m > 0.0 {
        format!("{m}m")
    } else {
        format!("{}s", seconds.floor())
    }
}

fn part_problem(i: &Inputs, p: &V) -> Option<String> {
    Some(format!(
        "{}: {} (since {}).",
        p["item"].as_str()?,
        trim_period(p["reason"].as_str()?),
        i.clock(i.at(&p["since"])?)
    ))
}

fn part_problem_fields(i: &Inputs, item: &str, reason: &str, since: u64) -> Option<String> {
    Some(format!(
        "{item}: {} (since {}).",
        trim_period(reason),
        i.clock(i.ctx.epoch_u64(since)?)
    ))
}

fn source_ref(v: &V) -> V {
    json!({"id":v["id"],"label":v["label"],"state":v["state"],"asOf":v["asOf"],"ageSeconds":v["ageSeconds"],"detail":v["detail"]})
}

fn part_ref(i: &Inputs, source: &V, collected: &V, part: Option<&V>, absent: &str) -> Option<V> {
    if !collected["error"].is_null() || collected["asOf"].is_null() || source["state"] == "stale" {
        return Some(source_ref(source));
    }
    let mut out = json!({"id":source["id"],"label":source["label"]});
    let Some(part) = part else {
        out["state"] = json!("missing");
        out["asOf"] = V::Null;
        out["ageSeconds"] = V::Null;
        out["detail"] = json!(absent);
        return Some(out);
    };
    let read = i.at(&part["readAt"]);
    if !part["problem"].is_null() {
        out["state"] = json!(if source["state"] == "unreadable" {
            "unreadable"
        } else {
            "error"
        });
        out["asOf"] = i.iso(read);
        out["ageSeconds"] = i.age(read);
        let shown = read
            .map(|t| format!(" Showing its reading from {}.", i.clock(t)))
            .unwrap_or_else(|| " No reading yet.".into());
        out["detail"] = json!(format!("{}{shown}", part_problem(i, &part["problem"])?));
    } else {
        let at = read.or_else(|| i.at(&collected["asOf"]));
        out["state"] = json!("ok");
        out["asOf"] = i.iso(at);
        out["ageSeconds"] = i.age(at);
        out["detail"] = json!(format!("Read {}.", i.clock(at?)));
    }
    Some(out)
}

fn part_ref_registry_compact(
    i: &Inputs,
    source: &V,
    reg: &CompactRegistrySnapshot,
    row: &CompactRegistryRow,
) -> Option<V> {
    if reg.error.is_some() || reg.as_of.is_none() || source["state"] == "stale" {
        return Some(source_ref(source));
    }
    let mut out = json!({"id": source["id"], "label": source["label"]});
    let read = i.ctx.epoch_u64(row.read_at);
    if let Some(prob) = row.problem.as_deref() {
        out["state"] = json!(if source["state"] == "unreadable" {
            "unreadable"
        } else {
            "error"
        });
        out["asOf"] = i.iso(read);
        out["ageSeconds"] = i.age(read);
        let shown = read
            .map(|t| format!(" Showing its reading from {}.", i.clock(t)))
            .unwrap_or_else(|| " No reading yet.".into());
        out["detail"] = json!(format!(
            "{}{shown}",
            part_problem_fields(i, &prob.item, &prob.reason, prob.since)?
        ));
    } else {
        let at = read.or_else(|| reg.as_of.and_then(|v| i.ctx.epoch_u64(v)));
        out["state"] = json!("ok");
        out["asOf"] = i.iso(at);
        out["ageSeconds"] = i.age(at);
        out["detail"] = json!(format!("Read {}.", i.clock(at?)));
    }
    Some(out)
}

fn collected_source_wire(
    i: &Inputs,
    id: &str,
    label: &str,
    c: &V,
    stale: i64,
    registry: bool,
) -> Option<V> {
    let at = if registry {
        i.registry_stamp()
    } else {
        i.at(&c["asOf"])
    };
    let mut out = json!({"id":id,"label":label,"asOf":i.iso(at),"ageSeconds":i.age(at),"staleAfterSeconds":stale});
    let (state, detail) = if c["error"].is_object() {
        let shown = at
            .map(|t| format!(" Showing the reading from {}.", i.clock(t)))
            .unwrap_or_default();
        (
            if c["missing"] == true {
                "missing"
            } else {
                "error"
            },
            format!(
                "{} (since {}).{shown}",
                trim_period(s(&c["error"]["reason"])),
                i.clock(i.at(&c["error"]["since"])?)
            ),
        )
    } else if at.is_none() {
        ("missing", "Waiting for the first reading.".into())
    } else if number(&out["ageSeconds"])? > stale as f64 {
        ("stale", format!("No new reading since {}.", i.clock(at?)))
    } else {
        let problems = array(&c["problems"]);
        if problems.is_empty() {
            ("ok", format!("Read {}.", i.clock(at?)))
        } else {
            let text = if registry {
                let since = problems.iter().filter_map(|p| i.at(&p["since"])).min()?;
                format!(
                    "{} unreadable since {}: {}{}. Their threads show the last good reading.",
                    plural(problems.len(), "registry file"),
                    i.clock(since),
                    problems
                        .iter()
                        .take(5)
                        .map(|p| format!("{} ({})", s(&p["item"]), s(&p["reason"])))
                        .collect::<Vec<_>>()
                        .join(", "),
                    if problems.len() > 5 {
                        format!(", and {} more", problems.len() - 5)
                    } else {
                        String::new()
                    }
                )
            } else {
                problems
                    .iter()
                    .map(|p| part_problem(i, p))
                    .collect::<Option<Vec<_>>>()?
                    .join(" ")
            };
            (
                if registry { "unreadable" } else { "error" },
                format!("{text} The rest read {}.", i.clock(at?)),
            )
        }
    };
    out["state"] = json!(state);
    out["detail"] = json!(detail);
    Some(out)
}

fn registry_source(i: &Inputs) -> Option<V> {
    match i.registry {
        RegistryInput::Wire(v) => {
            collected_source_wire(i, "pm-mail", "pm-mail registry", v, 120, true)
        }
        RegistryInput::Compact(reg) => {
            let at = i.registry_stamp();
            let mut out = json!({
                "id": "pm-mail",
                "label": "pm-mail registry",
                "asOf": i.iso(at),
                "ageSeconds": i.age(at),
                "staleAfterSeconds": 120
            });
            let (state, detail) = if let Some(err) = reg.error.as_deref() {
                let shown = at
                    .map(|t| format!(" Showing the reading from {}.", i.clock(t)))
                    .unwrap_or_default();
                (
                    if reg.missing == Some(true) {
                        "missing"
                    } else {
                        "error"
                    },
                    format!(
                        "{} (since {}).{shown}",
                        trim_period(&err.reason),
                        i.clock(i.ctx.epoch_u64(err.since)?)
                    ),
                )
            } else if at.is_none() {
                ("missing", "Waiting for the first reading.".into())
            } else if number(&out["ageSeconds"])? > 120.0 {
                ("stale", format!("No new reading since {}.", i.clock(at?)))
            } else {
                let problems = reg.problems.as_deref().unwrap_or(&[]);
                if problems.is_empty() {
                    ("ok", format!("Read {}.", i.clock(at?)))
                } else {
                    let since = problems
                        .iter()
                        .filter_map(|p| i.ctx.epoch_u64(p.since))
                        .min()?;
                    let text = format!(
                        "{} unreadable since {}: {}{}. Their threads show the last good reading.",
                        plural(problems.len(), "registry file"),
                        i.clock(since),
                        problems
                            .iter()
                            .take(5)
                            .map(|p| format!("{} ({})", p.item, p.reason))
                            .collect::<Vec<_>>()
                            .join(", "),
                        if problems.len() > 5 {
                            format!(", and {} more", problems.len() - 5)
                        } else {
                            String::new()
                        }
                    );
                    (
                        "unreadable",
                        format!("{text} The rest read {}.", i.clock(at?)),
                    )
                }
            };
            out["state"] = json!(state);
            out["detail"] = json!(detail);
            Some(out)
        }
    }
}

fn reports_source(i: &Inputs) -> Option<V> {
    let at = i.report_stamp();
    let mut out = json!({
        "id": "agent-reports",
        "label": "Agent reports",
        "asOf": i.iso(at),
        "ageSeconds": i.age(at),
        "staleAfterSeconds": 60
    });
    let (state, detail) = match i.reports {
        ReportsInput::Compact(owned) => {
            let st = owned.state.as_ref();
            if at.is_none() {
                ("missing", "Waiting for the first scan.".into())
            } else if st.state == "missing" {
                (
                    "missing",
                    format!("No agent reports folder at {}.", st.root),
                )
            } else if st.state == "error" {
                (
                    "error",
                    "The agent reports folder could not be listed.".into(),
                )
            } else if number(&out["ageSeconds"])? > 60.0 {
                (
                    "stale",
                    format!(
                        "The agent reports folder has not answered since {}.",
                        i.clock(at?)
                    ),
                )
            } else {
                let files: Vec<_> = st
                    .problems
                    .iter()
                    .filter(|p| !p.file.starts_with("limits/"))
                    .collect();
                let limits: Vec<&str> = st
                    .problems
                    .iter()
                    .filter(|p| p.file.starts_with("limits/"))
                    .map(|p| p.reason.as_ref())
                    .collect();
                let limits = if limits.is_empty() {
                    String::new()
                } else {
                    format!(" {}", limits.join(" "))
                };
                if !files.is_empty() {
                    let since = files
                        .iter()
                        .filter_map(|p| i.ctx.epoch_u64(p.since))
                        .min()?;
                    let shown = files
                        .iter()
                        .take(5)
                        .map(|p| {
                            format!(
                                "{} ({})",
                                p.file.strip_prefix("writers/").unwrap_or(&p.file),
                                p.reason
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let more = if files.len() > 5 {
                        format!(", and {} more", files.len() - 5)
                    } else {
                        String::new()
                    };
                    (
                        "unreadable",
                        format!(
                            "{} unreadable since {}: {shown}{more}.{limits}",
                            plural(files.len(), "file"),
                            i.clock(since)
                        ),
                    )
                } else {
                    (
                        "ok",
                        format!(
                            "{} and {} read {}.{limits}",
                            plural(st.threads.len(), "thread"),
                            plural(st.cards.len(), "card"),
                            i.clock(at?)
                        ),
                    )
                }
            }
        }
        ReportsInput::Wire(r) => {
            if at.is_none() {
                ("missing", "Waiting for the first scan.".into())
            } else if r["state"] == "missing" {
                (
                    "missing",
                    format!("No agent reports folder at {}.", s(&r["root"])),
                )
            } else if r["state"] == "error" {
                (
                    "error",
                    "The agent reports folder could not be listed.".into(),
                )
            } else if number(&out["ageSeconds"])? > 60.0 {
                (
                    "stale",
                    format!(
                        "The agent reports folder has not answered since {}.",
                        i.clock(at?)
                    ),
                )
            } else {
                let problems = array(&r["problems"]);
                let files: Vec<_> = problems
                    .iter()
                    .filter(|p| !s(&p["file"]).starts_with("limits/"))
                    .collect();
                let limits: Vec<_> = problems
                    .iter()
                    .filter(|p| s(&p["file"]).starts_with("limits/"))
                    .map(|p| s(&p["reason"]))
                    .collect();
                let limits = if limits.is_empty() {
                    String::new()
                } else {
                    format!(" {}", limits.join(" "))
                };
                if !files.is_empty() {
                    let since = files.iter().filter_map(|p| i.at(&p["since"])).min()?;
                    let shown = files
                        .iter()
                        .take(5)
                        .map(|p| {
                            format!(
                                "{} ({})",
                                s(&p["file"])
                                    .strip_prefix("writers/")
                                    .unwrap_or(s(&p["file"])),
                                s(&p["reason"])
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let more = if files.len() > 5 {
                        format!(", and {} more", files.len() - 5)
                    } else {
                        String::new()
                    };
                    (
                        "unreadable",
                        format!(
                            "{} unreadable since {}: {shown}{more}.{limits}",
                            plural(files.len(), "file"),
                            i.clock(since)
                        ),
                    )
                } else {
                    (
                        "ok",
                        format!(
                            "{} and {} read {}.{limits}",
                            plural(rows(&r["threads"]).len(), "thread"),
                            plural(rows(&r["cards"]).len(), "card"),
                            i.clock(at?)
                        ),
                    )
                }
            }
        }
    };
    out["state"] = json!(state);
    out["detail"] = json!(detail);
    Some(out)
}

fn thread_name_compact(row: &CompactSeenReport) -> &str {
    row.thread()
        .and_then(|t| t.agent.name.as_deref())
        .unwrap_or_else(|| row.writer.split_once('.').map(|(_, s)| s).unwrap_or(""))
}

fn thread_name_wire(row: &V) -> String {
    row["doc"]["agent"]["name"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            s(&row["writer"])
                .split_once('.')
                .map(|(_, s)| s)
                .unwrap_or("")
                .to_owned()
        })
}

fn threads(i: &Inputs, registry_src: &V) -> Option<Vec<V>> {
    let mut out_rows = Vec::new();
    let mut reporting: HashSet<String> = HashSet::new();
    match i.reports {
        ReportsInput::Compact(owned) => {
            for row in &owned.state.threads {
                let t = row.thread()?;
                let name = thread_name_compact(row);
                reporting.insert(name.to_owned());
                let (machine, short) = row.writer.split_once('.')?;
                reporting.insert(short.to_owned());
                let at = i.seen_compact(row)?;
                if i.after(at, 86_400_000) && t.state != ThreadState::Blocked {
                    continue;
                }
                let ahead = (row.written_at_ms as f64 - row.first_seen_at as f64) / 1000.0;
                let blocked_on = if owned.blocker_is_cleared(&row.writer) {
                    V::Null
                } else {
                    t.blocked_on
                        .as_deref()
                        .map(CompactBlockedOn::to_json)
                        .unwrap_or(V::Null)
                };
                let progress = t
                    .progress
                    .as_deref()
                    .map(CompactProgress::to_json)
                    .unwrap_or(V::Null);
                let problem = row
                    .problem
                    .as_deref()
                    .map(|p| json!(p.reason.as_ref()))
                    .unwrap_or(V::Null);
                out_rows.push(json!({
                    "name": name,
                    "writer": row.writer.as_ref(),
                    "machine": machine,
                    "project": t.project.as_deref(),
                    "stream": t.stream.as_deref(),
                    "state": t.state.as_str(),
                    "doing": t.doing.as_deref(),
                    "blockedOn": blocked_on,
                    "progress": progress,
                    "model": t.agent.model.as_deref(),
                    "effort": t.agent.effort.as_deref(),
                    "account": t.agent.account.as_deref(),
                    "source": "report",
                    "lastSeen": i.iso(Some(at)),
                    "ageSeconds": i.age(Some(at)),
                    "stale": i.stale_thread_compact(row, owned)?,
                    "clockAheadSeconds": if ahead > 120.0 { json!(ecma::math_round(ahead)) } else { V::Null },
                    "problem": problem,
                    "registrySource": null
                }));
            }
        }
        ReportsInput::Wire(wire) => {
            for row in rows(&wire["threads"]) {
                let name = thread_name_wire(row);
                reporting.insert(name.clone());
                let (machine, short) = s(&row["writer"]).split_once('.')?;
                reporting.insert(short.to_owned());
                let at = i.seen_wire(row)?;
                if i.after(at, 86_400_000) && row["doc"]["state"] != "blocked" {
                    continue;
                }
                let first = number(&row["firstSeenAt"])?;
                let ahead = (number(&row["writtenAtMs"]).unwrap_or(first) - first) / 1000.0;
                let d = &row["doc"];
                out_rows.push(json!({
                    "name": name,
                    "writer": row["writer"],
                    "machine": machine,
                    "project": d["project"],
                    "stream": d["stream"],
                    "state": d["state"],
                    "doing": d["doing"],
                    "blockedOn": d["blockedOn"],
                    "progress": d["progress"],
                    "model": d["agent"]["model"],
                    "effort": d["agent"]["effort"],
                    "account": d["agent"]["account"],
                    "source": "report",
                    "lastSeen": i.iso(Some(at)),
                    "ageSeconds": i.age(Some(at)),
                    "stale": i.stale_thread_wire(row)?,
                    "clockAheadSeconds": if ahead > 120.0 { json!(ecma::math_round(ahead)) } else { V::Null },
                    "problem": row["problem"]["reason"],
                    "registrySource": null
                }));
            }
        }
    }

    let hidden = rows(i.parts.index("hiddenThreadNames"));
    match i.registry {
        RegistryInput::Compact(reg) => {
            if let Some(entries) = &reg.data {
                for row in entries {
                    let name = row.thread.name.as_ref();
                    if reporting.contains(name) || hidden.iter().any(|h| h.as_str() == Some(name)) {
                        continue;
                    }
                    let at = if i.ctx.is_wire() {
                        match row.thread.last_seen_iso() {
                            Some(iso) => match i.at(&json!(iso)) {
                                Some(ms) => ms,
                                None => continue,
                            },
                            None => continue,
                        }
                    } else {
                        let Some(ms) = row.thread.last_seen_ms else {
                            continue;
                        };
                        ms
                    };
                    if i.after(at, 86_400_000) {
                        continue;
                    }
                    let file_problem = if let Some(prob) = row.problem.as_deref() {
                        let shown = i
                            .ctx
                            .epoch_u64(row.read_at)
                            .map(|t| format!(" Showing its reading from {}.", i.clock(t)))
                            .unwrap_or_default();
                        json!(format!(
                            "{}{shown}",
                            part_problem_fields(i, &prob.item, &prob.reason, prob.since)?
                        ))
                    } else {
                        V::Null
                    };
                    let last_seen = row.thread.last_seen_iso();
                    out_rows.push(json!({
                        "name": name,
                        "writer": null,
                        "machine": row.thread.machine.as_deref(),
                        "project": null,
                        "stream": null,
                        "state": null,
                        "doing": row.thread.about.as_deref(),
                        "blockedOn": null,
                        "progress": null,
                        "model": null,
                        "effort": null,
                        "account": null,
                        "source": "pm-mail",
                        "lastSeen": last_seen,
                        "ageSeconds": i.age(Some(at)),
                        "stale": !file_problem.is_null() || i.after(at, 1_800_000),
                        "clockAheadSeconds": null,
                        "problem": file_problem,
                        "registrySource": part_ref_registry_compact(i, registry_src, reg, row)?
                    }));
                }
            }
        }
        RegistryInput::Wire(reg_val) => {
            for thread in rows(&reg_val["data"]) {
                if reporting.contains(s(&thread["name"]))
                    || hidden.contains(thread.get("name").unwrap_or(&NULL))
                {
                    continue;
                }
                let Some(at) = i.at(&thread["lastSeen"]) else {
                    continue;
                };
                if i.after(at, 86_400_000) {
                    continue;
                }
                let file_problem = if thread["problem"].is_object() {
                    let shown = i
                        .at(&thread["readAt"])
                        .map(|t| format!(" Showing its reading from {}.", i.clock(t)))
                        .unwrap_or_default();
                    json!(format!("{}{shown}", part_problem(i, &thread["problem"])?))
                } else {
                    V::Null
                };
                out_rows.push(json!({
                    "name": thread["name"],
                    "writer": null,
                    "machine": thread["machine"],
                    "project": null,
                    "stream": null,
                    "state": null,
                    "doing": thread["about"],
                    "blockedOn": null,
                    "progress": null,
                    "model": null,
                    "effort": null,
                    "account": null,
                    "source": "pm-mail",
                    "lastSeen": thread["lastSeen"],
                    "ageSeconds": i.age(Some(at)),
                    "stale": !file_problem.is_null() || i.after(at, 1_800_000),
                    "clockAheadSeconds": null,
                    "problem": file_problem,
                    "registrySource": part_ref(i, registry_src, reg_val, Some(thread), "Not in the pm-mail registry.")?
                }));
            }
        }
    }

    let rank = |r: &V| {
        ["blocked", "waiting", "working", "paused", "idle", "done"]
            .iter()
            .position(|w| r["state"] == *w)
            .unwrap_or(9)
    };
    out_rows.sort_by(|a, b| {
        (a["source"] == "pm-mail")
            .cmp(&(b["source"] == "pm-mail"))
            .then_with(|| (a["stale"] == true).cmp(&(b["stale"] == true)))
            .then_with(|| rank(a).cmp(&rank(b)))
            .then_with(|| {
                number(&a["ageSeconds"])
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&number(&b["ageSeconds"]).unwrap_or(f64::INFINITY))
            })
            .then_with(|| i.ctx.compare(s(&a["name"]), s(&b["name"])))
    });
    Some(out_rows)
}

const STATES: [&str; 9] = [
    "todo", "working", "review", "fixing", "blocked", "ready", "landed", "done", "dropped",
];

fn empty_counts() -> V {
    let mut c = json!({"total": 0});
    for state in STATES {
        c[state] = json!(0);
    }
    c
}

fn counts_from_array(total: u64, counts: &[u64; 9]) -> V {
    let mut c = json!({"total": total});
    for (idx, state) in STATES.iter().enumerate() {
        c[*state] = json!(counts[idx]);
    }
    c
}

fn add_counts(into: &mut V, from: &V) -> Option<()> {
    for key in std::iter::once("total").chain(STATES) {
        into[key] = json!(into[key].as_u64()? + from[key].as_u64()?);
    }
    Some(())
}

fn completion(c: &V) -> Option<V> {
    let of = c["total"].as_u64()?.checked_sub(c["dropped"].as_u64()?)?;
    Some(if of > 0 {
        json!({"done": c["landed"].as_u64()? + c["done"].as_u64()?, "of": of})
    } else {
        V::Null
    })
}

fn projects(i: &Inputs) -> Option<Vec<V>> {
    let mut groups: Vec<(String, V, Vec<V>)> = Vec::new();
    match i.reports {
        ReportsInput::Compact(owned) => {
            let mut tasks: Vec<&Arc<CompactSeenReport>> = owned.state.tasks.iter().collect();
            tasks.sort_by(|a, b| {
                i.seen_compact(b)
                    .cmp(&i.seen_compact(a))
                    .then_with(|| i.ctx.compare(&a.writer, &b.writer))
            });
            for row in tasks {
                let t = row.tasks()?;
                let id = t.project.as_ref();
                let at = groups
                    .iter()
                    .position(|(s, _, _)| s == id)
                    .unwrap_or_else(|| {
                        groups.push((id.to_owned(), V::Null, Vec::new()));
                        groups.len() - 1
                    });
                if groups[at].1.is_null()
                    && let Some(label) = t.project_label.as_deref()
                {
                    groups[at].1 = json!(label);
                }
                if groups[at]
                    .2
                    .iter()
                    .any(|s| s["id"].as_str() == Some(t.stream.as_ref()))
                {
                    continue;
                }
                let mut state_counts = [0u64; 9];
                let mut items = Vec::with_capacity(t.items.len());
                for item in &t.items {
                    if let Some(idx) = item.state.known_index() {
                        state_counts[idx] += 1;
                    }
                    let mut val = item.to_json();
                    let updated_ms = if i.ctx.is_wire() {
                        match item.updated_at.as_deref() {
                            Some(raw) => i.at(&json!(raw)),
                            None => None,
                        }
                    } else if let Some(raw) = item.updated_at.as_deref() {
                        if ecma::iso_prefixed(raw) {
                            item.updated_at_ms
                        } else {
                            i.ctx.mark_missing();
                            None
                        }
                    } else {
                        None
                    };
                    val["ageSeconds"] = i.age(updated_ms);
                    items.push(val);
                }
                let counts = counts_from_array(t.items.len() as u64, &state_counts);
                let seen = i.seen_compact(row)?;
                let problem = row
                    .problem
                    .as_deref()
                    .map(|p| json!(p.reason.as_ref()))
                    .unwrap_or(V::Null);
                let stream_label = t.stream_label.as_deref().unwrap_or(t.stream.as_ref());
                groups[at].2.push(json!({
                    "id": t.stream.as_ref(),
                    "label": stream_label,
                    "owner": t.owner.as_deref(),
                    "goal": t.goal.as_deref(),
                    "writer": row.writer.as_ref(),
                    "board": i.board_compact(t.board.as_deref()),
                    "source": "tasks",
                    "asOf": i.iso(Some(seen)),
                    "ageSeconds": i.age(Some(seen)),
                    "stale": row.problem.is_some() || i.after(seen, 86_400_000),
                    "problem": problem,
                    "counts": counts,
                    "completion": completion(&counts)?,
                    "items": items
                }));
            }
            let mut threads: Vec<&Arc<CompactSeenReport>> = owned.state.threads.iter().collect();
            threads.sort_by_key(|r| std::cmp::Reverse(i.seen_compact(r)));
            for row in threads {
                let t = row.thread()?;
                let Some(project) = t.project.as_deref().filter(|s| !s.is_empty()) else {
                    continue;
                };
                let Some(stream) = t.stream.as_deref().filter(|s| !s.is_empty()) else {
                    continue;
                };
                let Some(link) = t
                    .links
                    .iter()
                    .find(|l| l.label.to_lowercase().contains("board"))
                else {
                    continue;
                };
                let at = groups
                    .iter()
                    .position(|(s, _, _)| s == project)
                    .unwrap_or_else(|| {
                        groups.push((project.to_owned(), V::Null, Vec::new()));
                        groups.len() - 1
                    });
                if groups[at]
                    .2
                    .iter()
                    .any(|s| s["id"].as_str() == Some(stream))
                {
                    continue;
                }
                let seen = i.seen_compact(row)?;
                let problem = row
                    .problem
                    .as_deref()
                    .map(|p| json!(p.reason.as_ref()))
                    .unwrap_or(V::Null);
                groups[at].2.push(json!({
                    "id": stream,
                    "label": stream,
                    "owner": thread_name_compact(row),
                    "goal": null,
                    "writer": row.writer.as_ref(),
                    "board": i.board_compact(Some(link.path.as_ref())),
                    "source": "board-only",
                    "asOf": i.iso(Some(seen)),
                    "ageSeconds": i.age(Some(seen)),
                    "stale": i.stale_thread_compact(row, owned)?,
                    "problem": problem,
                    "counts": null,
                    "completion": null,
                    "items": []
                }));
            }
        }
        ReportsInput::Wire(wire) => {
            let mut tasks: Vec<&V> = rows(&wire["tasks"]).iter().collect();
            tasks.sort_by(|a, b| {
                i.seen_wire(b)
                    .cmp(&i.seen_wire(a))
                    .then_with(|| i.ctx.compare(s(&a["writer"]), s(&b["writer"])))
            });
            for row in tasks {
                let d = &row["doc"];
                let id = s(&d["project"]);
                let at = groups
                    .iter()
                    .position(|(s, _, _)| s == id)
                    .unwrap_or_else(|| {
                        groups.push((id.to_owned(), V::Null, Vec::new()));
                        groups.len() - 1
                    });
                if groups[at].1.is_null() {
                    groups[at].1 = d["projectLabel"].clone();
                }
                if groups[at].2.iter().any(|s| s["id"] == d["stream"]) {
                    continue;
                }
                let mut counts = empty_counts();
                let mut items = array(&d["items"]);
                counts["total"] = json!(items.len());
                for item in &mut items {
                    let state = s(&item["state"]).to_owned();
                    if STATES.contains(&state.as_str()) {
                        counts[&state] = json!(counts[&state].as_u64()? + 1);
                    }
                    item["ageSeconds"] = i.age(i.at(&item["updatedAt"]));
                }
                let seen = i.seen_wire(row)?;
                groups[at].2.push(json!({
                    "id": d["stream"],
                    "label": d.get("streamLabel").filter(|v| !v.is_null()).unwrap_or(&d["stream"]),
                    "owner": d["owner"],
                    "goal": d["goal"],
                    "writer": row["writer"],
                    "board": i.board_wire(&d["board"], wire),
                    "source": "tasks",
                    "asOf": i.iso(Some(seen)),
                    "ageSeconds": i.age(Some(seen)),
                    "stale": !row["problem"].is_null() || i.after(seen, 86_400_000),
                    "problem": row["problem"]["reason"],
                    "counts": counts,
                    "completion": completion(&counts)?,
                    "items": items
                }));
            }
            let mut threads: Vec<&V> = rows(&wire["threads"]).iter().collect();
            threads.sort_by_key(|r| std::cmp::Reverse(i.seen_wire(r)));
            for row in threads {
                let d = &row["doc"];
                if s(&d["project"]).is_empty() || s(&d["stream"]).is_empty() {
                    continue;
                }
                let links = array(&d["links"]);
                let Some(link) = links
                    .iter()
                    .find(|l| s(&l["label"]).to_lowercase().contains("board"))
                else {
                    continue;
                };
                let id = s(&d["project"]);
                let at = groups
                    .iter()
                    .position(|(s, _, _)| s == id)
                    .unwrap_or_else(|| {
                        groups.push((id.to_owned(), V::Null, Vec::new()));
                        groups.len() - 1
                    });
                if groups[at].2.iter().any(|s| s["id"] == d["stream"]) {
                    continue;
                }
                let seen = i.seen_wire(row)?;
                groups[at].2.push(json!({
                    "id": d["stream"],
                    "label": d["stream"],
                    "owner": thread_name_wire(row),
                    "goal": null,
                    "writer": row["writer"],
                    "board": i.board_wire(&link["path"], wire),
                    "source": "board-only",
                    "asOf": i.iso(Some(seen)),
                    "ageSeconds": i.age(Some(seen)),
                    "stale": i.stale_thread_wire(row)?,
                    "problem": row["problem"]["reason"],
                    "counts": null,
                    "completion": null,
                    "items": []
                }));
            }
        }
    }

    let mut out = Vec::new();
    for (id, label, mut streams) in groups {
        streams.sort_by(|a, b| i.ctx.compare(s(&a["label"]), s(&b["label"])));
        let mut counts = empty_counts();
        for stream in &streams {
            if stream["counts"].is_object() {
                add_counts(&mut counts, &stream["counts"])?;
            }
        }
        out.push(json!({
            "id": id,
            "label": if label.is_null() { json!(id) } else { label },
            "streams": streams,
            "counts": counts,
            "completion": completion(&counts)?
        }));
    }
    out.sort_by(|a, b| i.ctx.compare(s(&a["label"]), s(&b["label"])));
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// Cards (mirrors `read_status_cards.rs`; only parses `doc_json` for truly open cards)

fn closing(e: &V) -> bool {
    ["answer", "note", "done"].contains(&s(&e["answer"]["kind"]))
}

fn is_open_compact(
    row: &CompactSeenReport,
    card: &crate::compact_share::reports::CompactCardDoc,
    answers: &[V],
) -> bool {
    card.state == CardState::Open
        && !answers.iter().filter(|e| closing(e)).any(|e| {
            e["answer"]["cardId"].as_str() == Some(card.id.as_ref())
                && e["answer"]["cardSeq"].as_u64() == Some(card.seq)
                && e["answer"]["cardSha256"]
                    .as_str()
                    .is_some_and(|h| row.sha256.eq_str(h))
        })
}

fn is_open_wire(row: &V, answers: &[V]) -> bool {
    let doc = &row["doc"];
    doc["state"] == "open"
        && !answers.iter().filter(|e| closing(e)).any(|e| {
            e["answer"]["cardId"] == doc["id"]
                && e["answer"]["cardSeq"] == doc["seq"]
                && row["sha256"] == e["answer"]["cardSha256"]
        })
}

fn delivery(i: &Inputs, entry: &V) -> Option<V> {
    let share = &entry["share"];
    let bell = &entry["doorbell"];
    let sends = array(i.parts.index("sends"));
    let summary = sends
        .iter()
        .rev()
        .find(|s| !entry["sendId"].is_null() && s["sendId"] == entry["sendId"])
        .map(|s| s["summary"].clone())
        .unwrap_or(V::Null);
    let warning = if summary.is_null() || summary["state"] == "done" {
        V::Null
    } else if summary["state"] == "skipped" {
        summary["detail"].clone()
    } else if summary["state"] == "pending" || summary["attempts"] == 0 {
        json!("The signed send summary is not on the share yet.")
    } else {
        let n = summary["attempts"].as_u64()?;
        json!(if n >= 20 {
            format!(
                "The signed send summary could not be written to the share after {n} attempts; no more retries."
            )
        } else {
            format!(
                "The signed send summary could not be written to the share yet (attempt {n} of 20); retrying."
            )
        })
    };
    let attempt = |step: &V| -> Option<String> {
        let n = step["attempts"].as_u64()?;
        Some(if n > 0 {
            format!(" (attempt {n} of 20)")
        } else {
            String::new()
        })
    };
    let (state, at, detail) = if entry["sharedOnly"] == true {
        (
            "published",
            V::Null,
            json!("The signed answer is on the share."),
        )
    } else if share["state"] == "skipped" {
        ("failed", share["at"].clone(), share["detail"].clone())
    } else if share["state"] == "failed" {
        let n = share["attempts"].as_u64()?;
        (
            "failed",
            share["at"].clone(),
            json!(if n >= 20 {
                format!(
                    "The signed copy could not be written to the share after {n} attempts; no more retries."
                )
            } else if share["detail"] == "The reports share is not available." {
                format!(
                    "The reports share is not available, so the signed copy is not written yet{}; retrying.",
                    attempt(share)?
                )
            } else {
                format!(
                    "The signed copy could not be written to the share yet{}; retrying.",
                    attempt(share)?
                )
            }),
        )
    } else if bell["state"] == "failed" {
        let n = bell["attempts"].as_u64()?;
        (
            "failed",
            bell["at"].clone(),
            json!(if n >= 20 {
                format!(
                    "The asker could not be rung after {n} attempts; no more retries. The signed answer is on the share."
                )
            } else {
                format!(
                    "The asker could not be rung yet{}; retrying. The signed answer is on the share.",
                    attempt(bell)?
                )
            }),
        )
    } else if share["state"] == "done"
        && bell["state"] == "skipped"
        && bell["detail"] == "No mail name; the agent reads its answers file."
    {
        ("rang", bell["at"].clone(), bell["detail"].clone())
    } else if bell["state"] == "skipped" {
        ("failed", bell["at"].clone(), bell["detail"].clone())
    } else if share["state"] == "done" && bell["state"] == "done" {
        ("rang", bell["at"].clone(), bell["detail"].clone())
    } else {
        ("pending", V::Null, json!("Delivering."))
    };
    Some(json!({
        "share": share,
        "doorbell": bell,
        "summary": summary,
        "summaryWarning": warning,
        "state": state,
        "at": at,
        "detail": detail
    }))
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn file_url(writer: &str, path: &str) -> String {
    format!(
        "/api/status/files/{}/{}",
        encode(writer),
        path.split('/').map(encode).collect::<Vec<_>>().join("/")
    )
}

fn media(writer: &str, item: &V) -> Option<V> {
    let mut out = item.clone();
    out.as_object()?;
    out["url"] = json!(file_url(writer, s(&item["path"])));
    out["posterUrl"] = if s(&item["poster"]).is_empty() {
        V::Null
    } else {
        json!(file_url(writer, s(&item["poster"])))
    };
    Some(out)
}

fn sections(writer: &str, items: &V) -> Option<Vec<V>> {
    array(items)
        .iter()
        .map(|item| {
            let mut v = item.clone();
            if item["type"] == "media" && item.get("unknown").is_none() {
                v["items"] = json!(
                    array(&item["items"])
                        .iter()
                        .map(|m| media(writer, m))
                        .collect::<Option<Vec<_>>>()?
                );
            }
            Some(v)
        })
        .collect()
}

fn card(i: &Inputs, doc: &V, hash: &V, problem: V, notes: Vec<V>) -> Option<V> {
    let anchored = doc["kind"] == "judgement" && !doc["recommendation"].is_null();
    let asker = match i.reports {
        ReportsInput::Compact(owned) => {
            if let Some(writer_str) = doc["writer"].as_str()
                && let Some(row) = owned
                    .state
                    .threads
                    .iter()
                    .find(|r| r.writer.as_ref() == writer_str)
            {
                if i.stale_thread_compact(row, owned)? {
                    json!("gone")
                } else {
                    json!(row.thread()?.state.as_str())
                }
            } else {
                json!("unknown")
            }
        }
        ReportsInput::Wire(wire) => {
            if let Some(row) = rows(&wire["threads"])
                .iter()
                .find(|r| r["writer"] == doc["writer"])
            {
                if i.stale_thread_wire(row)? {
                    json!("gone")
                } else {
                    row["doc"]["state"].clone()
                }
            } else {
                json!("unknown")
            }
        }
    };
    let writer = s(&doc["writer"]);
    let mut options = Vec::new();
    for o in array(&doc["options"]) {
        options.push(json!({
            "key": o["key"],
            "label": o["label"],
            "gets": o["gets"],
            "costs": o["costs"],
            "media": array(&o["media"]).iter().map(|m| media(writer, m)).collect::<Option<Vec<_>>>()?,
            "sections": sections(writer, &o["sections"])?,
            "stance": o["stance"]
        }));
    }
    let fallback_label = json!(
        s(&doc["id"])
            .split_once("--")
            .map(|(_, s)| s)
            .unwrap_or(s(&doc["id"]))
    );
    Some(json!({
        "id": doc["id"],
        "media": doc.get("media").cloned().unwrap_or(json!([])),
        "label": doc.get("label").filter(|v| !v.is_null()).unwrap_or(&fallback_label),
        "writer": doc["writer"],
        "seq": doc["seq"],
        "project": doc["project"],
        "stream": doc["stream"],
        "askedBy": doc["askedBy"],
        "title": doc["title"],
        "now": doc["now"],
        "nowAsOf": doc["nowAsOf"],
        "oldAnswer": doc["oldAnswer"],
        "whatChanged": doc["whatChanged"],
        "options": options,
        "recommendation": doc["recommendation"],
        "multiple": doc["multiple"] == true,
        "urgency": doc["urgency"],
        "blocks": doc["blocks"],
        "sections": sections(writer, &doc["sections"])?,
        "batch": doc["batch"],
        "evidence": doc.get("evidence").cloned().unwrap_or(json!([])),
        "replies": doc.get("replies").cloned().unwrap_or(json!([])),
        "kind": doc.get("kind").cloned().unwrap_or(json!("question")),
        "neededBy": doc["neededBy"],
        "exception": doc["exception"],
        "reversible": doc["reversible"],
        "verdict": doc["verdict"],
        "group": doc["group"],
        "agreement": doc["agreement"],
        "doubt": doc["doubt"],
        "appliesTo": doc["appliesTo"],
        "changesCanon": doc.get("changesCanon").cloned().unwrap_or(json!([])),
        "related": doc.get("related").cloned().unwrap_or(json!([])),
        "askedAt": doc["askedAt"],
        "ageSeconds": number(&i.age(i.at(&doc["askedAt"]))).unwrap_or(0.0),
        "askerState": asker,
        "cardSha256": hash,
        "notes": notes,
        "problem": if !problem.is_null() {
            problem.clone()
        } else if anchored {
            json!("This judgement card carries a recommendation, so it cannot be answered: a judgement must not anchor the answer.")
        } else {
            V::Null
        },
        "answerable": problem.is_null() && !anchored && doc["kind"] != "sample"
    }))
}

fn quiet_notice(i: &Inputs, doc: &V) -> Option<String> {
    const DAY_MS: i64 = 86_400_000;
    const EST_MS: i64 = 5 * 3_600_000;
    let asked = i.at(&doc["askedAt"])?;
    if !i.after(asked, 3 * DAY_MS) {
        return None;
    }
    let (last, ttl) = match i.reports {
        ReportsInput::Compact(owned) => {
            let writer_str = doc["writer"].as_str()?;
            let thread = owned
                .state
                .threads
                .iter()
                .find(|row| row.writer.as_ref() == writer_str && row.problem.is_none())?;
            let last = i.seen_compact(thread)?;
            let ttl = thread
                .thread()?
                .ttl_seconds
                .and_then(|v| i64::try_from(v).ok())
                .unwrap_or(86_400);
            (last, ttl)
        }
        ReportsInput::Wire(wire) => {
            let thread = rows(&wire["threads"])
                .iter()
                .find(|row| row["writer"] == doc["writer"] && row["problem"].is_null())?;
            let last = i.seen_wire(thread)?;
            let ttl = thread["doc"]["ttlSeconds"].as_i64().unwrap_or(86_400);
            (last, ttl)
        }
    };
    if !i.after(last, ttl * 1000) {
        return None;
    }
    let date = ecma::to_iso_string(last - EST_MS)?;
    Some(format!(
        "asked {} days ago; thread quiet since {}",
        (i.now - asked) / DAY_MS,
        &date[..10]
    ))
}

fn cards_compose(i: &Inputs) -> Option<V> {
    let answers = i.parts.index("answers").as_array()?;
    let answer_history_problem = i.parts.index("answerHistoryProblem");
    let mut open = Vec::new();
    match i.reports {
        ReportsInput::Compact(owned) => {
            for row in &owned.state.cards {
                let Some(card_meta) = row.card() else {
                    continue;
                };
                if !is_open_compact(row, card_meta, answers) {
                    continue;
                }
                let doc = card_meta.to_json();
                let notes = answers
                    .iter()
                    .filter(|e| !closing(e) && e["answer"]["cardId"] == doc["id"])
                    .map(|e| {
                        Some(json!({
                            "kind": if e["answer"]["kind"] == "object" { "object" } else { "note" },
                            "note": e["answer"]["note"],
                            "at": e["answer"]["answeredAt"],
                            "delivery": delivery(i, e)?
                        }))
                    })
                    .collect::<Option<Vec<_>>>()?;
                let highest = answers
                    .iter()
                    .filter(|e| e["answer"]["cardId"] == doc["id"])
                    .filter_map(|e| e["answer"]["cardSeq"].as_u64())
                    .max()
                    .unwrap_or(0);
                let problem = if let Some(prob) = row.problem.as_deref() {
                    json!(prob.reason.as_ref())
                } else if doc["seq"].as_u64()? < highest {
                    json!("its seq is lower than one already read, so this copy is ignored")
                } else {
                    V::Null
                };
                let hash = json!(row.sha256.to_hex_string());
                let mut c = card(i, &doc, &hash, problem, notes)?;
                if let Some(notice) = quiet_notice(i, &doc) {
                    c["quietNotice"] = json!(notice);
                }
                if !s(answer_history_problem).is_empty() {
                    c["answerable"] = json!(false);
                    c["problem"] = answer_history_problem.clone();
                }
                open.push(c);
            }
        }
        ReportsInput::Wire(wire) => {
            for row in rows(&wire["cards"]) {
                let doc = &row["doc"];
                if !is_open_wire(row, answers) {
                    continue;
                }
                let notes = answers
                    .iter()
                    .filter(|e| !closing(e) && e["answer"]["cardId"] == doc["id"])
                    .map(|e| {
                        Some(json!({
                            "kind": if e["answer"]["kind"] == "object" { "object" } else { "note" },
                            "note": e["answer"]["note"],
                            "at": e["answer"]["answeredAt"],
                            "delivery": delivery(i, e)?
                        }))
                    })
                    .collect::<Option<Vec<_>>>()?;
                let highest = answers
                    .iter()
                    .filter(|e| e["answer"]["cardId"] == doc["id"])
                    .filter_map(|e| e["answer"]["cardSeq"].as_u64())
                    .max()
                    .unwrap_or(0);
                let problem = if !row["problem"].is_null() {
                    row["problem"]["reason"].clone()
                } else if doc["seq"].as_u64()? < highest {
                    json!("its seq is lower than one already read, so this copy is ignored")
                } else {
                    V::Null
                };
                let mut c = card(i, doc, &row["sha256"], problem, notes)?;
                if let Some(notice) = quiet_notice(i, doc) {
                    c["quietNotice"] = json!(notice);
                }
                if !s(answer_history_problem).is_empty() {
                    c["answerable"] = json!(false);
                    c["problem"] = answer_history_problem.clone();
                }
                open.push(c);
            }
        }
    }

    let urgency = |c: &V| {
        ["now", "today", "this-week", "whenever"]
            .iter()
            .position(|s| c["urgency"] == *s)
            .map(|n| n as i32)
            .unwrap_or(-1)
    };
    open.sort_by(|a, b| {
        urgency(a)
            .cmp(&urgency(b))
            .then_with(|| {
                number(&b["ageSeconds"])
                    .partial_cmp(&number(&a["ageSeconds"]))
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| i.ctx.compare(s(&a["id"]), s(&b["id"])))
    });
    let mut batches: Vec<V> = Vec::new();
    for c in &open {
        if s(&c["batch"]).is_empty() {
            continue;
        }
        let at = batches
            .iter()
            .position(|b| b["id"] == c["batch"])
            .unwrap_or_else(|| {
                batches.push(json!({"id": c["batch"], "label": c["batch"], "cardIds": []}));
                batches.len() - 1
            });
        batches[at]["cardIds"].as_array_mut()?.push(c["id"].clone());
    }
    let mut history = Vec::new();
    for e in answers.iter().filter(|e| closing(e)).rev().take(50) {
        let a = &e["answer"];
        history.push(json!({
            "card": card(i, &e["card"], &a["cardSha256"], V::Null, vec![])?,
            "answer": {
                "kind": a["kind"],
                "choice": a["choice"],
                "note": a["note"],
                "via": a["via"],
                "answeredAt": a["answeredAt"],
                "device": a["device"],
                "delivery": delivery(i, e)?
            }
        }));
    }
    let reports_ok_complete = match i.reports {
        ReportsInput::Compact(owned) => {
            owned.state.state == "ok"
                && owned.state.complete
                && (i.scan_stamps[0].is_some() || owned.state.scanned_at.is_some())
        }
        ReportsInput::Wire(r) => {
            r["state"] == "ok"
                && r["complete"] == true
                && (i.scan_stamps[0].is_some() || !r["scannedAt"].is_null())
        }
    };
    let complete = s(answer_history_problem).is_empty()
        && reports_ok_complete
        && number(&i.age(i.report_stamp())).unwrap_or(0.0) <= 60.0;
    Some(json!({
        "source": "agent-reports",
        "open": open,
        "answerHistoryProblem": answer_history_problem,
        "answered": if !history.is_empty() || s(answer_history_problem).is_empty() {
            json!(history)
        } else {
            V::Null
        },
        "batches": batches,
        "complete": complete
    }))
}

// ---------------------------------------------------------------------------------------------
// Machines, Usage, Simlock panels (mirrors `read_status_panels.rs` & `simlock_view`)

fn warnings(sample: &V) -> Option<Vec<String>> {
    if sample.is_null() {
        return Some(vec![]);
    }
    let mut out = Vec::new();
    if let Some(free) = number(&gb(&sample["disk"]["freeBytes"]))
        && free < 50.0
    {
        out.push(format!(
            "Disk {} GB free, under the 50 GB floor",
            view::fixed(free, 1)?
        ));
    }
    if let Some(free) = number(&gb(&sample["memAvailableBytes"]))
        && free < 6.0
    {
        out.push(format!(
            "Memory {} GB available, under the 6 GB floor",
            view::fixed(free, 1)?
        ));
    }
    Some(out)
}

fn machines(i: &Inputs, browsers: &V) -> Option<Vec<V>> {
    let mut out_rows = Vec::new();
    for id in i.parts.index("machineIds").as_array()? {
        let id = s(id);
        let c = &i.parts.index("machines")[id];
        c.as_object()?;
        let sample = &c["data"];
        let at = i.at(&c["asOf"]);
        let age_val = i.age(at);
        let (mut state, mut detail) = if c["error"].is_object() {
            let shown = at
                .map(|t| format!(" Showing the reading from {}.", i.clock(t)))
                .unwrap_or_default();
            (
                "unreachable",
                Some(format!(
                    "{} (since {}).{shown}",
                    trim_period(s(&c["error"]["reason"])),
                    i.clock(i.at(&c["error"]["since"])?)
                )),
            )
        } else if at.is_none() {
            ("stale", Some("Waiting for the first reading.".to_owned()))
        } else if number(&age_val)? > 180.0 {
            (
                "stale",
                Some(format!("No new reading since {}.", i.clock(at?))),
            )
        } else {
            ("ok", None)
        };
        for (key, label, noun, read_at, held) in [
            (
                "cpu",
                "CPU",
                "CPU",
                i.at(&sample["cpuReadAt"]),
                number(&sample["busyPct"]).is_some(),
            ),
            (
                "disk",
                "Disk",
                "disk",
                i.at(&sample["diskReadAt"]),
                sample["disk"].is_object(),
            ),
            (
                "memory",
                "Memory",
                "memory",
                i.at(&sample["memoryReadAt"]),
                number(&sample["memAvailableBytes"]).is_some(),
            ),
        ] {
            let ps = array(&c["problems"]);
            let Some(p) = ps.iter().find(|p| p["item"] == key) else {
                continue;
            };
            if state == "ok" {
                state = "stale";
            }
            let kept = if held && read_at.is_some() {
                format!(" Showing the {noun} reading from {}.", i.clock(read_at?))
            } else {
                format!(" No earlier {noun} reading.")
            };
            let words = format!(
                "{label}: {} (since {}).{kept}",
                trim_period(s(&p["reason"])),
                i.clock(i.at(&p["since"])?)
            );
            detail = Some(detail.map(|d| format!("{d} {words}")).unwrap_or(words));
        }
        let bs = array(&i.parts.index("browsers")["data"]);
        let browser = bs.iter().find(|b| b["machine"] == id);
        let b = browser.unwrap_or(&V::Null);
        out_rows.push(json!({
            "id": id,
            "label": i.parts.index("machineLabels")[id],
            "state": state,
            "asOf": i.iso(at),
            "ageSeconds": age_val,
            "detail": detail,
            "cpu": if sample.is_object() {
                json!({"cores": sample["cores"], "load1": round1(&sample["load1"]), "busyPct": sample["busyPct"]})
            } else {
                V::Null
            },
            "memory": if sample.is_object() {
                json!({"availableGb": gb(&sample["memAvailableBytes"]), "totalGb": gb(&sample["memTotalBytes"])})
            } else {
                V::Null
            },
            "disk": if sample["disk"].is_object() {
                json!([{"mount": sample["disk"]["mount"], "freeGb": number(&gb(&sample["disk"]["freeBytes"])).unwrap_or(0.0), "totalGb": number(&gb(&sample["disk"]["totalBytes"])).unwrap_or(0.0)}])
            } else {
                json!([])
            },
            "browsers": b["browsers"],
            "recordings": b["recordings"],
            "gpuPct": b["gpuPct"],
            "sessions": b.get("sessions").cloned().unwrap_or(json!([])),
            "slotsSource": part_ref(i, browsers, i.parts.index("browsers"), browser, "browser-check has no row for this machine.")?,
            "warnings": warnings(sample)?
        }));
    }
    Some(out_rows)
}

fn machines_source(i: &Inputs, machine_rows: &[V]) -> Option<V> {
    let at = i
        .parts
        .index("machineIds")
        .as_array()?
        .iter()
        .filter_map(|id| i.at(&i.parts.index("machines")[s(id)]["asOf"]))
        .max();
    let mut out = json!({
        "id": "machines",
        "label": "Machines",
        "asOf": i.iso(at),
        "ageSeconds": i.age(at),
        "staleAfterSeconds": 180
    });
    let (state, detail) =
        if at.is_none() && machine_rows.iter().all(|r| r["state"] != "unreachable") {
            ("missing", "Waiting for the first reading.".into())
        } else {
            let off: Vec<_> = machine_rows.iter().filter(|r| r["state"] != "ok").collect();
            if off.is_empty() {
                (
                    "ok",
                    format!(
                        "All {} machines read by {}.",
                        machine_rows.len(),
                        i.clock_or_now(at)
                    ),
                )
            } else {
                (
                    "stale",
                    off.iter()
                        .map(|r| {
                            format!("{} {}: {}", s(&r["label"]), s(&r["state"]), s(&r["detail"]))
                                .trim_matches(js_whitespace)
                                .to_owned()
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            }
        };
    out["state"] = json!(state);
    out["detail"] = json!(detail);
    Some(out)
}

fn usage_source(i: &Inputs) -> Option<V> {
    let usage = i.parts.index("usage");
    if usage.is_null() {
        return Some(json!({
            "id": "usage",
            "label": "Account usage",
            "state": "missing",
            "asOf": null,
            "ageSeconds": null,
            "staleAfterSeconds": null,
            "detail": "Waiting for the dashboard’s first account sample."
        }));
    }
    let at = i.at(&usage["sampledAt"])?;
    let age_val = i.age(Some(at));
    let stale = number(&age_val)? > number(&usage["staleAfterSeconds"])?;
    Some(json!({
        "id": "usage",
        "label": "Account usage",
        "state": if stale { "stale" } else { "ok" },
        "asOf": i.iso(Some(at)),
        "ageSeconds": age_val,
        "staleAfterSeconds": usage["staleAfterSeconds"],
        "detail": if stale {
            format!("No account sample since {}.", i.clock(at))
        } else {
            format!("Sampled {}.", i.clock(at))
        }
    }))
}

fn usage(i: &Inputs, source: &V) -> Option<V> {
    let usage = i.parts.index("usage");
    if usage.is_null() {
        return Some(json!({"source": "usage", "warnings": [], "accounts": []}));
    }
    let mut accounts = Vec::new();
    let mut warn_rows: Vec<(f64, V)> = Vec::new();
    for account in usage["accounts"].as_array()? {
        if account["hidden"] == true {
            continue;
        }
        for w in account["windows"].as_array()? {
            if !["rate_limit", "balance"].contains(&s(&w["kind"])) {
                continue;
            }
            let reset = i.at(&w["resetAt"]);
            let sampled = i.at(w
                .get("sampledAt")
                .filter(|v| !v.is_null())
                .unwrap_or(&account["sampledAt"]));
            let reset_passed = w["resetPassed"] == true
                || reset.is_some_and(|r| i.due(r) && sampled.is_none_or(|t| t < r));
            let reading_at = sampled.or_else(|| i.at(&usage["sampledAt"]));
            let reading_stale = number(&i.age(reading_at))
                .zip(number(&usage["staleAfterSeconds"]))
                .is_some_and(|(age, limit)| age > limit);
            let stale = source["state"] != "ok"
                || account["status"] != "ok"
                || w["cached"] == true
                || reset_passed
                || reading_stale;
            let at_limit =
                number(&w["usedPercent"]).is_some_and(|n| n >= 100.0) && !reset_passed && !stale;
            let burn = if reset_passed { &V::Null } else { &w["burn"] };
            let full = i.at(&burn["projectedFullAt"]);
            accounts.push(json!({
                "id": account["id"],
                "provider": account["provider"],
                "label": account["label"],
                "host": account["host"],
                "active": account["active"],
                "window": w["label"],
                "kind": w["kind"],
                "usedPct": round1(&w["usedPercent"]),
                "remaining": w["remaining"],
                "limit": w["limit"],
                "unit": w["unit"],
                "resetsAt": w["resetAt"],
                "projectedFullAt": burn["projectedFullAt"],
                "runsOutBeforeReset": burn["runsOutBeforeReset"],
                "atLimit": at_limit,
                "stale": stale,
                "resetPassed": reset_passed
            }));
            if stale {
                continue;
            }
            let who = format!(
                "{} {} ({})",
                s(&account["provider"]),
                s(&account["label"]),
                s(&w["label"])
            );
            if at_limit {
                let text = reset
                    .map(|r| {
                        format!(
                            "{who}: at its limit until its {} {} reset",
                            i.day(r),
                            i.clock(r)
                        )
                    })
                    .unwrap_or_else(|| format!("{who}: at its limit"));
                warn_rows.push((0.0, json!({"tone": "bad", "text": text})));
                continue;
            }
            if burn["runsOutBeforeReset"] == true && full.is_some_and(|f| i.ahead(f)) {
                let full = full?;
                let text = reset
                    .map(|r| {
                        format!(
                            "{who}: full in {}, before its {} reset",
                            i.span_until(full),
                            i.day(r)
                        )
                    })
                    .unwrap_or_else(|| format!("{who}: full in {}", i.span_until(full)));
                warn_rows.push(((full - i.now) as f64, json!({"tone": "warn", "text": text})));
            }
        }
    }
    accounts.sort_by(|a, b| {
        i.ctx
            .compare(s(&a["provider"]), s(&b["provider"]))
            .then_with(|| i.ctx.compare(s(&a["label"]), s(&b["label"])))
            .then_with(|| i.ctx.compare(s(&a["window"]), s(&b["window"])))
    });
    warn_rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some(json!({
        "source": "usage",
        "warnings": warn_rows.into_iter().map(|(_, w)| w).collect::<Vec<_>>(),
        "accounts": accounts
    }))
}

fn simlock_view(data: &V) -> V {
    let mut platforms = Vec::new();
    let mut overall = Vec::new();
    for row in rows(&data["capacity"]) {
        if !row.is_object() {
            platforms.push(row.clone());
            continue;
        }
        let platform = s(&row["platform"]);
        let mut labeled = row.clone();
        labeled["label"] = json!(match platform {
            "ios" => "iPhone",
            "android" => "Android",
            "global" => "All phones at once",
            _ => "Other",
        });
        labeled["overall"] = json!(platform == "global");
        if platform == "global" {
            overall.push(labeled);
        } else {
            platforms.push(labeled);
        }
    }
    platforms.extend(overall);
    json!({
        "source": "simlock",
        "capacity": platforms,
        "leases": data.get("leases").cloned().unwrap_or(json!([]))
    })
}

// ---------------------------------------------------------------------------------------------
// Spend (mirrors `read_status_spend.rs`)

fn quota_seen(i: &Inputs, good: &V) -> Option<i64> {
    let first = i.at(&good["firstSeenAt"])?;
    Some(
        i.at(&good["writtenAtMs"])
            .map(|t| t.min(first))
            .unwrap_or(first),
    )
}

fn spend_source(i: &Inputs) -> Option<V> {
    let q = i.parts.index("quota");
    if q.is_null() {
        return Some(json!({
            "id": "quota-watch",
            "label": "Quota watch (estimates)",
            "state": "unconfigured",
            "asOf": null,
            "ageSeconds": null,
            "staleAfterSeconds": null,
            "detail": "Per-stream quota attribution is not connected on this server."
        }));
    }
    let at = if q["good"].is_object() {
        Some(quota_seen(i, &q["good"])?)
    } else {
        None
    };
    let mut out = json!({
        "id": "quota-watch",
        "label": "Quota watch (estimates)",
        "asOf": i.iso(at),
        "ageSeconds": i.age(at),
        "staleAfterSeconds": 1800
    });
    let showing = at
        .map(|t| format!(" Showing the report from {}.", i.clock(t)))
        .unwrap_or_default();
    let (state, detail) = if q["scannedAt"].is_null() {
        ("missing", "Waiting for the first scan.".into())
    } else if q["state"] == "error" {
        (
            "error",
            format!("The quota folder could not be listed.{showing}"),
        )
    } else if q["problem"].is_object() {
        (
            "unreadable",
            format!(
                "quota/report.json unreadable since {} ({}).{showing}",
                i.clock(i.at(&q["problem"]["since"])?),
                s(&q["problem"]["reason"])
            ),
        )
    } else if q["good"].is_null() {
        (
            "missing",
            "No quota-watch report at v1/quota/report.json.".into(),
        )
    } else if number(&out["ageSeconds"])? > 1800.0 {
        (
            "stale",
            format!(
                "No new quota report since {} (quota-watch writes every 10 minutes).",
                i.clock_or_now(at)
            ),
        )
    } else {
        (
            "ok",
            format!(
                "Report from {}. Every number is an estimate.",
                i.clock_or_now(at)
            ),
        )
    };
    out["state"] = json!(state);
    out["detail"] = json!(detail);
    Some(out)
}

fn meter(m: &str) -> String {
    match m {
        "five_hour" => "5-hour window".into(),
        "seven_day" => "7-day window".into(),
        "seven_day_opus" => "7-day Opus window".into(),
        "seven_day_sonnet" => "7-day Sonnet window".into(),
        "credits_balance" => "Credits".into(),
        _ => m.replace('_', " "),
    }
}

fn rounded_value(value: &V, unit: &V) -> V {
    number(value)
        .map(|n| {
            let scale = if unit == "percent" { 10.0 } else { 100.0 };
            json!(ecma::math_round(n * scale) / scale)
        })
        .unwrap_or(V::Null)
}

fn amount(value: &V, unit: &V, m: &str) -> Option<String> {
    let Some(n) = number(value) else {
        return Some("not read".into());
    };
    if unit.is_null() {
        return Some("not read".into());
    }
    let r = number(&rounded_value(value, unit))?;
    let tiny = n > 0.0 && r == 0.0;
    if unit == "credits" {
        Some(if tiny {
            "≈ under 0.01 credits".into()
        } else {
            format!("≈ {} credits", view::fixed(r, 2)?)
        })
    } else if unit == "percent" {
        let points = if tiny {
            "under 0.1 points".into()
        } else {
            format!(
                "{} {}",
                view::fixed(r, 1)?,
                if r == 1.0 { "point" } else { "points" }
            )
        };
        Some(format!("≈ {points} of the {}", meter(m)))
    } else {
        None
    }
}

fn coverage(value: &V, noun: &str) -> Option<V> {
    Some(match number(value) {
        None => json!(format!("coverage of the {noun} not known")),
        Some(n) if n >= 0.5 => V::Null,
        Some(n) => json!(format!(
            "covers {}% of the {noun}",
            view::round_word(n * 100.0)?
        )),
    })
}

fn budget_text(b: &V) -> Option<String> {
    let Some(used) = number(&b["use"]) else {
        return Some("No reading yet.".into());
    };
    let lower = if b["known"] == true { "" } else { "at least " };
    let per = if s(&b["per"]).is_empty() {
        String::new()
    } else {
        format!(
            " {}",
            match s(&b["per"]) {
                "day" => "today".to_owned(),
                "week" => "this week".to_owned(),
                "window" => "in the window".to_owned(),
                word => format!("per {word}"),
            }
        )
    };
    let Some(limit) = number(&b["limit"]) else {
        return Some(format!(
            "{lower}{}{per}; no limit set.",
            amount(&b["use"], &b["unit"], s(&b["meter"]))?
        ));
    };
    let what = if b["unit"] == "credits" {
        "credits".into()
    } else {
        format!("points of the {}", meter(s(&b["meter"])))
    };
    let digits = if b["unit"] == "credits" { 2 } else { 1 };
    let pct = match number(&b["percent"]) {
        Some(p) => format!(" ({}%)", view::round_word(p)?),
        None => String::new(),
    };
    Some(format!(
        "{lower}≈ {} of {} {what}{per}{pct}.",
        view::fixed(used, digits)?,
        view::fixed(limit, digits)?
    ))
}

fn stream_labels_from_compact(tasks: &[Arc<CompactSeenReport>]) -> HashMap<String, String> {
    let mut labels: HashMap<String, String> = HashMap::new();
    let mut clash: HashSet<String> = HashSet::new();
    for row in tasks {
        let Some(t) = row.tasks() else { continue };
        let id = t.stream.to_string();
        let label = t.stream_label.as_deref().unwrap_or(&id).to_owned();
        if labels.get(&id).is_some_and(|v| v != &label) {
            clash.insert(id.clone());
        }
        labels.insert(id, label);
    }
    for id in clash {
        labels.remove(&id);
    }
    labels
}

fn stream_labels(i: &Inputs) -> HashMap<String, String> {
    match i.reports {
        ReportsInput::Compact(owned) => stream_labels_from_compact(&owned.state.tasks),
        ReportsInput::Wire(wire) => {
            let mut labels: HashMap<String, String> = HashMap::new();
            let mut clash: HashSet<String> = HashSet::new();
            for row in rows(&wire["tasks"]) {
                let id = s(&row["doc"]["stream"]).to_owned();
                let label = row["doc"]["streamLabel"].as_str().unwrap_or(&id).to_owned();
                if labels.get(&id).is_some_and(|v| v != &label) {
                    clash.insert(id.clone());
                }
                labels.insert(id, label);
            }
            for id in clash {
                labels.remove(&id);
            }
            labels
        }
    }
}

fn stream_label(stream: &V, labels: &HashMap<String, String>) -> V {
    let id = s(stream);
    if id.starts_with('(') {
        stream.clone()
    } else {
        labels.get(id).map(|s| json!(s)).unwrap_or(stream.clone())
    }
}

fn spend_compose_with_labels(
    quota: &V,
    labels: &HashMap<String, String>,
    ctx: &FactsCtx<'_>,
) -> Option<V> {
    let good = &quota["good"];
    if good.is_null() {
        return Some(json!({
            "source": "quota-watch",
            "estimate": true,
            "asOf": null,
            "generatedAt": null,
            "writer": null,
            "methodId": null,
            "method": null,
            "periods": [],
            "budgets": [],
            "unattributed": [],
            "machinesNotScanned": [],
            "overLimitBudgets": 0
        }));
    }
    let doc = &good["doc"];
    let mut periods = Vec::new();
    for id in ["day", "week", "window"] {
        let p = &doc["periods"][id];
        if p.is_null() {
            continue;
        }
        let mut streams: Vec<V> = Vec::new();
        for row in p["rows"].as_array()? {
            let at = streams
                .iter()
                .position(|s| s["stream"] == row["stream"])
                .unwrap_or_else(|| {
                    streams.push(json!({
                        "stream": row["stream"],
                        "label": stream_label(&row["stream"], labels),
                        "bucket": s(&row["stream"]).starts_with('('),
                        "rows": []
                    }));
                    streams.len() - 1
                });
            streams[at]["rows"].as_array_mut()?.push(json!({
                "account": row["account"],
                "meter": row["meter"],
                "meterLabel": meter(s(&row["meter"])),
                "unit": row["unit"],
                "value": rounded_value(&row["value"], &row["unit"]),
                "valueText": amount(&row["value"], &row["unit"], s(&row["meter"]))?,
                "estimate": row["estimate"]
            }));
        }
        streams.sort_by(|a, b| {
            (a["bucket"] == true)
                .cmp(&(b["bucket"] == true))
                .then_with(|| ctx.compare(s(&a["label"]), s(&b["label"])))
        });
        let label = match id {
            "day" => "Today".to_owned(),
            "week" => "This week".to_owned(),
            _ => {
                let hours = match (ctx.epoch(&p["start"]), ctx.epoch(&p["end"])) {
                    (Some(a), Some(b)) => ecma::math_round((b - a) as f64 / 3_600_000.0),
                    _ => 0.0,
                };
                if hours != 0.0 {
                    format!(
                        "Last {} hour{}",
                        view::round_word(hours)?,
                        if hours == 1.0 { "" } else { "s" }
                    )
                } else {
                    "Window".into()
                }
            }
        };
        periods.push(json!({
            "id": id,
            "label": label,
            "period": p["period"],
            "start": p["start"],
            "end": p["end"],
            "coverage": p["coverage"],
            "coverageText": coverage(&p["coverage"], id)?,
            "streams": streams
        }));
    }
    let mut budgets = Vec::new();
    for b in doc["budgets"].as_array()? {
        let over = number(&b["percent"]).is_some_and(|v| v >= 100.0)
            || matches!((number(&b["limit"]), number(&b["use"])), (Some(l), Some(u)) if u >= l);
        let per = b
            .get("per")
            .filter(|v| !v.is_null())
            .unwrap_or(&b["period"]);
        let noun = if s(per).starts_with("day") {
            "day"
        } else if s(per).starts_with("week") {
            "week"
        } else {
            "window"
        };
        budgets.push(json!({
            "stream": b["stream"],
            "streamLabel": stream_label(&b["stream"], labels),
            "desc": b["desc"],
            "accounts": b["accounts"],
            "unit": b["unit"],
            "meter": b["meter"],
            "meterLabel": meter(s(&b["meter"])),
            "per": b["per"],
            "limit": b["limit"],
            "use": b["use"],
            "known": b["known"],
            "percent": b["percent"],
            "period": b["period"],
            "coverage": b["coverage"],
            "coverageText": coverage(&b["coverage"], noun)?,
            "overLimit": over,
            "estimate": b["estimate"],
            "text": budget_text(b)?
        }));
    }
    let mut unattributed = Vec::new();
    for u in doc["unattributed"].as_array()? {
        unattributed.push(json!({
            "account": u["account"],
            "meter": u["meter"],
            "meterLabel": meter(s(&u["meter"])),
            "unit": u["unit"],
            "amount": rounded_value(&u["amount"], &u["unit"]),
            "amountText": amount(&u["amount"], &u["unit"], s(&u["meter"]))?,
            "reason": u["reason"],
            "why": u["why"],
            "estimate": u["estimate"]
        }));
    }
    let first = ctx.epoch(&good["firstSeenAt"])?;
    let seen = ctx
        .epoch(&good["writtenAtMs"])
        .map(|t| t.min(first))
        .unwrap_or(first);
    Some(json!({
        "source": "quota-watch",
        "estimate": true,
        "asOf": ctx.iso(Some(seen)),
        "generatedAt": doc["generatedAt"],
        "writer": doc["writer"],
        "methodId": doc["methodId"],
        "method": doc["method"],
        "periods": periods,
        "budgets": budgets,
        "unattributed": unattributed,
        "machinesNotScanned": doc["machinesNotScanned"],
        "overLimitBudgets": budgets.iter().filter(|b| b["overLimit"] == true).count()
    }))
}

fn spend_compose(i: &Inputs) -> Option<V> {
    let labels = stream_labels(i);
    let quota = i.parts.index("quota");
    let out = spend_compose_with_labels(quota, &labels, &i.ctx)?;
    let good = &quota["good"];
    if !good.is_null() {
        // Record the same ISO lookup probe `read_status_spend::compose` records via `i.iso(Some(seen))`.
        let seen = quota_seen(i, good)?;
        i.trace
            .borrow_mut()
            .note_lookup(i.now, LookupKind::Iso, seen);
    }
    Some(out)
}

/// Direct `spendByStream` panel computation for `Produce::publish` with zero `Facts::build`
/// and zero intermediate `Presentation`.
pub(crate) fn spend_by_stream_compact(
    quota: &V,
    tasks: &[Arc<CompactSeenReport>],
    zone: &Zone,
    now_ms: i64,
    utc_offset_minutes: i64,
    account_order: Vec<String>,
) -> Result<V, &'static str> {
    let (clock, day) = zone.words(now_ms).ok_or("zone")?;
    let source = FactsSource::OwnedZone(zone.clone());
    let ctx = FactsCtx::new(&source, now_ms, clock, day, [None, None]).ok_or("date")?;
    let labels = stream_labels_from_compact(tasks);
    let spend = spend_compose_with_labels(quota, &labels, &ctx).ok_or("spend")?;
    if !ctx.valid() {
        return Err("spend");
    }
    let input = aac_status::spend::SpendInput {
        now_ms,
        utc_offset_minutes,
        account_order,
        spend: serde_json::from_value(spend).map_err(|_| "spend")?,
    };
    serde_json::to_value(aac_status::spend::compose(&input)).map_err(|_| "spend")
}

// ---------------------------------------------------------------------------------------------
// Headline strip (mirrors `read_status_strip.rs`)

fn listed(names: &[String]) -> String {
    let mut out = names.iter().take(4).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > 4 {
        out.push_str(&format!(" and {} more", names.len() - 4));
    }
    out
}

fn one(n: usize, singular: &str, many: &str) -> String {
    if n == 1 { singular.into() } else { many.into() }
}

fn strip_owner(update: &V) -> String {
    let b = &update["board"];
    if b["shared"] == true {
        format!(
            "the {} board kept by {}",
            b["streamLabel"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(s(&b["stream"])),
            s(&b["writer"])
        )
    } else {
        s(&update["title"]).into()
    }
}

fn strip_compose(view: &V) -> Option<V> {
    let counts = &view["headline"]["counts"];
    let n = counts["questions"].as_u64()? as usize;
    let decisions = counts["decisions"].as_u64()? as usize;
    let sources = view["sources"].as_array()?;
    let reports = sources.iter().find(|s| s["id"] == "agent-reports")?;
    let unread = ["missing", "error"].contains(&s(&reports["state"]));
    let mut items = vec![if n == 0 && unread {
        json!({
            "key": "questions",
            "section": "questions",
            "value": "–",
            "label": "questions for you",
            "short": "questions",
            "tone": "quiet",
            "detail": format!("Questions for you are unknown: {}", sentence(s(&reports["detail"])))
        })
    } else {
        json!({
            "key": "questions",
            "section": "questions",
            "value": n.to_string(),
            "label": one(n, "question for you", "questions for you"),
            "short": one(n, "question", "questions"),
            "tone": if n > 0 { "warn" } else { "quiet" },
            "detail": if n > 0 {
                format!("{n} {} for you.", one(n, "question", "questions"))
            } else {
                "No questions for you.".into()
            }
        })
    }];
    if decisions > 0 {
        items.push(json!({
            "key": "decisions",
            "value": decisions.to_string(),
            "label": one(decisions, "decision to look at", "decisions to look at"),
            "short": one(decisions, "decision", "decisions"),
            "tone": "warn",
            "section": "questions",
            "detail": format!("{decisions} {} to look at.", one(decisions, "decision", "decisions"))
        }));
    }
    let mut loops: Vec<V> = Vec::new();
    let mut loop_keys = Vec::new();
    let mut waiting = Vec::new();
    let pinned = &view["pinned"];
    if let Some(updates) = pinned["updates"].as_array() {
        for (index, u) in updates.iter().enumerate() {
            if u["blocked"]["waitingOnYou"] == true {
                waiting.push(u.clone());
            }
            if u["looping"]["answer"] != "yes" {
                continue;
            }
            let key = if u["board"].is_object() && !s(&u["looping"]["task"]).is_empty() {
                format!(
                    "board\n{}\n{}",
                    s(&u["board"]["writer"]),
                    s(&u["looping"]["task"])
                )
            } else {
                format!("pin\n{index}")
            };
            if let Some(at) = loop_keys.iter().position(|k| k == &key) {
                if loops[at]["board"]["shared"] == true && u["board"]["shared"] != true {
                    loops[at] = u.clone();
                }
            } else {
                loop_keys.push(key);
                loops.push(u.clone());
            }
        }
    } else {
        let why = if pinned.is_null() {
            "this server doesn't send them yet"
        } else {
            s(&pinned["unavailable"])
        };
        items.push(json!({
            "key": "pinned",
            "value": "–",
            "label": "pinned threads",
            "short": "pinned",
            "tone": "quiet",
            "section": "pinned",
            "detail": format!("Pinned threads are unavailable: {}", sentence(why))
        }));
    }
    if !loops.is_empty() {
        let n = loops.len();
        let names = loops
            .iter()
            .map(|u| {
                if !s(&u["looping"]["task"]).is_empty() {
                    format!("{} (task {})", strip_owner(u), s(&u["looping"]["task"]))
                } else {
                    strip_owner(u)
                }
            })
            .collect::<Vec<_>>();
        items.push(json!({
            "key": "looping",
            "value": n.to_string(),
            "label": one(n, "thread looping", "threads looping"),
            "short": "looping",
            "tone": "bad",
            "section": "pinned",
            "detail": format!("{n} pinned {} looping: {}.", one(n, "thread is", "threads are"), listed(&names))
        }));
    }
    if !waiting.is_empty() {
        let n = waiting.len();
        items.push(json!({
            "key": "waiting",
            "value": n.to_string(),
            "label": "waiting on you",
            "short": one(n, "waits on you", "wait on you"),
            "tone": "warn",
            "section": "pinned",
            "detail": format!(
                "{n} pinned {} on you: {}.",
                one(n, "thread waits", "threads wait"),
                listed(&waiting.iter().map(|u| s(&u["title"]).to_owned()).collect::<Vec<_>>())
            )
        }));
    }
    let blocked = counts["blockedItems"].as_u64()? as usize;
    if blocked > 0 {
        let mut names = Vec::new();
        for p in view["projects"].as_array()? {
            for st in p["streams"].as_array()? {
                for item in st["items"].as_array()? {
                    if item["state"] == "blocked" {
                        names.push(
                            format!("{} {}", s(&item["id"]), s(&item["title"]))
                                .trim_matches(js_whitespace)
                                .to_owned(),
                        );
                    }
                }
            }
        }
        items.push(json!({
            "key": "blocked",
            "value": blocked.to_string(),
            "label": "blocked",
            "short": "blocked",
            "tone": "warn",
            "section": null,
            "detail": if !names.is_empty() {
                format!("Blocked: {}.", listed(&names))
            } else {
                format!("{blocked} {} blocked.", one(blocked, "item is", "items are"))
            }
        }));
    }
    let over = if sources
        .iter()
        .any(|s| s["id"] == "quota-watch" && s["state"] == "ok")
    {
        view["spend"]["overLimitBudgets"].as_u64()? as usize
    } else {
        0
    };
    if over > 0 {
        items.push(json!({
            "key": "budgets",
            "value": over.to_string(),
            "label": "over budget (estimate)",
            "short": "over budget",
            "tone": "bad",
            "section": "spend",
            "detail": format!(
                "{over} {} at or over {} limit (estimate).",
                one(over, "budget is", "budgets are"),
                one(over, "its", "their")
            )
        }));
    }
    let mut total = empty_counts();
    for p in view["projects"].as_array()? {
        add_counts(&mut total, &p["counts"])?;
    }
    let done = completion(&total)?;
    if done.is_object() {
        let n = done["done"].as_u64()?;
        let of = done["of"].as_u64()?;
        items.push(json!({
            "key": "done",
            "value": format!("{n}/{of}"),
            "label": "items done",
            "short": "done",
            "tone": null,
            "section": null,
            "detail": format!("{n} of {of} items landed or done (dropped items are not counted).")
        }));
    }
    let behind: Vec<_> = sources
        .iter()
        .filter(|s| s["state"] != "ok" && s["state"] != "unconfigured")
        .collect();
    if !behind.is_empty() {
        let n = behind.len();
        let parts = behind
            .iter()
            .map(|src| {
                format!(
                    "{}: {}",
                    s(&src["label"]),
                    trim_period(s(&src["detail"]).trim_matches(js_whitespace))
                )
            })
            .collect::<Vec<_>>();
        items.push(json!({
            "key": "sources",
            "value": n.to_string(),
            "label": one(n, "source not current", "sources not current"),
            "short": "not current",
            "tone": "warn",
            "section": null,
            "detail": format!("Not current: {}.", parts.join("; "))
        }));
    }
    let mut worst: Option<&V> = None;
    for u in &loops {
        if worst.is_none_or(|w| {
            number(&u["looping"]["rounds"]).unwrap_or(0.0)
                > number(&w["looping"]["rounds"]).unwrap_or(0.0)
        }) {
            worst = Some(u);
        }
    }
    let worst = if let Some(u) = worst {
        json!({
            "state": "looping",
            "word": "Looping",
            "tone": "bad",
            "threadId": u["id"],
            "title": strip_owner(u),
            "task": u["looping"]["task"],
            "detail": sentence(u["looping"]["text"].as_str().filter(|s| !s.is_empty()).unwrap_or("Looping."))
        })
    } else if let Some(u) = waiting.first() {
        json!({
            "state": "waiting",
            "word": "Waiting on you",
            "tone": "warn",
            "threadId": u["id"],
            "title": u["title"],
            "task": null,
            "detail": sentence(u["blocked"]["text"].as_str().filter(|s| !s.is_empty()).unwrap_or("Waiting on you."))
        })
    } else {
        V::Null
    };
    Some(json!({"items": items, "worst": worst}))
}

// ---------------------------------------------------------------------------------------------
// Top-level compact Status composition entry point

fn compose_view(i: &Inputs) -> Option<V> {
    if let ReportsInput::Wire(wire) = i.reports {
        for key in ["threads", "tasks", "cards", "problems"] {
            wire[key].as_array()?;
        }
    }
    let registry = registry_source(i)?;
    let browsers = collected_source_wire(
        i,
        "browsers",
        "Browsers and recordings",
        i.parts.index("browsers"),
        180,
        false,
    )?;
    let machine_rows = machines(i, &browsers)?;
    let reports = reports_source(i)?;
    let quota = spend_source(i)?;
    let usage_src = usage_source(i)?;
    let sources = vec![
        reports,
        registry.clone(),
        machines_source(i, &machine_rows)?,
        browsers,
        collected_source_wire(
            i,
            "simlock",
            "Simlock",
            i.parts.index("simlock"),
            180,
            false,
        )?,
        usage_src.clone(),
        quota,
    ];
    let mut view = json!({
        "version": 1,
        "generatedAt": i.generated_at(),
        "refresh": {"pollSeconds": 30},
        "headline": {"strip": null},
        "sources": sources,
        "cards": cards_compose(i)?
    });
    match i.reports {
        ReportsInput::Compact(owned) => {
            if !owned.state.problems.is_empty() {
                view["reportProblems"] =
                    V::Array(owned.state.problems.iter().map(|p| p.to_json()).collect());
            }
        }
        ReportsInput::Wire(wire) => {
            if let Some(problems) = wire.get("problems").filter(|p| !array(p).is_empty()) {
                view["reportProblems"] = problems.clone();
            }
        }
    }
    if let Some(p) = i.parts.get("pinned") {
        view["pinned"] = p.clone();
    }
    if let Some(s) = i.parts.get("spendByStream") {
        view["spendByStream"] = s.clone();
    }
    view["projects"] = json!(projects(i)?);
    view["threads"] = json!({
        "source": "agent-reports",
        "also": ["pm-mail"],
        "rows": threads(i, &registry)?
    });
    view["machines"] = json!({
        "source": "machines",
        "also": ["browsers"],
        "rows": machine_rows
    });
    view["simlock"] = simlock_view(&i.parts.index("simlock")["data"]);
    view["usage"] = usage(i, &usage_src)?;
    view["spend"] = spend_compose(i)?;
    let mut view = aac_status::http_views::status_owned(view)?;
    view["headline"]["strip"] = strip_compose(&view)?;
    Some(view)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compose_compact(
    parts: Vec<(&str, &V)>,
    reports: ReportsInput<'_>,
    registry: RegistryInput<'_>,
    facts: &FactsSource,
    now_ms: i64,
    clock: String,
    day: String,
    scan_dates: [Option<(i64, DateTriple)>; 2],
) -> Result<(V, CompactFedTrace), Unproven> {
    let scan_stamps = scan_dates.each_ref().map(|d| d.as_ref().map(|(at, _)| *at));
    let ctx = FactsCtx::new(facts, now_ms, clock, day, scan_dates).ok_or(Unproven::Facts)?;
    let i = Inputs {
        reports,
        registry,
        scan_stamps,
        parts: Parts { entries: parts },
        ctx,
        now: now_ms,
        owner_cards: OnceCell::new(),
        trace: RefCell::new(TraceRecorder::recording()),
    };
    let view = compose_view(&i);
    let valid = i.ctx.valid();
    let traced = i.trace.into_inner().finish();
    match view {
        Some(view) if valid => Ok((view, traced)),
        _ if !valid => Err(Unproven::Facts),
        _ => Err(Unproven::Input),
    }
}
