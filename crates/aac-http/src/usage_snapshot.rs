//! The native usage share publisher used by the active collection owner.
//! The blocking task keeps slow share writes outside the request and collection pools.
//! The hosted store supplies history, burn and rollups when it is available.

use crate::registered::snapshot_settings as read_settings;
use aac_snapshot::client::{
    client_source_folder, computer_registry_file, sync_snapshot_client,
};
use aac_snapshot::publisher::{Deps, SnapshotPublisher};
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

/// The pre-fetched sign-in facts one sample carries, read by the publisher's injectable deps.
/// The rollups are not pre-fetched: R2 builds and writes them only when the hourly write is
/// due, through the publisher's `rollup_write` callback (the per-pass rollup slot below).
struct Facts {
    sign_in_rows: Vec<Value>,
    sign_in_failed: bool,
    renewal: Option<Value>,
}

/// One sample's rollup source, set only while its publish pass runs: the hosted store for an
/// active native sample, the payload's own rollup values for a pushed sample, or neither.
enum RollupSource {
    Store(Arc<crate::store_host::StoreHost>),
    Pushed(Value, Value),
}

/// The service's snapshot publisher and the per-pass facts slot.
pub(crate) struct UsageSnapshot {
    publisher: Arc<SnapshotPublisher>,
    facts: Arc<Mutex<Option<Facts>>>,
    /// The per-pass rollup source, read by the publisher's `rollup_write` callback when the
    /// hourly write is due (R2). Set only while a publish pass runs.
    rollup_slot: Arc<Mutex<Option<RollupSource>>>,
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

fn hostname() -> String {
    aac_files::process::hostname().unwrap_or_else(|| "unknown".to_owned())
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
        let rollup_slot: Arc<Mutex<Option<RollupSource>>> = Arc::new(Mutex::new(None));
        let rollup_source = Arc::clone(&rollup_slot);
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
            rollup_write: Arc::new(move |event, directory, write| {
                // R2: the publisher calls this only when the hourly write is due. Build the
                // rollups now (not on every sample) and stream each file straight to the share
                // writer. A pushed sample writes its own values; an active native sample reads
                // the hosted store; neither is a no-op that still marks the hour written.
                let source = rollup_source
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                match source {
                    Some(RollupSource::Store(host)) => {
                        let Some(engine) = host.analytics_engine() else {
                            return Ok(());
                        };
                        let projected = aac_snapshot::project_agent_usage(
                            &event.accounts,
                            event.sampled_at,
                            None,
                            true,
                        );
                        aac_snapshot::exports::write_rollup_exports(
                            &engine,
                            &event.accounts,
                            &projected,
                            event.sampled_at,
                            |resolution, body| {
                                let name = if resolution == "hour" {
                                    aac_snapshot::SNAPSHOT_HISTORY_HOURLY
                                } else {
                                    aac_snapshot::SNAPSHOT_HISTORY_DAILY
                                };
                                write(directory, name, &format!("{body}\n")).map_err(|_| ())
                            },
                        )
                        .map_err(|_| ())
                    }
                    Some(RollupSource::Pushed(hourly, daily)) => {
                        write(
                            directory,
                            aac_snapshot::SNAPSHOT_HISTORY_HOURLY,
                            &format!("{}\n", aac_snapshot::js::stringify(&hourly)),
                        )
                        .map_err(|_| ())?;
                        write(
                            directory,
                            aac_snapshot::SNAPSHOT_HISTORY_DAILY,
                            &format!("{}\n", aac_snapshot::js::stringify(&daily)),
                        )
                        .map_err(|_| ())
                    }
                    None => Ok(()),
                }
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
            rollup_slot,
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
        let rollup_slot = Arc::clone(&self.rollup_slot);
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
            };
            // R2: the rollups are not built here. An active native sample reads the hosted
            // store when the hourly write is due; a pushed sample carries its own values. A
            // failed pushed pre-fetch retries next sample, exactly as the TS dep did.
            let rollup_source = if active {
                store
                    .as_ref()
                    .map(|host| RollupSource::Store(Arc::clone(host)))
            } else {
                match parse_rollups(&payload["rollups"]) {
                    Ok(Some((hourly, daily))) => Some(RollupSource::Pushed(hourly, daily)),
                    Ok(None) => None,
                    Err(()) => {
                        publisher.note(&event);
                        return;
                    }
                }
            };
            // Every sample's readings count, even one whose write is skipped.
            publisher.note(&event);
            if let Some(mut inner) = publisher.try_begin() {
                *facts_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(facts);
                *rollup_slot.lock().unwrap_or_else(|e| e.into_inner()) = rollup_source;
                publisher.pass_since(&mut inner, &event, sign_in_since);
                *facts_slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
                *rollup_slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
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

/// The dashboard refresh interval, defaulting to 60 seconds as the TS reader does.
fn read_refresh_seconds(root: &Path) -> f64 {
    crate::public_auth::native_settings::refresh_interval_seconds(root) as f64
}
