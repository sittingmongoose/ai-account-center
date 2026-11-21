use crate::api::Event;
use chrono::{DateTime, SecondsFormat};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::time::{Duration, Instant};

pub(super) const HOUR: i64 = 3_600_000;
const MAX_BODY: u64 = 8 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);
const PASS_TIME: Duration = Duration::from_secs(105);
const PAGE_SIZE: usize = 500;

pub(super) fn iso(ms: i64) -> Option<String> {
    Some(DateTime::from_timestamp_millis(ms)?.to_rfc3339_opts(SecondsFormat::Millis, true))
}

pub(super) struct Window {
    pub events: Vec<Event>,
    pub unmeasured: BTreeMap<i64, u64>,
    pub unrecognized_models: BTreeMap<i64, u64>,
    pub newest: Option<i64>,
}

pub(super) struct Client<'a> {
    agent: ureq::Agent,
    url: String,
    cookie: &'a str,
    started: Instant,
    pub pages: usize,
}

impl<'a> Client<'a> {
    pub fn new(base: Option<&str>, cookie: &'a str) -> Self {
        Self {
            agent: ureq::AgentBuilder::new().redirects(0).timeout(TIMEOUT).build(),
            url: format!(
                "{}/api/dashboard/v2/query-billing-items",
                base.unwrap_or("https://cursor.com")
            ),
            cookie,
            started: Instant::now(),
            pages: 0,
        }
    }

    /// A capped window has no publishable cells. A completed window can replace whole hours.
    pub fn window(&mut self, start: i64, end: i64, page_limit: usize) -> Result<Option<Window>, &'static str> {
        let begin = iso(start).ok_or("bad_response")?;
        let finish = iso(end).ok_or("bad_response")?;
        let mut window = Window {
            events: Vec::new(),
            unmeasured: BTreeMap::new(),
            unrecognized_models: BTreeMap::new(),
            newest: None,
        };
        let mut ids = HashSet::new();
        let mut cursors = HashSet::new();
        let mut cursor: Option<String> = None;
        while self.pages < page_limit && self.started.elapsed() + TIMEOUT <= PASS_TIME {
            self.pages += 1;
            let mut request = self
                .agent
                .get(&self.url)
                .set("Cookie", self.cookie)
                .set("Accept", "application/json")
                .set("Referer", "https://cursor.com/dashboard?tab=usage")
                .set(
                    "User-Agent",
                    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36",
                )
                .query("scope", "user")
                .query("product", "cursor")
                .query("pageSize", "500")
                .query("startTime", &begin)
                .query("endTime", &finish);
            if let Some(cursor) = &cursor {
                request = request.query("cursor", cursor);
            }
            let response = match request.call() {
                Ok(response) if response.status() == 200 => response,
                Ok(response) if (300..400).contains(&response.status()) => return Err("signin_needed"),
                Err(ureq::Error::Status(401 | 403, _)) => return Err("signin_needed"),
                Err(ureq::Error::Status(500..=599, _)) => return Err("server_error"),
                Err(ureq::Error::Transport(_)) => return Err("network"),
                _ => return Err("bad_response"),
            };
            let mut bytes = Vec::new();
            response
                .into_reader()
                .take(MAX_BODY + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "network")?;
            if bytes.len() as u64 > MAX_BODY {
                return Err("bad_response");
            }
            let page: Value = serde_json::from_slice(&bytes).map_err(|_| "bad_response")?;
            let data = page["data"]
                .as_array()
                .filter(|data| data.len() <= PAGE_SIZE)
                .ok_or("bad_response")?;
            let paging = page["pagination"].as_object().ok_or("bad_response")?;
            let more = paging.get("hasMore").and_then(Value::as_bool).ok_or("bad_response")?;
            for item in data {
                if !item.is_object() || !item["product"].is_string() || !item["chargeCategory"].is_string() {
                    return Err("bad_response");
                }
                if item["product"] != "cursor" || item["chargeCategory"] != "usage" {
                    continue;
                }
                let at = item["eventTime"]
                    .as_str()
                    .and_then(|stamp| stamp.parse::<DateTime<chrono::FixedOffset>>().ok())
                    .ok_or("bad_response")?
                    .timestamp_millis();
                if at < start || at >= end {
                    continue;
                }
                let id = item["billingItemId"]
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 1024)
                    .ok_or("bad_response")?;
                if !ids.insert(id.to_owned()) {
                    continue;
                }
                window.newest = window.newest.max(Some(at));
                let hour = at.div_euclid(HOUR) * HOUR;
                if let Some((event, recognized)) = measured(item, at) {
                    if !recognized {
                        *window.unrecognized_models.entry(hour).or_default() += 1;
                    }
                    window.events.push(event);
                } else {
                    *window.unmeasured.entry(hour).or_default() += 1;
                }
            }
            let next = paging
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|next| !next.is_empty());
            if !more {
                return Ok(Some(window));
            }
            let Some(next) = next else { return Ok(Some(window)) };
            if next.len() > 4096 || !cursors.insert(next.to_owned()) {
                return Err("bad_response");
            }
            cursor = Some(next.to_owned());
        }
        Ok(None)
    }
}

fn model(value: &Value) -> (String, bool) {
    let model = value.as_str().unwrap_or_default();
    if model == "Claude 4.5 Sonnet (Thinking)" {
        return ("claude-sonnet-4-5".into(), true);
    }
    // Keep provider ids intact for the existing model matcher, including effort.
    if model.len() <= 200
        && model.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._:/".contains(&b))
    {
        return (model.to_owned(), true);
    }
    if model.len() <= 200 && !model.contains('@') {
        let slug = model
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("-")
            .to_ascii_lowercase();
        if !slug.is_empty() {
            return (slug, false);
        }
    }
    ("cursor-unknown-model".into(), false)
}

fn measured(item: &Value, at_ms: i64) -> Option<(Event, bool)> {
    let usage = item.get("usage")?.as_object()?;
    let mut tokens = [0; 4];
    for (count, key) in tokens.iter_mut().zip([
        "prompt-text-tokens",
        "completion-text-tokens",
        "cached-prompt-text-tokens",
        "cache-write-text-tokens",
    ]) {
        let value = usage.get(key)?;
        *count = value.as_u64().or_else(|| value.as_str()?.parse().ok())?;
        // The existing hourly wire format uses JavaScript integers. Keep a whole
        // 40-page sweep exact even if every item lands in one hour and model.
        if *count > 9_007_199_254_740_991 / (40 * PAGE_SIZE as u64 * 4) {
            return None;
        }
    }
    let (model, recognized) = model(&item["displayExtras"]["model"]);
    Some((
        Event {
            at_ms,
            model,
            session: None,
            tokens,
            cents: None,
        },
        recognized,
    ))
}
