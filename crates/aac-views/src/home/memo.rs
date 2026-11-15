//! R1b: the clock-independent results of Home's view builder, kept for one held dashboard generation and one page
//! zone and style, so a request computes only what depends on `now`.
//!
//! **What is kept.** The results of pure helper calls whose inputs are an account or window of the held dashboard
//! (its visible windows, a window's labels, exact reset and sampled times, amounts, the source line), a number (its
//! en-US text) or an instant (its display-zone words). They read nothing per request: not `now`, not the profiles or
//! the Antigravity copy, not the page's user or host. Each call site asks the memo with the same arguments it would
//! pass the helper, at the same point and under the same conditions, so the builder evaluates the same calls in the
//! same order and every result, decline included, is the one the helper gives. A memoized helper touches no other
//! state (`Fmt`'s missing-fact flag is set only by collation, which is never memoized).
//!
//! **Why it cannot serve another generation's text.** The memo belongs to one prepared generation
//! (`fed::Dashboard`), which is immutable and dropped with it. An account or window is named by the exact address of
//! its object inside that generation, from a table built once when the memo is made; an object that is not in the
//! table (from any other source) is never memoized. Zone facts and style are part of the memo's identity, compared
//! in full. Numbers are named by their bits, instants by their value.
//!
//! **Bounded.** At most [`CAP`] entries per memo (the rest are computed each time), and a generation keeps at most
//! [`SLOTS`] memos (one per page zone and style; the oldest goes).
use super::windows::WinSpec;
use crate::js::{R, Style, Zone};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};

/// Entries one memo holds at most.
pub const CAP: usize = 1 << 16;
/// Memos (page zone and style) one generation keeps.
pub const SLOTS: usize = 4;

/// A window's memoized part.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Part {
    /// `windowLabel(provider, w)`.
    Label,
    /// `fullWindowLabel(provider, w)`.
    FullLabel,
    /// `exact(w.resetAt, 'Resets ')`.
    ResetExact,
    /// `meterView`'s amount (`compact(used) of compactAmount(limit) unit`).
    Amount,
    /// `meterView`'s `left` amount.
    Left,
    /// `meterView`'s source line.
    Source,
}

/// An account's memoized part.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum AccountPart {
    /// `planLabel(withoutLabel(plan, label), provider)` (a provider card's plan).
    Plan,
    /// `hostLabel(account)`.
    Platform,
    /// A provider card's plan note (`cursorPlanNote` and the plan subscription's end).
    PlanNote,
}

/// What a memo entry is named by.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Key {
    /// The dashboard's shape check (`validate`).
    Valid,
    /// An account's part, by the account's index.
    Account(AccountPart, u32),
    /// An account's visible windows (`visibleWindows`), by the account's index in the generation.
    Visible(u32),
    /// A window's part, by (account, window) index and the window's override (Qwen's plan expiry: the window whose
    /// `expiresAt` it shows, or `u32::MAX`, and the plan-expiry flag), since the same object can be seen either way.
    Window(Part, u32, u32, u32, bool),
    /// `nf2(x)`, by the bits of `x`.
    Nf2(u64),
    /// `fmt.dateTime(t)`.
    DateTime(i64),
}

#[derive(Clone, Debug)]
enum Entry {
    Text(R<String>),
    Windows(R<Vec<WinSpec>>),
}

/// The memo of one generation and one page zone and style.
#[derive(Debug)]
pub struct Memo {
    zone: Zone,
    style: Style,
    /// The address of the generation's dashboard object.
    dashboard: usize,
    /// Object address -> (account index, window index or `u32::MAX` for the account itself).
    objects: HashMap<usize, (u32, u32)>,
    map: RwLock<HashMap<Key, Entry>>,
}

fn address(map: &Map<String, Value>) -> usize {
    std::ptr::from_ref(map) as usize
}

/// The account itself in [`Memo`]'s object table.
const ACCOUNT: u32 = u32::MAX;

