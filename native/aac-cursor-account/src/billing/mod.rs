//! Incremental account-wide Cursor Usage-tab reads. Only completed windows enter the cache.
mod api;
mod signin;

use api::{Client, HOUR, Window, iso};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

const DAY: i64 = 24 * HOUR;
const HISTORY: i64 = 31 * DAY;
const RECENT_OVERLAP: i64 = 6 * HOUR;
const HISTORY_PAGES: usize = 20;
const PASS_PAGES: usize = 40;
const MAX_SAVED: usize = 16 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
struct Checkpoint {
    from: i64,
    through: i64,
    read_at: Option<i64>,
    newest: Option<i64>,
    forward_span: i64,
    history_span: i64,
    hours: BTreeMap<i64, Cell>,
}

#[derive(Default, Deserialize, Serialize)]
struct Cell {
    rows: Vec<Value>,
    unmeasured: u64,
    #[serde(default)]
    unrecognized_models: u64,
}

impl Checkpoint {
    fn new(until: i64) -> Self {
        Self {
            from: until,
            through: until,
            read_at: None,
            newest: None,
            forward_span: DAY,
            history_span: DAY,
            hours: BTreeMap::new(),
        }
    }

    fn rewind(&mut self, until: i64) {
        // The hour containing the new end may contain rows from after it.
        let hour = until.div_euclid(HOUR) * HOUR;
        if hour <= self.from {
            *self = Self::new(until);
            return;
        }
        self.through = hour;
        self.hours.retain(|at, _| *at < hour);
        self.newest = self.newest.filter(|at| *at < hour);
        self.read_at = self.read_at.map(|at| at.min(until));
    }

    fn replace(&mut self, start: i64, end: i64, window: Window) {
        self.hours.retain(|hour, _| *hour < start || *hour >= end);
        for row in crate::api::hourly(&window.events) {
            if let Some(hour) = row["h"].as_str().and_then(crate::util::hour_epoch) {
                self.hours.entry(hour).or_default().rows.push(row);
            }
        }
        for (hour, count) in window.unmeasured {
            self.hours.entry(hour).or_default().unmeasured = count;
        }
        for (hour, count) in window.unrecognized_models {
            self.hours.entry(hour).or_default().unrecognized_models = count;
        }
        self.newest = self.newest.max(window.newest);
    }

    fn answer(&self, since: i64, until: i64, pages: usize) -> Value {
        let mut rows = Vec::new();
        let mut events = 0;
        let mut unmeasured = 0;
        let mut unrecognized_models = 0;
        for cell in self.hours.range(ceil_hour(since)..until).map(|(_, cell)| cell) {
            unmeasured += cell.unmeasured;
            unrecognized_models += cell.unrecognized_models;
            for row in &cell.rows {
                events += row["n"].as_u64().unwrap_or_default();
                rows.push(row);
            }
        }
        json!({
            "state": "read",
            "readAt": self.read_at,
            "coverageFrom": iso(self.from),
            "coverageUntil": iso(self.through),
            "newestItemAt": self.newest,
            "pages": pages,
            "events": events,
            "unmeasured": unmeasured,
            "unrecognizedModels": unrecognized_models,
            "rows": rows,
        })
    }
}

fn ceil_hour(ms: i64) -> i64 {
    ms.saturating_add(HOUR - 1).div_euclid(HOUR) * HOUR
}

fn unavailable(cause: &str) -> Value {
    json!({"state": "unavailable", "cause": cause})
}

fn test_base() -> Option<String> {
    let base = std::env::var("AAC_CURSOR_BILLING_BASE").ok()?;
    let port = base.strip_prefix("http://127.0.0.1:")?;
    (port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().ok()? != 0).then_some(base)
}

/// None delegates to the legacy API-key path. The loopback fixture can only read a synthetic root.
pub fn read(root: &Path, since: i64, until: i64, now: i64) -> Option<Value> {
    // The service explicitly opts in. Kits override this to 0 before launching the server.
    if std::env::var("AAC_CURSOR_BILLING_ENABLED").as_deref() != Ok("1") {
        return None;
    }
    let fixture_root = std::env::var_os("AAC_CURSOR_TEST_SIGNIN_DIR");
    let base = test_base();
    let test = if std::env::var_os("AAC_CURSOR_BILLING_BASE").is_some() || fixture_root.is_some() {
        base.as_ref()?;
        let test = std::path::PathBuf::from(fixture_root?);
        if test != root {
            return None;
        }
        Some(test)
    } else {
        if std::env::var_os("AAC_CURSOR_API_BASE").is_some() || std::env::var("NODE_ENV").as_deref() == Ok("test") {
            return None;
        }
        None
    };
    let session = match signin::load(test.as_deref(), now) {
        Ok(Some(session)) => session,
        Ok(None) => return None,
        Err(()) => return Some(unavailable("signin_needed")),
    };
    if since < 0 || until <= since || iso(until).is_none() || ceil_hour(since) >= until {
        return Some(unavailable("bad_response"));
    }
    let mut client = Client::new(base.as_deref(), &session.cookie);
    Some(collect(root, &session.account, since, until, now, &mut client).unwrap_or_else(unavailable))
}

