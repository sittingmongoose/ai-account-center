//! G16b: the owner's answer store in the service. With the owned Status group on, this process
//! holds `<ccs>/status/answers.lock` and is the store's only writer (crates/aac-status
//! `answers_engine`, the TS `StatusAnswers` ported whole): it runs the 60 s retry pass the TS
//! timer ran (`status-service.ts` `start()`), and records each send the front door forwards as
//! `PUT /internal/status/answers`. The front door keeps request admission and reads the history
//! back from the journal as any other
//! process does. Rings go out with the engine unlocked, as the TS ring ran outside its queue.
//!
//! The front door hands answers to its own TS store, for the rest of its process, the first time a
//! forward cannot reach this service; it then takes the lock over once this process is gone.

use aac_status::answer_delivery::RingResult;
use aac_status::answers_engine::{
    AnswerHost, AnswersEngine, Budget, CardRead, Prepared, RingClaim, Timing, closing_card_versions,
};
use aac_status::produce::answers_feed::{AnswersFeed, merge_shared};
use aac_status::share::NodeFs;
use aac_status::share::reports::{AgentReportsReader, Limits};
use serde_json::{Value as Json, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The retry cadence (`COLLECT_EVERY_MS`).
pub(crate) const RETRY_EVERY_MS: u64 = 60_000;
/// The largest send body the front door forwards (its own JSON body limit is smaller).
pub(crate) const SEND_CAP: usize = 2 * 1024 * 1024;

/// The service's host: a fresh card read on the share within its limit, pm-mail rings, the
/// system clock and random ids.
struct ServiceHost {
    reports_root: PathBuf,
}

impl AnswerHost for ServiceHost {
    fn read_card(&mut self, card_id: &str, limit: Duration) -> CardRead {
        let root = self.reports_root.clone();
        let id = card_id.to_owned();
        let (tx, rx) = std::sync::mpsc::channel();
        // A hung share keeps its thread; the send goes on without it, as the TS deadline did.
        std::thread::spawn(move || {
            let reader = AgentReportsReader::new(root, Limits::default());
            let _ = tx.send(reader.read_card_now(&NodeFs, &id));
        });
        match rx.recv_timeout(limit) {
            Ok(Ok(Some((doc, sha256)))) => CardRead::Card { doc, sha256 },
            Ok(Ok(None)) => CardRead::Absent,
            Ok(Err(_)) => CardRead::Failed,
            Err(_) => CardRead::TimedOut,
        }
    }
}

pub(crate) struct AnswersOwned {
    engine: Mutex<AnswersEngine>,
    /// Rings run on a fresh host so the engine stays unlocked meanwhile.
    ring_host: Mutex<ServiceHost>,
}

fn lock(engine: &Mutex<AnswersEngine>) -> std::sync::MutexGuard<'_, AnswersEngine> {
    engine.lock().unwrap_or_else(|p| p.into_inner())
}

impl AnswersOwned {
    pub(crate) fn new(ccs_dir: PathBuf, reports_root: PathBuf) -> Arc<Self> {
        let store = ccs_dir.join("status");
        Arc::new(Self {
            engine: Mutex::new(AnswersEngine::new(
                store,
                reports_root.clone(),
                Timing::default(),
                Box::new(ServiceHost {
                    reports_root: reports_root.clone(),
                }),
            )),
            ring_host: Mutex::new(ServiceHost { reports_root }),
        })
    }

    /// One ring pass over a claim, the engine unlocked across each ring.
    fn ring(&self, claim: RingClaim, budget: Option<&Budget>) {
        for group in &claim.groups {
            if budget.is_some_and(|b| b.left() == 0) {
                break;
            }
            let ring = match lock(&self.engine).prepare_ring(group) {
                Prepared::Ring(ring) => ring,
                Prepared::Nothing => continue,
                Prepared::NotOwner => break,
            };
            let result: RingResult = {
                let mut host = self.ring_host.lock().unwrap_or_else(|p| p.into_inner());
                host.ring(&ring.to, &ring.subject, &ring.body, ring.timeout)
            };
            lock(&self.engine).finish_ring(&ring, &result);
        }
        lock(&self.engine).release_rings(claim);
    }

    /// `retryPending` on the blocking pool.
    fn retry_blocking(&self) {
        let Some((budget, rings)) = lock(&self.engine).begin_retry() else {
            return;
        };
        let claim = lock(&self.engine).claim_rings(&rings);
        self.ring(claim, Some(&budget));
        lock(&self.engine).end_retry();
    }

