//! R1 (fed): Home's inputs as generations the TS owner pushes when they change, and the finished answer of one
//! request (`GET /api/views/home`) built from them: the bytes `res.json` would write for
//! `{schemaVersion, dashboard, profiles, antigravity, view}` and the revalidated-view identity for its `ETag`.
//!
//! Each feed is prepared once, when its generation arrives, and held immutable:
//! - [`Dashboard`]: the dashboard answer before its per-response pass, as `JSON.stringify` wrote it with
//!   `updatedAt: ""`, plus its facts (the `localeCompare` ranks of the words Home sorts by and the `Date.parse`
//!   epochs of every `resetAt`/`sampledAt` string, both computed by the server engine once per generation). The text
//!   must round-trip through [`crate::js_json`] byte for byte, so every byte served is the one Node would write.
//! - [`Opens`]: this process's Claude Open progress per profile id (`ClaudeOpenOperations.forScope`).
//! - the public Antigravity inventory, composed from the runtime's inventory (part 4) when it arrives.
//!
//! Per request only the clock (`now`, `updatedAt`), the page's context and the generation ids cross. The request
//! applies `markPassedResets` (`account-window-reset.ts`) with `now` while writing, reads the desktop manifest
//! (the caller), adds each Open's progress, and builds the view when the page sent its context. Anything not proven
//! declines with its reason, and the TS handler answers.
use super::{FedInput, build_fed};
use crate::decline::Decline;
use crate::js::Ranks;
use crate::js_json::{Writer, ordered};
use aac_api::home::{HOME_VIEW_VERSION, HomeContext};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{collections::HashMap, sync::Arc};

/// The view-version header word of the answer (`x-aac-view`).
pub const VIEW: &str = "home";
/// Its version: the view model's.
pub const VERSION: u32 = HOME_VIEW_VERSION;

/// One window's `markPassedResets` facts, fixed for the generation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Mark {
    /// `Date.parse(window.resetAt)` when `resetAt` is a string with a finite epoch.
    reset: Option<i64>,
    /// `Date.parse(window.sampledAt ?? account.sampledAt)` when that is a string with a finite epoch.
    sampled: Option<i64>,
    /// `window.resetPassed === true` as held.
    held: bool,
    /// The window has a `resetPassed` member (its position is kept when it changes).
    member: bool,
}

impl Mark {
    /// `readingPredatesReset(resetAt, sampledAt, now)`.
    fn passed(self, now: i64) -> bool {
        self.reset
            .is_some_and(|reset| reset <= now && self.sampled.is_none_or(|s| s < reset))
    }
}

/// One piece of a dashboard generation's answer, written once when the generation arrived: everything but the
/// per-response parts is fixed bytes, so a request copies them instead of writing the answer again.
#[derive(Debug)]
enum Part {
    /// Fixed text: the full bytes and the identity bytes.
    Text(Vec<u8>, Vec<u8>),
    /// This response's `updatedAt`.
    UpdatedAt,
    /// A window whose reset can pass: its text as held, and with `resetPassed` flipped (`markPassedResets`).
    Window {
        mark: Mark,
        held: (Vec<u8>, Vec<u8>),
        flipped: (Vec<u8>, Vec<u8>),
    },
}

