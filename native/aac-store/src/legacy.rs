//! One-time imports of retired caches that still hold usage no log has any more (docs/store.md 12.8).
//!
//! `cache/codex-native-usage-v1.json` (version 1, written until 2026-10-01) kept every Codex usage
//! record the old collector read, per rollout file. Most of those rollout files are gone, so it is the
//! only copy of that history. Its `inputTokens` include the cached input (the old parser did not
//! subtract `cached_input_tokens`), so uncached input is `inputTokens - cacheReadTokens -
//! cacheCreationTokens`. The file is read as a stream, one rollout file's records at a time, and
//! folded into session-hour cells; nothing but the cells is kept in memory. It holds no cost: the
//! cells carry none and the server prices them when it answers (contract `costUsd` null).

use crate::activity;
use crate::contract::{self, Report, Session, SessionPart};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::fail;
use serde::Deserialize;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

const HOUR_MS: u64 = contract::HOUR_MS;
/// Larger files are refused: the known one is 145 MB.
const MAX_FILE_BYTES: u64 = 1 << 30;
const MAX_RECORDS: u64 = 20_000_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(default)]
    input_tokens: f64,
    #[serde(default)]
    output_tokens: f64,
    #[serde(default)]
    cache_creation_tokens: f64,
    #[serde(default)]
    cache_read_tokens: f64,
    #[serde(default)]
    model: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    timestamp: String,
}

#[derive(Deserialize)]
struct FileRecords {
    #[serde(default)]
    entries: Vec<Record>,
}

#[derive(Default, Clone, Copy)]
struct Tokens {
    i: u64,
    o: u64,
    cw: u64,
    cr: u64,
    events: u64,
}

#[derive(Default)]
struct Fold {
    /// (hour index, session key, model) to totals.
    cells: BTreeMap<(u64, String, String), Tokens>,
    /// session key to (full hash, first ms, last ms, model to totals).
    sessions: BTreeMap<String, (String, u64, u64, BTreeMap<String, Tokens>)>,
    records: u64,
    skipped: u64,
    version: Option<u64>,
    /// Rollout files the cache lists.
    files: u64,
}

fn count(v: f64) -> u64 {
    if v.is_finite() && v > 0.0 { v.floor() as u64 } else { 0 }
}

impl Fold {
    fn add(&mut self, r: Record) {
        self.records += 1;
        let Some(at) = contract::parse_iso(&r.timestamp) else {
            self.skipped += 1;
            return;
        };
        let model = r.model.trim();
        if r.session_id.is_empty() || model.is_empty() || model.len() > 200 || !contract::label_ok(model, 200) {
            self.skipped += 1;
            return;
        }
        let (cr, cw) = (count(r.cache_read_tokens), count(r.cache_creation_tokens));
        let t = Tokens {
            // the old collector counted cached input inside input
            i: count(r.input_tokens).saturating_sub(cr).saturating_sub(cw),
            o: count(r.output_tokens),
            cw,
            cr,
            events: 1,
        };
        if t.i + t.o + t.cw + t.cr == 0 {
            self.skipped += 1;
            return;
        }
        let hash: String = Sha256::digest(format!("aac-session-v1:codex:{}", r.session_id).as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let key = hash[..16].to_string();
        let cell = self
            .cells
            .entry((at / HOUR_MS, key.clone(), model.to_string()))
            .or_default();
        cell.i += t.i;
        cell.o += t.o;
        cell.cw += t.cw;
        cell.cr += t.cr;
        cell.events += 1;
        let s = self
            .sessions
            .entry(key)
            .or_insert_with(|| (hash, at, at, BTreeMap::new()));
        s.1 = s.1.min(at);
        s.2 = s.2.max(at);
        let part = s.3.entry(model.to_string()).or_default();
        part.i += t.i;
        part.o += t.o;
        part.cw += t.cw;
        part.cr += t.cr;
        part.events += 1;
    }
}

struct FilesSeed<'a>(&'a mut Fold);

impl<'de> DeserializeSeed<'de> for FilesSeed<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for FilesSeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a map of rollout files")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        while map.next_key::<IgnoredAny>()?.is_some() {
            let file: FileRecords = map.next_value()?;
            self.0.files += 1;
            for record in file.entries {
                if self.0.records >= MAX_RECORDS {
                    return Err(serde::de::Error::custom("too many records"));
                }
                self.0.add(record);
            }
        }
        Ok(())
    }
}

struct TopSeed<'a>(&'a mut Fold);

impl<'de> DeserializeSeed<'de> for TopSeed<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for TopSeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("the codex-native-usage-v1 cache")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "version" => self.0.version = map.next_value::<Option<u64>>()?,
                "files" => map.next_value_seed(FilesSeed(&mut *self.0))?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

