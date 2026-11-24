use super::{Job, clock, expected_results, host_progress, hosts, rows};
use aac_core::{Cause, Failure, sha256_hex};
use aac_files::{
    BackupClass, Class, OwnedStore, Registry, StoreSpec,
    presence::{self, Presence},
    read::{self, Read, ReadPolicy},
};
use serde_json::{Map, Value, json};
use std::{path::Path, time::Duration};

const LEGACY: &str = "dashboard-job.json";
const EXPANDED: &str = "dashboard-job-expanded.json";
const LEGACY_CAP: usize = 65_536;
const EXPANDED_CAP: usize = 1024 * 1024;

pub(super) struct Writer {
    legacy: OwnedStore,
    expanded: OwnedStore,
    _marker: Presence,
}

impl Writer {
    pub(super) fn claim(root: &Path) -> Result<Self, Failure> {
        let mut registry = Registry::default();
        for (name, cap) in [(LEGACY, LEGACY_CAP), (EXPANDED, EXPANDED_CAP)] {
            registry.register(StoreSpec {
                path: name.into(),
                class: Class::State,
                mode: 0o600,
                max_size: cap,
                owner: "app-updates".into(),
                schema: "dashboard-app-updates-v1".into(),
                backup: BackupClass::Snapshot,
            })?;
        }
        // Legacy TS and Python still use the presence marker. The new writer additionally
        // holds each file's kernel lease for the whole lifetime of its job.
        let legacy = registry.claim(root, Path::new(LEGACY), "app-updates")?;
        let expanded = registry.claim(root, Path::new(EXPANDED), "app-updates")?;
        let restart = root.join("t3-restart.lock");
        if alive(&restart)? {
            return Err(busy());
        }
        let lock = root.join("dashboard-update.lock");
        let pid = std::process::id();
        let bytes = serde_json::to_vec(&json!({"pid":pid,"identity":identity(pid)}))
            .map_err(|_| damaged())?;
        let marker = match Presence::claim(&lock, &bytes) {
            Ok(marker) => marker,
            Err(error) if error.cause == Cause::Busy => {
                let observed = presence::observe(&lock)?;
                if let Some(observed) = observed {
                    if current(&observed) {
                        return Err(busy());
                    }
                    presence::remove_observed(&lock, &observed)?;
                }
                Presence::claim(&lock, &bytes)?
            }
            Err(error) => return Err(error),
        };
        if alive(&restart)? {
            return Err(busy());
        }
        Ok(Self {
            legacy,
            expanded,
            _marker: marker,
        })
    }

    pub(super) fn save(&self, job: &Job) -> Result<(), Failure> {
        let mut legacy = job.clone();
        legacy.results = job
            .results
            .iter()
            .filter(|row| {
                hosts().any(|host| host.legacy && row["platform"] == host.id)
                    && row["appId"].as_str().is_some_and(rows::legacy)
            })
            .map(rows::legacy_row)
            .collect();
        if let Some(hosts_map) = legacy.hosts.as_mut() {
            hosts_map.retain(|id, _| hosts().any(|host| host.id == id && host.legacy));
            for value in hosts_map.values_mut() {
                if value["currentApp"]
                    .as_str()
                    .is_some_and(|app| !rows::legacy(app))
                {
                    value["currentApp"] = Value::Null;
                    value["phase"] = Value::Null;
                    value["phaseSince"] = Value::Null;
                }
            }
        }
        let mut empty_scope = false;
        if let Some(scope) = legacy.scope.as_mut() {
            scope.retain(|id, _| hosts().any(|host| host.id == id && host.legacy));
            for apps in scope.values_mut() {
                if let Some(list) = apps.as_array_mut() {
                    list.retain(|app| {
                        app.as_str()
                            .is_some_and(|app| rows::legacy(app) && rows::app_known(app))
                    });
                }
            }
            scope.retain(|_, apps| apps.as_array().is_some_and(|apps| !apps.is_empty()));
            empty_scope = scope.is_empty();
        }
        if empty_scope {
            legacy.scope = None;
        }
        legacy.active_platform = legacy
            .active_platform
            .filter(|id| hosts().any(|host| host.id == id && host.legacy));
        let legacy_scope = job.scope.as_ref().map_or_else(
            || {
                hosts().filter(|host| host.legacy).count()
                    * rows::app_ids().filter(|app| rows::legacy(app)).count()
            },
            |scope| {
                hosts()
                    .filter(|host| host.legacy)
                    .map(|host| {
                        scope
                            .get(host.id)
                            .and_then(Value::as_array)
                            .map_or(0, |apps| {
                                apps.iter()
                                    .filter(|app| {
                                        app.as_str().is_some_and(|app| {
                                            rows::legacy(app) && rows::app_known(app)
                                        })
                                    })
                                    .count()
                            })
                    })
                    .sum()
            },
        );
        legacy.expected_results = job.expected_results.map(|n| n.min(legacy_scope));
        let bytes = serde_json::to_vec(&json!({"job":legacy})).map_err(|_| damaged())?;
        let expanded = serde_json::to_vec(&json!({"legacyDigest":sha256_hex(&bytes),"job":job}))
            .map_err(|_| damaged())?;
        self.legacy.write(&bytes)?;
        self.expanded.write(&expanded)
    }
}