/// A held dashboard generation.
#[derive(Debug)]
pub struct Dashboard {
    value: Value,
    ranks: Arc<Ranks>,
    parts: Vec<Part>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DashboardFacts {
    order: Ranks,
    epochs: HashMap<String, Option<i64>>,
}

/// A `resetAt`/`sampledAt` value's epoch: a string must have its fact (`null` is NaN); anything else is NaN.
fn epoch(
    value: Option<&Value>,
    epochs: &HashMap<String, Option<i64>>,
) -> Result<Option<i64>, Decline> {
    match value {
        Some(Value::String(text)) => epochs.get(text).copied().ok_or(Decline::Locale),
        _ => Ok(None),
    }
}

/// Prepare a dashboard generation: `data` is `JSON.stringify({...dashboard, updatedAt: ''})`, `facts` its ranks and
/// epochs. Declines (and every request for the generation then declines with it): text that does not round-trip,
/// a shape `markPassedResets` would throw on (TS answers its 500), a missing fact.
pub fn prepare_dashboard(data: &[u8], facts: &[u8]) -> Result<Dashboard, Decline> {
    let value: Value = serde_json::from_slice(data).map_err(|_| Decline::Shape)?;
    let facts: DashboardFacts = serde_json::from_slice(facts).map_err(|_| Decline::Locale)?;
    let top = value.as_object().ok_or(Decline::Shape)?;
    if !matches!(top.get("updatedAt"), Some(Value::String(s)) if s.is_empty()) {
        return Err(Decline::Shape);
    }
    let accounts = top
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or(Decline::Shape)?;
    let mut marks = Vec::with_capacity(accounts.len());
    for account in accounts {
        // `account.windows.map(...)`: anything but an object with a window list throws in TS.
        let account = account.as_object().ok_or(Decline::Shape)?;
        let windows = account
            .get("windows")
            .and_then(Value::as_array)
            .ok_or(Decline::Shape)?;
        let mut row = Vec::with_capacity(windows.len());
        for window in windows {
            row.push(match window {
                Value::Null => return Err(Decline::Shape),
                Value::Object(w) => {
                    // `window.sampledAt ?? account.sampledAt`
                    let sampled = match w.get("sampledAt") {
                        None | Some(Value::Null) => account.get("sampledAt"),
                        own => own,
                    };
                    Mark {
                        reset: epoch(w.get("resetAt"), &facts.epochs)?,
                        sampled: epoch(sampled, &facts.epochs)?,
                        held: w.get("resetPassed") == Some(&Value::Bool(true)),
                        member: w.contains_key("resetPassed"),
                    }
                }
                // A primitive or a list has no `resetAt`: never passed, written as is.
                _ => Mark::default(),
            });
        }
        marks.push(row);
    }
    let parts = template(&value, &marks);
    // The bytes the template writes as held must be the text Node wrote: every byte served is Node's.
    let mut held = Vec::with_capacity(data.len());
    for part in &parts {
        match part {
            Part::Text(full, _) => held.extend_from_slice(full),
            Part::UpdatedAt => held.extend_from_slice(b"\"\""),
            Part::Window {
                held: (full, _), ..
            } => held.extend_from_slice(full),
        }
    }
    if held != data {
        return Err(Decline::Number);
    }
    Ok(Dashboard {
        value,
        ranks: Arc::new(facts.order),
        parts,
    })
}

/// A held Open-progress generation: `forScope`'s operation per profile id.
#[derive(Debug, Default)]
pub struct Opens(HashMap<String, Value>);

/// Prepare an Open-progress generation: `[[profileId, operation], ...]`, each id once, each operation an object.
pub fn prepare_opens(data: &[u8]) -> Result<Opens, Decline> {
    let rows: Vec<(String, Value)> = serde_json::from_slice(data).map_err(|_| Decline::Shape)?;
    let mut map = HashMap::with_capacity(rows.len());
    for (id, operation) in rows {
        if !operation.is_object() || map.insert(id, operation).is_some() {
            return Err(Decline::Shape);
        }
    }
    Ok(Opens(map))
}

/// Prepare an Antigravity generation: the runtime's inventory in the ISO envelope (`rustAntigravityInventoryInput`),
/// composed now (part 4), or `null` when TS could not carry it exactly. The answer must have the shape
/// `isRustAntigravityInventory` admits.
pub fn prepare_antigravity(data: &[u8]) -> Result<Value, Decline> {
    let value: Value = serde_json::from_slice(data).map_err(|_| Decline::Shape)?;
    if value.is_null() {
        return Err(Decline::Shape);
    }
    let envelope = serde_json::from_value(value).map_err(|_| Decline::Shape)?;
    let composed = crate::antigravity::inventory(envelope).map_err(|_| Decline::Unsupported)?;
    let keys: Vec<&str> = composed
        .as_object()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let shaped = [
        &["schemaVersion", "hostId", "profiles"][..],
        &["schemaVersion", "hostId", "nativeUpdatePaused", "profiles"],
        &[
            "schemaVersion",
            "hostId",
            "runtimeServiceProblem",
            "profiles",
        ],
        &[
            "schemaVersion",
            "hostId",
            "nativeUpdatePaused",
            "runtimeServiceProblem",
            "profiles",
        ],
    ]
    .contains(&keys.as_slice());
    if shaped && composed.get("profiles").is_some_and(Value::is_array) {
        Ok(composed)
    } else {
        Err(Decline::Unsupported)
    }
}

/// One request's inputs.
pub struct Request<'a> {
    /// The held dashboard of this request's platform and transport.
    pub dashboard: &'a Dashboard,
    /// The held Open progress.
    pub opens: &'a Opens,
    /// The held public Antigravity inventory.
    pub antigravity: &'a Value,
    /// This request's manifest read: `{profiles: [...]}` as GET /api/claude/desktop-profiles's Rust read answers.
    pub manifest: &'a Value,
    /// The page's context, when TS could read it (otherwise the answer has no view, as today's handler).
    pub context: Option<&'a HomeContext>,
    /// The clock of the per-response pass (`markPassedResets`).
    pub now: i64,
    /// The answer's `updatedAt`.
    pub updated_at: &'a str,
}

