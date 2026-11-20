//! Cursor's two calls for the account's usage, and its plan usage (DESIGN 17.1; CURSOR.md source
//! B, as T3's `accountUsage.ts` and `apiKey.ts` make them). Fixed host, no redirects (the token is
//! never forwarded), 10 s per call, 1 MB per answer. Failures are fixed causes; nothing Cursor
//! writes back is ever printed.

use crate::util;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

pub const CURSOR_API: &str = "https://api2.cursor.sh";
const MAX_ANSWER_BYTES: u64 = 1024 * 1024;
const PAGE_SIZE: usize = 1000;
const MAX_PAGES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// 401 or 403: the key (or the token it gave) was refused.
    Rejected,
    Timeout,
    Network,
    /// 5xx.
    Server,
    /// Not the answer's expected shape, an inconsistent page count, a redirect, or too large.
    BadResponse,
}

impl Failure {
    pub fn cause(self) -> &'static str {
        match self {
            Failure::Rejected => "key_rejected",
            Failure::Timeout => "timeout",
            Failure::Network => "network",
            Failure::Server => "server_error",
            Failure::BadResponse => "bad_response",
        }
    }
}

pub struct Client {
    agent: ureq::Agent,
    base: String,
}

impl Client {
    /// The fixed Cursor host. `base` replaces it only when it is a loopback `http://127.0.0.1:`
    /// address (tests); anything else is ignored, so the key can only ever go to Cursor.
    pub fn new(base: Option<&str>) -> Client {
        let base = base
            .filter(|b| b.starts_with("http://127.0.0.1:") && !b.contains('@'))
            .unwrap_or(CURSOR_API)
            .trim_end_matches('/')
            .to_string();
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout(Duration::from_secs(10))
            .build();
        Client { agent, base }
    }

    fn post(&self, path: &str, bearer: &str, body: &str, connect: bool) -> Result<Value, Failure> {
        let mut request = self
            .agent
            .post(&format!("{}{path}", self.base))
            .set("Authorization", &format!("Bearer {bearer}"))
            .set("Content-Type", "application/json");
        if connect {
            request = request.set("connect-protocol-version", "1");
        }
        let response = match request.send_string(body) {
            Ok(response) => response,
            Err(ureq::Error::Status(code, _)) => {
                return Err(match code {
                    401 | 403 => Failure::Rejected,
                    500..=599 => Failure::Server,
                    _ => Failure::BadResponse,
                });
            }
            Err(ureq::Error::Transport(transport)) => {
                // The I/O error's kind, not its words: Windows words a timeout differently.
                let io_kind = std::error::Error::source(&transport)
                    .and_then(|source| source.downcast_ref::<std::io::Error>())
                    .map(std::io::Error::kind);
                let text = transport.to_string().to_ascii_lowercase();
                let timed_out = matches!(
                    io_kind,
                    Some(std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock)
                ) || text.contains("timed out")
                    || text.contains("timeout");
                return Err(if timed_out { Failure::Timeout } else { Failure::Network });
            }
        };
        // A redirect is an answer, never followed.
        if !(200..300).contains(&response.status()) {
            return Err(Failure::BadResponse);
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_ANSWER_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Failure::Network)?;
        if bytes.len() as u64 > MAX_ANSWER_BYTES {
            return Err(Failure::BadResponse);
        }
        serde_json::from_slice(&bytes).map_err(|_| Failure::BadResponse)
    }

    /// The key traded for an access token, as `@cursor/sdk` does.
    pub fn exchange(&self, key: &str) -> Result<String, Failure> {
        let answer = self.post("/auth/exchange_user_api_key", key, "{}", false)?;
        answer
            .get("accessToken")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty() && t.len() <= 16_384)
            .map(str::to_string)
            .ok_or(Failure::BadResponse)
    }
}