fn read_bytes(root: &Path, name: &str, cap: usize) -> Option<Vec<u8>> {
    let Read::Bytes(bytes, _) = read::read(&root.join(name), &ReadPolicy::capped(cap as u64))
    else {
        return None;
    };
    Some(bytes)
}

pub(super) fn load(root: &Path) -> Option<Job> {
    let old = read_bytes(root, LEGACY, LEGACY_CAP);
    let expanded = read_bytes(root, EXPANDED, EXPANDED_CAP)
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(|expanded| {
            let Some(digest) = expanded["legacyDigest"].as_str().filter(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }) else {
                return false;
            };
            match &old {
                Some(old) => digest == sha256_hex(old),
                None => root
                    .join(LEGACY)
                    .symlink_metadata()
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            }
        });
    let mut job = expanded.and_then(decode).or_else(|| {
        old.and_then(|raw| serde_json::from_slice(&raw).ok())
            .and_then(decode)
    })?;
    if job.running() && !alive(&root.join("dashboard-update.lock")).unwrap_or(true) {
        job.state = "failed".into();
        job.finished_at = Some(clock());
        job.active_platform = None;
        job.hosts = None;
        // The owner disappeared. Rows it never reported are unknown, so the
        // dashboard can show them and Retry failed can rerun the entire scope.
        for host in hosts() {
            if job.in_scope(host) {
                job.fill(host, "unknown", "host_unknown");
            }
        }
    }
    Some(job)
}

fn decode(saved: Value) -> Option<Job> {
    let raw = &saved["job"];
    let id = raw["id"].as_str()?;
    let state = raw["state"].as_str()?;
    if id.len() != 36
        || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
        || !matches!(state, "running" | "completed" | "failed")
    {
        return None;
    }
    let started_at = date(&raw["startedAt"])?;
    let mut scope = match raw.get("scope") {
        None => None,
        Some(value) => Some(decode_scope(value)?),
    };
    // The legacy rollback view of a retry with no legacy hosts/apps has an
    // empty host map and no expected rows. It is still a retry, not a full job.
    if scope.is_none()
        && raw["expectedResults"].as_u64() == Some(0)
        && raw["hosts"]
            .as_object()
            .is_some_and(|hosts| hosts.is_empty())
    {
        scope = Some(Map::new());
    }
    let raw_results = raw["results"]
        .as_array()
        .filter(|rows| rows.len() <= expected_results())?;
    let mut results = Vec::new();
    for row in raw_results {
        let Some(host) = hosts().find(|host| row["platform"] == host.id) else {
            continue;
        };
        let Some(app) = row["appId"].as_str().filter(|app| rows::app_known(app)) else {
            continue;
        };
        if scope
            .as_ref()
            .is_some_and(|scope| !scope_owns(scope, host.id, app))
        {
            continue;
        }
        results.push(rows::restore(host, app, row));
    }
    let scoped_results = scope.as_ref().map_or_else(expected_results, |scope| {
        scope
            .values()
            .filter_map(Value::as_array)
            .map(Vec::len)
            .sum()
    });
    let expected_results = raw["expectedResults"]
        .as_u64()
        .map(|count| count as usize)
        .filter(|count| {
            *count >= results.len() && *count <= expected_results() && *count <= scoped_results
        });
    Some(Job {
        id: id.into(), state: state.into(), started_at, finished_at: date(&raw["finishedAt"]),
        active_platform: hosts().find(|host| raw["activePlatform"] == host.id).map(|host| host.id.into()),
        hosts: raw["hosts"].as_object().map(|saved| {
            hosts()
                .filter(|host| scope.as_ref().is_none_or(|scope| scope.contains_key(host.id)))
                .map(|host| {
                    let state = &saved[host.id];
                    let progress = match state["state"].as_str() {
                        Some("done") => host_progress("done"),
                        Some("running") => {
                            let app = state["currentApp"]
                                .as_str()
                                .filter(|app| rows::app_known(app));
                            let phase = state["phase"].as_str().filter(|phase| {
                                ["checking", "updating", "downloading"].contains(phase)
                            });
                            json!({"state":"running","currentApp":app,"phase":phase,"phaseSince":date(&state["phaseSince"])})
                        }
                        _ => host_progress("waiting"),
                    };
                    (host.id.into(), progress)
                })
                .collect()
        }),
        cancel_requested: raw["cancelRequested"] == true, results, expected_results, scope,
    })
}

