//! Redesign 12. The round 9-11 reviewer witnesses not already committed elsewhere, and one exhaustive ordering check.
//! Every family replays recorded production responses (real router, native addon, Store and source reader with an
//! offline provider fixture) in every delivery order, with a hint and a send-deadline tick, through the production
//! `ResetCtx` paths. After every step it checks the four `reset_state` invariants against an independent model, and
//! checks a second page, a reload, a sibling alias and navigation at the same point.
use super::*;
use reset_state::{
    ACCOUNT_CHANGED_UNAVAILABLE, NEEDS_READ, OWN_SEND_WINDOW, PREPARING, STILL_UNCONFIRMED,
    TOKENS_OUTSTANDING, UNANSWERED_SEND, USAGE_UNAVAILABLE, unanswered_answer,
};

const ROUTE: &str = "codex:fixture";
const SIBLING: &str = "codex:fixture-sibling";
const OTHER: &str = "codex:other";
const SENT_AT: f64 = 102.;
const DEADLINE: f64 = SENT_AT + CONFIRM_LIFE_MS;

fn trace(prior: bool) -> serde_json::Value {
    serde_json::from_str(if prior {
        include_str!("../tests/fixtures/reset-refusal-after-success.json")
    } else {
        include_str!("../tests/fixtures/reset-refusal-first-action.json")
    })
    .unwrap()
}
fn aba() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../tests/fixtures/reset-source-failure-aba.json"
    ))
    .unwrap()
}
/// `name` may be a dotted path, such as `traceExtra.back`.
fn value<T: for<'de> Deserialize<'de>>(t: &serde_json::Value, name: &str) -> T {
    let found = name.split('.').fold(t, |v, part| &v[part]);
    assert!(!found.is_null(), "fixture has no {name}");
    serde_json::from_value(found.clone()).unwrap()
}
/// The same recorded inventory, as the route returns it when the quota read fails: evidence, no usable quota.
fn failed(t: &serde_json::Value, name: &str) -> Inventory {
    let mut inv: Inventory = value(t, name);
    inv.status = "unavailable".into();
    inv.reason = "reset-inventory-unavailable".into();
    inv
}
fn said(text: &str, bad: bool, check: bool) -> Said {
    Said {
        text: text.into(),
        bad,
        check,
    }
}
fn text(s: &Option<Said>) -> &str {
    s.as_ref().map_or("", |s| s.text.as_str())
}

// ---- RECHECK-11 M1 (own-success.rs), committed unchanged apart from formatting ------------------------------------

mod recheck11_own_success {
    use super::*;
    fn run(prior: bool) {
        let t = trace(prior);
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            let route = "codex:fixture";
            rc.show(route, "codex", false);
            rc.take(
                route,
                serde_json::from_value(t["before"].clone()).unwrap(),
                100.,
            );
            let canonical = rc.lock_key(route);
            let send = rc.begin_send(&canonical, 102.);
            // A hint/focus read observes this page's completed send before its POST callback arrives.
            let latest: Inventory = serde_json::from_value(t["afterRetry"].clone()).unwrap();
            let result: ClaimResult = serde_json::from_value(t["retry"].clone()).unwrap();
            assert_eq!(
                latest
                    .evidence
                    .as_ref()
                    .unwrap()
                    .latest_attempt
                    .as_ref()
                    .unwrap()
                    .attempt_key,
                result.attempt_key
            );
            println!(
                "callback revision={} read revision={}",
                result.evidence.as_ref().unwrap().revision,
                latest.evidence.as_ref().unwrap().revision
            );
            rc.take(route, latest.clone(), 120103.);
            rc.claim_answer(&canonical, send, Answer::Ok(result), 120104.);
            rc.take(route, latest, 120105.);
            let said = rc.said.get_untracked();
            println!(
                "own current result={said:?}, blocked={}",
                rc.unresolved.get_untracked().blocks()
            );
            assert!(!rc.unresolved.get_untracked().blocks());
            assert!(
                said.as_ref()
                    .is_some_and(|s| s.text.starts_with("Reset applied")),
                "own successful claim must remain acknowledged after an inventory/callback reorder"
            );
        });
    }
    #[test]
    fn inventory_ahead_of_own_first_callback_must_retain_own_fresh_success() {
        run(false)
    }
    #[test]
    fn inventory_ahead_of_own_retry_callback_must_retain_own_fresh_success() {
        run(true)
    }
}