/// The account a token names: 16 hex of sha256("aac-cursor-account-v1:" + the JWT's `sub`).
pub fn account_hash(token: &str) -> Option<String> {
    let payload = util::base64url_decode(token.split('.').nth(1)?)?;
    let claims: Value = serde_json::from_slice(&payload).ok()?;
    let subject = claims.get("sub")?.as_str().filter(|s| !s.is_empty())?;
    Some(util::sha256_hex(format!("aac-cursor-account-v1:{subject}").as_bytes())[..16].to_string())
}

/// One billed request.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub at_ms: i64,
    pub model: String,
    /// The conversation as a session key (`aac-session-v1:cursor:`), never the raw id.
    pub session: Option<String>,
    /// Input (uncached), output, cache read, cache write.
    pub tokens: [u64; 4],
    pub cents: Option<f64>,
}

fn number(value: Option<&Value>) -> Result<Option<f64>, Failure> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(v) if v.is_finite() && v >= 0.0 => Ok(Some(v)),
            _ => Err(Failure::BadResponse),
        },
        _ => Err(Failure::BadResponse),
    }
}

/// One measured token count: a finite, non-negative integer (a count is never fractional; a
/// missing field is `None`, which the caller treats as not measured, never as zero).
fn count_of(usage: &serde_json::Map<String, Value>, key: &str) -> Result<Option<u64>, Failure> {
    match usage.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(v) if v.is_finite() && v >= 0.0 && v == v.trunc() => Ok(Some(v as u64)),
            _ => Err(Failure::BadResponse),
        },
        _ => Err(Failure::BadResponse),
    }
}

/// One raw event's verdict. `Unmeasured`: the event carries a `tokenUsage` without every
/// measured count (the wire format documents no guarantee they are present, and an empty or
/// incomplete object must never become measured zeros); it is counted separately and reads as
/// not logged. `Outside`: no `tokenUsage` at all, or outside the window.
#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    Outside,
    Unmeasured,
    Measured(Event),
}

fn event(raw: &Value, since_ms: i64, until_ms: i64) -> Result<Parsed, Failure> {
    let Some(item) = raw.as_object() else {
        return Err(Failure::BadResponse);
    };
    let at = match item.get("timestamp") {
        Some(Value::String(text)) if !text.trim().is_empty() => text.trim().parse::<f64>().ok(),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
    .filter(|v| v.is_finite())
    .ok_or(Failure::BadResponse)?;
    let model = item
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty() && m.len() <= 200 && !m.chars().any(char::is_control))
        .ok_or(Failure::BadResponse)?;
    let at_ms = at as i64;
    if at_ms < since_ms || at_ms > until_ms {
        return Ok(Parsed::Outside);
    }
    let session = item
        .get("conversationId")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(|c| util::sha256_hex(format!("aac-session-v1:cursor:{c}").as_bytes())[..16].to_string());
    let usage = match item.get("tokenUsage") {
        None | Some(Value::Null) => return Ok(Parsed::Outside),
        Some(Value::Object(map)) => map,
        _ => return Err(Failure::BadResponse),
    };
    let cents = number(usage.get("totalCents"))?;
    let verdict = match (
        count_of(usage, "inputTokens")?,
        count_of(usage, "outputTokens")?,
        count_of(usage, "cacheReadTokens")?,
        count_of(usage, "cacheWriteTokens")?,
    ) {
        (Some(input), Some(output), Some(read), Some(write)) => Parsed::Measured(Event {
            at_ms,
            model: model.to_string(),
            session,
            tokens: [input, output, read, write],
            cents,
        }),
        _ => Parsed::Unmeasured,
    };
    Ok(verdict)
}

/// The longest suffix of `previous` equal to a prefix of `current` (T3's `boundaryOverlap`).
fn boundary_overlap(previous: &[String], current: &[String]) -> usize {
    let mut sequence: Vec<&str> = current.iter().map(String::as_str).collect();
    sequence.push("\u{0}");
    sequence.extend(previous.iter().map(String::as_str));
    let mut lengths = vec![0usize; sequence.len()];
    for index in 1..sequence.len() {
        let mut length = lengths[index - 1];
        while length > 0 && sequence[index] != sequence[length] {
            length = lengths[length - 1];
        }
        if sequence[index] == sequence[length] {
            length += 1;
        }
        lengths[index] = length;
    }
    lengths
        .last()
        .copied()
        .unwrap_or(0)
        .min(current.len())
        .min(previous.len())
}