    /// `submitSet` for one forwarded send: the per-entry results, then (after the reply) one
    /// ring per asker. The body is `{entries, device, clientSendId}`.
    pub(crate) async fn submit(self: &Arc<Self>, body: &[u8]) -> Result<Json, &'static str> {
        let request: Json = serde_json::from_slice(body).map_err(|_| "shape")?;
        let entries = request
            .get("entries")
            .and_then(Json::as_array)
            .cloned()
            .ok_or("shape")?;
        let device = request
            .get("device")
            .and_then(Json::as_str)
            .ok_or("shape")?
            .to_owned();
        let client = match request.get("clientSendId") {
            None | Some(Json::Null) => None,
            Some(Json::String(id)) => Some(id.clone()),
            Some(_) => return Err("shape"),
        };
        let owner = Arc::clone(self);
        // Never cancelled once started: a record is a write.
        let (results, claim) = tokio::task::spawn_blocking(move || {
            let mut engine = lock(&owner.engine);
            let submitted = engine.submit_set(&entries, &device, client.as_deref());
            match submitted {
                Ok(submitted) => {
                    // The claim passes to the ring in one step, as `submitSet`'s `finally` does.
                    let claim = engine.claim_rings(&submitted.recorded);
                    Ok((submitted.results, claim))
                }
                Err(_) => Err("failed"),
            }
        })
        .await
        .map_err(|_| "failed")??;
        let owner = Arc::clone(self);
        tokio::task::spawn_blocking(move || owner.ring(claim, None));
        Ok(json!({ "results": results }))
    }

    /// G16c: the feed's answer history. The cards' shared copies are scanned into the store first
    /// (`scanSharedAnswers`), then the journal's records and the verified share copies are merged
    /// (`mergeSharedAnswers`), the sends with them and the share map pruned to the open cards.
    pub(crate) fn feed(&self, cards: &[(Json, String)]) -> AnswersFeed {
        let mut engine = lock(&self.engine);
        let history = engine.list().map(|entries| entries.to_vec());
        let sends = engine.send_list();
        let closed = history
            .as_ref()
            .map(|entries| closing_card_versions(entries))
            .unwrap_or_default();
        engine.scan_shared_answers(cards, &closed);
        let shared = engine.shared_answers_for(cards);
        engine.prune_shared_answers(cards);
        match (history, sends) {
            (Ok(history), Ok(sends)) => AnswersFeed {
                answers: merge_shared(&history, &shared),
                sends,
                problem: None,
            },
            _ => AnswersFeed::unavailable(shared),
        }
    }

    /// The exit hook: pending completions and void lines, then the lock.
    pub(crate) fn close(&self) {
        lock(&self.engine).flush_completions();
        aac_status::writer_lock::release_writer_locks();
    }
}

/// The retry timer: at once, then every minute, single-flight.
pub(crate) async fn jobs(owner: Arc<AnswersOwned>, stop: Arc<AtomicBool>) {
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let pass = Arc::clone(&owner);
        let _ = tokio::task::spawn_blocking(move || pass.retry_blocking()).await;
        tokio::time::sleep(Duration::from_millis(RETRY_EVERY_MS)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(root: &std::path::Path) -> (String, String) {
        let doc = json!({
            "schema": "agent-reports/card@1", "id": "vm.lead--Q1", "writer": "vm.lead", "seq": 1,
            "writtenAt": "2026-11-19T06:00:00.000Z", "project": "proj", "stream": "S", "askedBy": "lead",
            "replyTo": {"pmMail": null, "t3Thread": null}, "state": "open", "label": "S-Q1",
            "title": "Question Q1", "now": "Plain words.", "nowAsOf": null, "oldAnswer": null,
            "whatChanged": null,
            "options": [
                {"key": "A", "label": "First", "gets": "Simple", "costs": null, "media": [], "sections": []},
                {"key": "B", "label": "Second", "gets": null, "costs": "More work", "media": [], "sections": []}
            ],
            "recommendation": {"key": "A", "why": "Simpler."}, "multiple": false, "urgency": "today",
            "blocks": null
        });
        let text = format!("{}\n", serde_json::to_string_pretty(&doc).unwrap());
        let dir = root.join("v1/writers/vm.lead/cards");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vm.lead--Q1.json"), &text).unwrap();
        let sha: String = {
            use sha2::Digest;
            sha2::Sha256::digest(text.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        ("vm.lead--Q1".into(), sha)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_forwarded_send_is_recorded_shared_and_the_lock_released_at_close() {
        let scratch = tempfile::tempdir().unwrap();
        let ccs = scratch.path().join("ccs");
        let root = scratch.path().join("reports");
        let (id, sha) = card(&root);
        let owner = AnswersOwned::new(ccs.clone(), root.clone());
        let entry =
            json!({"cardId": id, "cardSeq": 1, "cardSha256": sha, "choice": ["B"], "via": "deck"});
        let body =
            json!({"entries": [entry, {"cardId": "x"}], "device": "Mac", "clientSendId": null});
        let reply = owner.submit(body.to_string().as_bytes()).await.unwrap();
        let results = reply["results"].as_array().unwrap();
        assert_eq!(results[0]["outcome"], "recorded");
        assert_eq!(results[1]["code"], "invalid_entry");
        assert_eq!(
            owner.submit(b"{\"entries\":[]}").await.unwrap_err(),
            "shape"
        );
        // The ring pass (no pm-mail name: a skip, never a ring) runs after the reply.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let journal = std::fs::read_to_string(ccs.join("status/answers.jsonl")).unwrap();
        for line in [
            r#""type":"answer""#,
            r#""type":"send""#,
            r#""step":"share","state":"done""#,
            r#""step":"summary","state":"done""#,
            r#""step":"doorbell","state":"skipped""#,
        ] {
            assert!(journal.contains(line), "{line} in {journal}");
        }
        assert!(root.join("v1/answers/vm.lead--Q1.json").is_file());
        let lock = std::fs::read_to_string(ccs.join("status/answers.lock")).unwrap();
        assert!(lock.contains(&format!("\"pid\":{}", std::process::id())));
        let pass = Arc::clone(&owner);
        tokio::task::spawn_blocking(move || pass.retry_blocking())
            .await
            .unwrap();
        owner.close();
        assert!(!ccs.join("status/answers.lock").exists());
    }
}
