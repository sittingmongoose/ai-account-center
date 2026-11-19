//! The catalog seeding (`catalog.ts`'s `seedCatalog`), ported with its share mechanics: a
//! file the folder lacks is written whole through a hard link (which fails on a file another
//! machine wrote meanwhile); an existing `models.json` or `model-aliases.json` gains only the
//! packaged entries it lacks, by id, appended in the member's own layout, re-reading before
//! the rename so a concurrent owner edit is never clobbered. The share catalog is not a
//! private file: these are the reference's own mechanics, kept byte for byte (the one-file
//! policy gate allowlists this module for that reason).

use super::catalog::{parse_catalog_file, CATALOG_FILES};
use serde_json::Value as Json;
use std::fs;
use std::path::Path;

/// A seed that could not be written: the file, and the system's error code (never its message).
#[derive(Clone, Debug)]
pub struct SeedFailure {
    pub file: String,
    pub code: String,
}

fn error_code(error: &std::io::Error) -> String {
    let code = match error.raw_os_error() {
        Some(1) => "EPERM",
        Some(2) => "ENOENT",
        Some(13) => "EACCES",
        Some(17) => "EEXIST",
        Some(20) => "ENOTDIR",
        Some(21) => "EISDIR",
        Some(28) => "ENOSPC",
        Some(30) => "EROFS",
        Some(40) => "ELOOP",
        _ => "",
    };
    if code.is_empty() {
        "failed".to_string()
    } else {
        code.to_string()
    }
}

/// The members the seeder extends by id when the file exists.
fn append_members(name: &str) -> &'static [&'static str] {
    match name {
        "models.json" => &["models"],
        "model-aliases.json" => &["aliases", "effortSuffixes"],
        _ => &[],
    }
}

