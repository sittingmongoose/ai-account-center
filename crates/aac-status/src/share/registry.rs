//! The pm-mail registry reader (`PmMailRegistryReader` in status-collectors.ts): one JSON
//! file per registered thread in the inbox registry folder, read-only, every 30 s. A file
//! that is JSON but no thread entry is not a problem: pm-mail keeps other files here.

use super::{PERSISTENT_SHARE_ERROR, ShareFs, TransientMisses, compare, js_text, stat_key};
use crate::computers;
use crate::secret_policy::find_secret;
use crate::validate::real_timestamp;
use indexmap::IndexMap;
use serde_json::{Value as Json, json};
use std::collections::HashSet;
use std::path::PathBuf;

const REGISTRY_FILE_CAP: usize = 64 * 1024;

/// One named part of a source that failed while the rest was read (`ReadProblem`).
#[derive(Clone, Debug, PartialEq)]
pub struct RegistryProblem {
    pub item: String,
    pub reason: String,
    pub since: u64,
}

impl RegistryProblem {
    fn json(&self) -> Json {
        json!({"item": self.item, "reason": self.reason, "since": self.since})
    }
}

/// The next problem for an item, keeping `since` while the reason stays the same.
fn next_problem(before: Option<&RegistryProblem>, item: &str, reason: &str, now: u64) -> RegistryProblem {
    RegistryProblem {
        item: item.to_string(),
        reason: reason.to_string(),
        since: match before {
            Some(before) if before.reason == reason => before.since,
            _ => now,
        },
    }
}

/// JavaScript's `trim` whitespace set (the one `shortText` trims).
fn trim_js(text: &str) -> &str {
    text.trim_matches(|c: char| {
        matches!(
            c,
            '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
        )
    })
}

