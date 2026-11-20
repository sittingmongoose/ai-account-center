//! One bounded historical source per turn, with a cursor independent of the live 31-day scan.
use super::{ResolverPricing, experiment_roots, fixed_scanner, scan_state};
use crate::agent_transport::Agents;
use aac_store::{
    activity, analytics_cache, collect, contract, engine::Engine, error::Result, report,
};
use serde_json::Value;
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
    attempts: HashMap<(String, String), u8>,
}

struct Source<'a> {
    computer: &'a str,
    tool: &'a str,
    epoch: u64,
}

impl Source<'_> {
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
        let checkpoint = (!complete).then(|| cache.encode()).transpose()?;
        let worker = collect::worker_from_scan(
            &cache.rows,
            &cache.srows,
            self.tool,
            self.computer,
            now,
            pricing,
        );
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
        if let Some(checkpoint) = checkpoint {
            analytics_cache::put(store, &self.checkpoint(), &checkpoint)?;
        } else {
            self.mark(store, "done", None, now)?;
            store.set_meta(&self.marker(), &self.epoch.to_string())?;
            analytics_cache::remove(store, &self.checkpoint())?;
        }
        Ok(complete)
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
            let attempts = self
                .attempts
                .entry((host.id.clone(), tool.to_owned()))
                .or_default();
            if *attempts >= MAX_ATTEMPTS || !matches!(source.done(store), Ok(false)) {
                continue;
            }
            *attempts += 1;
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
                let state = super::scan_state(&answer, tool).0;
                let complete = matches!(state, "ok" | "no_usage" | "not_installed")
                    && !overflow.contains(tool)
                    && !experiments.is_some_and(|snapshot| snapshot.limited(tool));
                match source.accept(
                    store,
                    cache,
                    &answer,
                    complete,
                    now,
                    &ResolverPricing::new(root),
                ) {
                    Ok(complete) => done = complete,
                    Err(error) => tracing::warn!(
                        code = error.code,
                        "Analytics historical scan could not be committed"
                    ),
                }
            }
            if !done && *attempts == MAX_ATTEMPTS {
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
mod tests {
    use super::*;
    use serde_json::json;

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
}