fn decode_scope(value: &Value) -> Option<Map<String, Value>> {
    let saved = value.as_object().filter(|saved| !saved.is_empty())?;
    let mut scope = Map::new();
    for (id, apps) in saved {
        let host = hosts().find(|host| host.id == id)?;
        let apps = apps.as_array()?;
        if apps.is_empty() || apps.len() > rows::app_ids().count() {
            return None;
        }
        let mut kept = Vec::with_capacity(apps.len());
        for app in apps {
            let app = app.as_str().filter(|app| rows::app_known(app))?;
            if kept.iter().any(|saved: &String| saved.as_str() == app) {
                return None;
            }
            kept.push(app.to_owned());
        }
        scope.insert(host.id.to_owned(), json!(kept));
    }
    Some(scope)
}

fn scope_owns(scope: &Map<String, Value>, host: &str, app: &str) -> bool {
    scope
        .get(host)
        .and_then(Value::as_array)
        .is_some_and(|apps| apps.iter().any(|saved| saved.as_str() == Some(app)))
}

fn date(value: &Value) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value.as_str().filter(|v| v.len() <= 40)?)
        .ok()
        .map(|time| {
            time.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
}

fn busy() -> Failure {
    Failure::new(Cause::Busy, "app update lease already active")
}
fn damaged() -> Failure {
    Failure::new(Cause::Damaged, "app update journal could not be encoded")
}

fn alive(path: &Path) -> Result<bool, Failure> {
    Ok(presence::observe(path)?.as_ref().is_some_and(current))
}

fn current(observed: &presence::Observed) -> bool {
    let fresh = || {
        observed
            .modified
            .and_then(|time| time.elapsed().ok())
            .is_none_or(|age| age < Duration::from_secs(5))
    };
    let raw = observed
        .bytes
        .as_ref()
        .and_then(|raw| serde_json::from_slice::<Value>(raw).ok());
    let Some(raw) = raw else {
        return fresh();
    };
    let Some(pid) = raw["pid"]
        .as_u64()
        .and_then(|value| i32::try_from(value).ok())
        .filter(|pid| *pid > 0)
    else {
        return fresh();
    };
    if let Some(expected) = raw["identity"].as_str()
        && let Some(actual) = identity(pid as u32)
        && expected != actual
    {
        return false;
    }
    !aac_files::process::is_dead(pid as u32)
}

#[cfg(unix)]
fn identity(pid: u32) -> Option<String> {
    let bytes = aac_files::read_bounded(Path::new(&format!("/proc/{pid}/stat")), 16 * 1024).ok()?;
    let stat = std::str::from_utf8(&bytes).ok()?;
    let start = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?;
    let boot = aac_files::read_bounded(Path::new("/proc/sys/kernel/random/boot_id"), 128).ok()?;
    let boot = std::str::from_utf8(&boot).ok()?.trim();
    (!boot.is_empty()).then(|| format!("{boot}:{start}"))
}

/// The non-Unix boot records no start-time identity. The platform process probe still checks
/// whether the marker's PID is alive before a replacement.
#[cfg(not(unix))]
fn identity(_pid: u32) -> Option<String> {
    None
}