/// The finished answer: its bytes and its revalidated-view identity.
pub struct Answer {
    /// What `res.json` writes.
    pub bytes: Vec<u8>,
    /// The `ETag` identity text.
    pub identity: Vec<u8>,
}

/// `{...profile, openOperation}` for each manifest row with an id (`desktopProfilesRead` `withOperations`), after
/// `acceptRustDesktopProfiles`'s shape check.
fn profiles_with_opens(manifest: &Value, opens: &Opens) -> Result<Value, Decline> {
    let body = manifest.as_object().ok_or(Decline::Shape)?;
    let rows = match body.get("profiles") {
        Some(Value::Array(rows)) if body.len() == 1 => rows,
        _ => return Err(Decline::Shape),
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let row = row.as_object().ok_or(Decline::Shape)?;
        match row.get("id") {
            Some(Value::String(id)) if !id.is_empty() => {
                let operation = opens.0.get(id).cloned().unwrap_or(Value::Null);
                let mut with: Map<String, Value> = row.clone();
                // A spread keeps an existing member's position; a new one goes last.
                with.insert("openOperation".into(), operation);
                out.push(Value::Object(with));
            }
            _ => out.push(Value::Object(row.clone())),
        }
    }
    Ok(Value::Array(out))
}

/// A window as `markPassedResets` leaves it: `passed` differs from what is held, so `resetPassed` is set in place
/// or appended (`{ ...window, resetPassed: true }`), or removed.
fn write_flipped(w: &mut Writer, members: &Map<String, Value>, passed: bool, member: bool) {
    let mut object = w.object();
    for (key, value) in ordered(members) {
        if key == "resetPassed" {
            if passed {
                object.member(key, |w| w.raw(b"true"));
            }
            continue;
        }
        object.member(key, |w| w.value(value));
    }
    if passed && !member {
        object.member("resetPassed", |w| w.raw(b"true"));
    }
    object.end();
}

/// The generation's answer as parts: fixed text, with holes for `updatedAt` and for each window whose reset can
/// pass (both of its texts written now).
fn template(value: &Value, marks: &[Vec<Mark>]) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut w = Writer::with_capacity(64 * 1024);
    let Value::Object(top) = value else {
        w.value(value);
        let (full, identity) = w.cut();
        return vec![Part::Text(full, identity)];
    };
    let mut object = w.object();
    for (key, member) in ordered(top) {
        match (key, member) {
            ("updatedAt", _) => object.member(key, |w| {
                let (full, identity) = w.cut();
                parts.push(Part::Text(full, identity));
                parts.push(Part::UpdatedAt);
            }),
            ("accounts", Value::Array(accounts)) => object.member(key, |w| {
                w.raw(b"[");
                for (i, (account, marks)) in accounts.iter().zip(marks).enumerate() {
                    if i > 0 {
                        w.raw(b",");
                    }
                    let Value::Object(members) = account else {
                        w.value(account);
                        continue;
                    };
                    let mut row = w.object();
                    for (name, field) in ordered(members) {
                        match (name, field) {
                            ("windows", Value::Array(windows)) => row.member(name, |w| {
                                w.raw(b"[");
                                for (j, (window, mark)) in windows.iter().zip(marks).enumerate() {
                                    if j > 0 {
                                        w.raw(b",");
                                    }
                                    match window {
                                        // A window whose text can differ from the held one (its reset can
                                        // pass, or it holds `resetPassed: true`, which a window that never
                                        // passes loses): both texts now, a hole in the parts.
                                        Value::Object(members)
                                            if mark.reset.is_some() || mark.held =>
                                        {
                                            let (full, identity) = w.cut();
                                            parts.push(Part::Text(full, identity));
                                            let mut held = Writer::default();
                                            held.value(window);
                                            let mut flipped = Writer::default();
                                            write_flipped(
                                                &mut flipped,
                                                members,
                                                !mark.held,
                                                mark.member,
                                            );
                                            parts.push(Part::Window {
                                                mark: *mark,
                                                held: held.finish(),
                                                flipped: flipped.finish(),
                                            });
                                        }
                                        _ => w.value(window),
                                    }
                                }
                                w.raw(b"]");
                            }),
                            _ => row.member(name, |w| w.value(field)),
                        }
                    }
                    row.end();
                }
                w.raw(b"]");
            }),
            _ => object.member(key, |w| w.value(member)),
        }
    }
    object.end();
    let (full, identity) = w.cut();
    parts.push(Part::Text(full, identity));
    parts
}