/// The ids one member of a parsed file holds: model ids, alias keys or effort suffixes.
fn entry_ids(file: &Json, member: &str) -> Vec<String> {
    match member {
        "models" => file["models"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row["model"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        "aliases" => file["aliases"]
            .as_object()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default(),
        "effortSuffixes" => file["effortSuffixes"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(Json::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

struct NewEntry {
    id: String,
    wide: String,
    narrow: String,
}

/// Where the top-level member `key` holds its array or object in `text` (text that parses as
/// a JSON object): the opening and closing bracket offsets, or None when the member is
/// missing, named more than once, or holds no array or object (`memberSpan`).
fn member_span(text: &str, key: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut after_colon = false;
    let mut last_key: Option<String> = None;
    let mut named = 0usize;
    let mut span: Option<(usize, usize)> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'"' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            if depth == 1 && after_colon {
                after_colon = false;
            } else if depth == 1 {
                let end = (j + 1).min(bytes.len());
                last_key = serde_json::from_str::<String>(&text[i..end]).ok();
                if last_key.as_deref() == Some(key) {
                    named += 1;
                }
            }
            i = j;
        } else if depth == 1 && c == b':' {
            after_colon = true;
        } else if depth == 1 && c == b',' {
            after_colon = false;
        } else if c == b'{' || c == b'[' {
            if depth == 1 && after_colon && last_key.as_deref() == Some(key) {
                span = Some((i, usize::MAX));
            }
            if depth == 1 {
                after_colon = false;
            }
            depth += 1;
        } else if c == b'}' || c == b']' {
            depth = depth.saturating_sub(1);
            if depth == 1 {
                if let Some((open, close)) = span {
                    if close == usize::MAX {
                        span = Some((open, i));
                    }
                }
            }
        }
        i += 1;
    }
    match span {
        Some((open, close)) if named == 1 && close != usize::MAX && close > open => {
            Some((open, close))
        }
        _ => None,
    }
}

/// `text` with `entries` added at the end of the member at `span`, in the member's own layout
/// (its indentation and line ending, or one line when the member is written on one). Every
/// byte already in `text` stays as it was (`appendToMember`).
fn append_to_member(text: &str, span: (usize, usize), entries: &[NewEntry]) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let indented = |entry: &str, indent: &str| {
        entry
            .split('\n')
            .collect::<Vec<_>>()
            .join(&format!("{eol}{indent}"))
    };
    let (open, close) = span;
    let inner = &text[open + 1..close];
    if inner.trim().is_empty() {
        let line_start = text[..open].rfind('\n').map(|at| at + 1).unwrap_or(0);
        let outer: String = text[line_start..]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let body = entries
            .iter()
            .map(|entry| format!("{}{}", format!("{outer}  "), indented(&entry.wide, &format!("{outer}  "))))
            .collect::<Vec<_>>()
            .join(&format!(",{eol}"));
        return format!(
            "{}{eol}{body}{eol}{outer}{}",
            &text[..open + 1],
            &text[close..]
        );
    }
    let first = open + 1 + (inner.len() - inner.trim_start().len());
    let newline = text[..first].rfind('\n');
    let mut last = close - 1;
    while text.as_bytes()[last].is_ascii_whitespace() {
        last -= 1;
    }
    let insert = match newline {
        Some(at) if at > open => entries
            .iter()
            .map(|entry| {
                format!(
                    ",{eol}{}",
                    indented(&entry.wide, &text[at + 1..first])
                )
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => entries
            .iter()
            .map(|entry| format!(", {}", entry.narrow))
            .collect::<Vec<_>>()
            .join(""),
    };
    format!("{}{insert}{}", &text[..last + 1], &text[last + 1..])
}

/// The packaged entries a share file lacks, by id, in the packaged order, rendered for
/// appending (`missingEntries`).
fn missing_entries(member: &str, seed: &Json, have: &[String]) -> Vec<NewEntry> {
    let has = |id: &str| have.iter().any(|held| held == id);
    match member {
        "effortSuffixes" => seed["effortSuffixes"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(Json::as_str)
                    .filter(|suffix| !has(suffix))
                    .map(|suffix| NewEntry {
                        id: suffix.to_string(),
                        wide: Json::from(suffix).to_string(),
                        narrow: Json::from(suffix).to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "models" => seed["models"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter(|row| {
                        row["model"].as_str().is_some_and(|model| !has(model))
                    })
                    .map(|row| NewEntry {
                        id: row["model"].as_str().unwrap_or_default().to_string(),
                        wide: serde_json::to_string_pretty(row).unwrap_or_default(),
                        narrow: row.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => seed["aliases"]
            .as_object()
            .map(|map| {
                map.iter()
                    .filter(|(written, _)| !has(written))
                    .map(|(written, canonical)| {
                        let entry = format!(
                            "{}: {}",
                            Json::from(written.clone()),
                            canonical
                        );
                        NewEntry {
                            id: written.clone(),
                            wide: entry.clone(),
                            narrow: entry,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The share file after the appends, or None when they cannot be made: never a file that
/// reads differently (`appendedText`).
fn appended_text(name: &str, share_text: &str, appends: &[(String, Vec<String>, Vec<NewEntry>)]) -> Option<String> {
    let mut next = share_text.to_string();
    for (member, _, entries) in appends {
        if entries.is_empty() {
            continue;
        }
        let span = member_span(&next, member)?;
        next = append_to_member(&next, span, entries);
    }
    let parsed = parse_catalog_file(name, next.as_bytes()).ok()?;
    for (member, have, entries) in appends {
        let next_ids = entry_ids(&parsed, member);
        let mut expected: Vec<String> = have.clone();
        expected.extend(entries.iter().map(|entry| entry.id.clone()));
        if next_ids.len() != expected.len()
            || next_ids.iter().any(|id| !expected.contains(id))
        {
            return None;
        }
    }
    Some(next)
}

/// Adds to an existing share file the packaged entries it lacks, by id; never changes or
/// removes an entry already there (`appendMissing`).
fn append_missing(
    target: &Path,
    name: &str,
    members: &[&str],
    found_mode: u32,
    found_is_file: bool,
    seed_bytes: &[u8],
    failures: &mut Vec<SeedFailure>,
) -> bool {
    let destination = target.join(name);
    if !found_is_file {
        failures.push(SeedFailure {
            file: name.to_string(),
            code: "not-a-file".to_string(),
        });
        return false;
    }
    if parse_catalog_file(name, &seed_bytes).is_err() {
        failures.push(SeedFailure {
            file: name.to_string(),
            code: "invalid-seed".to_string(),
        });
        return false;
    }
    let seed: Json = match serde_json::from_slice(&seed_bytes) {
        Ok(seed) => seed,
        Err(_) => {
            failures.push(SeedFailure {
                file: name.to_string(),
                code: "invalid-seed".to_string(),
            });
            return false;
        }
    };
    for attempt in 0..3 {
        let temp = target.join(format!(
            ".{name}.tmp-{}-{}-{attempt}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        enum Outcome {
            Written,
            NoAppends,
            Retry,
        }
        let outcome = (|| -> Result<Outcome, std::io::Error> {
            let before = fs::read(&destination)?;
            let share = match parse_catalog_file(name, &before) {
                Ok(share) => share,
                Err(_) => {
                    failures.push(SeedFailure {
                        file: name.to_string(),
                        code: "invalid".to_string(),
                    });
                    return Ok(Outcome::NoAppends);
                }
            };
            let appends: Vec<(String, Vec<String>, Vec<NewEntry>)> = members
                .iter()
                .map(|member| {
                    let have = entry_ids(&share, member);
                    let entries = missing_entries(member, &seed, &have);
                    (member.to_string(), have, entries)
                })
                .collect();
            if appends.iter().all(|(_, _, entries)| entries.is_empty()) {
                return Ok(Outcome::NoAppends);
            }
            let before_text = String::from_utf8_lossy(&before).into_owned();
            let next = match appended_text(name, &before_text, &appends) {
                Some(next) => next,
                None => {
                    failures.push(SeedFailure {
                        file: name.to_string(),
                        code: "unmergeable".to_string(),
                    });
                    return Ok(Outcome::NoAppends);
                }
            };
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            fs::write(&temp, next.as_bytes())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(
                    &temp,
                    fs::Permissions::from_mode(found_mode & 0o777),
                );
            }
            #[cfg(not(unix))]
            let _ = found_mode;
            // Another writer (an owner's edit, another machine's start) since the read:
            // read again.
            let now = fs::read(&destination)?;
            if now != before {
                return Ok(Outcome::Retry);
            }
            fs::rename(&temp, &destination)?;
            Ok(Outcome::Written)
        })();
        let _ = fs::remove_file(&temp);
        match outcome {
            Ok(Outcome::Written) => return true,
            Ok(Outcome::NoAppends) => return false,
            Ok(Outcome::Retry) => continue,
            Err(error) => {
                failures.push(SeedFailure {
                    file: name.to_string(),
                    code: error_code(&error),
                });
                return false;
            }
        }
    }
    failures.push(SeedFailure {
        file: name.to_string(),
        code: "busy".to_string(),
    });
    false
}

/// Publishes the packaged catalog into an existing `v1/catalog` folder (`seedCatalog`).
/// Resolves the names it wrote; never throws.
pub fn seed_catalog(
    root: &Path,
    seed: &dyn Fn(&str) -> Option<Vec<u8>>,
    failures: &mut Vec<SeedFailure>,
) -> Vec<String> {
    let target = root.join("v1").join("catalog");
    let mut written = Vec::new();
    // List, then look (a listing refreshes the NFS view of a file another machine just wrote).
    if fs::read_dir(&target).is_err() {
        return written;
    }
    for name in CATALOG_FILES {
        let destination = target.join(name);
        let found = match fs::symlink_metadata(&destination) {
            Ok(meta) => Some(meta),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                failures.push(SeedFailure {
                    file: name.to_string(),
                    code: error_code(&error),
                });
                continue;
            }
        };
        if let Some(found) = found {
            let members = append_members(name);
            let packaged = seed(name).unwrap_or_default();
            if !members.is_empty()
                && append_missing(
                    &target,
                    name,
                    members,
                    {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            found.permissions().mode()
                        }
                        #[cfg(not(unix))]
                        0o644
                    },
                    found.is_file(),
                    &packaged,
                    failures,
                )
            {
                written.push(name.to_string());
            }
            continue;
        }
        let temp = target.join(format!(
            ".{name}.tmp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let outcome = (|| -> Result<(), std::io::Error> {
            let bytes = seed(name)
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no packaged seed"))?;
            fs::write(&temp, bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&temp, fs::Permissions::from_mode(0o644));
            }
            // `link`, not `rename`: it fails on a file another machine wrote since the look.
            fs::hard_link(&temp, &destination)?;
            Ok(())
        })();
        match outcome {
            Ok(()) => written.push(name.to_string()),
            Err(error) => {
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    failures.push(SeedFailure {
                        file: name.to_string(),
                        code: error_code(&error),
                    });
                }
            }
        }
        let _ = fs::remove_file(&temp);
    }
    written
}

/// The packaged catalog seeds, embedded at build time so the deployed service carries its
/// own commit's catalog (the files `docs/agent-reports/catalog` ships).
pub fn embedded_seed(name: &str) -> Option<&'static [u8]> {
    match name {
        "models.json" => Some(include_str!("../../../../docs/agent-reports/catalog/models.json").as_bytes()),
        "model-aliases.json" => Some(include_str!("../../../../docs/agent-reports/catalog/model-aliases.json").as_bytes()),
        "task-types.json" => Some(include_str!("../../../../docs/agent-reports/catalog/task-types.json").as_bytes()),
        "measures.json" => Some(include_str!("../../../../docs/agent-reports/catalog/measures.json").as_bytes()),
        "uses.json" => Some(include_str!("../../../../docs/agent-reports/catalog/uses.json").as_bytes()),
        "benchmarks.json" => Some(include_str!("../../../../docs/agent-reports/catalog/benchmarks.json").as_bytes()),
        _ => None,
    }
}
