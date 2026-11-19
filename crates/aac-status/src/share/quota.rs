//! The quota report (`quota-report.ts`): quota-watch's share copy at
//! `<reports>/v1/quota/report.json` (`quota-report@1`), read with the agent-reports rules and
//! projected to the sections the Status tab shows. One writer rewrites it whole.

use super::{
    ShareFs, TransientMisses, is_transient, stat_key, MISSING_GRACE_MS, PERSISTENT_SHARE_ERROR,
};
use crate::validate::{
    self, Invalid, Valid, count, fail, iso, list, obj, one_of, opt_iso, opt_text, text,
};
use serde_json::{Map, Value as Json, json};

pub const QUOTA_REPORT_SCHEMA: &str = "quota-report@1";
pub const QUOTA_REPORT_CAP: usize = 512 * 1024;

/// A finite number, or null when the report gives none (`num`).
fn num(value: Option<&Json>, field: &str) -> Valid<Option<f64>> {
    match value {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Number(number)) => Ok(number.as_f64()),
        _ => Err(fail(field)),
    }
}

/// A 0..1 share of a period (`share`).
fn share(value: Option<&Json>, field: &str) -> Valid<Option<f64>> {
    let n = num(value, field)?;
    if let Some(n) = n {
        if !(0.0..=1.0).contains(&n) {
            return Err(fail(field));
        }
    }
    Ok(n)
}

fn unit(value: Option<&Json>, field: &str) -> Valid<String> {
    one_of(value, field, &["credits", "percent"])
}

/// `estimate: false` only when the report says so; otherwise an estimate.
fn estimate(value: Option<&Json>) -> bool {
    value != Some(&Json::Bool(false))
}

fn period(value: Option<&Json>, field: &str) -> Valid<Json> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(Json::Null);
    };
    let p = obj(Some(value), field)?;
    Ok(json!({
        "period": opt_text(p.get("period"), &format!("{field}.period"), 80)?,
        "start": opt_iso(p.get("start"), &format!("{field}.start"))?,
        "end": opt_iso(p.get("end"), &format!("{field}.end"))?,
        "coverage": share(p.get("coverage"), &format!("{field}.coverage"))?,
        "rows": list(p.get("rows"), &format!("{field}.rows"), 2000, |row, at| {
            let r = obj(Some(row), at)?;
            Ok(json!({
                "stream": text(r.get("stream"), &format!("{at}.stream"), 120)?,
                "account": text(r.get("account"), &format!("{at}.account"), 120)?,
                "meter": text(r.get("meter"), &format!("{at}.meter"), 80)?,
                "unit": unit(r.get("unit"), &format!("{at}.unit"))?,
                "value": num(r.get("value"), &format!("{at}.value"))?,
                "estimate": estimate(r.get("estimate")),
            }))
        })?,
    }))
}