/// Every billed event in [since, until], pages read in order, as `(measured events, pages read,
/// unmeasured events)`. An event the account gained while paging can repeat at a page boundary:
/// the final count proves how many copies to drop (T3's rule). A count that does not reconcile
/// is a bad answer, never a partial total. An unmeasured event (no usable `tokenUsage`) counts
/// separately and never becomes a row.
pub fn events(
    client: &Client,
    token: &str,
    since_ms: i64,
    until_ms: i64,
) -> Result<(Vec<Event>, usize, usize), Failure> {
    if since_ms < 0 || until_ms < since_ms {
        return Err(Failure::BadResponse);
    }
    let mut raw_pages: Vec<(Vec<Parsed>, usize)> = Vec::new();
    let mut previous: Vec<String> = Vec::new();
    let mut total: Option<usize> = None;
    for page in 1..=MAX_PAGES + 1 {
        if page > MAX_PAGES {
            return Err(Failure::BadResponse);
        }
        let body = json!({"page": page, "pageSize": PAGE_SIZE, "startDate": since_ms.to_string(),
                          "endDate": until_ms.to_string()});
        let answer = client.post(
            "/aiserver.v1.DashboardService/GetFilteredUsageEvents",
            token,
            &body.to_string(),
            true,
        )?;
        let Some(map) = answer.as_object() else {
            return Err(Failure::BadResponse);
        };
        if ["error", "message", "code"].iter().any(|k| map.contains_key(*k)) {
            return Err(Failure::BadResponse);
        }
        let count = if map.is_empty() {
            Some(0)
        } else {
            match map.get("totalUsageEventsCount") {
                None => None,
                Some(value) => Some(value.as_u64().ok_or(Failure::BadResponse)? as usize),
            }
        };
        let events: Vec<Value> = match map.get("usageEventsDisplay") {
            Some(Value::Array(items)) => items.clone(),
            None if map.is_empty() || map.keys().all(|k| k == "totalUsageEventsCount") => Vec::new(),
            _ => return Err(Failure::BadResponse),
        };
        if events.len() > PAGE_SIZE {
            return Err(Failure::BadResponse);
        }
        if let Some(count) = count {
            if total.is_some_and(|t| t != count) {
                return Err(Failure::BadResponse);
            }
            total = Some(count);
        }
        let printed: Vec<String> = events.iter().map(util::canonical).collect();
        let overlap = boundary_overlap(&previous, &printed);
        let mut normalized = Vec::with_capacity(events.len());
        for raw in &events {
            normalized.push(event(raw, since_ms, until_ms)?);
        }
        raw_pages.push((normalized, overlap));
        previous = printed;
        if events.len() < PAGE_SIZE {
            break;
        }
    }
    let raw_count: usize = raw_pages.iter().map(|(page, _)| page.len()).sum();
    let mut removals = match total {
        Some(total) if raw_count < total => return Err(Failure::BadResponse),
        Some(total) => raw_count - total,
        None => 0,
    };
    let pages = raw_pages.len();
    let (mut kept, mut unmeasured) = (Vec::new(), 0usize);
    for (page, overlap) in raw_pages {
        let drop = removals.min(overlap);
        removals -= drop;
        for verdict in page.into_iter().skip(drop) {
            match verdict {
                Parsed::Measured(event) => kept.push(event),
                Parsed::Unmeasured => unmeasured += 1,
                Parsed::Outside => {}
            }
        }
    }
    if removals != 0 {
        return Err(Failure::BadResponse);
    }
    Ok((kept, pages, unmeasured))
}

