//! One bounded historical source per turn, with a cursor independent of the live 31-day scan.
use super::{ResolverPricing, claude_mirrors, experiment_roots, fixed_scanner, scan_state};
use crate::agent_transport::Agents;
use aac_store::{
    activity, analytics_cache, collect, contract, engine::Engine, error::Result, report,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const LOOKBACK: i64 = contract::EARLIEST_MS as i64;
const INTERVAL_MS: i64 = 120_000;
const TRANSPORT_MS: u64 = 60_000;
const MAX_ATTEMPTS: u8 = 60;

#[derive(Default)]
pub(super) struct Job {
    next_due: i64,
    next_source: usize,
    attempts: HashMap<(String, String), Attempts>,
}

#[derive(Default)]
struct Attempts {
    scans: u8,
    partial_published: bool,
}

#[derive(Clone, Copy)]
struct Source<'a> {
    computer: &'a str,
    tool: &'a str,
    epoch: u64,
}

pub(super) fn rescan_legacy_source(
    store: &Engine,
    host: &str,
    tool: &str,
    epoch: u64,
) -> Result<()> {
    let source = Source {
        computer: host,
        tool,
        epoch,
    };
    analytics_cache::remove(store, &source.checkpoint())?;
    store.set_meta(&source.ready_marker(), "")?;
    store.set_meta(&source.marker(), "")
}

impl<'a> Source<'a> {
    fn marker(&self) -> String {
        format!("analytics_history_v1:{}:{}", self.computer, self.tool)
    }

    fn checkpoint(&self) -> String {
        format!("history/{}/{}/{}", self.epoch, self.computer, self.tool)
    }

    fn done(&self, store: &Engine) -> Result<bool> {
        Ok(store.get_meta(&self.marker())?.as_deref() == Some(&self.epoch.to_string()))
    }

    fn load(&self, store: &Engine) -> Result<scan_state::Cache> {
        match analytics_cache::get(store, &self.checkpoint())? {
            Some(bytes) => Ok(serde_json::from_slice(&bytes)?),
            None => Ok(scan_state::Cache::default()),
        }
    }

    fn ready_marker(&self) -> String {
        format!("{}:scanned", self.marker())
    }

    fn ready(&self, store: &Engine) -> Result<bool> {
        Ok(store.get_meta(&self.ready_marker())?.as_deref() == Some(&self.epoch.to_string()))
    }

    fn key(&self) -> (String, String) {
        (self.computer.to_owned(), self.tool.to_owned())
    }

    fn accept(
        &self,
        store: &Engine,
        mut cache: scan_state::Cache,
        response: &Value,
        complete: bool,
        now: i64,
        pricing: &dyn collect::Pricing,
    ) -> Result<bool> {
        let complete = cache.apply(self.tool, response, complete, LOOKBACK);
        if self.tool == "claude" {
            // A later computer can reveal the original of a mirror seen in this scan.
            analytics_cache::put(store, &self.checkpoint(), &cache.encode()?)?;
            let mut part = self.claude_part(cache, 0);
            super::claude_contributions::reconcile(
                store,
                std::slice::from_ref(&part),
                now,
                pricing,
            )?;
            claude_mirrors::keep_historical_originals(store, &mut part)?;
            if !part.rows.is_empty() || !part.sessions.is_empty() {
                self.publish(store, &part.rows, &part.sessions, false, now, pricing)?;
            }
            if complete {
                store.set_meta(&self.ready_marker(), &self.epoch.to_string())?;
            }
            return Ok(complete);
        }
        let checkpoint = (!complete).then(|| cache.encode()).transpose()?;
        self.publish(store, &cache.rows, &cache.srows, complete, now, pricing)?;
        if let Some(checkpoint) = checkpoint {
            analytics_cache::put(store, &self.checkpoint(), &checkpoint)?;
        }
        Ok(complete)
    }