fn store_error(error: rusqlite::Error) -> &'static str {
    match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => "read_in_progress",
        _ => "store_unsafe",
    }
}

fn open_store(root: &Path) -> Result<Connection, &'static str> {
    let cache = root.join("cache");
    fs::create_dir_all(&cache).map_err(|_| "store_unsafe")?;
    let file = cache.join("cursor-billing-v1.sqlite");
    let mut create = OpenOptions::new();
    create.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        create.mode(0o600);
    }
    match create.open(&file) {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
        Err(_) => return Err("store_unsafe"),
    }
    if !fs::symlink_metadata(&file).is_ok_and(|meta| meta.is_file()) {
        return Err("store_unsafe");
    }
    let db = Connection::open(file).map_err(store_error)?;
    db.busy_timeout(Duration::from_secs(2)).map_err(store_error)?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS observations (account TEXT PRIMARY KEY, state TEXT NOT NULL);")
        .map_err(store_error)?;
    Ok(db)
}

fn collect(
    root: &Path,
    account: &str,
    since: i64,
    until: i64,
    now: i64,
    client: &mut Client<'_>,
) -> Result<Value, &'static str> {
    let mut db = open_store(root)?;
    // One writer, including the request, so two observations cannot regress the watermark.
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(store_error)?;
    let saved: Option<String> = tx
        .query_row(
            "SELECT state FROM observations WHERE account = ?1 AND length(CAST(state AS BLOB)) <= ?2",
            params![account, MAX_SAVED],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_error)?;
    let oldest = ceil_hour(until.saturating_sub(HISTORY).max(0));
    let mut state = saved
        .as_deref()
        .and_then(|data| serde_json::from_str::<Checkpoint>(data).ok())
        .filter(|state| state.through > oldest)
        .unwrap_or_else(|| Checkpoint::new(until));

    if state.read_at.is_some() && until < state.through {
        state.rewind(until);
    }
    let result = advance(&mut state, oldest, until, now, client)?;
    let encoded = serde_json::to_string(&state).map_err(|_| "store_unsafe")?;
    if encoded.len() > MAX_SAVED {
        return Err("store_unsafe");
    }
    tx.execute(
        "INSERT INTO observations VALUES (?1, ?2)
         ON CONFLICT(account) DO UPDATE SET state = excluded.state",
        params![account, encoded],
    )
    .map_err(store_error)?;
    tx.commit().map_err(store_error)?;
    if result {
        Ok(state.answer(since, until, client.pages))
    } else {
        Err("backfill_pending")
    }
}

fn advance(
    state: &mut Checkpoint,
    oldest: i64,
    until: i64,
    now: i64,
    client: &mut Client<'_>,
) -> Result<bool, &'static str> {
    let first = state.read_at.is_none();
    let (start, end, page_limit) = if first {
        (
            (until.div_euclid(HOUR) * HOUR - state.forward_span).max(oldest),
            until,
            HISTORY_PAGES,
        )
    } else {
        let hour = state.through.div_euclid(HOUR) * HOUR;
        let start = (hour - RECENT_OVERLAP).max(state.from).max(oldest);
        (start, until.min(hour + state.forward_span), PASS_PAGES)
    };
    let Some(window) = client.window(start, end, page_limit)? else {
        // Retry a smaller set of whole hours. Never persist a paging token or an incomplete cell.
        state.forward_span = (state.forward_span / 2 / HOUR * HOUR).max(HOUR);
        return Ok(false);
    };
    state.replace(start, end, window);
    state.through = end;
    state.forward_span = DAY;
    state.from = if first { start } else { state.from.max(oldest) };
    state.hours.retain(|hour, _| *hour >= oldest);
    state.newest = state.newest.filter(|at| *at >= oldest);
    state.read_at = Some(now.min(until));
    if !first && end == until && state.from > oldest && client.pages < PASS_PAGES {
        let backfill_start = (state.from - state.history_span).max(oldest);
        let limit = (client.pages + HISTORY_PAGES).min(PASS_PAGES);
        if let Some(window) = client.window(backfill_start, state.from, limit)? {
            state.replace(backfill_start, state.from, window);
            state.from = backfill_start;
            state.history_span = DAY;
        } else {
            state.history_span = (state.history_span / 2 / HOUR * HOUR).max(HOUR);
        }
    }
    Ok(true)
}