/// Whether the plan meter (`quota`'s answer) shows use in a billing cycle that overlaps
/// [since, until]. Cursor answers the events call with an empty message (`{}`, protobuf's "no
/// events") both when the account used nothing and when the key's token may not see its events:
/// the 2026-11-20 read gave `{}` for the events and the aggregated events while this answer read
/// 49% used. An empty events read is only "no usage" when this is false. A cycle without a start
/// cannot be placed and is false, as is a plan that reports no percentage above zero.
pub fn plan_shows_use(quota: &Value, since_ms: i64, until_ms: i64) -> bool {
    let used = quota["planUsage"].as_object().is_some_and(|plan| {
        ["totalPercentUsed", "autoPercentUsed", "apiPercentUsed"]
            .iter()
            .any(|key| plan.get(*key).and_then(Value::as_f64).is_some_and(|v| v > 0.0))
    });
    let Some(start) = quota["billingCycleStartMs"].as_i64() else {
        return false;
    };
    let end_ok = match quota["billingCycleEndMs"].as_i64() {
        Some(end) => end >= since_ms,
        None => true,
    };
    used && start <= until_ms && end_ok
}

/// One hour and model of the account's usage.
#[derive(Default)]
struct HourCell {
    tokens: [u64; 4],
    events: u64,
    cents: f64,
    uncosted: u64,
}

/// The account's usage per hour and model: tokens, events, Cursor's own cost where it gave one.
pub fn hourly(events: &[Event]) -> Vec<Value> {
    let mut cells: BTreeMap<(String, String), HourCell> = BTreeMap::new();
    for event in events {
        let Some(hour) = util::hour_label(event.at_ms) else {
            continue;
        };
        let cell = cells.entry((hour, event.model.clone())).or_default();
        for (total, value) in cell.tokens.iter_mut().zip(event.tokens) {
            *total += value;
        }
        cell.events += 1;
        match event.cents {
            Some(cents) => cell.cents += cents,
            None => cell.uncosted += 1,
        }
    }
    cells
        .into_iter()
        .map(|((hour, model), cell)| {
            let mut row = Map::new();
            row.insert("h".into(), json!(hour));
            row.insert("m".into(), json!(model));
            row.insert("i".into(), json!(cell.tokens[0]));
            row.insert("o".into(), json!(cell.tokens[1]));
            row.insert("cr".into(), json!(cell.tokens[2]));
            row.insert("cw".into(), json!(cell.tokens[3]));
            row.insert("n".into(), json!(cell.events));
            row.insert("cents".into(), json!(cell.cents));
            row.insert("uncosted".into(), json!(cell.uncosted));
            Value::Object(row)
        })
        .collect()
}

/// The plan usage the quota card shows (DESIGN 17.6), for comparison with the desktop sign-in.
pub fn quota(client: &Client, token: &str) -> Result<Value, Failure> {
    let answer = client.post("/aiserver.v1.DashboardService/GetCurrentPeriodUsage", token, "{}", true)?;
    let Some(map) = answer.as_object() else {
        return Err(Failure::BadResponse);
    };
    let epoch = |key: &str| -> Result<Option<i64>, Failure> {
        match map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => text.trim().parse::<i64>().map(Some).map_err(|_| Failure::BadResponse),
            Some(Value::Number(n)) => n.as_i64().map(Some).ok_or(Failure::BadResponse),
            _ => Err(Failure::BadResponse),
        }
    };
    let mut plan = Map::new();
    if let Some(Value::Object(usage)) = map.get("planUsage") {
        for key in ["totalPercentUsed", "autoPercentUsed", "apiPercentUsed"] {
            // The reported percentage, unclamped: a plan over its quota reads above 100, and a
            // comparison must see that. Any cap belongs to the presentation.
            if let Some(value) = number(usage.get(key))? {
                plan.insert(key.into(), json!(value));
            }
        }
    }
    Ok(json!({
        "billingCycleStartMs": epoch("billingCycleStart")?,
        "billingCycleEndMs": epoch("billingCycleEnd")?,
        "planUsage": plan,
    }))
}