    fn claude_part(
        &self,
        mut cache: scan_state::Cache,
        state_index: usize,
    ) -> claude_mirrors::Part<'a> {
        claude_mirrors::Part {
            host: self.computer,
            state_index,
            rows: cache.rows,
            sessions: cache.srows,
            fingerprints: cache.fingerprints.remove("claude").unwrap_or(Value::Null),
        }
    }

    fn publish(
        &self,
        store: &Engine,
        rows: &[Value],
        sessions: &[Value],
        complete: bool,
        now: i64,
        pricing: &dyn collect::Pricing,
    ) -> Result<()> {
        let worker =
            collect::worker_from_scan(rows, sessions, self.tool, self.computer, now, pricing);
        let value = report::rows_report(&report::RowsReportArgs {
            computer: self.computer,
            tool: self.tool,
            data: &[worker],
            kind: "backfill",
            read_at: now,
            complete,
            cause: None,
            now,
        });
        commit_report(store, value)?;
        if complete {
            self.mark(store, "done", None, now)?;
            store.set_meta(&self.marker(), &self.epoch.to_string())?;
            analytics_cache::remove(store, &self.checkpoint())?;
        }
        Ok(())
    }

    fn mark(&self, store: &Engine, state: &str, cause: Option<&str>, now: i64) -> Result<()> {
        activity::backfill_mark(
            store,
            self.computer,
            self.tool,
            activity::BackfillRow {
                state: state.to_owned(),
                cause: cause.map(str::to_owned),
                at: now as u64,
                ..Default::default()
            },
        )
    }
}