// ---- the same reorder inside the send's proof window: provenance answers, U1 still holds -------------------------

#[test]
fn inventory_ahead_of_own_callback_inside_the_send_window_says_the_own_result_and_keeps_u1() {
    for prior in [false, true] {
        let t = trace(prior);
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            rc.show(ROUTE, "codex", false);
            rc.take(ROUTE, value(&t, "before"), 100.);
            let canonical = rc.lock_key(ROUTE);
            let send = rc.begin_send(&canonical, SENT_AT);
            rc.take(ROUTE, value(&t, "afterRetry"), 110.);
            assert_eq!(text(&rc.said.get_untracked()), UNANSWERED_SEND);
            rc.claim_answer(&canonical, send, Answer::Ok(value(&t, "retry")), 111.);
            let shown = rc.said.get_untracked().unwrap();
            assert_eq!(
                shown.text,
                format!("Reset applied and confirmed. {OWN_SEND_WINDOW}")
            );
            assert!(shown.check && !shown.bad);
            assert!(
                rc.unresolved.get_untracked().blocks(),
                "only an admitted receipt or a read after the deadline lifts U1"
            );
            rc.take(ROUTE, value(&t, "afterRetry"), DEADLINE);
            assert_eq!(
                rc.said.get_untracked(),
                Some(said("Reset applied and confirmed.", false, false))
            );
            assert!(!rc.unresolved.get_untracked().blocks());
        });
    }
}

#[test]
fn a_stale_no_send_refusal_inside_the_send_window_says_nothing_was_sent_and_keeps_u1() {
    for prior in [false, true] {
        let t = trace(prior);
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            rc.show(ROUTE, "codex", false);
            rc.take(ROUTE, value(&t, "before"), 100.);
            let canonical = rc.lock_key(ROUTE);
            let send = rc.begin_send(&canonical, SENT_AT);
            rc.take(ROUTE, value(&t, "recovered"), 110.);
            let refusal: Refusal = value(&t, "refused");
            rc.claim_answer(&canonical, send, Answer::Refused(refusal.clone()), 111.);
            let refused = refusal_said(&refusal, "codex");
            assert_eq!(
                rc.said.get_untracked(),
                Some(said(
                    &format!("{} {OWN_SEND_WINDOW}", refused.text),
                    true,
                    true
                ))
            );
            assert!(rc.unresolved.get_untracked().blocks());
            rc.take(ROUTE, value(&t, "recovered"), DEADLINE);
            assert_eq!(rc.said.get_untracked(), Some(refused));
            assert!(!rc.unresolved.get_untracked().blocks());
        });
    }
}

// ---- RECHECK-9 ABA control (witness-ui.py `recheck9_aba`) over its own recorded trace ------------------------------

#[test]
fn failed_reads_follow_reverted_identity_without_old_confirmation() {
    Owner::new().with(|| {
        let t = aba();
        let rc = ResetCtx::new();
        rc.show("codex:reused", "codex", false);
        for value in [
            &t["prior"],
            &t["unavailable"],
            &t["traceExtra"]["back"],
            &t["traceExtra"]["again"],
        ] {
            let inv: Inventory = serde_json::from_value(value.clone()).unwrap();
            let unavailable = inv.status != "ok";
            rc.take("codex:reused", inv, 3.);
            if unavailable {
                let said = rc.said.get_untracked().unwrap();
                println!("ABA {:?}", said.text);
                assert!(!said.text.contains("Reset applied"));
                assert!(said.text.contains("unavailable"));
                assert!(rc.unresolved.get_untracked().blocks());
            }
        }
    });
}