/// The dashboard answer of this response: the generation's parts with this response's `updatedAt` and its passed
/// resets.
fn write_dashboard(w: &mut Writer, dashboard: &Dashboard, updated_at: &str, now: i64) {
    for part in &dashboard.parts {
        match part {
            Part::Text(full, identity) => w.raw_pair(full, identity),
            Part::UpdatedAt => w.string(updated_at),
            Part::Window {
                mark,
                held,
                flipped,
            } => {
                let (full, identity) = if mark.passed(now) == mark.held {
                    held
                } else {
                    flipped
                };
                w.raw_pair(full, identity);
            }
        }
    }
}

/// The finished answer of one Home request, or the reason it declines.
pub fn respond(request: Request) -> Result<Answer, Decline> {
    let profiles = profiles_with_opens(request.manifest, request.opens)?;
    // The view's own serializer: the Home spec proves its bytes are JSON.stringify's, and it has no clock-only
    // member, so its identity text is the same bytes.
    let view = match request.context {
        None => b"null".to_vec(),
        Some(ctx) => {
            let built = build_fed(FedInput {
                ctx,
                dashboard: &request.dashboard.value,
                profiles: &profiles,
                antigravity: request.antigravity,
                ranks: request.dashboard.ranks.clone(),
                updated_at: Some(request.updated_at),
            })
            .map_err(Decline::from)?;
            serde_json::to_vec(&built).map_err(|_| Decline::Internal)?
        }
    };
    let mut w = Writer::with_capacity(256 * 1024);
    let mut top = w.object();
    top.member("schemaVersion", |w| w.raw(b"1"));
    top.member("dashboard", |w| {
        write_dashboard(w, request.dashboard, request.updated_at, request.now);
    });
    top.member("profiles", |w| w.value(&profiles));
    top.member("antigravity", |w| w.value(request.antigravity));
    top.member("view", |w| w.raw_pair(&view, &view));
    top.end();
    let (bytes, identity) = w.finish();
    Ok(Answer { bytes, identity })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js_json;
    use serde_json::json;

    fn text(value: &Value) -> Vec<u8> {
        js_json::write(value)
    }

    fn dashboard(windows: Value) -> Vec<u8> {
        text(&json!({
            "schemaVersion": 1,
            "updatedAt": "",
            "accounts": [{"id": "a", "sampledAt": "2026-11-14T10:00:00.000Z", "windows": windows}],
        }))
    }

    fn facts(epochs: Value) -> Vec<u8> {
        text(&json!({"order": {"a": 0}, "epochs": epochs}))
    }

    #[test]
    fn passed_resets_follow_mark_passed_resets_with_the_request_clock() {
        // account-window-reset.test.ts semantics: passed once now reaches the reset and the reading predates it.
        let reset = 1_794_650_400_000_i64; // 2026-11-14T12:00:00.000Z
        let data = dashboard(json!([
            {"key": "five", "resetAt": "2026-11-14T12:00:00.000Z"},
            {"key": "held", "resetAt": "2026-11-14T12:00:00.000Z", "resetPassed": true, "z": 1},
            {"key": "after", "resetAt": "2026-11-14T12:00:00.000Z", "sampledAt": "2026-11-14T12:30:00.000Z"},
            5,
            // Held as passed but without a reset: markPassedResets removes the flag at any clock.
            {"key": "never", "resetPassed": true}
        ]));
        let held = prepare_dashboard(
            &data,
            &facts(json!({
                "2026-11-14T12:00:00.000Z": reset,
                "2026-11-14T10:00:00.000Z": reset - 7_200_000,
                "2026-11-14T12:30:00.000Z": reset + 1_800_000,
            })),
        )
        .expect("prepared");
        let write = |now: i64| {
            let mut w = Writer::default();
            write_dashboard(&mut w, &held, "2026-11-14T13:00:00.000Z", now);
            String::from_utf8(w.into_bytes()).unwrap_or_default()
        };
        assert_eq!(
            write(reset - 1),
            r#"{"schemaVersion":1,"updatedAt":"2026-11-14T13:00:00.000Z","accounts":[{"id":"a","sampledAt":"2026-11-14T10:00:00.000Z","windows":[{"key":"five","resetAt":"2026-11-14T12:00:00.000Z"},{"key":"held","resetAt":"2026-11-14T12:00:00.000Z","z":1},{"key":"after","resetAt":"2026-11-14T12:00:00.000Z","sampledAt":"2026-11-14T12:30:00.000Z"},5,{"key":"never"}]}]}"#
        );
        assert_eq!(
            write(reset),
            r#"{"schemaVersion":1,"updatedAt":"2026-11-14T13:00:00.000Z","accounts":[{"id":"a","sampledAt":"2026-11-14T10:00:00.000Z","windows":[{"key":"five","resetAt":"2026-11-14T12:00:00.000Z","resetPassed":true},{"key":"held","resetAt":"2026-11-14T12:00:00.000Z","resetPassed":true,"z":1},{"key":"after","resetAt":"2026-11-14T12:00:00.000Z","sampledAt":"2026-11-14T12:30:00.000Z"},5,{"key":"never"}]}]}"#
        );
    }

    #[test]
    fn generations_that_cannot_be_served_exactly_decline() {
        let windows = json!([{"resetAt": "2026-11-14T12:00:00.000Z"}]);
        let all = json!({"2026-11-14T12:00:00.000Z": 1, "2026-11-14T10:00:00.000Z": null});
        assert!(prepare_dashboard(&dashboard(windows.clone()), &facts(all.clone())).is_ok());
        // Text Node would not write (a number spelled another way): the bytes would not be Node's.
        let mut odd = dashboard(windows.clone());
        let at = odd.windows(2).position(|w| w == b":1").map_or(0, |i| i + 1);
        odd.splice(at..=at, b"1.0".iter().copied());
        assert_eq!(
            prepare_dashboard(&odd, &facts(all.clone())).err(),
            Some(Decline::Number)
        );
        // A missing epoch fact, a window markPassedResets throws on, no placeholder clock.
        assert_eq!(
            prepare_dashboard(&dashboard(windows.clone()), &facts(json!({}))).err(),
            Some(Decline::Locale)
        );
        assert_eq!(
            prepare_dashboard(&dashboard(json!([null])), &facts(all.clone())).err(),
            Some(Decline::Shape)
        );
        let clocked = text(&json!({"updatedAt": "2026-11-14T13:00:00.000Z", "accounts": []}));
        assert_eq!(
            prepare_dashboard(&clocked, &facts(all)).err(),
            Some(Decline::Shape)
        );
        assert_eq!(prepare_opens(br#"[["p","x"]]"#).err(), Some(Decline::Shape));
        assert_eq!(
            prepare_opens(br#"[["p",{}],["p",{}]]"#).err(),
            Some(Decline::Shape)
        );
        assert_eq!(prepare_antigravity(b"null").err(), Some(Decline::Shape));
    }

    #[test]
    fn profiles_get_their_open_progress_and_the_answer_has_no_view_without_a_context() {
        let held = prepare_dashboard(&dashboard(json!([])), &facts(json!({}))).expect("prepared");
        let opens = prepare_opens(br#"[["p1",{"id":"op_1","state":"opened"}]]"#).expect("opens");
        let manifest = json!({"profiles": [{"id": "p1", "email": "a@example.invalid"}, {"id": "p2"}, {"email": "x"}]});
        let answer = respond(Request {
            dashboard: &held,
            opens: &opens,
            antigravity: &json!({"schemaVersion": 1, "hostId": "ubuntu", "profiles": []}),
            manifest: &manifest,
            context: None,
            now: 0,
            updated_at: "2026-11-14T13:00:00.000Z",
        })
        .map_err(|d| d.as_str())
        .expect("answer");
        assert_eq!(
            String::from_utf8(answer.bytes).unwrap_or_default(),
            r#"{"schemaVersion":1,"dashboard":{"schemaVersion":1,"updatedAt":"2026-11-14T13:00:00.000Z","accounts":[{"id":"a","sampledAt":"2026-11-14T10:00:00.000Z","windows":[]}]},"profiles":[{"id":"p1","email":"a@example.invalid","openOperation":{"id":"op_1","state":"opened"}},{"id":"p2","openOperation":null},{"email":"x"}],"antigravity":{"schemaVersion":1,"hostId":"ubuntu","profiles":[]},"view":null}"#
        );
    }
}
