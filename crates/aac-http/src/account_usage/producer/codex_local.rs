//! The bare Codex profile may recover quota from its local desktop rollout.
use aac_files::read::{self, Ancestors, ReadPolicy};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const TAIL_BYTES: u64 = 1024 * 1024;
const MAX_FILES: usize = 20;
const MAX_DIRECTORIES: usize = 50_000;

pub(super) fn read(scope: &Path, now: i64) -> Option<Value> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let base = match std::env::var_os("CODEX_HOME") {
        Some(path) => {
            let path = PathBuf::from(path);
            if let Ok(relative) = path.strip_prefix("~") {
                home.join(relative)
            } else {
                path
            }
        }
        None => home.join(".codex"),
    };
    if !base.is_absolute() || (scope != home.join(".ccs") && !base.starts_with(scope)) {
        return None;
    }
    read_sessions(&base.join("sessions"), now)
}

fn read_sessions(root: &Path, now: i64) -> Option<Value> {
    let mut files = BTreeSet::new();
    let mut visited = 0;
    walk(root, true, &mut visited, &mut files);
    files.iter().rev().find_map(|file| tail(root, file, now))
}

fn walk(folder: &Path, dates: bool, visited: &mut usize, files: &mut BTreeSet<PathBuf>) {
    if *visited >= MAX_DIRECTORIES {
        return;
    }
    *visited += 1;
    let Ok(entries) = read::entries(folder, MAX_DIRECTORIES) else {
        return;
    };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            dirs.push(entry.path());
        } else if kind.is_file()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        {
            files.insert(entry.path());
            if files.len() > MAX_FILES {
                files.pop_first();
            }
        }
    }
    let prune = dates
        && dirs.iter().all(|dir| {
            dir.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(date_partition)
        });
    dirs.sort_unstable_by(|a, b| b.cmp(a));
    for dir in dirs {
        if prune && files.len() >= MAX_FILES {
            break;
        }
        walk(&dir, prune, visited, files);
    }
}

fn date_partition(name: &str) -> bool {
    let digits = |field: &str| field.bytes().all(|byte| byte.is_ascii_digit());
    match name.len() {
        2 | 4 => digits(name),
        10 => name
            .split_once('-')
            .and_then(|(year, rest)| rest.split_once('-').map(|(month, day)| (year, month, day)))
            .is_some_and(|(y, m, d)| {
                y.len() == 4 && m.len() == 2 && d.len() == 2 && digits(y) && digits(m) && digits(d)
            }),
        _ => false,
    }
}

fn tail(root: &Path, file: &Path, now: i64) -> Option<Value> {
    let policy = ReadPolicy::capped(u64::MAX).ancestors(Ancestors::NoLinkBelow(root));
    let (mut file, metadata) = read::open(file, &policy).ok()?;
    file.seek(SeekFrom::Start(metadata.len().saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut data = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut data).ok()?;
    let windows = String::from_utf8_lossy(&data)
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(200)
        .find_map(read_limits)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|time| i64::try_from(time.as_millis()).ok());
    let sample = modified
        .filter(|time| now.saturating_sub(*time) > 300_000)
        .unwrap_or(now);
    Some(json!({
        "status":"ok","usageSource":"local","windows":windows,
        "fetchedAt":aac_num::ecma::to_iso_string(now),"sampledAt":aac_num::ecma::to_iso_string(sample)
    }))
}

fn read_limits(line: &str) -> Option<Vec<Value>> {
    let decoded: Value = serde_json::from_str(line).ok()?;
    let payload = decoded
        .get("payload")
        .filter(|value| value.is_object())
        .unwrap_or(&decoded);
    let limits = &payload["rate_limits"];
    let windows = [
        ("primary", "five_hour", "5h"),
        ("secondary", "seven_day", "week"),
    ]
    .into_iter()
    .filter_map(|(field, key, label)| {
        let window = &limits[field];
        let used = window["used_percent"].as_f64()?.clamp(0.0, 100.0);
        let reset = window["resets_at"]
            .as_f64()
            .and_then(|seconds| aac_num::ecma::to_iso_string((seconds * 1000.0) as i64));
        Some(
            json!({"key":key,"label":label,"usedPercent":used,"remainingPercent":100.0-used,
                "resetAt":reset,"windowMinutes":window["window_minutes"]}),
        )
    })
    .collect::<Vec<_>>();
    (!windows.is_empty()).then_some(windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_the_most_recent_session_with_a_real_window() {
        let temp = tempfile::tempdir().expect("fixture directory");
        let date = temp.path().join("2026/11/19");
        std::fs::create_dir_all(&date).expect("fixture folders");
        std::fs::write(
            date.join("rollout-b.jsonl"),
            br#"{"payload":{"rate_limits":null}}"#,
        )
        .expect("empty exec rollout");
        std::fs::write(date.join("rollout-a.jsonl"),
            br#"{"payload":{"rate_limits":{"primary":{"used_percent":24,"resets_at":1795090800,"window_minutes":300}}}}
{"payload":{"rate_limits":null}}
"#).expect("measured rollout");
        let now = aac_core::UtcMs::now().expect("fixture clock").get() as i64;
        let sample = read_sessions(temp.path(), now).expect("recorded quota");
        assert_eq!(sample["windows"][0]["key"], "five_hour");
        assert_eq!(sample["windows"][0]["usedPercent"].as_f64(), Some(24.0));
        assert_eq!(
            sample["windows"][0]["remainingPercent"].as_f64(),
            Some(76.0)
        );
        assert_eq!(sample["sampledAt"], sample["fetchedAt"]);
    }
}