impl Memo {
    fn new(dashboard: &Value, zone: Zone, style: Style) -> Self {
        let mut objects = HashMap::new();
        if let Some(Value::Array(accounts)) = dashboard.get("accounts") {
            for (a, account) in accounts.iter().enumerate() {
                let (Value::Object(m), Ok(a)) = (account, u32::try_from(a)) else {
                    continue;
                };
                objects.insert(address(m), (a, ACCOUNT));
                if let Some(Value::Array(windows)) = m.get("windows") {
                    for (w, window) in windows.iter().enumerate() {
                        if let (Value::Object(wm), Ok(w)) = (window, u32::try_from(w)) {
                            objects.insert(address(wm), (a, w));
                        }
                    }
                }
            }
        }
        Memo {
            zone,
            style,
            dashboard: dashboard.as_object().map_or(0, address),
            objects,
            map: RwLock::new(HashMap::new()),
        }
    }

    /// Whether `data` is this memo's own dashboard object.
    pub(crate) fn is_for(&self, data: &Value) -> bool {
        data.as_object()
            .is_some_and(|m| self.dashboard != 0 && address(m) == self.dashboard)
    }

    /// The index of an account of this generation; `None` for any other object.
    pub(crate) fn account(&self, account: &Map<String, Value>) -> Option<u32> {
        match self.objects.get(&address(account)) {
            Some((a, ACCOUNT)) => Some(*a),
            _ => None,
        }
    }

    /// The (account, window) index of a window of this generation; `None` for any other object.
    pub(crate) fn window(&self, window: &Map<String, Value>) -> Option<(u32, u32)> {
        match self.objects.get(&address(window)) {
            Some((_, ACCOUNT)) | None => None,
            Some(found) => Some(*found),
        }
    }

    fn get(&self, key: Key) -> Option<Entry> {
        self.map.read().ok()?.get(&key).cloned()
    }

    fn put(&self, key: Key, entry: &Entry) {
        if let Ok(mut map) = self.map.write()
            && map.len() < CAP
        {
            map.entry(key).or_insert_with(|| entry.clone());
        }
    }

    /// A memoized text: the kept result, or `compute`'s (kept for the next request).
    pub(crate) fn text(&self, key: Key, compute: impl FnOnce() -> R<String>) -> R<String> {
        if let Some(Entry::Text(kept)) = self.get(key) {
            return kept;
        }
        let result = compute();
        self.put(key, &Entry::Text(result.clone()));
        result
    }

    /// A memoized window list.
    pub(crate) fn windows(
        &self,
        key: Key,
        compute: impl FnOnce() -> R<Vec<WinSpec>>,
    ) -> R<Vec<WinSpec>> {
        if let Some(Entry::Windows(kept)) = self.get(key) {
            return kept;
        }
        let result = compute();
        self.put(key, &Entry::Windows(result.clone()));
        result
    }

    /// Entries kept (tests).
    pub fn len(&self) -> usize {
        self.map.read().map_or(0, |map| map.len())
    }