/// `shortText`: control characters become spaces, the result is trimmed, a secret refuses
/// it whole, and a long text is cut with an ellipsis (UTF-16 units, as JavaScript slices).
fn short_text(value: Option<&Json>, max: usize) -> Option<String> {
    let Some(Json::String(value)) = value else {
        return None;
    };
    let clean: String = value
        .chars()
        .map(|c| {
            let u = c as u32;
            if u <= 0x1f || (0x7f..=0x9f).contains(&u) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let clean = trim_js(&clean);
    if clean.is_empty() || find_secret(clean).is_some() {
        return None;
    }
    let units: Vec<u16> = clean.encode_utf16().collect();
    if units.len() > max {
        let cut = String::from_utf16_lossy(&units[..max - 1]);
        Some(format!("{cut}..."))
    } else {
        Some(clean.to_string())
    }
}

/// A registry host name or working folder -> one of the registered machines, or null
/// (`registryMachine`): the packaged table first, then the fixed heuristics.
pub fn registry_machine(host: Option<&Json>, cwd: Option<&Json>) -> Option<&'static str> {
    let h = host.and_then(Json::as_str).unwrap_or_default().to_lowercase();
    if let Some(known) = named_computer(&h) {
        return known["machineId"].as_str();
    }
    if !h.is_empty() {
        if h.contains("mac") || h.ends_with(".local") {
            return Some("mac");
        }
        if h.contains("ubuntu") || h == "vm" {
            return Some("vm");
        }
        if h.contains("win") || h.contains("pc") || h.contains("desktop") {
            return Some("windows");
        }
    }
    let dir = cwd.and_then(Json::as_str).unwrap_or_default();
    if dir.starts_with("/Users/") || dir.starts_with("/Volumes/") {
        return Some("mac");
    }
    let bytes = dir.as_bytes();
    if (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/'))
        || dir.starts_with("\\\\")
    {
        return Some("windows");
    }
    if dir.starts_with("/data/") || dir.contains("nas1") {
        return Some("nas1");
    }
    if dir.starts_with("/home/") || dir.starts_with("/mnt/") {
        return Some("vm");
    }
    None
}

/// `namedComputer`: an exact id, alias or name first; then the first non-legacy host whose
/// name the given string contains (the legacy hosts after, in shipped order).
fn named_computer(lower: &str) -> Option<&'static Json> {
    let registry = computers::registry().as_object()?;
    for (id, row) in registry {
        let mut spellings = vec![id.as_str()];
        if let Some(alias) = row["sshAlias"].as_str() {
            spellings.push(alias);
        }
        if let Some(names) = row["names"].as_array() {
            for name in names {
                if let Some(name) = name.as_str() {
                    spellings.push(name);
                }
            }
        }
        if spellings.iter().any(|value| value.to_lowercase() == lower) {
            return Some(row);
        }
    }
    let mut hosts: Vec<(&String, &Json)> = registry.iter().collect();
    hosts.sort_by_key(|(_, row)| row["legacy"].as_bool().unwrap_or(false));
    for (_, row) in hosts {
        if let Some(names) = row["names"].as_array()
            && names
                .iter()
                .filter_map(Json::as_str)
                .any(|name| lower.contains(&name.to_lowercase()))
        {
            return Some(row);
        }
    }
    None
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`
fn thread_name(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    value.chars().count() <= 64 && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// One registry file -> a thread row, or null when it is not one (`parseRegistryEntry`).
pub fn parse_registry_entry(raw: &Json) -> Option<Json> {
    let rec = raw.as_object()?;
    let name = rec.get("name").and_then(Json::as_str)?;
    if !thread_name(name) {
        return None;
    }
    let last_seen = rec
        .get("last_seen")
        .and_then(Json::as_str)
        .filter(|value| real_timestamp(value))
        .and_then(aac_num::ecma::parse_instant)
        .and_then(aac_num::ecma::to_iso_string);
    Some(json!({
        "name": name,
        "kind": short_text(rec.get("kind"), 32),
        "machine": registry_machine(rec.get("machine"), rec.get("cwd")),
        "about": short_text(rec.get("about"), 300),
        "lastSeen": last_seen,
    }))
}

/// One scan's answer: the thread rows (each with its read time and problem), every named
/// problem, and whether anything changed.
pub struct RegistryScan {
    pub threads: Vec<Json>,
    pub problems: Vec<RegistryProblem>,
    pub changed: bool,
}

#[derive(Clone)]
struct FileEntry {
    key: String,
    thread: Option<Json>,
    read_at: u64,
    problem: Option<RegistryProblem>,
}

pub struct PmMailRegistryReader {
    dir: PathBuf,
    files: IndexMap<String, FileEntry>,
    failing: IndexMap<String, RegistryProblem>,
    misses: TransientMisses,
}

impl PmMailRegistryReader {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            files: IndexMap::new(),
            failing: IndexMap::new(),
            misses: TransientMisses::default(),
        }
    }

    fn fail(&mut self, name: &str, reason: &str, since: u64, changed: &mut bool) {
        if let Some(entry) = self.files.get_mut(name) {
            let problem = next_problem(entry.problem.as_ref(), name, reason, since);
            if entry.problem.as_ref().map(|p| p.reason.as_str()) != Some(reason) {
                *changed = true;
            }
            entry.problem = Some(problem);
            // Read again next scan, whatever the file's stat says then.
            entry.key = String::new();
            return;
        }
        let before = self.failing.get(name).cloned();
        if before.as_ref().map(|p| p.reason.as_str()) != Some(reason) {
            *changed = true;
        }
        self.failing
            .insert(name.to_string(), next_problem(before.as_ref(), name, reason, since));
    }

    /// Resolves the thread rows, or a fixed-word error when the folder cannot be listed.
    pub fn scan(&mut self, fs: &dyn ShareFs, now: u64) -> Result<RegistryScan, &'static str> {
        let changed = self.scan_changes(fs, now)?;
        Ok(self.snapshot(changed))
    }

    pub fn scan_changes(&mut self, fs: &dyn ShareFs, now: u64) -> Result<bool, &'static str> {
        let names = match fs.list(&self.dir) {
            Ok(entries) => entries.into_iter().map(|entry| entry.name).collect::<Vec<_>>(),
            Err(error) => {
                return Err(if error.kind() == std::io::ErrorKind::NotFound {
                    "The pm-mail registry folder is missing."
                } else {
                    "The pm-mail registry folder could not be listed."
                });
            }
        };
        let mut changed = false;
        let mut seen = HashSet::new();
        for name in names {
            if name.starts_with('.') || !name.ends_with(".json") {
                continue;
            }
            let file = self.dir.join(&name);
            let stat = match fs.stat(&file) {
                Ok(stat) => stat,
                Err(error) => {
                    seen.insert(name.clone());
                    if error.kind() == std::io::ErrorKind::NotFound {
                        // Gone between the listing and the stat: pm-mail's rename-over.
                        if let Some(since) = self.misses.miss(&name, now) {
                            self.fail(&name, PERSISTENT_SHARE_ERROR, since, &mut changed);
                        }
                        continue;
                    }
                    self.misses.clear(&name);
                    self.fail(&name, "could not be read", now, &mut changed);
                    continue;
                }
            };
            self.misses.clear(&name);
            seen.insert(name.clone());
            if !stat.file {
                self.fail(&name, "is not a regular file", now, &mut changed);
                continue;
            }
            if stat.size > REGISTRY_FILE_CAP as u64 {
                self.fail(&name, "is larger than the 64 KB cap", now, &mut changed);
                continue;
            }
            let key = stat_key(&stat);
            if let Some(entry) = self.files.get(&name)
                && entry.key == key
                && entry.problem.is_none()
            {
                continue;
            }
            let bytes = match fs.read(&file, REGISTRY_FILE_CAP) {
                Ok(bytes) => bytes,
                // The reader parses what readRegularFile hands back (cap + 1 bytes); a file
                // grown past that is truncated text, which is not valid JSON.
                Err(super::ReadError::OverCap) => {
                    self.fail(&name, "is not valid JSON", now, &mut changed);
                    continue;
                }
                Err(super::ReadError::Io(_)) => {
                    self.fail(&name, "could not be read", now, &mut changed);
                    continue;
                }
            };
            // The registry reader decodes UTF-8 lossily, as `bytes.toString('utf8')` does.
            let text = String::from_utf8_lossy(&bytes);
            let raw: Json = match serde_json::from_str(&text) {
                Ok(raw) => raw,
                Err(_) => {
                    self.fail(&name, "is not valid JSON", now, &mut changed);
                    continue;
                }
            };
            // A file that is JSON but no thread entry is not a problem: pm-mail keeps other
            // files here.
            let thread = parse_registry_entry(&raw);
            let before = self.files.get(&name);
            if before.is_none()
                || before.is_some_and(|entry| entry.problem.is_some())
                || before.is_some_and(|entry| {
                    js_text(&entry.thread.clone().unwrap_or(Json::Null))
                        != js_text(&thread.clone().unwrap_or(Json::Null))
                })
            {
                changed = true;
            }
            self.files.insert(
                name.clone(),
                FileEntry {
                    key,
                    thread,
                    read_at: now,
                    problem: None,
                },
            );
            self.failing.shift_remove(&name);
        }
        self.misses.keep_only(&seen);
        let gone: Vec<String> = self
            .files
            .keys()
            .filter(|name| !seen.contains(*name))
            .cloned()
            .collect();
        for name in gone {
            self.files.shift_remove(&name);
            changed = true;
        }
        let gone: Vec<String> = self
            .failing
            .keys()
            .filter(|name| !seen.contains(*name))
            .cloned()
            .collect();
        for name in gone {
            self.failing.shift_remove(&name);
            changed = true;
        }
        Ok(changed)
    }

    pub fn snapshot(&self, changed: bool) -> RegistryScan {
        let threads: Vec<Json> = self
            .files
            .values()
            .filter_map(|entry| {
                let thread = entry.thread.as_ref()?;
                let mut row = thread.as_object()?.clone();
                row.insert("readAt".to_string(), json!(entry.read_at));
                row.insert(
                    "problem".to_string(),
                    entry.problem.as_ref().map(|p| p.json()).unwrap_or(Json::Null),
                );
                Some(Json::Object(row))
            })
            .collect();
        let mut problems: Vec<RegistryProblem> = self
            .files
            .values()
            .filter_map(|entry| entry.problem.clone())
            .chain(self.failing.values().cloned())
            .collect();
        problems.sort_by(|a, b| compare(&a.item, &b.item));
        RegistryScan {
            threads,
            problems,
            changed,
        }
    }
}