/// The projected report (`project`): the sections the Status tab shows, in order.
pub fn project(raw: &Json) -> Valid<Json> {
    let doc = obj(Some(raw), "document")?;
    if doc.get("schema").and_then(Json::as_str) != Some(QUOTA_REPORT_SCHEMA) {
        return Err(fail("schema"));
    }
    let writer = doc.get("writer").and_then(Json::as_str).unwrap_or_default();
    if !crate::validate::is_writer_name(writer) {
        return Err(fail("writer"));
    }
    if doc.get("estimate") != Some(&Json::Bool(true)) {
        return Err(fail("estimate"));
    }
    let stream_estimates = match doc.get("streamEstimates") {
        None | Some(Json::Null) => Map::new(),
        other => obj(other, "streamEstimates")?.clone(),
    };
    Ok(json!({
        "writer": writer,
        "seq": count(doc.get("seq"), "seq")?,
        "writtenAt": iso(doc.get("writtenAt"), "writtenAt")?,
        "generatedAt": opt_iso(doc.get("generatedAt"), "generatedAt")?,
        "methodId": opt_text(doc.get("methodId"), "methodId", 80)?,
        "method": opt_text(doc.get("method"), "method", 2000)?,
        "periods": {
            "day": period(stream_estimates.get("day"), "streamEstimates.day")?,
            "week": period(stream_estimates.get("week"), "streamEstimates.week")?,
            "window": period(stream_estimates.get("window"), "streamEstimates.window")?,
        },
        "budgets": list(doc.get("budgetRows"), "budgetRows", 500, |row, at| {
            let r = obj(Some(row), at)?;
            let known = match r.get("known") {
                Some(Json::Bool(known)) => *known,
                _ => return Err(fail(&format!("{at}.known"))),
            };
            Ok(json!({
                "stream": text(r.get("stream"), &format!("{at}.stream"), 120)?,
                "desc": opt_text(r.get("desc"), &format!("{at}.desc"), 200)?,
                "accounts": list(r.get("accounts"), &format!("{at}.accounts"), 50, |a, w| {
                    text(Some(a), w, 120)
                })?,
                "unit": unit(r.get("unit"), &format!("{at}.unit"))?,
                "meter": text(r.get("meter"), &format!("{at}.meter"), 80)?,
                "per": opt_text(r.get("per"), &format!("{at}.per"), 40)?,
                "limit": num(r.get("limit"), &format!("{at}.limit"))?,
                "use": num(r.get("use"), &format!("{at}.use"))?,
                "known": known,
                "percent": num(r.get("percent"), &format!("{at}.percent"))?,
                "period": opt_text(r.get("period"), &format!("{at}.period"), 80)?,
                "coverage": share(r.get("coverage"), &format!("{at}.coverage"))?,
                "estimate": estimate(r.get("estimate")),
            }))
        })?,
        "unattributed": list(doc.get("unattributedRows"), "unattributedRows", 500, |row, at| {
            let r = obj(Some(row), at)?;
            Ok(json!({
                "account": text(r.get("account"), &format!("{at}.account"), 120)?,
                "meter": text(r.get("meter"), &format!("{at}.meter"), 80)?,
                "unit": match r.get("unit") {
                    None | Some(Json::Null) => Json::Null,
                    other => json!(unit(other, &format!("{at}.unit"))?),
                },
                "amount": num(r.get("amount"), &format!("{at}.amount"))?,
                "reason": text(r.get("reason"), &format!("{at}.reason"), 120)?,
                "why": opt_text(r.get("why"), &format!("{at}.why"), 500)?,
                "estimate": estimate(r.get("estimate")),
            }))
        })?,
        "machinesNotScanned": list(doc.get("machinesNotScanned"), "machinesNotScanned", 20, |row, at| {
            let r = obj(Some(row), at)?;
            Ok(json!({
                "name": text(r.get("name"), &format!("{at}.name"), 80)?,
                "thisPass": match r.get("thisPass") {
                    Some(Json::Bool(this_pass)) => json!(this_pass),
                    _ => Json::Null,
                },
                "error": opt_text(r.get("error"), &format!("{at}.error"), 500)?,
                "runsMissed": match r.get("runsMissed") {
                    None | Some(Json::Null) => Json::Null,
                    other => json!(count(other, &format!("{at}.runsMissed"))?),
                },
                "runsInWindow": match r.get("runsInWindow") {
                    None | Some(Json::Null) => Json::Null,
                    other => json!(count(other, &format!("{at}.runsInWindow"))?),
                },
            }))
        })?,
    }))
}

/// Bytes of report.json -> the projected report and its digest, or a fixed reason.
pub fn parse_quota_report(bytes: &[u8]) -> Result<(Json, String), String> {
    let raw = validate::json_from_bytes(bytes, QUOTA_REPORT_CAP)?;
    match project(&raw) {
        Ok(doc) => Ok((doc, super::sha256_hex(bytes))),
        Err(Invalid(reason)) => Err(reason),
    }
}

/// The last good copy and when AAC first saw that version.
#[derive(Clone, Debug)]
pub struct QuotaGood {
    pub doc: Json,
    pub sha256: String,
    pub first_seen_at: u64,
    pub written_at_ms: i64,
}

#[derive(Clone, Debug)]
pub struct QuotaProblem {
    pub reason: String,
    pub since: u64,
}

#[derive(Clone, Debug)]
pub struct QuotaReportState {
    pub state: &'static str,
    pub scanned_at: Option<u64>,
    pub good: Option<QuotaGood>,
    pub problem: Option<QuotaProblem>,
}

/// The one file this reader reads, as `TransientMisses` names it.
const REPORT: &str = "quota/report.json";

pub struct QuotaReportReader {
    root: std::path::PathBuf,
    good: Option<QuotaGood>,
    problem: Option<QuotaProblem>,
    state: &'static str,
    scanned_at: Option<u64>,
    stat_key: String,
    missing_since: Option<u64>,
    misses: TransientMisses,
}

impl QuotaReportReader {
    pub fn new(root: std::path::PathBuf) -> Self {
        Self {
            root,
            good: None,
            problem: None,
            state: "missing",
            scanned_at: None,
            stat_key: String::new(),
            missing_since: None,
            misses: TransientMisses::default(),
        }
    }

    fn set_problem(&mut self, reason: &str, since: u64) {
        if self.problem.as_ref().map(|p| p.reason.as_str()) != Some(reason) {
            self.problem = Some(QuotaProblem {
                reason: reason.to_string(),
                since,
            });
        }
    }

    /// No report listed: keep the last good copy through the rename gap, then report missing.
    fn absent(&mut self, now: u64) {
        self.misses.clear(REPORT);
        let missing_since = *self.missing_since.get_or_insert(now);
        if self.good.is_none() || now.saturating_sub(missing_since) >= MISSING_GRACE_MS {
            self.good = None;
            self.problem = None;
            self.stat_key = String::new();
            self.state = "missing";
        }
    }

