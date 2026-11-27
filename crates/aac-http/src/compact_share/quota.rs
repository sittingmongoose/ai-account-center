//! Compact quota-report reader (`v1/quota/report.json`, `quota-report@1`).
//!
//! Stores the projected quota document as compact `js_text` UTF-8 bytes plus `[u8; 32]`
//! digest and numeric `StatStamp` instead of retaining a `serde_json::Value` DOM tree
//! inside the reader.

use super::primitives::{CompactSha, CompactTransientMisses, StatStamp};
use aac_status::share::quota::{QUOTA_REPORT_CAP, parse_quota_report};
use aac_status::share::{
    MISSING_GRACE_MS, PERSISTENT_SHARE_ERROR, ReadError, ShareFs, is_transient, js_text,
};
use serde_json::{Value as Json, json};
use std::path::PathBuf;

const REPORT: &str = "quota/report.json";

#[derive(Clone, Debug)]
struct CompactQuotaGood {
    doc_json: Box<[u8]>,
    seq: u64,
    sha256: CompactSha,
    first_seen_at: u64,
    written_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompactQuotaProblem {
    reason: Box<str>,
    since: u64,
}

pub(crate) struct CompactQuotaReader {
    root: PathBuf,
    good: Option<CompactQuotaGood>,
    problem: Option<CompactQuotaProblem>,
    state: &'static str,
    scanned_at: Option<u64>,
    stamp: Option<StatStamp>,
    missing_since: Option<u64>,
    misses: CompactTransientMisses,
}

impl CompactQuotaReader {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            good: None,
            problem: None,
            state: "missing",
            scanned_at: None,
            stamp: None,
            missing_since: None,
            misses: CompactTransientMisses::default(),
        }
    }

    fn set_problem(&mut self, reason: &str, since: u64) {
        if self.problem.as_ref().map(|p| p.reason.as_ref()) != Some(reason) {
            self.problem = Some(CompactQuotaProblem {
                reason: reason.into(),
                since,
            });
        }
    }

    fn absent(&mut self, now: u64) {
        self.misses.clear(REPORT);
        let missing_since = *self.missing_since.get_or_insert(now);
        if self.good.is_none() || now.saturating_sub(missing_since) >= MISSING_GRACE_MS {
            self.good = None;
            self.problem = None;
            self.stamp = None;
            self.state = "missing";
        }
    }

    fn failed_read(&mut self, error: &std::io::Error, now: u64) {
        if !is_transient(error) {
            self.misses.clear(REPORT);
            self.set_problem("could not be read", now);
            return;
        }
        if let Some(since) = self.misses.miss(REPORT, now) {
            self.stamp = None;
            self.set_problem(PERSISTENT_SHARE_ERROR, since);
        }
    }

    pub(crate) fn scan(&mut self, fs: &dyn ShareFs, now: u64) -> bool {
        let before = (
            self.state,
            self.good.as_ref().map(|g| g.sha256.clone()),
            self.problem.as_ref().map(|p| p.reason.clone()),
        );
        let folder = self.root.join("v1").join("quota");
        let file = folder.join("report.json");
        let names = match fs.list(&folder) {
            Ok(entries) => entries
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>(),
            Err(error) => {
                if error.kind() == std::io::ErrorKind::NotFound {
                    self.absent(now);
                } else if is_transient(&error) {
                    // Stale handle: keep everything and retry next scan.
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
            self.stamp = None;
            self.set_problem("is not a regular file", now);
            self.scanned_at = Some(now);
            return self.changed_since(before);
        }
        let stamp = StatStamp::from_stat(&stat);
        if self.stamp == Some(stamp) {
            self.misses.clear(REPORT);
            self.scanned_at = Some(now);
            return self.changed_since(before);
        }
        if stat.size > QUOTA_REPORT_CAP as u64 {
            self.stamp = Some(stamp);
            self.set_problem("larger than the 512 KB cap", now);
            self.scanned_at = Some(now);
            return self.changed_since(before);
        }
        let bytes = match fs.read(&file, QUOTA_REPORT_CAP) {
            Ok(bytes) => bytes,
            Err(ReadError::OverCap) => {
                self.misses.clear(REPORT);
                self.stamp = Some(stamp);
                self.set_problem("larger than the 512 KB cap", now);
                self.scanned_at = Some(now);
                return self.changed_since(before);
            }
            Err(ReadError::Io(error)) => {
                self.failed_read(&error, now);
                self.scanned_at = Some(now);
                return self.changed_since(before);
            }
        };
        self.misses.clear(REPORT);
        self.stamp = Some(stamp);
        let (doc, sha_hex) = match parse_quota_report(&bytes) {
            Ok(parsed) => parsed,
            Err(reason) => {
                self.set_problem(&reason, now);
                self.scanned_at = Some(now);
                return self.changed_since(before);
            }
        };
        self.problem = None;
        let seq = doc["seq"].as_u64().unwrap_or(0);
        if let Some(good) = &self.good
            && seq < good.seq
        {
            self.scanned_at = Some(now);
            return self.changed_since(before);
        }
        let sha = CompactSha::from_hex_or_str(&sha_hex);
        if self.good.as_ref().map(|g| &g.sha256) != Some(&sha) {
            let written_at_ms =
                aac_num::ecma::parse_instant(doc["writtenAt"].as_str().unwrap_or_default())
                    .unwrap_or(0);
            let doc_json = js_text(&doc).into_bytes().into_boxed_slice();
            self.good = Some(CompactQuotaGood {
                doc_json,
                seq,
                sha256: sha,
                first_seen_at: now,
                written_at_ms,
            });
        }
        self.scanned_at = Some(now);
        self.changed_since(before)
    }

    fn changed_since(&self, before: (&'static str, Option<CompactSha>, Option<Box<str>>)) -> bool {
        (
            self.state,
            self.good.as_ref().map(|g| g.sha256.clone()),
            self.problem.as_ref().map(|p| p.reason.clone()),
        ) != before
    }

    pub(crate) fn state_json(&self) -> Json {
        let good = match &self.good {
            None => Json::Null,
            Some(good) => {
                let doc: Json = serde_json::from_slice(&good.doc_json).unwrap_or(Json::Null);
                json!({
                    "doc": doc,
                    "sha256": good.sha256.to_hex_string(),
                    "firstSeenAt": good.first_seen_at,
                    "writtenAtMs": good.written_at_ms,
                })
            }
        };
        let problem = match &self.problem {
            None => Json::Null,
            Some(problem) => json!({"reason": problem.reason.as_ref(), "since": problem.since}),
        };
        json!({
            "state": self.state,
            "scannedAt": self.scanned_at,
            "good": good,
            "problem": problem,
        })
    }
}
