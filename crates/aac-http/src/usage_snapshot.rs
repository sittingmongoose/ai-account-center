//! G18 wiring: the front door pushes each finished analytics sample to
//! `POST /internal/usage-snapshot/sample`; the service runs the `aac-snapshot` publisher with
//! real file effects. The publish pass runs on its own blocking task, never holding a read
//! permit, so a stalled share holds one write and never the dashboard (the TS publisher's
//! rule). Sign-in rows, the renewal summary and the rollup files arrive pre-fetched in the
//! payload until G5/G10 and the store handoff land; the publisher's deadline recheck, atomic
//! writes, staleness and once-an-hour rollups are all the ported TS behaviour.

use aac_snapshot::client::{
    client_source_folder, computer_registry_file, runner_source_folder, sync_snapshot_client,
};
use aac_snapshot::publisher::{Deps, SnapshotPublisher};
use aac_snapshot::settings::validate_settings;
use aac_snapshot::snapshot::{LongBurnEntry, LongBurnSpan, SampleEvent};
use aac_views::decline::Decline;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// The sample payload cap: the quota history alone is bounded at 32 MiB, plus accounts and facts.
pub(crate) const USAGE_SAMPLE_CAP: usize = 40 * 1024 * 1024;

/// The pre-fetched facts one sample carries, read by the publisher's injectable deps.
struct Facts {
    sign_in_rows: Vec<Value>,
    sign_in_failed: bool,
    renewal: Option<Value>,
    rollups: Result<Option<(Value, Value)>, ()>,
}

/// The service's snapshot publisher and the per-pass facts slot.
pub(crate) struct UsageSnapshot {
    publisher: Arc<SnapshotPublisher>,
    facts: Arc<Mutex<Option<Facts>>>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `os.hostname` for the failed-collection row: the Linux proc file, else the environment.
fn hostname() -> String {
    if let Ok(bytes) = aac_files::read_bounded(Path::new("/proc/sys/kernel/hostname"), 256) {
        if let Ok(text) = String::from_utf8(bytes) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_owned())
}

fn span(value: &Value) -> LongBurnSpan {
    LongBurnSpan {
        span_ms: value["spanMs"].as_i64().unwrap_or(0),
        consumed: value["consumed"].as_f64(),
        rate: value["rate"].as_f64(),
        coverage: value["coverage"].as_f64().unwrap_or(0.0),
    }
}

impl UsageSnapshot {
    /// Build the publisher with the service's real file effects under the boot root.
    pub(crate) fn new(ccs_dir: PathBuf, package_root: PathBuf) -> Self {
        let facts: Arc<Mutex<Option<Facts>>> = Arc::new(Mutex::new(None));
        let settings_root = ccs_dir.clone();
        let refresh_root = ccs_dir.clone();
        let client_root = package_root.clone();
        let sign_in_facts = Arc::clone(&facts);
        let renewal_facts = Arc::clone(&facts);
        let rollup_facts = Arc::clone(&facts);
        let deps = Deps {
            settings: Arc::new(move || read_settings(&settings_root)),
            write: Arc::new(|directory, name, body| {
                aac_files::share::write_atomically(Path::new(directory), name, body, 0o644)
            }),
            is_directory: Arc::new(|directory| {
                std::fs::metadata(directory).map(|meta| meta.is_dir())
            }),
            refresh_seconds: Arc::new(move || read_refresh_seconds(&refresh_root)),
            sync_client: Arc::new(move |directory| {
                sync_snapshot_client(
                    Path::new(directory),
                    &client_source_folder(&client_root),
                    &computer_registry_file(&client_root),
                    Some(&runner_source_folder(&client_root)),
                )
            }),
            sign_in: Arc::new(move || {
                let guard = sign_in_facts.lock().unwrap_or_else(|e| e.into_inner());
                match guard.as_ref() {
                    Some(facts) if facts.sign_in_failed => Err(()),
                    Some(facts) => Ok(facts.sign_in_rows.clone()),
                    None => Ok(Vec::new()),
                }
            }),
            sign_in_renewal: Arc::new(move || {
                let guard = renewal_facts.lock().unwrap_or_else(|e| e.into_inner());
                guard.as_ref().and_then(|facts| facts.renewal.clone())
            }),
            rollup_exports: Arc::new(move |_event| {
                let guard = rollup_facts.lock().unwrap_or_else(|e| e.into_inner());
                guard.as_ref().map(|facts| facts.rollups.clone()).unwrap_or(Ok(None))
            }),
            now_ms: Arc::new(now_ms),
            hostname: Arc::new(hostname),
            log: Arc::new(|event, directory| {
                let message = if event == "agent_usage_snapshot.write_stalled" {
                    "An agent usage snapshot write has not finished; later samples wait for it"
                } else {
                    "The agent usage snapshot could not be written"
                };
                tracing::warn!(target: "aac_http", event, directory, "{message}");
            }),
        };
        Self {
            publisher: Arc::new(SnapshotPublisher::new(deps)),
            facts,
        }
    }