    /// No entry kept yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The memos of one held generation (one per page zone and style, at most [`SLOTS`]).
#[derive(Debug, Default)]
pub struct Memos(Mutex<Vec<Arc<Memo>>>);

impl Memos {
    /// The memo for this page zone and style over `dashboard` (the generation's own value, which this store is kept
    /// with), made on first use.
    pub fn get(&self, dashboard: &Value, zone: &Zone, style: Style) -> Arc<Memo> {
        let Ok(mut slots) = self.0.lock() else {
            return Arc::new(Memo::new(dashboard, zone.clone(), style));
        };
        if let Some(at) = slots
            .iter()
            .position(|m| m.style == style && m.zone == *zone)
        {
            let memo = slots.remove(at);
            slots.push(memo.clone());
            return memo;
        }
        let memo = Arc::new(Memo::new(dashboard, zone.clone(), style));
        if slots.len() >= SLOTS {
            slots.remove(0);
        }
        slots.push(memo.clone());
        memo
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js::Decline;
    use serde_json::json;
    use std::cell::Cell;

    fn zone(offset: i64) -> Zone {
        Zone::proven(0, 10_000_000_000_000, &[[0, offset]]).expect("a proven zone")
    }

    #[test]
    fn only_the_held_generations_own_objects_are_named() {
        let dashboard = json!({"accounts": [{"id": "a", "windows": [{"key": "w"}, 3]}, "x"]});
        let memo = Memo::new(&dashboard, zone(0), crate::js::STYLES[0]);
        let account = dashboard["accounts"][0].as_object().expect("an account");
        let window = dashboard["accounts"][0]["windows"][0]
            .as_object()
            .expect("a window");
        assert_eq!(memo.account(account), Some(0));
        assert_eq!(memo.window(window), Some((0, 0)));
        assert_eq!(memo.account(window), None);
        assert_eq!(memo.window(account), None);
        // An equal object elsewhere (another generation, a request's own data) is not this one.
        let copy = dashboard.clone();
        assert_eq!(
            memo.account(copy["accounts"][0].as_object().expect("a copy")),
            None
        );
        assert!(memo.is_for(&dashboard) && !memo.is_for(&copy));
    }

    #[test]
    fn a_kept_result_is_computed_once_declines_included() {
        let memo = Memo::new(&json!({"accounts": []}), zone(0), crate::js::STYLES[0]);
        let calls = Cell::new(0);
        for _ in 0..3 {
            let got = memo.text(Key::Nf2(1), || {
                calls.set(calls.get() + 1);
                Err(Decline("number"))
            });
            assert_eq!(got, Err(Decline("number")));
        }
        assert_eq!((calls.get(), memo.len()), (1, 1));
        assert_eq!(
            memo.text(Key::Nf2(2), || Ok("2".into())),
            Ok("2".to_string())
        );
        assert_eq!(
            memo.text(Key::Nf2(2), || Ok("other".into())),
            Ok("2".into())
        );
    }

    #[test]
    fn a_memo_holds_at_most_cap_entries_and_a_generation_at_most_slots_memos() {
        let memo = Memo::new(&json!({"accounts": []}), zone(0), crate::js::STYLES[0]);
        for i in 0..CAP as i64 + 10 {
            let _ = memo.text(Key::DateTime(i), || Ok(String::new()));
        }
        assert_eq!(memo.len(), CAP);
        // Past the cap a result is still right, only not kept.
        assert_eq!(
            memo.text(Key::DateTime(-1), || Ok("x".into())),
            Ok("x".into())
        );
        assert_eq!(
            memo.text(Key::DateTime(-1), || Ok("y".into())),
            Ok("y".into())
        );
        let dashboard = json!({"accounts": []});
        let memos = Memos::default();
        let first = memos.get(&dashboard, &zone(0), crate::js::STYLES[0]);
        assert!(Arc::ptr_eq(
            &first,
            &memos.get(&dashboard, &zone(0), crate::js::STYLES[0])
        ));
        // Another zone or style is another memo; the oldest goes past SLOTS.
        assert!(!Arc::ptr_eq(
            &first,
            &memos.get(&dashboard, &zone(60), crate::js::STYLES[0])
        ));
        assert!(!Arc::ptr_eq(
            &first,
            &memos.get(&dashboard, &zone(0), crate::js::STYLES[2])
        ));
        for offset in 1..=SLOTS as i64 {
            let _ = memos.get(&dashboard, &zone(offset * 30), crate::js::STYLES[0]);
        }
        assert!(memos.0.lock().map(|slots| slots.len()).unwrap_or(0) <= SLOTS);
        assert!(!Arc::ptr_eq(
            &first,
            &memos.get(&dashboard, &zone(0), crate::js::STYLES[0])
        ));
    }
}
