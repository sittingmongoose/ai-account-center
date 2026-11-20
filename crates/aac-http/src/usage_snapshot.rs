//! The native usage share publisher used by the active collection owner.
//! The blocking task keeps slow share writes outside the request and collection pools.
//! The hosted store supplies history, burn and rollups when it is available.

use aac_snapshot::client::{
    client_source_folder, computer_registry_file, runner_source_folder, sync_snapshot_client,
};
use aac_snapshot::publisher::{Deps, SnapshotPublisher};
use aac_snapshot::settings::validate_settings;
use aac_snapshot::snapshot::{LongBurnEntry, LongBurnSpan, SampleEvent};
#[cfg(unix)]
use aac_views::decline::Decline;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
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
    /// The boot root, for the saved snapshot setting (the active-sample guard).
    ccs_dir: PathBuf,
    /// The hosted store, whose own reads answer the history, long burn and rollups.
    store: OnceLock<Arc<crate::store_host::StoreHost>>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `os.hostname` for the failed-collection row: the Linux proc file, else the environment.
fn hostname() -> String {
    if let Ok(bytes) = aac_files::read_bounded(Path::new("/proc/sys/kernel/hostname"), 256)
        && let Ok(text) = String::from_utf8(bytes)
    {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    std::env::var(if cfg!(windows) {
        "COMPUTERNAME"
    } else {
        "HOSTNAME"
    })
    .unwrap_or_else(|_| "unknown".to_owned())
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
    pub(crate) fn new(ccs_dir: PathBuf, store: Option<Arc<crate::store_host::StoreHost>>) -> Self {
        let boot_root = ccs_dir.clone();
        let facts: Arc<Mutex<Option<Facts>>> = Arc::new(Mutex::new(None));
        let settings_root = ccs_dir.clone();
        let refresh_root = ccs_dir.clone();
        let client_root = crate::agent_transport::package_root().unwrap_or_else(|| ccs_dir.clone());
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
                guard
                    .as_ref()
                    .map(|facts| facts.rollups.clone())
                    .unwrap_or(Ok(None))
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
            ccs_dir: boot_root,
            store: store.map(OnceLock::from).unwrap_or_default(),
        }
    }

    /// Bind the Windows publisher to the boot's existing store when publication first needs it.
    #[cfg(windows)]
    pub(crate) fn attach_store(&self, store: Arc<crate::store_host::StoreHost>) {
        let _ = self.store.set(store);
    }

    /// Accept one pushed sample onto its own blocking task: parse, note its readings, then
    /// publish. Returns at once; neither the parse nor the share write holds the socket or a
    /// read permit, so a stalled share holds one write and never the dashboard.
    #[cfg(unix)]
    pub(crate) fn accept(&self, bytes: axum::body::Bytes) -> Result<Value, Decline> {
        self.queue(move || serde_json::from_slice::<Value>(&bytes).map_err(|_| ()));
        Ok(json!({ "accepted": true }))
    }

    /// Publish a completed generation assembled by the collector in this process.
    pub(crate) fn accept_native(&self, payload: Value) {
        self.queue(move || Ok(payload));
    }

    fn queue(&self, payload: impl FnOnce() -> Result<Value, ()> + Send + 'static) {
        let publisher = Arc::clone(&self.publisher);
        let facts_slot = Arc::clone(&self.facts);
        let store = self.store.get().cloned();
        let boot_root = self.ccs_dir.clone();
        tokio::task::spawn_blocking(move || {
            let Ok(payload) = payload() else {
                tracing::warn!(target: "aac_http", event = "agent_usage_snapshot.sample_unparsable", "A pushed usage sample could not be parsed");
                return;
            };
            let Some(event) = parse_event(&payload["event"]) else {
                tracing::warn!(target: "aac_http", event = "agent_usage_snapshot.sample_shape", "A pushed usage sample was not the expected shape");
                return;
            };
            let sign_in_since = payload["signIn"]["since"].as_i64();
            let mut event = event;
            // The native trigger sends the accounts only; this service's own store answers the
            // history, long burn and rollups, under the publisher's active-sample guard.
            let native = payload["event"]["history"].is_null() && payload["rollups"].is_null();
            let active = native && sample_active(&boot_root, event.accounts.len());
            if active {
                enrich(store.as_deref(), &mut event);
            }
            let facts = Facts {
                sign_in_rows: payload["signIn"]["rows"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
                sign_in_failed: payload["signIn"]["failed"].as_bool().unwrap_or(false),
                renewal: if payload["signInRenewal"].is_null() {
                    None
                } else {
                    Some(payload["signInRenewal"].clone())
                },
                rollups: if active {
                    rollups(store.as_deref(), &event)
                } else {
                    parse_rollups(&payload["rollups"])
                },
            };
            // Every sample's readings count, even one whose write is skipped.
            publisher.note(&event);
            if let Some(mut inner) = publisher.try_begin() {
                *facts_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(facts);
                publisher.pass_since(&mut inner, &event, sign_in_since);
                *facts_slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        });
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

/// Whether a sample would publish: a folder is set and there are accounts. The sign-in probes and
/// the store reads run only then, exactly the publisher's own guards.
pub(crate) fn sample_active(root: &Path, accounts: usize) -> bool {
    accounts > 0 && read_settings(root).is_some()
}

/// The 7-day rolling history the snapshot publishes (`SNAPSHOT_HISTORY_MS`).
const SNAPSHOT_HISTORY_MS: i64 = 7 * 86_400_000;

/// The facts the front door used to pre-fetch, from the one hosted store: the 7-day history and
/// the long burn for every live window. The store owner answers both; a failure retries next
/// sample, exactly as the TypeScript publisher's deps did.
fn enrich(store: Option<&crate::store_host::StoreHost>, event: &mut SampleEvent) {
    let Some(engine) = store.and_then(|host| host.analytics_engine()) else {
        return;
    };
    if event.history.is_none() {
        let query = json!({
            "fromMs": event.sampled_at.saturating_sub(SNAPSHOT_HISTORY_MS).max(0),
            "toMs": event.sampled_at.max(0),
            "nowMs": event.sampled_at.max(0),
            "thinning": { "kind": "history" },
        });
        if let Ok(records) = aac_store::ops::call(&engine, "historyRecords", &query.to_string()) {
            let collected = records
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("sampledAt"))
                .cloned()
                .or_else(|| aac_num::ecma::to_iso_string(event.sampled_at).map(Value::String))
                .unwrap_or(Value::Null);
            event.history = Some(json!({
                "schemaVersion": 1,
                "collectedSince": collected,
                "records": records,
            }));
        }
    }
    if event.long_burn.is_none()
        && let Ok(burns) =
            aac_snapshot::exports::long_burn_for(&engine, &event.accounts, event.sampled_at)
    {
        event.long_burn = Some(burns);
    }
}

/// The hourly and daily rollup files for one sample, or the retry signal (`Err`) the publisher
/// treats as a failed pre-fetch.
fn rollups(
    store: Option<&crate::store_host::StoreHost>,
    event: &SampleEvent,
) -> Result<Option<(Value, Value)>, ()> {
    let Some(engine) = store.and_then(|host| host.analytics_engine()) else {
        return Ok(None);
    };
    let projected =
        aac_snapshot::project_agent_usage(&event.accounts, event.sampled_at, None, true);
    match aac_snapshot::exports::build_rollup_exports(
        &engine,
        &event.accounts,
        &projected,
        event.sampled_at,
    ) {
        Ok((hourly, daily)) => Ok(Some((hourly, daily))),
        Err(_) => Err(()),
    }
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
    crate::public_auth::native_settings::refresh_interval_seconds(root) as f64
}