impl Job {
    fn publish_claude(
        &mut self,
        store: &Engine,
        sources: &[Source<'_>],
        now: i64,
        pricing: &dyn collect::Pricing,
    ) -> Result<()> {
        let mut pending = Vec::new();
        for source in sources {
            if source.done(store)? {
                continue;
            }
            let ready = source.ready(store)?;
            if !ready {
                let Some(attempts) = self.attempts.get(&source.key()) else {
                    return Ok(());
                };
                if attempts.scans < MAX_ATTEMPTS {
                    return Ok(());
                }
                if attempts.partial_published {
                    continue;
                }
            }
            pending.push((*source, ready));
        }
        if pending.is_empty() {
            return Ok(());
        }
        let mut parts = Vec::with_capacity(pending.len());
        let mut states = Vec::with_capacity(pending.len());
        for (source, ready) in &pending {
            parts.push(source.claude_part(source.load(store)?, states.len()));
            let state = if *ready { "ok" } else { "unreadable" };
            states.push(json!({ "tool": "claude", "host": source.computer, "state": state }));
        }
        super::claude_contributions::reconcile(store, &parts, now, pricing)?;
        let previous = claude_mirrors::load(store, claude_mirrors::HISTORY)?;
        let owners = claude_mirrors::choose(
            &mut parts,
            &mut states,
            &claude_mirrors::Owners::new(),
            &previous,
        );
        if owners != *previous {
            // Persist the choices before publishing. A crash between computers must
            // retry with the same choices, even after a completed checkpoint is removed.
            claude_mirrors::save(store, claude_mirrors::HISTORY, &owners)?;
        }
        for ((source, ready), part) in pending.into_iter().zip(parts) {
            super::claude_contributions::remember(store, &part, "backfill", now, pricing)?;
            source.publish(store, &part.rows, &part.sessions, ready, now, pricing)?;
            if !ready {
                self.attempts
                    .entry(source.key())
                    .or_default()
                    .partial_published = true;
            }
        }
        Ok(())
    }

    pub(super) async fn run(
        &mut self,
        store: &Engine,
        agents: &Arc<Agents>,
        root: &Path,
        experiments: Option<&experiment_roots::Snapshot>,
        now: i64,
    ) {
        if now < self.next_due {
            return;
        }
        self.next_due = now.saturating_add(INTERVAL_MS);
        let Ok(epoch) = activity::recovery_epoch(store) else {
            return;
        };
        let hosts = agents.usage_hosts().await;
        let candidates: Vec<_> = hosts
            .iter()
            .filter(|host| host.agent_available || fixed_scanner::eligible(host))
            .flat_map(|host| aac_providers::scanner_kinds().map(move |kind| (host, kind)))
            .collect();
        let pricing = ResolverPricing::new(root);
        let claude_sources: Vec<_> = candidates
            .iter()
            .filter(|(_, tool)| *tool == "claude")
            .map(|(host, tool)| Source {
                computer: &host.id,
                tool,
                epoch,
            })
            .collect();
        if let Err(error) = self.publish_claude(store, &claude_sources, now, &pricing) {
            tracing::warn!(
                code = error.code,
                "Claude historical scans could not be committed"
            );
        }
        for _ in 0..candidates.len() {
            let (host, tool) = candidates[self.next_source % candidates.len()];
            self.next_source = self.next_source.wrapping_add(1);
            let source = Source {
                computer: &host.id,
                tool,
                epoch,
            };
            let local = host.alias.is_none();
            if local
                && experiments.map_or_else(
                    || experiment_roots::tracks(tool),
                    |snapshot| snapshot.discovering(tool),
                )
            {
                continue;
            }
            let attempts = self.attempts.entry(source.key()).or_default();
            if attempts.scans >= MAX_ATTEMPTS
                || !matches!(source.done(store), Ok(false))
                || tool == "claude" && !matches!(source.ready(store), Ok(false))
            {
                continue;
            }
            attempts.scans += 1;
            let Ok(cache) = source.load(store) else {
                continue;
            };
            let preferences = crate::account_reads::preferences(root);
            let (mut extra, mut overflow, _) = super::extra_roots(preferences.as_ref(), &host.id);
            let experiments = experiments.filter(|_| local);
            if let Some(experiments) = experiments {
                experiments.add_zcode_paths(&mut extra, &mut overflow);
            }
            let kinds = [tool.to_owned()];
            let mut request = super::scan_request(
                &kinds,
                LOOKBACK,
                extra,
                false,
                experiments
                    .map(experiment_roots::Snapshot::scanner_roots)
                    .as_ref(),
            );
            request["deadlineMs"] = Value::from(TRANSPORT_MS - 5_000);
            let batches =
                scan_state::requests(request, &kinds, &cache.fingerprints, &HashMap::new(), now);
            let Some((_, request)) = batches.into_iter().next() else {
                return;
            };
            let call_id = format!("history-{}-{tool}-{now}", host.id);
            let answer = super::scan_host(agents, host, &request, TRANSPORT_MS, &call_id).await;
            let mut done = false;
            if let Ok(answer) = answer
                && super::valid_scan_answer(&answer, &kinds)
            {
                let readable = super::has_readable_records(&answer, &cache, tool);
                let state = super::scan_state(&answer, tool, readable).0;
                let complete = matches!(state, "ok" | "no_usage" | "not_installed")
                    && !overflow.contains(tool)
                    && !experiments.is_some_and(|snapshot| snapshot.limited(tool));
                match source.accept(store, cache, &answer, complete, now, &pricing) {
                    Ok(complete) => done = complete,
                    Err(error) => tracing::warn!(
                        code = error.code,
                        "Analytics historical scan could not be committed"
                    ),
                }
            }
            if !done && attempts.scans == MAX_ATTEMPTS {
                let _ = source.mark(store, "failed", Some("pass_limit"), now);
            }
            break;
        }
    }
}

/// Commit measured rows within the store report bounds before issuing a recovery receipt.
pub(super) fn commit_report(store: &Engine, value: Value) -> Result<()> {
    for mut validated in report::contract_reports(&[value])? {
        let sessions = std::mem::take(&mut validated.sessions);
        // Each report is below the contract cap. Only the last batch declares coverage.
        let total = sessions.chunks(contract::MAX_SESSIONS).len().max(1);
        let mut cells = std::mem::take(&mut validated.cells);
        for index in 0..total {
            let mut part = validated.clone();
            part.cells = std::mem::take(&mut cells);
            let from = index * contract::MAX_SESSIONS;
            part.sessions =
                sessions[from..(from + contract::MAX_SESSIONS).min(sessions.len())].to_vec();
            if index + 1 < total {
                part.state = "partial".into();
                part.complete = false;
                part.cover_from = None;
            }
            let imported = activity::ingest(store, &[part])?;
            if imported.accepted != 1 || !imported.refused.is_empty() {
                return Err(aac_store::error::Error::new(
                    "invalid",
                    "Historical usage report refused",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod matrix;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_muse_copy_keeps_one_store_session_in_both_path_orders() {
        muse_copy_keeps_one_store_session(true);
    }

    /// RECHECK-ROUND3 M1: the same with the owner's day still open (nothing settled, no closed
    /// day): the published owner keeps one Store session in both path orders.
    #[test]
    fn unsettled_muse_copy_keeps_one_store_session_in_both_path_orders() {
        muse_copy_keeps_one_store_session(false);
    }

    fn muse_copy_keeps_one_store_session(settle: bool) {
        for (first, second) in [("a", "b"), ("b", "a")] {
            for hard_link in [false, true] {
                let fixture = tempfile::tempdir().expect("fixture home");
                let home = fixture.path();
                let db = home.join("store");
                let open = || {
                    aac_store::ops::open_engine(&db.to_string_lossy(), None, None)
                        .expect("open Store")
                };
                let now = aac_store::engine::now_ms() as i64;
                let at = (now - 8 * super::super::DAY).div_euclid(3_600_000) * 3_600_000;
                let sessions = home.join(".local/share/muse/sessions");
                let paths = [first, second].map(|name| sessions.join(name).join("session.jsonl"));
                for path in &paths {
                    std::fs::create_dir_all(path.parent().expect("session directory"))
                        .expect("create session directory");
                }
                let log = json!({"recorded_at":at / 1000,
                    "payload":{"event":{"kind":"model_completed","model":"glm-5",
                    "usage":{"input_tokens":100,"output_tokens":10,"cache_read_tokens":0,"cache_write_tokens":0}}}});
                std::fs::write(&paths[0], format!("{log}\n")).expect("original Muse log");
                let age = |path: &Path| {
                    std::fs::File::options()
                        .write(true)
                        .open(path)
                        .expect("open fixture log")
                        .set_modified(
                            std::time::UNIX_EPOCH + std::time::Duration::from_millis(at as u64),
                        )
                        .expect("set fixture mtime");
                };
                age(&paths[0]);
                let scan = |cache: &scan_state::Cache| {
                    let mut prints = cache.fingerprints.clone();
                    prints.insert(
                        "_store".into(),
                        json!({"settledBefore":{"muse":if settle { at + super::super::DAY } else { 0 }},
                            "settledSlot":1}),
                    );
                    aac_usage::protocol::scan(
                        &aac_usage::protocol::parse_request(
                            json!({"kinds":["muse"],"minDateMs":0,"fingerprints":prints})
                                .to_string()
                                .as_bytes(),
                        )
                        .expect("request"),
                        home,
                        &aac_usage::discover::Env::default(),
                    )
                };
                let publish = |store: &Engine, cache: &mut scan_state::Cache| {
                    assert!(cache.apply("muse", &scan(cache), true, 0));
                    let source = Source {
                        computer: "nas1",
                        tool: "muse",
                        epoch: 0,
                    };
                    source
                        .publish(
                            store,
                            &cache.rows,
                            &cache.srows,
                            false,
                            now,
                            &ResolverPricing::new(home),
                        )
                        .expect("publish Muse");
                };
                let read = |store: &Engine| {
                    let body = json!({"fromMs":at,"toMs":at+3_600_000}).to_string();
                    let result = aac_store::ops::call(store, "activityRange", &body)
                        .expect("activity report");
                    assert_eq!(
                        result["sessions"]["total"], 1,
                        "{first}/{second}/{hard_link}/settled {settle}: {result}"
                    );
                    assert_eq!(result["rows"][0][5], 100, "{result}");
                    assert_eq!(result["rows"][0][6], 10, "{result}");
                    assert_eq!(result["rows"][0][12], 1, "{result}");
                };
                let store = open();
                let mut cache = scan_state::Cache::default();
                publish(&store, &mut cache);
                publish(&store, &mut cache);
                if settle {
                    activity::close_days(&store, now as u64).expect("settle Muse hour");
                }
                read(&store);
                let snapshot = cache.encode().expect("scanner checkpoint");
                store.close().expect("close Store");
                drop(store);
                if hard_link {
                    std::fs::hard_link(&paths[0], &paths[1]).expect("create identical hard link");
                } else {
                    std::fs::copy(&paths[0], &paths[1]).expect("copy original");
                    age(&paths[1]);
                }
                let store = open();
                let mut cache =
                    serde_json::from_slice(&snapshot).expect("decode scanner checkpoint");
                publish(&store, &mut cache);
                read(&store);
            }
        }
    }

    #[test]
    fn analytics_historical_scan_recovers_old_log_and_resumes_after_restart() {
        let home = tempfile::tempdir().expect("temporary scan home");
        let root = home.path();
        let store_path = root.join("store");
        let open = || {
            aac_store::ops::open_engine(&store_path.to_string_lossy(), None, None)
                .expect("open fixture store")
        };
        let now = aac_store::engine::now_ms() as i64;
        let at = (now - 60 * super::super::DAY).div_euclid(3_600_000) * 3_600_000;
        let stamp = aac_num::ecma::to_iso_string(at).expect("old usage time");
        let log = root.join(".codex/sessions/fixture/rollout-2026-09-20T12-00-00-3e43c7f5-744b-4790-9d0b-06042ecf0a20.jsonl");
        std::fs::create_dir_all(log.parent().expect("rollout directory"))
            .expect("create fixture directory");
        let rows = [
            json!({"type":"session_meta","payload":{"id":"recovered-session","originator":"codex_cli_rs","cli_version":"1"}}),
            json!({"type":"turn_context","payload":{"model":"gpt-6.1-sol"}}),
            json!({"timestamp":stamp,"type":"event_msg","payload":{
                "type":"token_count","info":{"total_token_usage":{"input_tokens":20,"output_tokens":2}}
            }}),
            json!({"timestamp":stamp,"type":"event_msg","payload":{
                "type":"token_count","info":{"total_token_usage":{"input_tokens":70,"output_tokens":7}}
            }}),
        ];
        let mut text = rows
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        std::fs::write(log, text).expect("write surviving historical rollout");
        let kinds = ["codex".to_owned()];
        let scan = |cutoff, cache: &scan_state::Cache, budget| {
            let request = super::super::scan_request(&kinds, cutoff, json!({}), false, None);
            let (_, request) =
                scan_state::requests(request, &kinds, &cache.fingerprints, &HashMap::new(), now)
                    .into_iter()
                    .next()
                    .expect("historical scanner request");
            let mut request = aac_usage::protocol::parse_request(request.to_string().as_bytes())
                .expect("parse scan request");
            request.line_budget = budget;
            aac_usage::protocol::scan(&request, root, &aac_usage::discover::Env::default())
        };
        assert_eq!(
            scan(now - 31 * super::super::DAY, &Default::default(), None)["rows"],
            json!([])
        );
        let source = Source {
            computer: "ubuntu",
            tool: "codex",
            epoch: 0,
        };
        let pricing = ResolverPricing::new(root);
        let store = open();
        let cache = source.load(&store).expect("fresh checkpoint");
        let partial = scan(LOOKBACK, &cache, Some(3));
        assert!(
            !source
                .accept(&store, cache, &partial, false, now, &pricing)
                .expect("save first slice")
        );
        assert!(!source.done(&store).expect("read completion marker"));
        store.close().expect("close after sliced scan");
        drop(store);
        let store = open();
        let cache = source.load(&store).expect("resumed checkpoint");
        let answer = scan(LOOKBACK, &cache, None);
        assert_eq!(answer["rows"][0]["i"], 50);
        assert!(
            source
                .accept(&store, cache, &answer, true, now, &pricing)
                .expect("commit historical scan")
        );
        assert!(source.done(&store).expect("committed completion marker"));
        let range = aac_store::ops::call(
            &store,
            "activityRange",
            &json!({
                "fromMs":at, "toMs":at + 3_600_000
            })
            .to_string(),
        )
        .expect("read recovered hour");
        assert_eq!(range["rows"][0][5], 70);
        assert_eq!(range["rows"][0][6], 7);
        assert!(
            activity::settled_before(&store, "ubuntu")
                .expect("live settle line")
                .is_empty()
        );
        store.close().expect("close after completed scan");
        drop(store);
        let store = open();
        assert!(source.done(&store).expect("receipt survives restart"));
        let rebuilt = Source { epoch: 1, ..source };
        assert!(
            !rebuilt
                .done(&store)
                .expect("old receipt expires on rebuild")
        );
    }

    #[test]
    fn recheck_offline_original_then_returns() {
        offline_original_then_returns(false);
    }

    #[test]
    fn recheck_migrated_mirror_then_original_returns() {
        offline_original_then_returns(true);
    }

    fn offline_original_then_returns(migrate: bool) {
        let home = tempfile::tempdir().expect("temporary scan home");
        let root = home.path();
        let store_path = root.join("store");
        let open = || {
            aac_store::ops::open_engine(&store_path.to_string_lossy(), None, None)
                .expect("open fixture store")
        };
        let now = aac_store::engine::now_ms() as i64;
        let at = (now - 60 * super::super::DAY).div_euclid(3_600_000) * 3_600_000;
        let event = json!({
            "type":"assistant",
            "timestamp":aac_num::ecma::to_iso_string(at).expect("old usage time"),
            "sessionId":"shared-claude-session",
            "message":{
                "id":"same-assistant-message","model":"claude-opus-4-5",
                "usage":{"input_tokens":70,"output_tokens":30}
            }
        });
        let scan = |computer: &str, directory: &str, mirrored: bool| {
            let home = root.join(computer);
            let project = home.join(".claude/projects").join(directory);
            std::fs::create_dir_all(&project).expect("project directory");
            std::fs::write(project.join("same-log.jsonl"), format!("{event}\n"))
                .expect("assistant log");
            let request = json!({"kinds":["claude"], "minDateMs":LOOKBACK, "cwd":false});
            let request = aac_usage::protocol::parse_request(request.to_string().as_bytes())
                .expect("scanner request");
            let answer =
                aac_usage::protocol::scan(&request, &home, &aac_usage::discover::Env::default());
            let row = &answer["rows"][0];
            assert_eq!(row["i"], 70, "{computer}: {answer}");
            let fingerprint =
                &answer["kinds"]["claude"]["fingerprints"][row["f"].as_str().expect("file key")];
            assert_eq!(fingerprint["claudeMirror"], mirrored);
            assert!(fingerprint["claudeName"].is_string());
            answer
        };
        let original = Source {
            computer: "nas1",
            tool: "claude",
            epoch: 0,
        };
        let mirror = Source {
            computer: "odroid",
            ..original
        };
        let pricing = ResolverPricing::new(root);
        let submit = |store: &Engine, source: &Source<'_>, answer: &Value| {
            let cache = source.load(store).expect("load source checkpoint");
            source
                .accept(store, cache, answer, true, now, &pricing)
                .expect("accept Claude scan");
        };
        let store = open();
        let read = |store: &Engine| {
            aac_store::ops::call(
                store,
                "activityRange",
                &json!({"fromMs":at, "toMs":at + 3_600_000}).to_string(),
            )
            .expect("read historical Claude hour")
        };
        let mirror_answer = scan("odroid", "ssh-12345678-1234-1234-1234-123456789abc", true);
        let sources = [mirror, original];
        if migrate {
            let dir = root.join("cache/analytics-remote-v1");
            std::fs::create_dir_all(&dir).expect("legacy cache directory");
            std::fs::write(
                dir.join("odroid.json"),
                json!({
                    "version": 8, "lastScanAt": aac_num::ecma::to_iso_string(now),
                    "fingerprints": {"claude": mirror_answer["kinds"]["claude"]["fingerprints"]},
                    "rows": mirror_answer["rows"], "srows": mirror_answer["srows"],
                })
                .to_string(),
            )
            .expect("legacy mirror cache");
            let migration = super::super::migration::Migration {
                store: &store,
                root,
                local: Some("nas1"),
                local_originals: Default::default(),
                now,
                pricing: ResolverPricing::new(root),
            };
            assert!(migration.run().expect("migrate available mirror"));
        } else {
            submit(&store, &mirror, &mirror_answer);
            Job::default()
                .publish_claude(&store, &[mirror], now, &pricing)
                .expect("publish the available mirror");
        }
        let first = read(&store);
        assert_eq!(first["rows"][0][5], 70);
        assert_eq!(first["rows"][0][6], 30);
        activity::close_days(&store, now as u64).expect("settle the published mirror hour");
        store.close().expect("close between scanners");
        drop(store);

        let store = open();
        submit(&store, &original, &scan("nas1", "original-project", false));
        Job::default()
            .publish_claude(
                &store,
                if migrate { &sources[1..] } else { &sources },
                now,
                &pricing,
            )
            .expect("publish after the original returns");
        let once = read(&store);
        let rows = once["rows"].as_array().expect("historical rows");
        let totals: [u64; 3] = [5, 6, 12].map(|index| {
            rows.iter()
                .map(|row| row[index].as_u64().expect("historical count"))
                .sum()
        });
        assert_eq!(totals, [70, 30, 1], "one log counted once: {once}");
    }

    #[test]
    fn analytics_historical_claude_mirror_is_counted_once_across_computers() {
        let home = tempfile::tempdir().expect("temporary scan home");
        let root = home.path();
        let store_path = root.join("store");
        let open = || {
            aac_store::ops::open_engine(&store_path.to_string_lossy(), None, None)
                .expect("open fixture store")
        };
        let now = aac_store::engine::now_ms() as i64;
        let at = (now - 60 * super::super::DAY).div_euclid(3_600_000) * 3_600_000;
        let event = json!({
            "type":"assistant",
            "timestamp":aac_num::ecma::to_iso_string(at).expect("old usage time"),
            "sessionId":"shared-claude-session",
            "message":{
                "id":"same-assistant-message","model":"claude-opus-4-5",
                "usage":{"input_tokens":70,"output_tokens":30}
            }
        });
        let scan = |computer: &str, directory: &str, mirrored: bool| {
            let home = root.join(computer);
            let project = home.join(".claude/projects").join(directory);
            std::fs::create_dir_all(&project).expect("project directory");
            std::fs::write(project.join("same-log.jsonl"), format!("{event}\n"))
                .expect("assistant log");
            let request = json!({"kinds":["claude"], "minDateMs":LOOKBACK, "cwd":false});
            let request = aac_usage::protocol::parse_request(request.to_string().as_bytes())
                .expect("scanner request");
            let answer =
                aac_usage::protocol::scan(&request, &home, &aac_usage::discover::Env::default());
            let row = &answer["rows"][0];
            assert_eq!(row["i"], 70, "{computer}: {answer}");
            let fingerprint =
                &answer["kinds"]["claude"]["fingerprints"][row["f"].as_str().expect("file key")];
            assert_eq!(fingerprint["claudeMirror"], mirrored);
            assert!(fingerprint["claudeName"].is_string());
            answer
        };
        let original = Source {
            computer: "nas1",
            tool: "claude",
            epoch: 0,
        };
        let mirror = Source {
            computer: "odroid",
            ..original
        };
        let pricing = ResolverPricing::new(root);
        let submit = |store: &Engine, source: &Source<'_>, answer: &Value| {
            let cache = source.load(store).expect("load source checkpoint");
            source
                .accept(store, cache, answer, true, now, &pricing)
                .expect("accept Claude scan");
        };
        let store = open();
        submit(
            &store,
            &mirror,
            &scan("odroid", "ssh-12345678-1234-1234-1234-123456789abc", true),
        );
        let read = |store: &Engine| {
            aac_store::ops::call(
                store,
                "activityRange",
                &json!({"fromMs":at, "toMs":at + 3_600_000}).to_string(),
            )
            .expect("read historical Claude hour")
        };
        let sources = [mirror, original];
        Job::default()
            .publish_claude(&store, &sources, now, &pricing)
            .expect("wait for the other scanner");
        assert_eq!(
            read(&store)["rows"],
            json!([]),
            "the mirror is still pending"
        );
        store.close().expect("close between scanners");
        drop(store);

        let store = open();
        submit(&store, &original, &scan("nas1", "original-project", false));
        let early = read(&store);
        assert_eq!(
            early["rows"][0][5], 70,
            "the original is visible before the batch closes"
        );
        Job::default()
            .publish_claude(&store, &sources, now, &pricing)
            .expect("publish both results after restart");
        let once = read(&store);
        let rows = once["rows"].as_array().expect("historical cells");
        assert_eq!(rows.len(), 1, "the original and mirror are one log: {once}");
        assert_eq!(rows[0][5], 70);
        assert_eq!(rows[0][6], 30);
        assert_eq!(once["sessions"]["total"], 1);
        assert_eq!(once["sessions"]["sample"][0]["computer"], "nas1");
        let owners = claude_mirrors::load(&store, claude_mirrors::HISTORY).expect("redb owners");
        assert_eq!(
            owners.values().map(String::as_str).collect::<Vec<_>>(),
            vec!["nas1"]
        );
        assert!(mirror.done(&store).expect("mirror scan receipt"));
        assert!(original.done(&store).expect("original scan receipt"));
        store.close().expect("close after recording");
        drop(store);

        let store = open();
        Job::default()
            .publish_claude(&store, &sources, now, &pricing)
            .expect("repeat completed historical pass");
        assert_eq!(read(&store)["rows"], once["rows"]);
    }

    #[test]
    fn analytics_historical_omp_attributes_kimi_through_the_shared_cursor() {
        let home = tempfile::tempdir().expect("temporary OMP home");
        let root = home.path();
        let engine = aac_store::ops::open_engine(&root.join("store").to_string_lossy(), None, None)
            .expect("open fixture Store");
        let now = aac_store::engine::now_ms() as i64;
        let at = now - 60 * super::super::DAY;
        let stamp = aac_num::ecma::to_iso_string(at).expect("historical time");
        let directory = root.join(".omp/agent/sessions");
        std::fs::create_dir_all(&directory).expect("session directory");
        let assistant = |model, provider, input, output| {
            json!({
                "type":"message", "timestamp":stamp,
                "message":{"role":"assistant","model":model,"provider":provider,
                    "usage":{"input":input,"output":output,"cacheRead":0,"cacheWrite":0}}
            })
        };
        let rows = [
            assistant("kimi-k3", "moonshotai", 40, 10),
            assistant("qwen3-max", "alibaba", 80, 20),
        ];
        let body = rows
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(
            directory.join("2026-09-21T13-00-00_kimi-composed.jsonl"),
            body,
        )
        .expect("write OMP usage");
        let request = json!({"kinds":["omp"],"minDateMs":LOOKBACK,"cwd":false});
        let request = aac_usage::protocol::parse_request(request.to_string().as_bytes())
            .expect("historical scan request");
        let answer =
            aac_usage::protocol::scan(&request, root, &aac_usage::discover::Env::default());
        assert_eq!(
            answer["rows"].as_array().expect("OMP rows").len(),
            2,
            "{answer}"
        );
        let source = Source {
            computer: "ubuntu",
            tool: "omp",
            epoch: 0,
        };
        let pricing = ResolverPricing::new(root);
        for _ in 0..2 {
            assert!(
                source
                    .accept(
                        &engine,
                        source.load(&engine).expect("checkpoint"),
                        &answer,
                        true,
                        now,
                        &pricing
                    )
                    .expect("commit historical OMP")
            );
        }
        assert!(source.done(&engine).expect("historical scanner receipt"));
        let cells = activity::dump_cells(&engine).expect("stored OMP activity");
        assert_eq!(cells.len(), 2);
        let find = |model: &str| {
            &cells
                .iter()
                .find(|(computer, tool, row)| {
                    computer == "ubuntu" && tool == "omp" && row.m == model
                })
                .expect("OMP model")
                .2
        };
        let kimi = find("kimi-k3");
        assert_eq!((kimi.r.as_str(), kimi.i, kimi.o), ("kimi-code", 40, 10));
        let qwen = find("qwen3-max");
        assert_ne!(qwen.r, "kimi-code");
        assert_eq!((qwen.i, qwen.o), (80, 20));
    }
}