// ---- the exhaustive ordering check -------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ev {
    /// This page's claim callback: the recorded successful `retry` answer.
    Callback,
    /// This page's claim callback: a recorded refusal body.
    Refusal(&'static str),
    /// A GET on this page and on the second page.
    Read(&'static str),
    /// The same GET when the quota read fails.
    Unavailable(&'static str),
    /// A read-only check of the recorded attempt, with no local send.
    Check,
    /// A hint or focus invalidation on both pages.
    Hint,
    /// The clock passes the send's proof deadline: later reads start after it.
    Tick,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Own {
    Nothing,
    Prepare,
    Send,
}

#[derive(Clone, Debug, PartialEq)]
enum OwnAnswer {
    Nothing,
    Preparing,
    Unanswered,
    Completed(String),
    Refused(Refusal),
}

fn rev(e: &Evidence) -> u64 {
    e.revision.parse().unwrap()
}
fn progress(r: Option<&ClaimResult>) -> (u64, bool) {
    r.map_or((0, false), |r| {
        (r.attempt_order.parse().unwrap(), !r.pending)
    })
}
fn mark(r: Option<&ClaimResult>) -> Option<(String, String, String)> {
    r.map(|r| {
        (
            r.attempt_order.clone(),
            r.attempt_key.clone(),
            r.outcome.clone(),
        )
    })
}

/// The invariants restated over delivered events, without reading `AccountState`.
#[derive(Clone, Debug)]
struct Model {
    own: Own,
    /// 1. The highest-revision evidence delivered so far (the fixture check makes equal revisions identical).
    admitted: Evidence,
    /// The newest inventory delivered on either page: what a reload may read.
    newest: Inventory,
    /// 2. Fixed when the action began.
    frontier: Option<(String, String, String)>,
    frontier_progress: (u64, bool),
    answer: OwnAnswer,
    callback: Option<String>,
    /// 3. The send is open until its admitted receipt, its admitted no-send refusal or an admitted read after the deadline.
    open: bool,
    refresh: bool,
    failure: Option<bool>,
    late: bool,
}
impl Model {
    fn new(before: &Inventory, own: Own) -> Self {
        let admitted = *before.evidence.clone().unwrap();
        Self {
            own,
            frontier: mark(admitted.latest_attempt.as_ref()),
            frontier_progress: progress(admitted.latest_attempt.as_ref()),
            admitted,
            newest: before.clone(),
            answer: match own {
                Own::Nothing => OwnAnswer::Nothing,
                Own::Prepare => OwnAnswer::Preparing,
                Own::Send => OwnAnswer::Unanswered,
            },
            callback: None,
            open: own == Own::Send,
            refresh: before.status != "ok",
            failure: (before.status != "ok").then_some(false),
            late: false,
        }
    }
    fn admit(&mut self, e: &Evidence) -> bool {
        let ok =
            rev(e) > rev(&self.admitted) || (rev(e) == rev(&self.admitted) && *e == self.admitted);
        if ok {
            self.admitted = e.clone();
        }
        ok
    }
    fn completed(&self) -> bool {
        matches!(self.answer, OwnAnswer::Completed(_))
    }
    fn deliver(&mut self, t: &serde_json::Value, ev: Ev) {
        match ev {
            Ev::Callback => {
                let r: ClaimResult = value(t, "retry");
                if self.admit(r.evidence.as_ref().unwrap()) {
                    self.open = false;
                }
                if self.own == Own::Send
                    && !self.completed()
                    && progress(Some(&r)).0 > self.frontier_progress.0
                {
                    self.answer = OwnAnswer::Completed(r.attempt_key.clone());
                }
                self.callback = Some(r.attempt_key);
            }
            Ev::Refusal(body) => {
                let mut r: Refusal = value(t, body);
                let e = *r.evidence.take().unwrap();
                if self.admit(&e) && r.sent == Some(false) {
                    self.open = false;
                }
                if self.own == Own::Send
                    && !self.completed()
                    && progress(e.latest_attempt.as_ref()) >= self.frontier_progress
                {
                    self.answer = OwnAnswer::Refused(r);
                }
            }
            Ev::Read(name) | Ev::Unavailable(name) => {
                let inv = if matches!(ev, Ev::Read(_)) {
                    value(t, name)
                } else {
                    failed(t, name)
                };
                if rev(inv.evidence.as_ref().unwrap())
                    >= rev(self.newest.evidence.as_ref().unwrap())
                {
                    self.newest = inv.clone();
                }
                if self.admit(inv.evidence.as_ref().unwrap()) {
                    if inv.status == "ok" {
                        self.refresh = false;
                        self.failure = None;
                        if self.late {
                            self.open = false;
                        }
                    } else {
                        self.refresh = true;
                        self.failure = Some(false);
                    }
                }
            }
            Ev::Check => {
                let r: ClaimResult = value(t, "retry");
                self.admit(r.evidence.as_ref().unwrap());
            }
            Ev::Hint => self.refresh = true,
            Ev::Tick => self.late = true,
        }
    }
    fn pending(&self) -> Option<&ClaimResult> {
        self.admitted.latest_attempt.as_ref().filter(|r| r.pending)
    }
    fn tokens(&self) -> bool {
        self.admitted
            .claim_tokens
            .as_ref()
            .unwrap()
            .until_ms
            .is_some()
    }
    fn blocked(&self) -> bool {
        self.refresh || self.open || self.pending().is_some() || self.tokens()
    }
    /// 4. The current text, from the three facts above.
    fn expected(&self) -> Option<Said> {
        let latest = self.admitted.latest_attempt.as_ref();
        // (said, definitive, said before U1)
        let own = match &self.answer {
            OwnAnswer::Nothing => None,
            OwnAnswer::Completed(key) => latest
                .filter(|r| &r.attempt_key == key)
                .map(|r| (result_said(r, "codex"), true, true)),
            _ if mark(latest) != self.frontier => None,
            OwnAnswer::Preparing => Some((said(PREPARING, false, false), false, true)),
            OwnAnswer::Unanswered => {
                Some((said(&unanswered_answer(None), true, true), false, false))
            }
            OwnAnswer::Refused(r) => {
                Some((refusal_said(r, "codex"), r.sent == Some(false), !self.open))
            }
        };
        if self.open {
            return Some(match own {
                Some((s, true, _)) => said(&format!("{} {OWN_SEND_WINDOW}", s.text), s.bad, true),
                Some((s, false, true)) => {
                    said(&format!("{} {UNANSWERED_SEND}", s.text), true, true)
                }
                _ => said(UNANSWERED_SEND, true, true),
            });
        }
        if let Some((mut s, _, _)) = own {
            s.check |= self.refresh || self.pending().is_some() || self.tokens();
            return Some(s);
        }
        if let Some(changed) = self.failure {
            let text = if changed {
                ACCOUNT_CHANGED_UNAVAILABLE
            } else {
                USAGE_UNAVAILABLE
            };
            return Some(said(text, true, true));
        }
        if self.refresh {
            return Some(said(NEEDS_READ, true, true));
        }
        if self.tokens() {
            return Some(said(TOKENS_OUTSTANDING, true, true));
        }
        self.pending().map(|_| said(STILL_UNCONFIRMED, true, true))
    }
}

/// Spec-level checks that do not depend on the model's projection.
fn universal(m: &Model, shown: &Option<Said>) {
    let text = text(shown);
    assert!(!text.contains('%') && !text.contains("back to"), "{text}");
    let latest = m.admitted.latest_attempt.as_ref();
    if text.contains("Reset applied") {
        assert!(
            m.callback.is_some() && latest.map(|r| &r.attempt_key) == m.callback.as_ref(),
            "a receipt is current only as this context's own answer: {text}"
        );
    }
    if text.contains("Nothing was sent") {
        assert!(
            matches!(&m.answer, OwnAnswer::Refused(r) if r.sent == Some(false))
                && mark(latest) == m.frontier,
            "a refusal is current only at its own frontier: {text}"
        );
    }
}
/// A page without this context's action can only say what the server says.
fn foreign(shown: &Option<Said>) {
    let text = text(shown);
    for own in ["Reset applied", "Not used", "Nothing was sent", PREPARING] {
        assert!(!text.contains(own), "{text}");
    }
}
fn pending_key(m: &Model) -> Option<String> {
    m.pending().map(|r| r.attempt_key.clone())
}

fn orders(events: &[Ev]) -> Vec<Vec<Ev>> {
    fn go(left: &mut Vec<Ev>, at: usize, out: &mut Vec<Vec<Ev>>) {
        if at == left.len() {
            out.push(left.clone());
            return;
        }
        for i in at..left.len() {
            left.swap(at, i);
            go(left, at + 1, out);
            left.swap(at, i);
        }
    }
    let mut out = vec![];
    go(&mut events.to_vec(), 0, &mut out);
    out
}

/// Recorded fixtures must be a consistent Store history: equal revisions are identical, and a higher revision never
/// lowers attempt progress. The model's admission rule relies on it.
fn consistent(t: &serde_json::Value) {
    let mut all: Vec<Evidence> = vec![];
    for (_, v) in t.as_object().unwrap() {
        if let Some(e) = v.get("evidence").filter(|e| !e.is_null()) {
            all.push(serde_json::from_value(e.clone()).unwrap());
        }
    }
    for a in &all {
        assert!(a.valid());
        for b in &all {
            if rev(a) == rev(b) {
                assert_eq!(a, b);
            }
            if rev(a) < rev(b) {
                assert!(progress(a.latest_attempt.as_ref()) <= progress(b.latest_attempt.as_ref()));
            }
        }
    }
}

fn replay(t: &serde_json::Value, own: Own, order: &[Ev]) {
    let rc = ResetCtx::new();
    let page2 = ResetCtx::new();
    let before: Inventory = value(t, "before");
    for page in [rc, page2] {
        page.show(ROUTE, "codex", false);
        page.take(ROUTE, before.clone(), 100.);
    }
    let canonical = rc.lock_key(ROUTE);
    assert!(rc.bind_alias(
        SIBLING,
        &canonical,
        before.binding_revision.as_ref().unwrap().parse().unwrap()
    ));
    let mut m = Model::new(&before, own);
    let mut m2 = Model::new(&before, Own::Nothing);
    let id = match own {
        Own::Send => Some(rc.begin_send(&canonical, SENT_AT)),
        Own::Prepare => Some(rc.begin_action(&canonical)),
        Own::Nothing => None,
    };
    let check = |m: &Model, m2: &Model, at: &str| {
        let expected = m.expected();
        assert_eq!(rc.said.get_untracked(), expected, "{at}");
        universal(m, &expected);
        let lock = rc.unresolved.get_untracked();
        assert_eq!(lock.blocks(), m.blocked(), "{at}");
        assert_eq!(lock.key, pending_key(m), "{at}");
        // Sibling alias and navigation project the same canonical state.
        rc.show(SIBLING, "codex", false);
        assert_eq!(rc.said.get_untracked(), expected, "sibling {at}");
        rc.show(OTHER, "codex", false);
        rc.show(ROUTE, "codex", false);
        assert_eq!(rc.said.get_untracked(), expected, "navigation {at}");
        // A second page sees the same server, never this page's answers.
        assert_eq!(
            page2.said.get_untracked(),
            m2.expected(),
            "second page {at}"
        );
        assert_eq!(
            page2.unresolved.get_untracked().blocks(),
            m2.blocked(),
            "second page {at}"
        );
        foreign(&page2.said.get_untracked());
        // A reload starts from the newest server read alone.
        let fresh = ResetCtx::new();
        fresh.show(ROUTE, "codex", false);
        fresh.take(ROUTE, m.newest.clone(), 1.);
        let reload = Model::new(&m.newest, Own::Nothing);
        assert_eq!(fresh.said.get_untracked(), reload.expected(), "reload {at}");
        assert_eq!(
            fresh.unresolved.get_untracked().blocks(),
            reload.blocked(),
            "reload {at}"
        );
        foreign(&fresh.said.get_untracked());
    };
    check(&m, &m2, "setup");
    for (step, ev) in order.iter().copied().enumerate() {
        let now = if m.late {
            DEADLINE + 1. + step as f64
        } else {
            110. + step as f64
        };
        match ev {
            Ev::Callback => {
                rc.claim_answer(&canonical, id.unwrap(), Answer::Ok(value(t, "retry")), now)
            }
            Ev::Refusal(body) => rc.claim_answer(
                &canonical,
                id.unwrap(),
                Answer::Refused(value(t, body)),
                now,
            ),
            Ev::Read(name) => {
                for page in [rc, page2] {
                    page.take(ROUTE, value(t, name), now);
                }
            }
            Ev::Unavailable(name) => {
                for page in [rc, page2] {
                    page.take(ROUTE, failed(t, name), now);
                }
            }
            Ev::Check => {
                rc.receive(&canonical, &value(t, "retry"), None);
            }
            Ev::Hint => {
                rc.invalidate();
                page2.invalidate();
            }
            Ev::Tick => {}
        }
        m.deliver(t, ev);
        if matches!(ev, Ev::Read(_) | Ev::Unavailable(_) | Ev::Hint | Ev::Tick) {
            m2.deliver(t, ev);
        }
        check(&m, &m2, &format!("{order:?} step {step} {ev:?}"));
    }
}

/// Replays every order of `events` for both recorded traces (no earlier claim, and an earlier claim).
fn family(own: Own, events: &[Ev]) -> usize {
    let mut runs = 0;
    for prior in [false, true] {
        let t = trace(prior);
        consistent(&t);
        for order in orders(events) {
            Owner::new().with(|| replay(&t, own, &order));
            runs += 1;
        }
    }
    runs
}

#[test]
fn every_order_of_own_success_and_own_retry_success() {
    use Ev::*;
    let events = [
        Callback,
        Read("duringRetry"),
        Read("afterRetry"),
        Read("recovered"),
        Hint,
        Tick,
    ];
    // prior=false is the first claim, prior=true the retry after an earlier claim.
    assert_eq!(family(Own::Send, &events), 2 * 720);
}

#[test]
fn every_order_of_own_refusal_with_a_delayed_pre_send_read() {
    use Ev::*;
    let events = [
        Refusal("refused"),
        Read("recovered"),
        Read("withToken"),
        Hint,
        Tick,
    ];
    assert_eq!(family(Own::Send, &events), 2 * 120);
}

#[test]
fn every_order_of_old_and_stale_refusals_with_newer_pending_and_confirmed_evidence() {
    use Ev::*;
    let mut runs = 0;
    for body in ["refused", "consumedWhilePending", "consumedAfterSuccess"] {
        let events = [
            Refusal(body),
            Read("recovered"),
            Read("duringRetry"),
            Read("afterRetry"),
            Hint,
            Tick,
        ];
        runs += family(Own::Send, &events);
    }
    assert_eq!(runs, 3 * 2 * 720);
}

#[test]
fn every_order_of_foreign_pending_and_success_with_and_without_a_local_prepare() {
    use Ev::*;
    let events = [
        Read("duringRetry"),
        Read("afterRetry"),
        Read("recovered"),
        Check,
        Hint,
        Tick,
    ];
    assert_eq!(
        family(Own::Nothing, &events) + family(Own::Prepare, &events),
        2 * 2 * 720
    );
}

#[test]
fn every_order_of_unavailable_reads_around_own_success_and_own_refusal() {
    use Ev::*;
    let success = [
        Callback,
        Unavailable("afterRetry"),
        Read("afterRetry"),
        Hint,
        Tick,
    ];
    let refusal = [
        Refusal("refused"),
        Unavailable("recovered"),
        Read("recovered"),
        Hint,
        Tick,
    ];
    assert_eq!(
        family(Own::Send, &success) + family(Own::Send, &refusal),
        2 * 2 * 120
    );
}

/// The recorded X -> Y -> X -> Y saved-login changes while quota reads fail (RECHECK-9 trace), in every order.
/// Per canonical account: the newest admitted read decides state; the route follows the newest binding.
#[test]
fn every_order_of_account_replacement_and_reversion_while_quota_reads_fail() {
    let t = aba();
    let names = [
        "prior",
        "unavailable",
        "recovered",
        "traceExtra.back",
        "traceExtra.again",
        "hint",
    ];
    let mut runs = 0;
    for order in orders(&names.map(Ev::Read)) {
        runs += 1;
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            rc.show("codex:reused", "codex", false);
            // canonical -> (newest admitted inventory, hinted since it)
            let mut best: Vec<(String, Inventory, bool)> = vec![];
            let mut bound: Option<(u64, String)> = None;
            for (step, ev) in order.iter().enumerate() {
                let Ev::Read(name) = *ev else { unreachable!() };
                if name == "hint" {
                    rc.invalidate();
                    best.iter_mut().for_each(|b| b.2 = true);
                } else {
                    let inv: Inventory = value(&t, name);
                    let key = inv.canonical_account_key.clone().unwrap();
                    let binding: u64 = inv.binding_revision.as_ref().unwrap().parse().unwrap();
                    assert_eq!(binding, rev(inv.evidence.as_ref().unwrap()));
                    rc.take("codex:reused", inv.clone(), step as f64);
                    match best.iter_mut().find(|b| b.0 == key) {
                        Some(b) if rev(b.1.evidence.as_ref().unwrap()) >= binding => {}
                        Some(b) => *b = (key.clone(), inv, false),
                        None => best.push((key.clone(), inv, false)),
                    }
                    if bound.as_ref().is_none_or(|(at, _)| binding > *at) {
                        bound = Some((binding, key));
                    }
                }
                let Some((_, key)) = &bound else {
                    continue;
                };
                assert_eq!(&rc.lock_key("codex:reused"), key);
                let (_, inv, hinted) = best.iter().find(|b| &b.0 == key).unwrap();
                let mut m = Model::new(inv, Own::Nothing);
                if inv.status != "ok" {
                    m.failure = Some(inv.reason == "account-changed-usage-unavailable");
                }
                m.refresh |= *hinted;
                let shown = rc.said.get_untracked();
                let expected = m.expected();
                let notice = format!("{ACCOUNT_CHANGED} ");
                let bare = text(&shown).strip_prefix(&notice).unwrap_or(text(&shown));
                assert!(
                    bare == text(&expected) || (expected.is_none() && bare == ACCOUNT_CHANGED),
                    "{order:?} step {step}: {shown:?} vs {expected:?}"
                );
                foreign(&shown);
                assert_eq!(
                    rc.unresolved.get_untracked().blocks(),
                    m.blocked(),
                    "{order:?} step {step}"
                );
                assert_eq!(rc.unresolved.get_untracked().key, pending_key(&m));
                if m.pending().is_some() {
                    assert!(
                        history_rows(&rc.history_of("codex:reused"))
                            .iter()
                            .all(|r| r.result != "Used, confirmed")
                    );
                }
            }
        });
    }
    assert_eq!(runs, 720);
}