    /// Accept one pushed sample onto its own blocking task: parse, note its readings, then
    /// publish. Returns at once; neither the parse nor the share write holds the socket or a
    /// read permit, so a stalled share holds one write and never the dashboard.
    pub(crate) fn accept(&self, bytes: axum::body::Bytes) -> Result<Value, Decline> {
        let publisher = Arc::clone(&self.publisher);
        let facts_slot = Arc::clone(&self.facts);
        tokio::task::spawn_blocking(move || {
            let Ok(payload) = serde_json::from_slice::<Value>(&bytes) else {
                tracing::warn!(target: "aac_http", event = "agent_usage_snapshot.sample_unparsable", "A pushed usage sample could not be parsed");
                return;
            };
            let Some(event) = parse_event(&payload["event"]) else {
                tracing::warn!(target: "aac_http", event = "agent_usage_snapshot.sample_shape", "A pushed usage sample was not the expected shape");
                return;
            };
            let sign_in_since = payload["signIn"]["since"].as_i64();
            let facts = Facts {
                sign_in_rows: payload["signIn"]["rows"].as_array().cloned().unwrap_or_default(),
                sign_in_failed: payload["signIn"]["failed"].as_bool().unwrap_or(false),
                renewal: if payload["signInRenewal"].is_null() { None } else { Some(payload["signInRenewal"].clone()) },
                rollups: parse_rollups(&payload["rollups"]),
            };
            // Every sample's readings count, even one whose write is skipped.
            publisher.note(&event);
            if let Some(mut inner) = publisher.try_begin() {
                *facts_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(facts);
                publisher.pass_since(&mut inner, &event, sign_in_since);
                *facts_slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        });
        Ok(json!({ "accepted": true }))
    }
}

fn parse_event(value: &Value) -> Option<SampleEvent> {
    let accounts = value["accounts"].as_array()?.clone();
    let sampled_at = value["sampledAt"].as_i64()?;
    let history = if value["history"].is_null() {
        None
    } else {
        Some(value["history"].clone())
    };
    let long_burn = value["longBurn"].as_array().map(|entries| {
        entries
            .iter()
            .filter_map(|entry| {
                let key = entry[0].as_str()?.to_owned();
                let burns = &entry[1];
                Some((
                    key,
                    LongBurnEntry {
                        last7_days: span(&burns["last7Days"]),
                        last30_days: span(&burns["last30Days"]),
                    },
                ))
            })
            .collect::<HashMap<String, LongBurnEntry>>()
    });
    Some(SampleEvent {
        accounts,
        history,
        sampled_at,
        long_burn,
    })
}

fn parse_rollups(value: &Value) -> Result<Option<(Value, Value)>, ()> {
    if value.is_null() {
        return Ok(None);
    }
    // A failed pre-fetch retries on the next sample, as the TS publisher's rollup dep does.
    if value["failed"].as_bool() == Some(true) {
        return Err(());
    }
    Ok(Some((value["hourly"].clone(), value["daily"].clone())))
}

/// The saved snapshot setting, or None while no folder is configured (registry row read).
fn read_settings(root: &Path) -> Option<aac_snapshot::SnapshotSettings> {
    let (file, policy) = crate::registered::file(root, "agent-usage-snapshot")?;
    let aac_files::read::Read::Bytes(bytes, _) = aac_files::read::read(&file, &policy) else {
        return None;
    };
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    validate_settings(&value)
}

/// The dashboard refresh interval, defaulting to 60 seconds as the TS reader does.
fn read_refresh_seconds(root: &Path) -> f64 {
    match crate::unix::read_settings(root) {
        Ok(settings) => settings.refresh_interval_seconds as f64,
        Err(_) => 60.0,
    }
}