    /// A failed stat or read of the listed report: the race once, a named problem twice.
    fn failed_read(&mut self, error: &std::io::Error, now: u64) {
        if !is_transient(error) {
            self.misses.clear(REPORT);
            self.set_problem("could not be read", now);
            return;
        }
        if let Some(since) = self.misses.miss(REPORT, now) {
            // Read again on the first scan that can, whatever the stat says then.
            self.stat_key = String::new();
            self.set_problem(PERSISTENT_SHARE_ERROR, since);
        }
    }

    /// One rescan; resolves whether anything the view shows changed (state, digest, reason).
    pub fn scan(&mut self, fs: &dyn ShareFs, now: u64) -> bool {
        let before = (
            self.state,
            self.good.as_ref().map(|good| good.sha256.clone()),
            self.problem.as_ref().map(|problem| problem.reason.clone()),
        );
        let folder = self.root.join("v1").join("quota");
        let file = folder.join("report.json");
        let names = match fs.list(&folder) {
            Ok(entries) => entries.into_iter().map(|entry| entry.name).collect::<Vec<_>>(),
            Err(error) => {
                if error.kind() == std::io::ErrorKind::NotFound {
                    self.absent(now);
                } else if is_transient(&error) {
                    // A stale handle: keep everything and try again next scan.
                } else {
                    self.state = "error";
                }
                self.scanned_at = Some(now);
                return self.changed_since(before);
            }
        };
        if !names.iter().any(|name| name == "report.json") {
            self.absent(now);
            self.scanned_at = Some(now);
            return self.changed_since(before);
        }
        self.missing_since = None;
        self.state = "ok";
        let stat = match fs.stat(&file) {
            Ok(stat) => stat,
            Err(error) => {
                self.failed_read(&error, now);
                self.scanned_at = Some(now);
                return self.changed_since(before);
            }
        };
        if !stat.file {
            self.misses.clear(REPORT);
            self.stat_key = String::new();
            self.set_problem("is not a regular file", now);
            self.scanned_at = Some(now);
            return self.changed_since(before);
        }
        let key = stat_key(&stat);
        if key == self.stat_key {
            self.misses.clear(REPORT);
            self.scanned_at = Some(now);
            return self.changed_since(before);
        }
        if stat.size > QUOTA_REPORT_CAP as u64 {
            self.stat_key = key;
            self.set_problem("larger than the 512 KB cap", now);
            self.scanned_at = Some(now);
            return self.changed_since(before);
        }
        let bytes = match fs.read(&file, QUOTA_REPORT_CAP) {
            Ok(bytes) => bytes,
            Err(super::ReadError::OverCap) => {
                self.misses.clear(REPORT);
                self.stat_key = key;
                self.set_problem("larger than the 512 KB cap", now);
                self.scanned_at = Some(now);
                return self.changed_since(before);
            }
            Err(super::ReadError::Io(error)) => {
                self.failed_read(&error, now);
                self.scanned_at = Some(now);
                return self.changed_since(before);
            }
        };
        self.misses.clear(REPORT);
        self.stat_key = key;
        let (doc, sha256) = match parse_quota_report(&bytes) {
            Ok(parsed) => parsed,
            Err(reason) => {
                self.set_problem(&reason, now);
                self.scanned_at = Some(now);
                return self.changed_since(before);
            }
        };
        self.problem = None;
        // An older write landing late: ignored.
        let seq = doc["seq"].as_u64().unwrap_or(0);
        if let Some(good) = &self.good {
            if seq < good.doc["seq"].as_u64().unwrap_or(0) {
                self.scanned_at = Some(now);
                return self.changed_since(before);
            }
        }
        if self.good.as_ref().map(|good| good.sha256.as_str()) != Some(sha256.as_str()) {
            self.good = Some(QuotaGood {
                written_at_ms: aac_num::ecma::parse_instant(
                    doc["writtenAt"].as_str().unwrap_or_default(),
                )
                .unwrap_or(0),
                doc,
                sha256,
                first_seen_at: now,
            });
        }
        self.scanned_at = Some(now);
        self.changed_since(before)
    }

    fn changed_since(
        &self,
        before: (&'static str, Option<String>, Option<String>),
    ) -> bool {
        (
            self.state,
            self.good.as_ref().map(|good| good.sha256.clone()),
            self.problem.as_ref().map(|problem| problem.reason.clone()),
        ) != before
    }

    pub fn snapshot(&self) -> QuotaReportState {
        QuotaReportState {
            state: self.state,
            scanned_at: self.scanned_at,
            good: self.good.clone(),
            problem: self.problem.clone(),
        }
    }
}

/// The snapshot as the Status envelope's `quota` member.
pub fn state_json(state: &QuotaReportState) -> Json {
    let good = match &state.good {
        None => Json::Null,
        Some(good) => json!({
            "doc": good.doc,
            "sha256": good.sha256,
            "firstSeenAt": good.first_seen_at,
            "writtenAtMs": good.written_at_ms,
        }),
    };
    let problem = match &state.problem {
        None => Json::Null,
        Some(problem) => json!({"reason": problem.reason, "since": problem.since}),
    };
    json!({
        "state": state.state,
        "scannedAt": state.scanned_at,
        "good": good,
        "problem": problem,
    })
}