/// Imports `codex-native-usage-v1.json` once (the store remembers the file's SHA-256): its records as
/// session-hour cells and sessions of the computer that wrote them, Codex, added or raised only.
pub fn import_codex_v1(engine: &Engine, file: &Path, computer: &str) -> Result<Json> {
    if !contract::ident_ok(computer) {
        fail!("invalid", "computer is a lower-case identifier");
    }
    let meta = std::fs::symlink_metadata(file).map_err(|e| Error::new("not_found", e.to_string()))?;
    if !meta.file_type().is_file() || meta.len() > MAX_FILE_BYTES {
        fail!("invalid", "the cache is not a regular file of a readable size");
    }
    let (sha, _) = crate::fsx::sha256_file(file)?;
    let marker = format!("ts:activity_import_codex_v1:{}", &sha[..32]);
    if engine.get_meta(&marker)?.is_some() {
        return Ok(json!({ "imported": false, "reason": "already imported" }));
    }
    let mut fold = Fold::default();
    {
        let reader = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(file)?);
        let mut de = serde_json::Deserializer::from_reader(reader);
        TopSeed(&mut fold)
            .deserialize(&mut de)
            .map_err(|e| Error::new("invalid", format!("the cache could not be read: {e}")))?;
        de.end().map_err(|e| Error::new("invalid", e.to_string()))?;
    }
    if fold.version != Some(1) {
        fail!("unsupported", "only version 1 of the Codex cache is known");
    }
    let iso = |ms: u64| crate::usage::iso(ms as i64);
    let newest = fold.cells.keys().map(|k| k.0).max().unwrap_or(0);
    let read_at = iso((newest + 1) * HOUR_MS);
    let mut reports = Vec::new();
    let mut cells = Vec::new();
    for ((hour, session, model), t) in &fold.cells {
        cells.push(contract::Cell {
            hour: iso(hour * HOUR_MS),
            session_key: Some(session.clone()),
            session_hash: fold.sessions.get(session).map(|s| s.0.clone()),
            route: String::new(),
            model_raw: model.clone(),
            input_tokens: t.i,
            output_tokens: t.o,
            cache_creation_tokens: t.cw,
            cache_read_tokens: t.cr,
            events: Some(t.events),
            ..Default::default()
        });
    }
    let sessions: Vec<Session> = fold
        .sessions
        .iter()
        .map(|(key, (hash, first, last, parts))| Session {
            session_key: key.clone(),
            session_hash: Some(hash.clone()),
            first: iso(*first),
            last: iso(*last),
            parts: parts
                .iter()
                .take(contract::MAX_PARTS)
                .map(|(model, t)| SessionPart {
                    route: String::new(),
                    model_raw: model.clone(),
                    input_tokens: t.i,
                    output_tokens: t.o,
                    cache_creation_tokens: t.cw,
                    cache_read_tokens: t.cr,
                    ..Default::default()
                })
                .collect(),
            target: Some("codex".to_string()),
            cwd: None,
        })
        .collect();
    // within the contract's bounds per report
    let mut cell_chunks = cells
        .chunks(contract::MAX_CELLS / 2)
        .map(|c| c.to_vec())
        .collect::<Vec<_>>();
    let session_chunks = sessions
        .chunks(contract::MAX_SESSIONS / 2)
        .map(|c| c.to_vec())
        .collect::<Vec<_>>();
    let parts = cell_chunks.len().max(session_chunks.len()).max(1);
    cell_chunks.resize(parts, Vec::new());
    for (i, chunk) in cell_chunks.into_iter().enumerate() {
        reports.push(Report {
            schema: contract::REPORT_SCHEMA.to_string(),
            computer: computer.to_string(),
            tool: "codex".to_string(),
            read_at: read_at.clone(),
            state: "partial".to_string(),
            // the source label: a retired cache, its file count, never a live read
            cause: Some("retired_cache".to_string()),
            counts: Some(contract::Counts {
                files_found: Some(fold.files),
                files_read: Some(fold.files),
                ..Default::default()
            }),
            cells: chunk,
            sessions: session_chunks.get(i).cloned().unwrap_or_default(),
            kind: Some("import".to_string()),
            ..Default::default()
        });
    }
    let mut put = 0;
    let mut refused = Vec::new();
    for report in &reports {
        let r = activity::ingest(engine, std::slice::from_ref(report))?;
        put += r.put;
        refused.extend(r.refused);
    }
    if refused.is_empty() {
        engine.set_meta(&marker, &fold.records.to_string())?;
    }
    Ok(json!({
        "imported": refused.is_empty(), "records": fold.records, "skipped": fold.skipped, "files": fold.files,
        "cells": cells.len(), "sessions": sessions.len(), "put": put, "refused": refused,
        "oldestMs": fold.cells.keys().map(|k| k.0 * HOUR_MS).min(), "newestMs": fold.cells.keys().map(|k| k.0 * HOUR_MS).max(),
    }))
}
