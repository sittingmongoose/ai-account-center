//! Bounded compressed Analytics answers and per-filter singleflight with stale-while-revalidate.
use crate::compress::CachedBody;
use aac_store::{analytics_cache, engine::Engine};
use aac_views::decline::Decline;
use std::{
    collections::VecDeque,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::Mutex as AsyncMutex;

const MAX_ENTRIES: usize = 4;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_RAW: usize = 16 * 1024 * 1024;
const MAX_STALE_MS: i64 = 300_000;

#[derive(Clone)]
struct Entry {
    version: String,
    at: i64,
    body: CachedBody,
    stale: CachedBody,
}
#[derive(Default)]
struct Slot {
    value: Mutex<Option<Entry>>,
    flight: Arc<AsyncMutex<()>>,
    revision: std::sync::atomic::AtomicU64,
}
type SlotEntry = (u64, String, Arc<Slot>);

#[derive(Default)]
pub(crate) struct Saved {
    slots: Mutex<VecDeque<SlotEntry>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Disk {
    filter: String,
    version: String,
    at: i64,
    raw: Vec<u8>,
}

fn body(raw: Vec<u8>) -> CachedBody {
    let etag = crate::compress::weak_etag_sha1(&raw);
    CachedBody::from_raw_with_identity(raw, etag).0
}
fn entry(version: String, at: i64, raw: Vec<u8>) -> Result<Entry, Decline> {
    let mut value: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| Decline::Shape)?;
    let map = value.as_object_mut().ok_or(Decline::Shape)?;
    map.insert("stale".into(), true.into());
    map.insert("refreshing".into(), true.into());
    if let Some(activity) = map
        .get_mut("activity")
        .and_then(serde_json::Value::as_object_mut)
    {
        activity.insert("refreshing".into(), true.into());
    }
    let stale = body(serde_json::to_vec(&value).map_err(|_| Decline::Internal)?);
    Ok(Entry {
        version,
        at,
        body: body(raw),
        stale,
    })
}
fn disk_key(filter: &str) -> String {
    // Four fixed slots bound persistent storage, including arbitrary custom ranges.
    let hash = aac_core::sha256_hex(filter.as_bytes());
    let slot = u8::from_str_radix(&hash[..2], 16).unwrap_or(0) % 4;
    format!("answer/analytics/v1/{slot}")
}
fn load(engine: &Engine, filter: &str) -> Option<Entry> {
    use std::io::Read;
    let packed = analytics_cache::get(engine, &disk_key(filter)).ok()??;
    let decoder = zstd::stream::read::Decoder::new(packed.as_slice()).ok()?;
    let mut bytes = Vec::new();
    decoder
        .take((MAX_RAW + 4096) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    let saved: Disk = aac_store::msgpack::from_bytes(&bytes).ok()?;
    if saved.filter != filter || saved.raw.len() > MAX_RAW {
        return None;
    }
    entry(saved.version, saved.at, saved.raw).ok()
}
fn persist(engine: &Engine, filter: String, version: String, at: i64, raw: Vec<u8>) {
    let key = disk_key(&filter);
    let saved = Disk {
        filter,
        version,
        at,
        raw,
    };
    if let Ok(bytes) = aac_store::msgpack::to_bytes(&saved)
        && let Ok(packed) = zstd::stream::encode_all(bytes.as_slice(), 1)
    {
        let _ = analytics_cache::put(engine, &key, &packed);
    }
}

impl Saved {
    fn slot(&self, engine: &Engine, filter: &str) -> Arc<Slot> {
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((_, _, slot)) = slots
            .iter()
            .find(|(id, key, _)| *id == engine.incarnation() && key == filter)
        {
            return slot.clone();
        }
        // Never evict a composing slot: otherwise a flood could compose the same query twice.
        let slot = Arc::new(Slot::default());
        slots.push_back((engine.incarnation(), filter.to_owned(), slot.clone()));
        slot
    }
    fn trim(&self) {
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        let mut bytes: usize = slots
            .iter()
            .map(|(_, _, slot)| {
                slot.value
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_ref()
                    .map_or(0, |e| e.body.stored_len() + e.stale.stored_len())
            })
            .sum();
        let mut i = 0;
        while i < slots.len() && (slots.len() > MAX_ENTRIES || bytes > MAX_BYTES) {
            let slot = &slots[i].2;
            if Arc::strong_count(slot) == 1 && slot.flight.try_lock().is_ok() {
                if let Some(e) = slot
                    .value
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_ref()
                {
                    bytes = bytes.saturating_sub(e.body.stored_len() + e.stale.stored_len());
                }
                slots.remove(i);
            } else {
                i += 1;
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn answer<F, Fut>(
        self: &Arc<Self>,
        engine: Arc<Engine>,
        filter: String,
        version: String,
        now: i64,
        reload: bool,
        build: F,
    ) -> Result<(u16, CachedBody), Decline>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(u16, Vec<u8>), Decline>> + Send + 'static,
    {
        let slot = self.slot(&engine, &filter);
        let admitted_revision = slot.revision.load(std::sync::atomic::Ordering::Acquire);
        let current = slot.value.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if !reload
            && let Some(hit) = &current
            && hit.version == version
            && now >= hit.at
            && now - hit.at < 30_000
        {
            return Ok((200, hit.body.clone()));
        }
        if !reload && let Some(hit) = current.filter(|e| now >= e.at && now - e.at < MAX_STALE_MS) {
            if let Ok(guard) = slot.flight.clone().try_lock_owned() {
                let saved = self.clone();
                let slot = slot.clone();
                tokio::spawn(async move {
                    let _guard = guard;
                    let _ = saved
                        .build(&slot, engine, filter, version, now, build)
                        .await;
                    drop(_guard);
                    drop(slot);
                    saved.trim();
                });
            }
            return Ok((200, hit.stale));
        }
        let _guard = slot.flight.clone().lock_owned().await;
        if (!reload
            || slot.revision.load(std::sync::atomic::Ordering::Acquire) != admitted_revision)
            && let Some(hit) = slot
                .value
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
            && hit.version == version
            && now >= hit.at
            && now - hit.at < 30_000
        {
            return Ok((200, hit.body.clone()));
        }
        if !reload {
            let e = engine.clone();
            let f = filter.clone();
            if let Ok(Some(hit)) = tokio::task::spawn_blocking(move || load(&e, &f)).await
                && now >= hit.at
                && now - hit.at < MAX_STALE_MS
            {
                let fresh = hit.version == version && now - hit.at < 30_000;
                let body = if fresh {
                    hit.body.clone()
                } else {
                    hit.stale.clone()
                };
                *slot.value.lock().unwrap_or_else(|p| p.into_inner()) = Some(hit);
                if fresh {
                    drop(_guard);
                    drop(slot);
                    self.trim();
                } else {
                    let saved = self.clone();
                    tokio::spawn(async move {
                        let _ = saved
                            .build(&slot, engine, filter, version, now, build)
                            .await;
                        drop(_guard);
                        drop(slot);
                        saved.trim();
                    });
                }
                return Ok((200, body));
            }
        }
        let answer = self.build(&slot, engine, filter, version, now, build).await;
        drop(_guard);
        drop(slot);
        self.trim();
        answer
    }
    #[allow(clippy::too_many_arguments)]
    async fn build<F, Fut>(
        &self,
        slot: &Slot,
        engine: Arc<Engine>,
        filter: String,
        version: String,
        now: i64,
        build: F,
    ) -> Result<(u16, CachedBody), Decline>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(u16, Vec<u8>), Decline>> + Send + 'static,
    {
        let (status, raw) = build().await?;
        if status != 200 || raw.len() > MAX_RAW {
            return Ok((status, body(raw)));
        }
        let hit = tokio::task::spawn_blocking(move || {
            let hit = entry(version.clone(), now, raw.clone())?;
            persist(&engine, filter, version, now, raw);
            Ok::<_, Decline>(hit)
        })
        .await
        .map_err(|_| Decline::Internal)??;
        let body = hit.body.clone();
        *slot.value.lock().unwrap_or_else(|p| p.into_inner()) = Some(hit);
        slot.revision
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        Ok((status, body))
    }
}

/// Scope stale responses to the same account inventory and disclosure preferences. Time and
/// database generation can change behind a stale answer; identity and capture policy cannot.
pub(crate) fn keys(
    engine: &Engine,
    root: &std::path::Path,
    query: Option<&str>,
    inputs: &str,
    refreshing: bool,
    now: i64,
) -> Result<(String, String), Decline> {
    let mut settings = Vec::new();
    for name in [
        "account-visibility.json",
        "dashboard-preferences.json",
        "analytics-capture.json",
        "models-dev-registry-cache.json",
    ] {
        let stamp = std::fs::symlink_metadata(root.join(name)).ok().map(|m| {
            #[cfg(unix)]
            let identity = {
                use std::os::unix::fs::MetadataExt;
                format!(
                    "{}:{}:{}:{}:{}:{}",
                    m.dev(),
                    m.ino(),
                    m.mode(),
                    m.uid(),
                    m.ctime(),
                    m.ctime_nsec()
                )
            };
            #[cfg(not(unix))]
            let identity = format!("{:?}", m.created());
            format!(
                "{}:{:?}:{}:{}:{identity}",
                m.len(),
                m.modified(),
                m.permissions().readonly(),
                m.file_type().is_symlink()
            )
        });
        settings.push(format!("{name}:{stamp:?}"));
    }
    let filter =
        aac_core::sha256_hex(format!("v1:{query:?}:{inputs}:{refreshing}:{settings:?}").as_bytes());
    let generation = analytics_cache::answer_generation(engine).map_err(|_| Decline::Internal)?;
    Ok((filter, format!("{generation}:{}", now.div_euclid(30_000))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn engine(root: &std::path::Path) -> Arc<Engine> {
        Arc::new(
            aac_store::ops::open_engine(root.join("store").to_str().expect("fixture"), None, None)
                .expect("fixture"),
        )
    }
    fn raw() -> Vec<u8> {
        br#"{"updatedAt":"fixed","stale":false,"refreshing":false,"activity":{}}"#.to_vec()
    }
    #[tokio::test]
    async fn saved_reuses_exact_bytes_and_survives_memory_eviction() {
        let root = tempfile::tempdir().expect("fixture");
        let engine = engine(root.path());
        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let saved = Arc::new(Saved::default());
            for _ in 0..2 {
                let calls = calls.clone();
                let (_, body) = saved
                    .answer(
                        engine.clone(),
                        "q".into(),
                        "v".into(),
                        100,
                        false,
                        move || async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            Ok((200, raw()))
                        },
                    )
                    .await
                    .expect("fixture");
                assert_eq!(&body.serve(false).expect("fixture").0[..], raw());
            }
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "one compose including persistent hits"
        );
    }
    #[tokio::test]
    async fn saved_answer_survives_engine_reopen_and_corrupt_bytes_recompose() {
        let root = tempfile::tempdir().expect("fixture");
        let first = engine(root.path());
        let (filter, version) =
            keys(&first, root.path(), None, "accounts", false, 100).expect("keys");
        Arc::new(Saved::default())
            .answer(
                first.clone(),
                filter.clone(),
                version.clone(),
                100,
                false,
                || async { Ok((200, raw())) },
            )
            .await
            .expect("first");
        first.close().expect("close");
        drop(first);
        let reopened = engine(root.path());
        let key = keys(&reopened, root.path(), None, "accounts", false, 101).expect("keys");
        assert_eq!(key.0, filter);
        // Opening the engine appends a ledger event, so a new generation must revalidate.
        assert_ne!(key.1, version);
        let saved = Arc::new(Saved::default());
        let gate = Arc::new(tokio::sync::Notify::new());
        let wait = gate.clone();
        let (_, hit) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            saved.answer(
                reopened.clone(),
                filter.clone(),
                key.1.clone(),
                101,
                false,
                move || async move {
                    wait.notified().await;
                    Ok((200, raw()))
                },
            ),
        )
        .await
        .expect("persistent stale answers before rebuild")
        .expect("reopened response");
        let value: serde_json::Value =
            serde_json::from_slice(&hit.serve(false).expect("bytes").0).expect("JSON");
        assert_eq!(value["updatedAt"], "fixed");
        assert_eq!(value["stale"], true);
        assert_eq!(value["refreshing"], true);
        gate.notify_one();
        let slot = saved.slot(&reopened, &filter);
        let guard = slot.flight.lock().await;
        drop(guard);
        let version = key.1;
        analytics_cache::put(&reopened, &disk_key(&filter), b"corrupt").expect("corrupt fixture");
        let (_, rebuilt) = Arc::new(Saved::default())
            .answer(reopened, filter, version, 101, false, || async {
                Ok((200, raw()))
            })
            .await
            .expect("honest recomposition");
        assert_eq!(&rebuilt.serve(false).expect("bytes").0[..], raw());
    }
    #[tokio::test]
    async fn cold_concurrent_reads_compose_once() {
        let root = tempfile::tempdir().expect("fixture");
        let engine = engine(root.path());
        let saved = Arc::new(Saved::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..12 {
            let saved = saved.clone();
            let engine = engine.clone();
            let calls = calls.clone();
            tasks.push(tokio::spawn(async move {
                saved
                    .answer(
                        engine,
                        "q".into(),
                        "v".into(),
                        100,
                        false,
                        move || async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            tokio::task::yield_now().await;
                            Ok((200, raw()))
                        },
                    )
                    .await
                    .expect("fixture")
            }));
        }
        for task in tasks {
            task.await.expect("fixture");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn failed_refresh_keeps_honest_stale_and_policy_change_does_not_reuse_it() {
        let root = tempfile::tempdir().expect("fixture");
        let engine = engine(root.path());
        let saved = Arc::new(Saved::default());
        saved
            .answer(
                engine.clone(),
                "q".into(),
                "one".into(),
                100,
                false,
                || async { Ok((200, raw())) },
            )
            .await
            .expect("fixture");
        let (_, stale) = saved
            .answer(
                engine.clone(),
                "q".into(),
                "two".into(),
                200,
                false,
                || async { Err(Decline::Busy) },
            )
            .await
            .expect("fixture");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&stale.serve(false).expect("fixture").0)
                .expect("fixture")["stale"],
            true
        );
        let other = saved
            .answer(
                engine,
                "changed-policy".into(),
                "two".into(),
                200,
                false,
                || async { Err(Decline::Busy) },
            )
            .await;
        assert!(matches!(other, Err(Decline::Busy)));
    }
    #[test]
    fn keys_track_clock_preferences_and_store_without_cache_write_feedback() {
        let root = tempfile::tempdir().expect("fixture");
        let engine = engine(root.path());
        let key = keys(
            &engine,
            root.path(),
            Some("range=7d"),
            "accounts",
            false,
            100,
        )
        .expect("fixture");
        analytics_cache::put(&engine, "unrelated-cache", b"bytes").expect("fixture");
        assert_eq!(
            keys(
                &engine,
                root.path(),
                Some("range=7d"),
                "accounts",
                false,
                101
            )
            .expect("fixture"),
            key
        );
        let clock = keys(
            &engine,
            root.path(),
            Some("range=7d"),
            "accounts",
            false,
            30_000,
        )
        .expect("fixture");
        assert_eq!(clock.0, key.0);
        assert_ne!(clock.1, key.1);
        std::fs::write(
            root.path().join("analytics-capture.json"),
            br#"{"cwd":false}"#,
        )
        .expect("fixture");
        assert_ne!(
            keys(
                &engine,
                root.path(),
                Some("range=7d"),
                "accounts",
                false,
                100
            )
            .expect("fixture")
            .0,
            key.0
        );
        engine
            .set_meta("activity_recovery_epoch", "2")
            .expect("fixture");
        assert_ne!(
            analytics_cache::answer_generation(&engine).expect("fixture"),
            key.1.split(':').next().expect("fixture")
        );
    }
    #[tokio::test]
    async fn compressed_large_answer_has_identical_digest_and_bounded_cache() {
        let root = tempfile::tempdir().expect("fixture");
        let engine = engine(root.path());
        let saved = Arc::new(Saved::default());
        let raw = serde_json::to_vec(
            &serde_json::json!({"activity":{}, "padding":"chart point ".repeat(350_000)}),
        )
        .expect("fixture");
        let digest = aac_core::sha256_hex(&raw);
        let build_raw = raw.clone();
        let (_, first) = saved
            .answer(
                engine.clone(),
                "large".into(),
                "v".into(),
                100,
                false,
                move || async { Ok((200, build_raw)) },
            )
            .await
            .expect("fixture");
        assert_eq!(
            aac_core::sha256_hex(&first.serve(false).expect("fixture").0),
            digest
        );
        let start = std::time::Instant::now();
        for _ in 0..100 {
            let (_, hit) = saved
                .answer(
                    engine.clone(),
                    "large".into(),
                    "v".into(),
                    101,
                    false,
                    || async { panic!("warm compose") },
                )
                .await
                .expect("fixture");
            assert_eq!(
                hit.serve(true).expect("fixture").0,
                first.serve(true).expect("fixture").0
            );
        }
        eprintln!(
            "4 MiB saved-answer warm gzip mean: {:?}",
            start.elapsed() / 100
        );
        for i in 0..12 {
            saved
                .answer(
                    engine.clone(),
                    format!("filter-{i}"),
                    "v".into(),
                    100,
                    false,
                    || async { Ok((200, super::tests::raw())) },
                )
                .await
                .expect("fixture");
        }
        assert!(saved.slots.lock().expect("fixture").len() <= MAX_ENTRIES);
    }
    #[tokio::test]
    async fn cancelled_cold_composer_releases_singleflight() {
        let root = tempfile::tempdir().expect("fixture");
        let engine = engine(root.path());
        let saved = Arc::new(Saved::default());
        let entered = Arc::new(tokio::sync::Notify::new());
        let s = saved.clone();
        let e = engine.clone();
        let signal = entered.clone();
        let task = tokio::spawn(async move {
            s.answer(e, "q".into(), "v".into(), 100, false, move || async move {
                signal.notify_one();
                std::future::pending().await
            })
            .await
        });
        entered.notified().await;
        task.abort();
        let _ = task.await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            saved.answer(engine, "q".into(), "v".into(), 100, false, || async {
                Ok((200, raw()))
            }),
        )
        .await
        .expect("fixture")
        .expect("fixture");
    }
    #[tokio::test]
    async fn changed_generation_serves_marked_stale_then_replaces_it() {
        let root = tempfile::tempdir().expect("fixture");
        let engine = engine(root.path());
        let saved = Arc::new(Saved::default());
        saved
            .answer(
                engine.clone(),
                "q".into(),
                "one".into(),
                100,
                false,
                || async { Ok((200, raw())) },
            )
            .await
            .expect("fixture");
        let gate = Arc::new(tokio::sync::Notify::new());
        let wait = gate.clone();
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            saved.answer(
                engine.clone(),
                "q".into(),
                "two".into(),
                200,
                false,
                move || async move {
                    wait.notified().await;
                    Ok((200, raw()))
                },
            ),
        )
        .await
        .expect("stale must answer while rebuild is blocked")
        .expect("fixture");
        let stale: serde_json::Value =
            serde_json::from_slice(&answer.1.serve(false).expect("fixture").0).expect("fixture");
        assert_eq!(stale["stale"], true);
        assert_eq!(stale["refreshing"], true);
        gate.notify_one();
    }
}
