//! Deterministic real-sized synthetic fixture generator (`scripts/perf/memory-fixture.mjs`).
//!
//! Generates zero real data: neutral synthetic identifiers, `@example.invalid` emails,
//! and seeded Mulberry32 PRNG values only. Migrates directly into `aac-store` via the
//! workspace Rust crate (no `.node` binding required).

use super::base_fixtures::{
    canonical_json, iso_ms, iso_sec, js_num, set_path_mtime_ms, to_fixed_json, to_fixed_num,
    write_json_compact, write_scoreboard_fixture_for_perf, write_status_fixture_for_perf,
};
use crate::SilentExit;
use aac_core::sha256_hex;
use base64::Engine as _;
use ed25519_dalek::pkcs8::EncodePublicKey;
use ed25519_dalek::{Signer, SigningKey};
use indexmap::IndexMap;
use pem_rfc7468::LineEnding;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MIN_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MIN_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

const MACHINES: [&str; 4] = ["vm", "mac", "windows", "nas1"];
const HOSTS: [&str; 4] = ["ubuntu", "mac", "windows", "nas1"];
const PROJECTS: [&str; 4] = [
    "example-alpha",
    "example-beta",
    "example-gamma",
    "example-delta",
];
const STREAMS: [&str; 8] = ["CORE", "UI", "DATA", "OPS", "TESTS", "DOCS", "PERF", "SYNC"];
const THREAD_STATES: [&str; 6] = ["working", "waiting", "blocked", "paused", "idle", "done"];
const TASK_STATES: [&str; 8] = [
    "todo", "ready", "working", "review", "fixing", "blocked", "done", "landed",
];
const FLAG_KINDS: [&str; 5] = [
    "shortcut",
    "incomplete-read",
    "test-weakened",
    "scope-cut",
    "false-claim",
];

const PKCS8_V1_ED25519_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

const PRICING_DATA_JSON: &str = include_str!("../../../aac-views/src/pricing/data.json");

#[derive(Clone, Copy)]
struct ModelSpec {
    id: &'static str,
    weight: f64,
    cache_k: f64,
    cache_create: f64,
}

#[derive(Clone, Copy)]
struct ProviderSpec {
    id: &'static str,
    tool: &'static str,
    route: &'static str,
    platform: &'static str,
    source: &'static str,
    plan: &'static str,
    share: usize,
    models: &'static [ModelSpec],
}

const CLAUDE_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "claude-opus-4-7",
        weight: 0.4,
        cache_k: 8.5,
        cache_create: 0.32,
    },
    ModelSpec {
        id: "claude-opus-4-6",
        weight: 0.15,
        cache_k: 7.5,
        cache_create: 0.3,
    },
    ModelSpec {
        id: "claude-sonnet-4-6",
        weight: 0.25,
        cache_k: 6.5,
        cache_create: 0.25,
    },
    ModelSpec {
        id: "claude-sonnet-4-5",
        weight: 0.1,
        cache_k: 5.5,
        cache_create: 0.22,
    },
    ModelSpec {
        id: "claude-haiku-4-5",
        weight: 0.1,
        cache_k: 4.0,
        cache_create: 0.18,
    },
];

const CODEX_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "gpt-5-codex",
        weight: 0.45,
        cache_k: 5.5,
        cache_create: 0.0,
    },
    ModelSpec {
        id: "gpt-5.2-codex",
        weight: 0.25,
        cache_k: 5.0,
        cache_create: 0.0,
    },
    ModelSpec {
        id: "gpt-5",
        weight: 0.2,
        cache_k: 4.0,
        cache_create: 0.0,
    },
    ModelSpec {
        id: "gpt-5-mini",
        weight: 0.1,
        cache_k: 3.0,
        cache_create: 0.0,
    },
];

const ANTIGRAVITY_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "gemini-2.5-pro",
        weight: 0.45,
        cache_k: 3.5,
        cache_create: 0.0,
    },
    ModelSpec {
        id: "gemini-2.5-flash",
        weight: 0.3,
        cache_k: 2.5,
        cache_create: 0.0,
    },
    ModelSpec {
        id: "gemini-3-pro-preview",
        weight: 0.15,
        cache_k: 3.0,
        cache_create: 0.0,
    },
    ModelSpec {
        id: "gemini-3-flash-preview",
        weight: 0.1,
        cache_k: 2.0,
        cache_create: 0.0,
    },
];

const PROVIDER_SPECS: [ProviderSpec; 3] = [
    ProviderSpec {
        id: "claude",
        tool: "claude",
        route: "anthropic",
        platform: "mac",
        source: "Claude quota on Mac",
        plan: "max",
        share: 7,
        models: CLAUDE_MODELS,
    },
    ProviderSpec {
        id: "codex",
        tool: "codex",
        route: "openai",
        platform: "ubuntu",
        source: "Codex quota on Ubuntu",
        plan: "pro",
        share: 6,
        models: CODEX_MODELS,
    },
    ProviderSpec {
        id: "antigravity",
        tool: "antigravity",
        route: "google-antigravity",
        platform: "ubuntu",
        source: "Antigravity quota on Ubuntu",
        plan: "pro",
        share: 6,
        models: ANTIGRAVITY_MODELS,
    },
];

/// Default options matching `DEFAULT_OPTIONS` in `scripts/perf/memory-fixture.mjs`.
#[derive(Debug, Clone, PartialEq)]
pub struct FixtureOptions {
    /// Deterministic PRNG seed.
    pub seed: u64,
    /// Total Scoreboard run files.
    pub runs: usize,
    /// Number of writers producing runs.
    pub run_writers: usize,
    /// Total Status writer directories.
    pub writers: usize,
    /// Total pm-mail registry JSON files.
    pub registry_entries: usize,
    /// Days of quota and activity history.
    pub days: usize,
    /// Synthetic accounts distributed across 3 providers.
    pub accounts: usize,
    /// Quota samples per day per account.
    pub quota_samples_per_day: usize,
    /// Activity sessions distributed across the window.
    pub activity_sessions: usize,
    /// Synthetic pricing providers in `models-dev-registry-cache.json`.
    pub pricing_providers: usize,
    /// Synthetic models per pricing provider.
    pub pricing_models_per_provider: usize,
    /// Skip migrating into `aac-store` redb when true.
    pub skip_store: bool,
    /// Optional explicit epoch milliseconds clock override.
    pub now: Option<i64>,
}

impl Default for FixtureOptions {
    fn default() -> Self {
        Self {
            seed: 20_261_125,
            runs: 7500,
            run_writers: 20,
            writers: 2000,
            registry_entries: 4500,
            days: 60,
            accounts: 19,
            quota_samples_per_day: 24,
            activity_sessions: 1800,
            pricing_providers: 200,
            pricing_models_per_provider: 40,
            skip_store: false,
            now: None,
        }
    }
}

/// Deterministic Mulberry32 PRNG matching `createRng` in `scripts/perf/memory-fixture.mjs`.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u32,
}

impl Rng {
    /// Create a seeded Mulberry32 generator (`(seed >>> 0) || 1`).
    pub fn new(seed: u64) -> Self {
        let s = seed as u32;
        Self {
            state: if s == 0 { 1 } else { s },
        }
    }

    fn next_u32(&mut self) -> u32 {
        self.state = self.state.wrapping_add(0x6d2b_79f5);
        let mut t = (self.state ^ (self.state >> 15)).wrapping_mul(1 | self.state);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        t ^ (t >> 14)
    }

    /// Return the next uniform float in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        f64::from(self.next_u32()) / 4_294_967_296.0
    }

    /// Return an integer uniformly distributed in `[min, max]` inclusive.
    pub fn int(&mut self, min: i64, max: i64) -> i64 {
        min + (self.next_f64() * ((max - min + 1) as f64)).floor() as i64
    }

    /// Pick a random slice element.
    pub fn pick<'a, T>(&mut self, list: &'a [T]) -> &'a T {
        let idx = (self.next_f64() * (list.len() as f64)).floor() as usize;
        &list[idx.min(list.len().saturating_sub(1))]
    }

    /// Return `len` lowercase hex characters from the generator.
    pub fn hex(&mut self, len: usize) -> String {
        let mut out = String::with_capacity(len + 8);
        while out.len() < len {
            let v = self.next_u32();
            let _ = write!(&mut out, "{v:08x}");
        }
        out.truncate(len);
        out
    }
}

/// Synthetic account record used to generate quota and activity usage logs.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntheticAccount {
    /// 0-based synthetic account index (or 1000+ for legacy source rows).
    pub index: usize,
    /// Account identifier (`<provider>:synth-XX` or `<provider>:usage`).
    pub id: String,
    /// Provider identifier.
    pub provider: String,
    /// Tool identifier.
    pub tool: String,
    /// Route identifier.
    pub route: String,
    /// Synthetic email address (`None` for legacy source rows).
    pub email: Option<String>,
    /// SHA-256 Analytics account identity matching `AccountKey::derive`.
    pub identity: String,
    /// Subscription plan label.
    pub plan: Option<String>,
    /// Platform name (`mac` or `ubuntu`).
    pub platform: String,
    /// Quota source description.
    pub source: String,
    /// Whether this account is marked active.
    pub is_active: bool,
}

impl SyntheticAccount {
    fn models(&self) -> &'static [ModelSpec] {
        PROVIDER_SPECS
            .iter()
            .find(|spec| spec.id == self.provider)
            .map_or(&[], |spec| spec.models)
    }
}

/// Counts recorded in `fixture-manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FixtureCounts {
    /// Total writer directories.
    pub writers: usize,
    /// Number of run writers.
    pub run_writers: usize,
    /// Total run files.
    pub runs: usize,
    /// Total pm-mail registry files.
    pub registry_entries: usize,
    /// Number of synthetic providers.
    pub providers: usize,
    /// Number of synthetic accounts.
    pub accounts: usize,
    /// Number of legacy `<provider>:usage` source accounts.
    pub legacy_source_accounts: usize,
    /// Number of history days.
    pub days: usize,
    /// Total quota observations generated.
    pub quota_observations: usize,
    /// Total activity events generated.
    pub activity_events: usize,
    /// Total activity sessions generated.
    pub activity_sessions: usize,
    /// Total pricing cache providers.
    pub pricing_providers: usize,
    /// Total pricing cache models.
    pub pricing_models: usize,
}

/// Generated fixture manifest written to `fixture-manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixtureManifest {
    /// Manifest schema version (1).
    pub schema_version: u32,
    /// Seed used for generation.
    pub seed: u64,
    /// Anchor timestamp in epoch milliseconds.
    pub now: i64,
    /// Anchor timestamp in ISO-8601 seconds format.
    pub now_iso: String,
    /// Resolved fixture root path.
    pub root: PathBuf,
    /// Isolated fake `HOME` path.
    pub home: PathBuf,
    /// Isolated `.ccs` path.
    pub ccs_dir: PathBuf,
    /// Isolated `agent-reports` path.
    pub reports_dir: PathBuf,
    /// Isolated `pm-mail-registry` path.
    pub registry_dir: PathBuf,
    /// Isolated `usage-logs` path.
    pub usage_logs_dir: PathBuf,
    /// Generated item counts.
    pub counts: FixtureCounts,
    /// Store migration stats, or `None` when `--skip-store` was used.
    pub store: Option<Json>,
    /// Generation wall time in milliseconds.
    pub generation_ms: u64,
}

struct DeterministicKey {
    signing: SigningKey,
    private_key_pem: String,
    public_key_pem: String,
    key_id: String,
}

fn deterministic_ed25519_key(seed: u64) -> Result<DeterministicKey, Box<dyn Error>> {
    let digest = Sha256::digest(format!("aac-memory-fixture-ed25519:{seed}").as_bytes());
    let mut raw_seed = [0_u8; 32];
    raw_seed.copy_from_slice(&digest);
    let signing = SigningKey::from_bytes(&raw_seed);

    let mut pkcs8_der = Vec::with_capacity(48);
    pkcs8_der.extend_from_slice(&PKCS8_V1_ED25519_PREFIX);
    pkcs8_der.extend_from_slice(&raw_seed);
    let private_key_pem = pem_rfc7468::encode_string("PRIVATE KEY", LineEnding::LF, &pkcs8_der)
        .map_err(|err| format!("PEM encode private key failed: {err}"))?;

    let verifying = signing.verifying_key();
    let public_key_pem = verifying.to_public_key_pem(LineEnding::LF)?;
    let spki_der = verifying.to_public_key_der()?;
    let key_id = sha256_hex(spki_der.as_bytes())[..16].to_string();

    Ok(DeterministicKey {
        signing,
        private_key_pem,
        public_key_pem,
        key_id,
    })
}

fn static_rates_map() -> Result<BTreeMap<String, [f64; 4]>, Box<dyn Error>> {
    let doc: Json = serde_json::from_str(PRICING_DATA_JSON)?;
    let mut rates = BTreeMap::new();
    if let Some(reg) = doc.get("registry").and_then(Json::as_object) {
        for (k, v) in reg {
            let input = v
                .get("inputPerMillion")
                .and_then(Json::as_f64)
                .unwrap_or(3.0);
            let output = v
                .get("outputPerMillion")
                .and_then(Json::as_f64)
                .unwrap_or(15.0);
            let cw = v
                .get("cacheCreationPerMillion")
                .and_then(Json::as_f64)
                .unwrap_or(3.75);
            let cr = v
                .get("cacheReadPerMillion")
                .and_then(Json::as_f64)
                .unwrap_or(0.3);
            rates.insert(k.clone(), [input, output, cw, cr]);
        }
    }
    Ok(rates)
}

#[derive(Clone)]
struct CatalogModel {
    full: String,
    provider: String,
    id: String,
    harness: String,
    efforts: Vec<Option<String>>,
}

struct CatalogSeeds {
    models: Vec<CatalogModel>,
    task_types: Vec<String>,
}

fn load_catalog_seeds(repo_root: &Path) -> Result<CatalogSeeds, Box<dyn Error>> {
    let catalog_dir = repo_root.join("docs").join("agent-reports").join("catalog");
    let models_doc: Json =
        serde_json::from_str(&fs::read_to_string(catalog_dir.join("models.json"))?)?;
    let types_doc: Json =
        serde_json::from_str(&fs::read_to_string(catalog_dir.join("task-types.json"))?)?;

    let models = models_doc
        .get("models")
        .and_then(Json::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    let full = entry.get("model")?.as_str()?.to_string();
                    let (provider, id) = match full.find('/') {
                        Some(slash) if slash > 0 => {
                            (full[..slash].to_string(), full[slash + 1..].to_string())
                        }
                        _ => ("example".to_string(), full.clone()),
                    };
                    let harness = entry
                        .get("runtimes")
                        .and_then(Json::as_array)
                        .and_then(|r| r.first())
                        .and_then(Json::as_str)
                        .unwrap_or("omp")
                        .to_string();
                    let raw_efforts: Vec<Option<String>> = entry
                        .get("efforts")
                        .and_then(Json::as_array)
                        .map(|list| {
                            list.iter()
                                .filter_map(|v| v.as_str().map(|s| Some(s.to_string())))
                                .collect()
                        })
                        .unwrap_or_default();
                    let efforts = if raw_efforts.is_empty() {
                        vec![None]
                    } else {
                        raw_efforts
                    };
                    Some(CatalogModel {
                        full,
                        provider,
                        id,
                        harness,
                        efforts,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let task_types = types_doc
        .get("taskTypes")
        .and_then(Json::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| entry.get("id")?.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();

    Ok(CatalogSeeds { models, task_types })
}

fn set_mode_0700(path: &Path) -> Result<(), Box<dyn Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn set_mode_0600(path: &Path) -> Result<(), Box<dyn Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn seed_deterministic_status_answers(
    reports_dir: &Path,
    ccs_dir: &Path,
    seed: u64,
    now_ms: i64,
) -> Result<(), Box<dyn Error>> {
    let v1 = reports_dir.join("v1");
    let writers_dir = v1.join("writers");
    let store_dir = ccs_dir.join("status");
    fs::create_dir_all(&store_dir)?;
    set_mode_0700(&store_dir)?;

    let key = deterministic_ed25519_key(seed)?;
    let key_file = store_dir.join("answer-key.pem");
    fs::write(&key_file, &key.private_key_pem)?;
    set_mode_0600(&key_file)?;

    let keys_dir = v1.join("keys");
    fs::create_dir_all(&keys_dir)?;
    fs::write(keys_dir.join("aac-answers.pub"), &key.public_key_pem)?;
    fs::write(
        keys_dir.join(format!("{}.pub", key.key_id)),
        &key.public_key_pem,
    )?;

    type AnswerTarget<'a> = (&'a str, &'a str, &'a [&'a str], &'a str, &'a str, i64);
    let targets: [AnswerTarget<'_>; 3] = [
        (
            "mac.aac-claude-relogin",
            "mac.aac-claude-relogin--Q7",
            &["B"],
            "Windows gets its own runner; keep the Mac one simple.",
            "deck",
            20 * HOUR_MS,
        ),
        (
            "vm.pm-closure-lead",
            "vm.pm-closure-lead--Q1",
            &["A"],
            "",
            "recommended",
            3 * HOUR_MS,
        ),
        (
            "vm.aac-status-lead",
            "vm.aac-status-lead--Q2",
            &[],
            "Could you show both side by side first?",
            "deck",
            30 * MIN_MS,
        ),
    ];

    let mut lines: Vec<Json> = Vec::new();
    for (i, (writer, card_id, choice, note, via, ago)) in targets.iter().enumerate() {
        let card_path = writers_dir
            .join(writer)
            .join("cards")
            .join(format!("{card_id}.json"));
        if !card_path.exists() {
            continue;
        }
        let raw = fs::read(&card_path)?;
        let doc: Json = serde_json::from_slice(&raw)?;
        let card_sha = sha256_hex(&raw);
        let kind = if choice.is_empty() { "note" } else { "answer" };
        let answered_at = iso_ms(now_ms - *ago);
        let unsigned = json!({
            "schema": "agent-reports/answer@1",
            "cardId": card_id,
            "cardSeq": doc["seq"],
            "cardSha256": card_sha,
            "kind": kind,
            "choice": choice,
            "note": note,
            "via": via,
            "answeredAt": answered_at,
            "answeredBy": "owner",
            "device": "iPhone",
        });
        let canon = canonical_json(&unsigned);
        let sig_bytes = key.signing.sign(canon.as_bytes()).to_bytes();
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig_bytes);
        let mut answer = unsigned.clone();
        if let Some(obj) = answer.as_object_mut() {
            obj.insert(
                "signature".into(),
                json!({
                    "alg": "ed25519",
                    "keyId": key.key_id,
                    "value": sig_b64,
                }),
            );
        }
        let id_hash = sha256_hex(format!("answer-uuid:{seed}:{i}:{card_id}"));
        let id = format!(
            "{}-{}-4{}-8{}-{}",
            &id_hash[0..8],
            &id_hash[8..12],
            &id_hash[13..16],
            &id_hash[17..20],
            &id_hash[20..32]
        );
        let file_name = if kind == "note" {
            let flat: String = id.chars().filter(|c| *c != '-').collect();
            format!("{card_id}.note-{}.json", &flat[..12])
        } else {
            format!("{card_id}.json")
        };
        write_json_compact(&v1.join("answers").join(file_name), &answer, None)?;
        let delivered_at = iso_ms(now_ms - *ago + MIN_MS);
        let pm_mail = doc["replyTo"]["pmMail"].as_str().unwrap_or("");
        lines.push(json!({
            "type": "answer",
            "id": id,
            "at": answered_at,
            "answer": answer,
            "card": doc,
        }));
        lines.push(json!({
            "type": "delivery",
            "id": id,
            "step": "share",
            "state": "done",
            "at": delivered_at,
            "detail": "Signed copy written to the share.",
        }));
        lines.push(json!({
            "type": "delivery",
            "id": id,
            "step": "doorbell",
            "state": "done",
            "at": delivered_at,
            "detail": format!("Rang {pm_mail} through pm-mail."),
        }));
    }

    let mut jsonl = String::new();
    for line in &lines {
        jsonl.push_str(&serde_json::to_string(line)?);
        jsonl.push('\n');
    }
    let answers_file = store_dir.join("answers.jsonl");
    fs::write(&answers_file, jsonl)?;
    set_mode_0600(&answers_file)?;
    Ok(())
}

/// Build `total_accounts` synthetic accounts distributed across `claude`, `codex`, and `antigravity`.
pub fn build_synthetic_accounts(total_accounts: usize) -> Vec<SyntheticAccount> {
    let total_share: usize = PROVIDER_SPECS.iter().map(|s| s.share).sum();
    let mut allocations: Vec<usize> = PROVIDER_SPECS
        .iter()
        .enumerate()
        .map(|(idx, spec)| {
            if idx == PROVIDER_SPECS.len() - 1 {
                0
            } else {
                let raw = ((total_accounts * spec.share) as f64) / (total_share as f64);
                aac_num::ecma::math_round(raw).max(1.0) as usize
            }
        })
        .collect();
    let allocated: usize = allocations.iter().sum();
    allocations[PROVIDER_SPECS.len() - 1] = total_accounts.saturating_sub(allocated).max(1);
    while allocations.iter().sum::<usize>() > total_accounts {
        if let Some((max_idx, _)) = allocations.iter().enumerate().max_by_key(|&(_, v)| v) {
            allocations[max_idx] = allocations[max_idx].saturating_sub(1);
        }
    }

    let mut accounts = Vec::with_capacity(total_accounts);
    for (p_idx, spec) in PROVIDER_SPECS.iter().enumerate() {
        let count = allocations[p_idx];
        for i in 1..=count {
            let id = format!("{}:synth-{i:02}", spec.id);
            let email = format!("synth.{}.{i:02}@example.invalid", spec.id);
            let tuple_json = serde_json::to_string(&(spec.id, id.as_str(), email.to_lowercase()))
                .unwrap_or_default();
            let identity = sha256_hex(tuple_json);
            accounts.push(SyntheticAccount {
                index: accounts.len(),
                id,
                provider: spec.id.to_string(),
                tool: spec.tool.to_string(),
                route: spec.route.to_string(),
                email: Some(email),
                identity,
                plan: Some(spec.plan.to_string()),
                platform: spec.platform.to_string(),
                source: spec.source.to_string(),
                is_active: i == 1,
            });
        }
    }
    accounts
}

/// Build legacy `<provider>:usage` source accounts matching `config/tables/providers.json`.
pub fn build_legacy_source_accounts(
    repo_root: &Path,
) -> Result<Vec<SyntheticAccount>, Box<dyn Error>> {
    let table_path = repo_root
        .join("config")
        .join("tables")
        .join("providers.json");
    let table: Json = serde_json::from_str(&fs::read_to_string(table_path)?)?;
    let providers = table
        .get("additionalProviders")
        .and_then(Json::as_array)
        .ok_or("config/tables/providers.json missing additionalProviders")?;

    let mut out = Vec::with_capacity(providers.len());
    for (i, entry) in providers.iter().enumerate() {
        let provider = entry
            .as_str()
            .ok_or("additionalProviders entry must be a string")?;
        let id = format!("{provider}:usage");
        let tuple_json = serde_json::to_string(&(provider, id.as_str(), Json::Null))?;
        let identity = sha256_hex(tuple_json);
        out.push(SyntheticAccount {
            index: 1000 + i,
            id,
            provider: provider.to_string(),
            tool: provider.to_string(),
            route: provider.to_string(),
            email: None,
            identity,
            plan: None,
            platform: "ubuntu".to_string(),
            source: "Synthetic usage source".to_string(),
            is_active: false,
        });
    }
    Ok(out)
}

struct WriterGenResult {
    total_writers: usize,
    run_writers: Vec<String>,
}

fn generate_writers(
    repo_root: &Path,
    reports_dir: &Path,
    rng: &mut Rng,
    now_ms: i64,
    total_writers: usize,
    run_writer_count: usize,
) -> Result<WriterGenResult, Box<dyn Error>> {
    let writers_dir = reports_dir.join("v1").join("writers");
    let mut existing: Vec<String> = if writers_dir.exists() {
        fs::read_dir(&writers_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect()
    } else {
        Vec::new()
    };
    existing.sort();
    let remaining = total_writers.saturating_sub(existing.len());
    let mut run_writers = Vec::new();
    let mut all_created = Vec::with_capacity(remaining);
    let catalog = load_catalog_seeds(repo_root)?;

    for i in 0..remaining {
        let is_run_writer = i < run_writer_count;
        let machine = MACHINES[i % MACHINES.len()];
        let short_name = if is_run_writer {
            format!("synth-run-{:02}", i + 1)
        } else {
            format!("synth-w-{:04}", i + 1)
        };
        let writer = format!("{machine}.{short_name}");
        let writer_dir = writers_dir.join(&writer);
        fs::create_dir_all(&writer_dir)?;
        if is_run_writer {
            run_writers.push(writer.clone());
        }
        all_created.push(writer.clone());

        let is_recent = is_run_writer || i < ((remaining as f64) * 0.48).floor() as usize;
        let ago_ms = if is_recent {
            rng.int(2, 48 * 60) * MIN_MS
        } else {
            rng.int(49, 60 * 24) * HOUR_MS + rng.int(0, 59) * MIN_MS
        };
        let written_ms = now_ms - ago_ms;
        let written_at = iso_sec(written_ms);
        let project = PROJECTS[i % PROJECTS.len()];
        let stream = STREAMS[i % STREAMS.len()];
        let state = if is_recent {
            THREAD_STATES[i % 4]
        } else if i % 3 == 0 {
            "idle"
        } else {
            "done"
        };
        let model = &catalog.models[i % catalog.models.len()];
        let seq = 10 + (i % 40) as i64;
        let t3_thread = format!("synth-thread-{:05}", i + 1);

        let ttl_seconds = if state == "done" || state == "idle" {
            Json::Null
        } else {
            Json::from(1800)
        };
        let blocked_on = if state == "blocked" {
            json!({
                "kind": "review",
                "what": format!("Waiting on synthetic review for {short_name}"),
                "since": iso_sec(written_ms - 20 * MIN_MS),
            })
        } else {
            Json::Null
        };
        let thread_doc = json!({
            "schema": "agent-reports/thread@1",
            "writer": writer,
            "seq": seq,
            "writtenAt": written_at,
            "ttlSeconds": ttl_seconds,
            "agent": {
                "name": short_name,
                "harness": model.harness,
                "model": model.id,
                "effort": model.efforts[0],
                "account": format!("synth-acct-{:02}", (i % 19) + 1),
                "machine": machine,
                "t3Thread": t3_thread,
                "parent": null,
            },
            "project": project,
            "stream": stream,
            "state": state,
            "doing": format!("Synthetic workload item {short_name} on {stream}"),
            "blockedOn": blocked_on,
            "progress": {"done": i % 8, "total": 8, "unit": "items"},
            "links": [],
        });
        write_json_compact(
            &writer_dir.join("thread.json"),
            &thread_doc,
            Some(written_ms),
        )?;

        let left = if state == "done" {
            json!([])
        } else {
            json!([
                {"id": format!("L-{i}-1"), "title": format!("Finish synthetic step A for {short_name}")},
                {"id": format!("L-{i}-2"), "title": format!("Review synthetic step B for {short_name}")},
            ])
        };
        let blockers = if state == "blocked" {
            json!([{"text": format!("Synthetic review pending for {short_name}"), "waitingOn": "review"}])
        } else {
            json!([])
        };
        let status_doc = json!({
            "schema": "agent-reports/status@1",
            "writer": writer,
            "seq": seq,
            "writtenAt": written_at,
            "t3Thread": t3_thread,
            "summary": format!("Synthetic status summary for {short_name} in stream {stream}."),
            "percent": ((i % 11) * 10).min(100),
            "done": [
                {"id": format!("D-{i}-1"), "title": format!("Completed baseline task for {short_name}")},
                {"id": format!("D-{i}-2"), "title": format!("Verified synthetic check for {short_name}")},
            ],
            "left": left,
            "blockers": blockers,
            "looping": {"state": "no", "text": null},
        });
        write_json_compact(
            &writer_dir.join("status.json"),
            &status_doc,
            Some(written_ms),
        )?;

        let item_count = if is_run_writer { 6 } else { 2 + (i % 3) };
        let mut items = Vec::with_capacity(item_count);
        for k in 0..item_count {
            let item_state = TASK_STATES[(i + k) % TASK_STATES.len()];
            let landed = if item_state == "landed" {
                json!({"sha": rng.hex(10), "at": written_at})
            } else {
                Json::Null
            };
            items.push(json!({
                "id": format!("T-{:04}-{}", i + 1, k + 1),
                "title": format!("Synthetic task {} for {short_name}", k + 1),
                "state": item_state,
                "owner": short_name,
                "branch": null,
                "head": null,
                "reviews": [],
                "landed": landed,
                "blockedOn": null,
                "why": null,
                "updatedAt": written_at,
            }));
        }
        let tasks_doc = json!({
            "schema": "agent-reports/tasks@1",
            "writer": writer,
            "seq": seq,
            "writtenAt": written_at,
            "project": project,
            "projectLabel": format!("Project {project}"),
            "stream": stream,
            "streamLabel": format!("Stream {stream}"),
            "owner": short_name,
            "goal": format!("Synthetic stream goal for {stream}"),
            "board": null,
            "items": items,
        });
        write_json_compact(&writer_dir.join("tasks.json"), &tasks_doc, Some(written_ms))?;
        let _ = set_path_mtime_ms(&writer_dir, written_ms);
    }

    let final_run_writers = if run_writers.is_empty() {
        existing
            .iter()
            .take(run_writer_count.max(1))
            .cloned()
            .collect()
    } else {
        run_writers
    };
    Ok(WriterGenResult {
        total_writers: existing.len() + all_created.len(),
        run_writers: final_run_writers,
    })
}

#[allow(clippy::too_many_arguments)]
fn generate_runs(
    repo_root: &Path,
    reports_dir: &Path,
    run_writers: &[String],
    rng: &mut Rng,
    now_ms: i64,
    total_runs: usize,
    days: usize,
    existing_run_count: usize,
) -> Result<usize, Box<dyn Error>> {
    let to_create = total_runs.saturating_sub(existing_run_count);
    let catalog = load_catalog_seeds(repo_root)?;
    let work_types: Vec<&str> = catalog
        .task_types
        .iter()
        .map(String::as_str)
        .filter(|t| *t != "review")
        .collect();
    let writers_dir = reports_dir.join("v1").join("writers");
    let mut created_work_ids: Vec<String> = Vec::new();
    let mut ensured_dirs: HashSet<PathBuf> = HashSet::new();

    for i in 0..to_create {
        let writer = &run_writers[i % run_writers.len()];
        let is_review = !created_work_ids.is_empty() && i % 4 == 3;
        let denom = to_create.max(1) as f64;
        let span = ((days as i64) * DAY_MS - 2 * HOUR_MS) as f64;
        let age_ms = ((((to_create - 1 - i) as f64) / denom) * span).floor() as i64 + HOUR_MS;
        let jitter_ms = rng.int(0, 45) * MIN_MS;
        let ended_ms = (now_ms - 5 * MIN_MS).min(now_ms - age_ms + jitter_ms);
        let wall = if is_review {
            rng.int(120, 600)
        } else {
            rng.int(240, 2400)
        };
        let started_ms = ended_ms - wall * 1000;
        let model_offset = rng.int(0, (catalog.models.len() - 1) as i64) as usize;
        let model = &catalog.models[(i * 7 + model_offset) % catalog.models.len()];
        let effort = rng.pick(&model.efforts).clone();
        let task_type = if is_review {
            "review"
        } else {
            work_types[i % work_types.len()]
        };
        let local_id = format!("{}-{:05}", if is_review { "REV" } else { "RUN" }, i + 1);
        let id = format!("{writer}--{local_id}");
        let ended_iso = iso_sec(ended_ms);
        let date_dir = &ended_iso[..10];
        let dir = writers_dir.join(writer).join("runs").join(date_dir);
        if ensured_dirs.insert(dir.clone()) {
            fs::create_dir_all(&dir)?;
        }

        let is_bench = !is_review && (task_type == "plan" || task_type == "scout") && i % 5 == 0;
        let outcome_roll = rng.next_f64();
        let outcome = if outcome_roll < 0.92 {
            "done"
        } else if outcome_roll < 0.97 {
            "failed"
        } else {
            "partial"
        };
        let has_flag = !is_review && outcome == "done" && rng.next_f64() < 0.03;

        let task_size = if task_type == "review" || task_type == "audit-read" {
            json!({"records": rng.int(10, 80)})
        } else if task_type == "mechanical" || task_type == "docs" {
            json!({"files": rng.int(2, 25)})
        } else if task_type == "plan" {
            json!({"words": rng.int(1200, 9000)})
        } else if task_type == "scout" {
            json!({"items": rng.int(30, 150)})
        } else {
            json!({"tasks": 1})
        };

        let credits = to_fixed_json(1.0 + rng.next_f64() * 14.0, 2);
        let usd = to_fixed_json(0.25 + rng.next_f64() * 4.5, 2);
        let delta_pct = to_fixed_json(0.5 + rng.next_f64() * 5.0, 1);
        let tok_in = rng.int(10_000, 400_000);
        let tok_out = rng.int(1_500, 35_000);
        let flags = if has_flag {
            let flag_kind = *rng.pick(&FLAG_KINDS);
            json!([{
                "kind": flag_kind,
                "detail": format!("Synthetic conduct finding on {local_id}"),
                "evidence": format!("/mnt/Cursor/share/example-project/evidence/{local_id}.md"),
                "reportedBy": "synth-reviewer",
            }])
        } else {
            json!([])
        };
        let evidence = if i % 6 == 0 {
            json!([{"label": "Synthetic report", "path": format!("/mnt/Cursor/share/example-project/runs/{local_id}.md")}])
        } else {
            json!([])
        };

        let mut doc = Map::new();
        doc.insert("schema".into(), Json::String("agent-reports/run@1".into()));
        doc.insert("id".into(), Json::String(id.clone()));
        doc.insert("writer".into(), Json::String(writer.clone()));
        doc.insert("seq".into(), Json::from(if i % 19 == 0 { 2 } else { 1 }));
        doc.insert("writtenAt".into(), Json::String(iso_sec(ended_ms + 30_000)));
        doc.insert(
            "source".into(),
            Json::String(if is_bench { "benchmark" } else { "observed" }.into()),
        );
        if is_bench {
            doc.insert(
                "benchmark".into(),
                Json::String(
                    if task_type == "plan" {
                        "plan-pilot2"
                    } else {
                        "scout-sv3-t9"
                    }
                    .into(),
                ),
            );
        }
        doc.insert(
            "project".into(),
            Json::String(PROJECTS[i % PROJECTS.len()].into()),
        );
        doc.insert(
            "stream".into(),
            Json::String(STREAMS[i % STREAMS.len()].into()),
        );
        doc.insert(
            "task".into(),
            json!({
                "id": format!("SYNTH-{:05}", i + 1),
                "type": task_type,
                "title": format!("Synthetic {task_type} {local_id}"),
                "size": task_size,
            }),
        );
        doc.insert(
            "model".into(),
            json!({
                "provider": model.provider,
                "id": model.id,
                "harness": model.harness,
                "effort": effort,
                "account": format!("synth-acct-{:02}", (i % 19) + 1),
            }),
        );
        doc.insert("startedAt".into(), Json::String(iso_sec(started_ms)));
        doc.insert("endedAt".into(), Json::String(ended_iso));
        doc.insert("wallSeconds".into(), Json::from(wall));
        doc.insert("outcome".into(), Json::String(outcome.into()));
        doc.insert(
            "cost".into(),
            json!({
                "credits": credits,
                "usd": usd,
                "quota": {
                    "account": format!("synth-acct-{:02}", (i % 19) + 1),
                    "window": "5h",
                    "deltaPct": delta_pct,
                },
                "tokens": {
                    "input": tok_in,
                    "output": tok_out,
                },
            }),
        );
        doc.insert(
            "scope".into(),
            json!({
                "filesAssigned": 12,
                "filesRead": if has_flag { 5 } else { 12 },
                "recordsAssigned": 20,
                "recordsJudged": 20,
            }),
        );
        doc.insert("flags".into(), flags);
        doc.insert("evidence".into(), evidence);

        if task_type == "scout" {
            doc.insert(
                "process".into(),
                json!({"id": "scout-pipeline", "version": if i % 2 == 0 { "2.0" } else { "1.0" }, "sha256": null}),
            );
            doc.insert(
                "metrics".into(),
                json!({
                    "factsKept": rng.int(80, 125),
                    "factsTotal": 127,
                    "verdictsChanged": rng.int(0, 10),
                    "verdictsTotal": 40,
                }),
            );
        } else if task_type == "plan" {
            doc.insert(
                "process".into(),
                json!({"id": "a", "version": "pilot/run_a.py", "sha256": null}),
            );
            doc.insert(
                "metrics".into(),
                json!({
                    "silent": rng.int(0, 8),
                    "goldAgreed": rng.int(45, 65),
                    "answerKeyFinal": 1,
                    "modelSeconds": rng.int(30, 180),
                }),
            );
        }

        if is_review {
            let max_back = created_work_ids.len().min(20) as i64;
            let back = rng.int(1, max_back) as usize;
            let target_idx = created_work_ids.len().saturating_sub(back);
            let target_id = created_work_ids[target_idx].clone();
            let verdict_roll = rng.next_f64();
            let verdict = if verdict_roll < 0.78 {
                "pass"
            } else if verdict_roll < 0.96 {
                "fail"
            } else {
                "cannot-verify"
            };
            let overturned = verdict == "fail";
            let high = if overturned { rng.int(1, 2) } else { 0 };
            let medium = rng.int(0, 2);
            let low = rng.int(0, 3);
            let disputed = overturned && rng.next_f64() < 0.15;
            let by = match &effort {
                Some(eff) => format!("{} {eff}", model.full),
                None => model.full.clone(),
            };
            doc.insert(
                "review".into(),
                json!({
                    "of": target_id,
                    "verdict": verdict,
                    "findings": {"high": high, "medium": medium, "low": low},
                    "overturned": overturned,
                    "by": by,
                    "disputed": disputed,
                }),
            );
        } else {
            created_work_ids.push(id.clone());
        }

        write_json_compact(
            &dir.join(format!("{id}.json")),
            &Json::Object(doc),
            Some(ended_ms),
        )?;
    }

    Ok(existing_run_count + to_create)
}

fn generate_mail_registry(
    reports_dir: &Path,
    rng: &mut Rng,
    now_ms: i64,
    total_entries: usize,
) -> Result<usize, Box<dyn Error>> {
    let reg_dir = reports_dir.join("pm-mail-registry");
    fs::create_dir_all(&reg_dir)?;
    let existing = fs::read_dir(&reg_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .count();
    let to_create = total_entries.saturating_sub(existing);
    let kinds = ["claude", "codex", "omp", "muse", "zcode"];

    for i in 0..to_create {
        let name = format!("synth-mail-{:05}", i + 1);
        let machine_key = MACHINES[i % MACHINES.len()];
        let (machine, cwd) = match machine_key {
            "vm" => ("ubuntuserver", "/home/agent/work/example-project"),
            "mac" => ("agent-mac.local", "/Users/agent/work/example-project"),
            "windows" => ("win-workstation", "C:\\Users\\agent\\work\\example-project"),
            _ => ("nas1-host", "/data/agent/work/example-project"),
        };
        let ago_ms = if i < 250 {
            rng.int(1, 24 * 60) * MIN_MS
        } else if i < 1050 {
            rng.int(25, 7 * 24) * HOUR_MS
        } else {
            rng.int(8 * 24, 60 * 24) * HOUR_MS
        };
        let last_seen = now_ms - ago_ms;
        let entry = json!({
            "name": name,
            "kind": kinds[i % kinds.len()],
            "cwd": cwd,
            "about": format!("Synthetic thread {name} handling {} stream work", STREAMS[i % STREAMS.len()]),
            "registered": iso_ms(last_seen - 2 * HOUR_MS),
            "last_seen": iso_ms(last_seen),
            "machine": machine,
        });
        write_json_compact(
            &reg_dir.join(format!("{name}.json")),
            &entry,
            Some(last_seen),
        )?;
    }

    Ok(existing + to_create)
}

struct PricingGenResult {
    providers: usize,
    models: usize,
}

fn generate_models_dev_cache(
    ccs_dir: &Path,
    rates: &BTreeMap<String, [f64; 4]>,
    rng: &mut Rng,
    now_ms: i64,
    provider_count: usize,
    models_per_provider: usize,
) -> Result<PricingGenResult, Box<dyn Error>> {
    let mut providers = Map::new();
    let seeded: [(&str, &[&str]); 3] = [
        (
            "anthropic",
            &[
                "claude-opus-4-7",
                "claude-opus-4-6",
                "claude-sonnet-4-6",
                "claude-sonnet-4-5",
                "claude-haiku-4-5",
            ],
        ),
        (
            "openai",
            &["gpt-5-codex", "gpt-5.2-codex", "gpt-5", "gpt-5-mini"],
        ),
        (
            "google",
            &[
                "gemini-2.5-pro",
                "gemini-2.5-flash",
                "gemini-3-pro-preview",
                "gemini-3-flash-preview",
            ],
        ),
    ];

    let mut total_models = 0;
    for (provider_id, model_ids) in seeded {
        let mut models = Map::new();
        for model_id in model_ids {
            let rate = rates
                .get(*model_id)
                .copied()
                .unwrap_or([3.0, 15.0, 3.75, 0.3]);
            models.insert(
                (*model_id).to_string(),
                json!({
                    "id": model_id,
                    "name": format!("Synthetic {model_id}"),
                    "family": provider_id,
                    "attachment": true,
                    "reasoning": true,
                    "tool_call": true,
                    "limit": {"context": 200_000, "output": 32_000},
                    "cost": {
                        "input": js_num(rate[0]),
                        "output": js_num(rate[1]),
                        "cache_write": js_num(rate[2]),
                        "cache_read": js_num(rate[3]),
                    },
                }),
            );
            total_models += 1;
        }
        providers.insert(
            provider_id.to_string(),
            json!({
                "id": provider_id,
                "name": format!("Synthetic {provider_id}"),
                "models": models,
            }),
        );
    }

    for p in 0..provider_count {
        let provider_id = format!("synth-provider-{:03}", p + 1);
        let mut models = Map::new();
        for m in 0..models_per_provider {
            let model_id = format!("{provider_id}-model-{:02}", m + 1);
            let input = to_fixed_num(0.25 + rng.next_f64() * 14.0, 2);
            let output = to_fixed_num(input * (3.0 + rng.next_f64() * 2.0), 2);
            models.insert(
                model_id.clone(),
                json!({
                    "id": model_id,
                    "name": format!("Synthetic Model {}.{}", p + 1, m + 1),
                    "family": provider_id,
                    "attachment": m % 2 == 0,
                    "reasoning": m % 3 == 0,
                    "limit": {"context": 128_000, "output": 16_384},
                    "modalities": {"input": ["text"], "output": ["text"]},
                    "release_date": "2026-06-01",
                    "cost": {
                        "input": js_num(input),
                        "output": js_num(output),
                        "cache_read": to_fixed_json(input * 0.1, 3),
                        "cache_write": to_fixed_json(input * 1.25, 3),
                    },
                }),
            );
            total_models += 1;
        }
        providers.insert(
            provider_id.clone(),
            json!({
                "id": provider_id,
                "name": format!("Synthetic Provider {}", p + 1),
                "npm": format!("@example/{provider_id}"),
                "models": models,
            }),
        );
    }

    let num_providers = providers.len();
    let payload = json!({
        "version": 1,
        "fetchedAt": now_ms,
        "providers": providers,
    });
    write_json_compact(
        &ccs_dir.join("models-dev-registry-cache.json"),
        &payload,
        Some(now_ms),
    )?;
    Ok(PricingGenResult {
        providers: num_providers,
        models: total_models,
    })
}

fn synthetic_account_windows(
    account: &SyntheticAccount,
    t_ms: i64,
    start_ms: i64,
    rng: &mut Rng,
) -> Vec<Json> {
    let minute = (t_ms - start_ms).div_euclid(MIN_MS);
    let i = account.index as i64;
    let (_, _, _, hour_of_day, _, _, _) = aac_num::civil::utc_fields(t_ms);
    let busy = hour_of_day >= 8 + (i % 5) && hour_of_day < 17 + (i % 4);
    let five_start = minute.div_euclid(300) * 300;
    let week_start = minute.div_euclid(10_080) * 10_080;
    let base_five = if busy {
        ((minute - five_start) as f64) / (3.2 + ((i % 4) as f64))
    } else {
        (((five_start / 300) % 9) as f64) * 2.5
    };
    let five = to_fixed_num((base_five + rng.next_f64() * 1.5).min(100.0), 1);
    let week = to_fixed_num(
        (((minute - week_start) as f64) / ((95 + i * 4) as f64) + rng.next_f64() * 0.8).min(100.0),
        1,
    );
    let five_reset = iso_ms(start_ms + (five_start + 300) * MIN_MS);
    let week_reset = iso_ms(start_ms + (week_start + 10_080) * MIN_MS);

    if account.provider == "claude" {
        let opus = to_fixed_num((week * 0.55).min(100.0), 1);
        return vec![
            json!({
                "key": "five_hour",
                "label": "Five-hour usage",
                "kind": "rate_limit",
                "usedPercent": js_num(five),
                "remainingPercent": to_fixed_json(100.0 - five, 1),
                "resetAt": five_reset,
                "windowMinutes": 300,
                "used": null,
                "limit": null,
                "remaining": null,
                "unit": null,
            }),
            json!({
                "key": "seven_day",
                "label": "Weekly usage",
                "kind": "rate_limit",
                "usedPercent": js_num(week),
                "remainingPercent": to_fixed_json(100.0 - week, 1),
                "resetAt": week_reset,
                "windowMinutes": 10080,
                "used": null,
                "limit": null,
                "remaining": null,
                "unit": null,
            }),
            json!({
                "key": "seven_day_opus",
                "label": "Weekly Opus",
                "kind": "rate_limit",
                "usedPercent": js_num(opus),
                "remainingPercent": to_fixed_json(100.0 - opus, 1),
                "resetAt": week_reset,
                "windowMinutes": 10080,
                "used": null,
                "limit": null,
                "remaining": null,
                "unit": null,
            }),
        ];
    }

    if account.provider == "codex" {
        let credits = aac_num::ecma::math_round(5000.0 - (minute as f64) / ((35 + i * 2) as f64))
            .max(0.0) as i64;
        return vec![
            json!({
                "key": "five_hour",
                "label": "5h",
                "kind": "rate_limit",
                "usedPercent": js_num(five),
                "remainingPercent": to_fixed_json(100.0 - five, 1),
                "resetAt": five_reset,
                "windowMinutes": 300,
                "used": null,
                "limit": null,
                "remaining": null,
                "unit": null,
            }),
            json!({
                "key": "seven_day",
                "label": "week",
                "kind": "rate_limit",
                "usedPercent": js_num(week),
                "remainingPercent": to_fixed_json(100.0 - week, 1),
                "resetAt": week_reset,
                "windowMinutes": 10080,
                "used": null,
                "limit": null,
                "remaining": null,
                "unit": null,
            }),
            json!({
                "key": "credits",
                "label": "Credits",
                "kind": "balance",
                "usedPercent": null,
                "remainingPercent": null,
                "resetAt": null,
                "windowMinutes": null,
                "used": null,
                "limit": null,
                "remaining": credits,
                "unit": "credits",
            }),
        ];
    }

    let flash = to_fixed_num((five * 0.4).min(100.0), 1);
    vec![
        json!({
            "key": "gemini-pro",
            "label": "Gemini Pro",
            "kind": "rate_limit",
            "usedPercent": js_num(five),
            "remainingPercent": to_fixed_json(100.0 - five, 1),
            "resetAt": five_reset,
            "windowMinutes": 300,
            "used": null,
            "limit": null,
            "remaining": null,
            "unit": null,
        }),
        json!({
            "key": "gemini-flash",
            "label": "Gemini Flash",
            "kind": "rate_limit",
            "usedPercent": js_num(flash),
            "remainingPercent": to_fixed_json(100.0 - flash, 1),
            "resetAt": five_reset,
            "windowMinutes": 300,
            "used": null,
            "limit": null,
            "remaining": null,
            "unit": null,
        }),
    ]
}

fn model_cost_usd(
    rates: &BTreeMap<String, [f64; 4]>,
    model_id: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
    let rate = rates
        .get(model_id)
        .copied()
        .unwrap_or([3.0, 15.0, 3.75, 0.3]);
    let raw = ((input_tokens as f64) * rate[0]
        + (output_tokens as f64) * rate[1]
        + (cache_creation_tokens as f64) * rate[2]
        + (cache_read_tokens as f64) * rate[3])
        / 1e6;
    to_fixed_num(raw, 6)
}

struct AnalyticsGenResult {
    quota_files: Vec<PathBuf>,
    activity_files: Vec<PathBuf>,
    quota_observations: usize,
    legacy_source_accounts: usize,
    activity_events: usize,
    sessions: usize,
}

#[derive(Clone)]
struct SynthSession {
    session_key: String,
    session_hash: String,
    account: SyntheticAccount,
    host: &'static str,
    first_ms: i64,
    last_ms: i64,
    model: ModelSpec,
    cwd_key: String,
    cwd_path: String,
}

#[allow(clippy::too_many_arguments)]
fn generate_analytics_usage_logs(
    repo_root: &Path,
    usage_logs_dir: &Path,
    rates: &BTreeMap<String, [f64; 4]>,
    accounts: &[SyntheticAccount],
    rng: &mut Rng,
    now_ms: i64,
    days: usize,
    quota_samples_per_day: usize,
    activity_sessions: usize,
) -> Result<AnalyticsGenResult, Box<dyn Error>> {
    fs::create_dir_all(usage_logs_dir)?;
    let quota_dir = usage_logs_dir.join("quota-history");
    fs::create_dir_all(&quota_dir)?;

    let legacy_accounts = build_legacy_source_accounts(repo_root)?;
    let mut quota_accounts: Vec<SyntheticAccount> =
        Vec::with_capacity(accounts.len() + legacy_accounts.len());
    quota_accounts.extend_from_slice(accounts);
    quota_accounts.extend(legacy_accounts.iter().cloned());

    let start_ms = now_ms - (days as i64) * DAY_MS;
    let step_ms = (5 * MIN_MS).max(DAY_MS / (quota_samples_per_day.max(1) as i64));
    let mut quota_observations = 0;
    let mut quota_files = Vec::with_capacity(days);

    for d in 0..days {
        let day_start = start_ms + (d as i64) * DAY_MS;
        let mut records = Vec::new();
        for s in 0..quota_samples_per_day {
            let t = day_start + (s as i64) * step_ms;
            if t >= now_ms {
                break;
            }
            let stamped = iso_ms(t);
            for account in &quota_accounts {
                let status = if rng.next_f64() < 0.004 {
                    "error"
                } else {
                    "ok"
                };
                let windows = if status == "ok" {
                    synthetic_account_windows(account, t, start_ms, rng)
                } else {
                    Vec::new()
                };
                records.push(json!({
                    "identity": account.identity,
                    "provider": account.provider,
                    "accountId": account.id,
                    "plan": account.plan,
                    "sampledAt": stamped,
                    "observedAt": stamped,
                    "platform": account.platform,
                    "source": account.source,
                    "status": status,
                    "isActive": account.is_active,
                    "windows": windows,
                }));
            }
        }
        quota_observations += records.len();
        let day_file = quota_dir.join(format!("quota-day-{:03}.json", d + 1));
        write_json_compact(
            &day_file,
            &json!({
                "schemaVersion": 1,
                "collectedSince": iso_ms(start_ms),
                "records": records,
            }),
            None,
        )?;
        quota_files.push(day_file);
    }

    let mut accounts_by_provider: BTreeMap<&str, Vec<&SyntheticAccount>> = BTreeMap::new();
    for account in accounts {
        accounts_by_provider
            .entry(account.provider.as_str())
            .or_default()
            .push(account);
    }

    let mut sessions_per_provider: BTreeMap<&str, Vec<SynthSession>> = BTreeMap::new();
    for spec in &PROVIDER_SPECS {
        sessions_per_provider.insert(spec.id, Vec::new());
    }
    for s in 0..activity_sessions {
        let account = &accounts[s % accounts.len()];
        let host = HOSTS[(s * 3 + account.index) % HOSTS.len()];
        let session_hash = sha256_hex(format!("aac-synth-session:{}:{s}", account.provider));
        let session_key = session_hash[..16].to_string();
        let span_hours = rng.int(2, 18);
        let max_offset = ((days as i64) * 24 - span_hours - 1).max(0);
        let start_hour_offset = rng.int(0, max_offset);
        let first_ms = start_ms + start_hour_offset * HOUR_MS;
        let last_ms = (now_ms - HOUR_MS).min(first_ms + span_hours * HOUR_MS);
        let models = account.models();
        let model = models[s % models.len()];
        let cwd_key = sha256_hex(format!("cwd:{host}:{}", s % 12))[..16].to_string();
        let cwd_path = format!("~/work/example-project-{:02}", (s % 12) + 1);
        if let Some(list) = sessions_per_provider.get_mut(account.provider.as_str()) {
            list.push(SynthSession {
                session_key,
                session_hash,
                account: account.clone(),
                host,
                first_ms,
                last_ms,
                model,
                cwd_key,
                cwd_path,
            });
        }
    }

    let mut total_activity_events = 0;
    let mut activity_files = Vec::with_capacity(PROVIDER_SPECS.len());

    for spec in &PROVIDER_SPECS {
        let provider_accounts = accounts_by_provider
            .get(spec.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let provider_sessions = sessions_per_provider
            .get(spec.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        let mut hour_sessions: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
        for (idx, sess) in provider_sessions.iter().enumerate() {
            let mut h = sess.first_ms;
            while h <= sess.last_ms {
                hour_sessions.entry(h).or_default().push(idx);
                h += HOUR_MS;
            }
        }

        let mut lines: Vec<String> = Vec::new();
        let mut h = start_ms;
        while h < now_ms {
            let (_, _, _, hod, _, _, _) = aac_num::civil::utc_fields(h);
            let dow = (h.div_euclid(DAY_MS) + 4).rem_euclid(7);
            let hod_f = hod as f64;
            let work_wave = (-((hod_f - 15.0).powi(2)) / 18.0).exp()
                + 0.35 * (-((hod_f - 21.0).powi(2)) / 6.0).exp();
            let weekend = if dow == 0 || dow == 6 { 0.5 } else { 1.0 };
            let intensity = work_wave * weekend * (0.6 + rng.next_f64() * 0.8);
            if intensity < 0.05 {
                h += HOUR_MS;
                continue;
            }

            struct Entry<'a> {
                host: &'static str,
                account: &'a SyntheticAccount,
                model: ModelSpec,
                session_key: Option<String>,
                session_hash: Option<String>,
                cwd_key: Option<String>,
                cwd_path: Option<String>,
            }
            let mut entries: Vec<Entry<'_>> = Vec::new();
            if let Some(indices) = hour_sessions.get(&h) {
                for &idx in indices {
                    let sess = &provider_sessions[idx];
                    entries.push(Entry {
                        host: sess.host,
                        account: &sess.account,
                        model: sess.model,
                        session_key: Some(sess.session_key.clone()),
                        session_hash: Some(sess.session_hash.clone()),
                        cwd_key: Some(sess.cwd_key.clone()),
                        cwd_path: Some(sess.cwd_path.clone()),
                    });
                }
            }
            let floor_count = 4_usize.saturating_sub(entries.len());
            let hour_idx = h.div_euclid(HOUR_MS) as usize;
            for e in 0..floor_count {
                let account = provider_accounts[(hour_idx + e) % provider_accounts.len()];
                let models = account.models();
                let model = models[(hour_idx + e) % models.len()];
                let host = HOSTS[(hour_idx + account.index + e) % HOSTS.len()];
                let fallback_sess = if provider_sessions.is_empty() {
                    None
                } else {
                    Some(&provider_sessions[(hour_idx + e) % provider_sessions.len()])
                };
                entries.push(Entry {
                    host,
                    account,
                    model,
                    session_key: fallback_sess.map(|s| s.session_key.clone()),
                    session_hash: fallback_sess.map(|s| s.session_hash.clone()),
                    cwd_key: fallback_sess.map(|s| s.cwd_key.clone()),
                    cwd_path: fallback_sess.map(|s| s.cwd_path.clone()),
                });
            }

            let hour_iso = iso_sec(h);
            for entry in entries {
                let base = 1.6e6 * intensity * entry.model.weight * (0.65 + rng.next_f64() * 0.7);
                let input_tokens = aac_num::ecma::math_round(base).max(500.0) as u64;
                let output_tokens = aac_num::ecma::math_round(base * (0.06 + rng.next_f64() * 0.08))
                    .max(100.0) as u64;
                let cache_creation_tokens = if entry.model.cache_create > 0.0 {
                    aac_num::ecma::math_round(base * entry.model.cache_create) as u64
                } else {
                    0
                };
                let cache_read_tokens = aac_num::ecma::math_round(
                    base * entry.model.cache_k * (0.75 + rng.next_f64() * 0.5),
                ) as u64;
                let cost_usd = model_cost_usd(
                    rates,
                    entry.model.id,
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                );
                let events =
                    aac_num::ecma::math_round((input_tokens as f64) / 35_000.0).max(1.0) as u64;
                lines.push(serde_json::to_string(&json!({
                    "hour": hour_iso,
                    "provider": spec.id,
                    "tool": spec.tool,
                    "route": spec.route,
                    "host": entry.host,
                    "accountId": entry.account.id,
                    "accountKey": entry.account.identity,
                    "modelRaw": entry.model.id,
                    "sessionKey": entry.session_key,
                    "sessionHash": entry.session_hash,
                    "cwdKey": entry.cwd_key,
                    "cwdPath": entry.cwd_path,
                    "inputTokens": input_tokens,
                    "outputTokens": output_tokens,
                    "cacheCreationTokens": cache_creation_tokens,
                    "cacheReadTokens": cache_read_tokens,
                    "costUsd": js_num(cost_usd),
                    "events": events,
                }))?);
            }
            h += HOUR_MS;
        }

        let file = usage_logs_dir.join(format!("activity-{}.jsonl", spec.id));
        let mut body = lines.join("\n");
        if !lines.is_empty() {
            body.push('\n');
        }
        fs::write(&file, body)?;
        total_activity_events += lines.len();
        activity_files.push(file);
    }

    Ok(AnalyticsGenResult {
        quota_files,
        activity_files,
        quota_observations,
        legacy_source_accounts: legacy_accounts.len(),
        activity_events: total_activity_events,
        sessions: activity_sessions,
    })
}

fn window_identity(win: &Json) -> String {
    let key = win.get("key").cloned().unwrap_or(Json::Null);
    let kind = win.get("kind").cloned().unwrap_or(Json::Null);
    let unit = win.get("unit").cloned().unwrap_or(Json::Null);
    serde_json::to_string(&[key, kind, unit]).unwrap_or_else(|_| "[]".to_string())
}

fn migrate_store_from_fixture(
    ccs_dir: &Path,
    reports_dir: &Path,
    quota_files: &[PathBuf],
    activity_files: &[PathBuf],
    now_ms: i64,
    days: usize,
) -> Result<Json, Box<dyn Error>> {
    let store_dir = ccs_dir.join("store");
    fs::create_dir_all(&store_dir)?;
    set_mode_0700(&store_dir)?;

    let engine =
        aac_store::ops::open_engine_with(&store_dir.to_string_lossy(), None, Some(0), None)
            .map_err(|e| format!("open aac-store failed: {e}"))?;

    let call = |method: &str, payload: &Json| -> Result<Json, Box<dyn Error>> {
        let args = serde_json::to_string(payload)?;
        aac_store::ops::call(&engine, method, &args)
            .map_err(|e| format!("aac-store {method} failed: {e}").into())
    };

    let result = (|| -> Result<Json, Box<dyn Error>> {
        let mut quota_sampled = 0_u64;
        let mut quota_seen = 0_u64;
        let mut quota_rejected = 0_u64;
        let mut activity_reports = 0_u64;
        let mut activity_cells_put = 0_u64;
        let mut runs_recorded = 0_u64;
        let mut runs_refused = 0_u64;
        let mut runs_passes = 0_u64;

        // 1. Migrate quota observations
        let mut batch: Vec<Json> = Vec::with_capacity(2000);
        let mut flush_quota = |batch: &mut Vec<Json>| -> Result<(), Box<dyn Error>> {
            if batch.is_empty() {
                return Ok(());
            }
            let res = call("record", &json!({ "observations": batch }))?;
            quota_sampled += res["sampled"].as_u64().unwrap_or(0);
            quota_seen += res["seen"].as_u64().unwrap_or(0);
            let rej = res["rejected"].as_u64().unwrap_or(0);
            quota_rejected += rej;
            if rej > 0 {
                return Err(format!("Store rejected {rej} synthetic quota observations").into());
            }
            batch.clear();
            Ok(())
        };

        for file in quota_files {
            let doc: Json = serde_json::from_str(&fs::read_to_string(file)?)?;
            let label = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut max_day_ms = 0_i64;
            if let Some(records) = doc.get("records").and_then(Json::as_array) {
                for (idx, rec) in records.iter().enumerate() {
                    let sampled_ms =
                        aac_num::ecma::parse_instant(rec["sampledAt"].as_str().unwrap_or(""))
                            .ok_or("invalid sampledAt")?;
                    let observed_ms =
                        aac_num::ecma::parse_instant(rec["observedAt"].as_str().unwrap_or(""))
                            .ok_or("invalid observedAt")?;
                    if sampled_ms > max_day_ms {
                        max_day_ms = sampled_ms;
                    }
                    let windows: Vec<Json> = rec
                        .get("windows")
                        .and_then(Json::as_array)
                        .map(|arr| {
                            arr.iter()
                                .map(|w| {
                                    let reset_at_ms = w
                                        .get("resetAt")
                                        .and_then(Json::as_str)
                                        .and_then(aac_num::ecma::parse_instant);
                                    json!({
                                        "identity": window_identity(w),
                                        "key": w["key"],
                                        "label": w["label"],
                                        "kind": w.get("kind").cloned().unwrap_or(Json::Null),
                                        "unit": w.get("unit").cloned().unwrap_or(Json::Null),
                                        "used_pct": w.get("usedPercent").cloned().unwrap_or(Json::Null),
                                        "remaining_pct": w.get("remainingPercent").cloned().unwrap_or(Json::Null),
                                        "used": w.get("used").cloned().unwrap_or(Json::Null),
                                        "limit": w.get("limit").cloned().unwrap_or(Json::Null),
                                        "remaining": w.get("remaining").cloned().unwrap_or(Json::Null),
                                        "reset_at_ms": reset_at_ms,
                                        "expires_at_ms": null,
                                        "window_minutes": w.get("windowMinutes").cloned().unwrap_or(Json::Null),
                                        "unlimited": null,
                                        "enabled": null,
                                        "cached_sampled_at_ms": null,
                                        "pool_id": null,
                                        "pool_id_source": null,
                                        "pool_label": null,
                                        "model_ids": null,
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    batch.push(json!({
                        "account": {
                            "identity": rec["identity"],
                            "provider": rec["provider"],
                            "account_id": rec["accountId"],
                            "plan": rec.get("plan").cloned().unwrap_or(Json::Null),
                            "platform": rec["platform"],
                            "source": rec["source"],
                            "status": rec["status"],
                            "is_active": rec["isActive"].as_bool() == Some(true),
                        },
                        "sampled_at_ms": sampled_ms,
                        "observed_at_ms": observed_ms,
                        "windows": windows,
                        "import_ref": format!("{label}#{idx}"),
                    }));
                    if batch.len() >= 2000 {
                        flush_quota(&mut batch)?;
                    }
                }
            }
            flush_quota(&mut batch)?;
            if max_day_ms > 0 {
                let maintain_ms = now_ms.min(max_day_ms + 3 * HOUR_MS);
                let _ = call("maintain", &json!({ "nowMs": maintain_ms }))?;
            }
        }

        // 2. Migrate hourly activity & sessions
        let cover_from = iso_sec(now_ms - (days as i64) * DAY_MS);
        let read_at = iso_sec(now_ms);

        struct CellAcc {
            hour: String,
            session_key: Option<String>,
            session_hash: Option<String>,
            route: String,
            model_raw: String,
            account_key: Option<String>,
            account_source: Option<String>,
            input_tokens: u64,
            output_tokens: u64,
            cache_creation_tokens: u64,
            cache_read_tokens: u64,
            cost_usd: f64,
            events: u64,
        }
        struct PartAcc {
            route: String,
            model_raw: String,
            input_tokens: u64,
            output_tokens: u64,
            cache_creation_tokens: u64,
            cache_read_tokens: u64,
            cost_usd: f64,
        }
        struct SessAcc {
            session_key: String,
            session_hash: Option<String>,
            first: String,
            last: String,
            parts: IndexMap<String, PartAcc>,
            cwd: Option<(String, String)>,
        }
        struct HostToolBucket {
            computer: String,
            tool: String,
            cells: IndexMap<String, CellAcc>,
            sessions: IndexMap<String, SessAcc>,
        }

        for file in activity_files {
            let raw = fs::read_to_string(file)?;
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mut by_host_tool: IndexMap<String, HostToolBucket> = IndexMap::new();
            for line in trimmed.lines() {
                if line.is_empty() {
                    continue;
                }
                let ev: Json = serde_json::from_str(line)?;
                let host = ev["host"].as_str().unwrap_or("").to_string();
                let tool = ev["tool"].as_str().unwrap_or("").to_string();
                let key = format!("{host}|{tool}");
                let bucket = by_host_tool.entry(key).or_insert_with(|| HostToolBucket {
                    computer: host,
                    tool,
                    cells: IndexMap::new(),
                    sessions: IndexMap::new(),
                });

                let hour = ev["hour"].as_str().unwrap_or("").to_string();
                let session_key = ev
                    .get("sessionKey")
                    .and_then(Json::as_str)
                    .map(ToString::to_string);
                let session_hash = ev
                    .get("sessionHash")
                    .and_then(Json::as_str)
                    .map(ToString::to_string);
                let route = ev["route"].as_str().unwrap_or("").to_string();
                let model_raw = ev["modelRaw"].as_str().unwrap_or("").to_string();
                let account_key = ev
                    .get("accountKey")
                    .and_then(Json::as_str)
                    .map(ToString::to_string);
                let input_tokens = ev["inputTokens"].as_u64().unwrap_or(0);
                let output_tokens = ev["outputTokens"].as_u64().unwrap_or(0);
                let cache_creation_tokens = ev["cacheCreationTokens"].as_u64().unwrap_or(0);
                let cache_read_tokens = ev["cacheReadTokens"].as_u64().unwrap_or(0);
                let cost_usd = ev["costUsd"].as_f64().unwrap_or(0.0);
                let events = ev["events"].as_u64().unwrap_or(0);

                let cell_key = format!(
                    "{hour}|{}|{route}|{model_raw}|{}",
                    session_key.as_deref().unwrap_or(""),
                    account_key.as_deref().unwrap_or(""),
                );
                let cell = bucket.cells.entry(cell_key).or_insert_with(|| CellAcc {
                    hour: hour.clone(),
                    session_key: session_key.clone(),
                    session_hash: session_hash.clone(),
                    route: route.clone(),
                    model_raw: model_raw.clone(),
                    account_source: account_key.as_ref().map(|_| "fixture".to_string()),
                    account_key: account_key.clone(),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_read_tokens: 0,
                    cost_usd: 0.0,
                    events: 0,
                });
                cell.input_tokens += input_tokens;
                cell.output_tokens += output_tokens;
                cell.cache_creation_tokens += cache_creation_tokens;
                cell.cache_read_tokens += cache_read_tokens;
                cell.cost_usd = to_fixed_num(cell.cost_usd + cost_usd, 6);
                cell.events += events;

                if let Some(sk) = session_key {
                    let cwd = match (
                        ev.get("cwdKey").and_then(Json::as_str),
                        ev.get("cwdPath").and_then(Json::as_str),
                    ) {
                        (Some(k), Some(p)) => Some((k.to_string(), p.to_string())),
                        _ => None,
                    };
                    let sess = bucket
                        .sessions
                        .entry(sk.clone())
                        .or_insert_with(|| SessAcc {
                            session_key: sk,
                            session_hash,
                            first: hour.clone(),
                            last: hour.clone(),
                            parts: IndexMap::new(),
                            cwd,
                        });
                    if hour < sess.first {
                        sess.first = hour.clone();
                    }
                    if hour > sess.last {
                        sess.last = hour.clone();
                    }
                    let part_key = format!("{route}|{model_raw}");
                    let part = sess.parts.entry(part_key).or_insert_with(|| PartAcc {
                        route,
                        model_raw,
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_creation_tokens: 0,
                        cache_read_tokens: 0,
                        cost_usd: 0.0,
                    });
                    part.input_tokens += input_tokens;
                    part.output_tokens += output_tokens;
                    part.cache_creation_tokens += cache_creation_tokens;
                    part.cache_read_tokens += cache_read_tokens;
                    part.cost_usd = to_fixed_num(part.cost_usd + cost_usd, 6);
                }
            }

            for bucket in by_host_tool.values() {
                let cells_json: Vec<Json> = bucket
                    .cells
                    .values()
                    .map(|c| {
                        json!({
                            "hour": c.hour,
                            "sessionKey": c.session_key,
                            "sessionHash": c.session_hash,
                            "route": c.route,
                            "modelRaw": c.model_raw,
                            "accountKey": c.account_key,
                            "accountSource": c.account_source,
                            "inputTokens": c.input_tokens,
                            "outputTokens": c.output_tokens,
                            "cacheCreationTokens": c.cache_creation_tokens,
                            "cacheReadTokens": c.cache_read_tokens,
                            "costUsd": js_num(c.cost_usd),
                            "fallbackCostUsd": 0,
                            "events": c.events,
                        })
                    })
                    .collect();
                let sessions_json: Vec<Json> = bucket
                    .sessions
                    .values()
                    .map(|s| {
                        let parts_json: Vec<Json> = s
                            .parts
                            .values()
                            .take(32)
                            .map(|p| {
                                json!({
                                    "route": p.route,
                                    "modelRaw": p.model_raw,
                                    "inputTokens": p.input_tokens,
                                    "outputTokens": p.output_tokens,
                                    "cacheCreationTokens": p.cache_creation_tokens,
                                    "cacheReadTokens": p.cache_read_tokens,
                                    "costUsd": js_num(p.cost_usd),
                                    "fallbackCostUsd": 0,
                                })
                            })
                            .collect();
                        let cwd_json = s
                            .cwd
                            .as_ref()
                            .map(|(k, p)| json!({"key": k, "path": p}))
                            .unwrap_or(Json::Null);
                        json!({
                            "sessionKey": s.session_key,
                            "sessionHash": s.session_hash,
                            "first": s.first,
                            "last": s.last,
                            "parts": parts_json,
                            "target": null,
                            "cwd": cwd_json,
                        })
                    })
                    .collect();

                let report = json!({
                    "schema": "aac.usage-report.v1",
                    "computer": bucket.computer,
                    "tool": bucket.tool,
                    "readAt": read_at,
                    "state": "ok",
                    "complete": true,
                    "coverFrom": cover_from,
                    "kind": "import",
                    "cells": cells_json,
                    "sessions": sessions_json,
                });
                let res = call("activityIngest", &json!({ "reports": [report] }))?;
                if res
                    .get("refused")
                    .and_then(Json::as_array)
                    .is_some_and(|arr| !arr.is_empty())
                {
                    return Err(format!(
                        "Store refused synthetic activity report ({}/{}): {}",
                        bucket.computer, bucket.tool, res["refused"]
                    )
                    .into());
                }
                activity_reports += 1;
                activity_cells_put += res["put"].as_u64().unwrap_or(0);
            }
        }

        // 3. Migrate agent-report runs
        let mut more = true;
        let mut first_pass = true;
        while more {
            let scan = call(
                "runsScan",
                &json!({ "root": reports_dir.to_string_lossy(), "full": first_pass }),
            )?;
            first_pass = false;
            let mut files: Vec<Json> = Vec::new();
            let mut refused: Vec<Json> = scan
                .get("refused")
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default();
            if let Some(scan_files) = scan.get("files").and_then(Json::as_array) {
                for item in scan_files {
                    let text = item.get("text").and_then(Json::as_str).unwrap_or("");
                    if text.contains('\0') {
                        refused.push(json!({
                            "path": item["path"],
                            "size": item["size"],
                            "mtimeMs": item["mtimeMs"],
                            "reason": "file contains NUL bytes",
                            "sha256": sha256_hex(text),
                        }));
                        continue;
                    }
                    if serde_json::from_str::<Json>(text).is_ok() {
                        files.push(item.clone());
                    } else {
                        refused.push(json!({
                            "path": item["path"],
                            "size": item["size"],
                            "mtimeMs": item["mtimeMs"],
                            "reason": "not valid JSON",
                            "sha256": sha256_hex(text),
                        }));
                    }
                }
            }
            let files_len = files.len();
            let refused_len = refused.len();
            let _rec_res = call(
                "runsRecord",
                &json!({
                    "files": files,
                    "refused": refused,
                    "threads": scan.get("threads").cloned().unwrap_or_else(|| json!({})),
                    "catalog": {
                        "models": scan.get("catalog").and_then(|c| c.get("models")).cloned().unwrap_or(Json::Null),
                        "aliases": scan.get("catalog").and_then(|c| c.get("aliases")).cloned().unwrap_or(Json::Null),
                    },
                    "nowMs": now_ms,
                }),
            )?;
            runs_passes += 1;
            runs_recorded += files_len as u64;
            runs_refused += refused_len as u64;
            more = scan.get("more").and_then(Json::as_bool).unwrap_or(false)
                && (files_len > 0 || refused_len > 0);
            if runs_passes > 20 {
                break;
            }
        }

        let _ = call("maintain", &json!({ "nowMs": now_ms, "compact": true }))?;
        let status = call("status", &json!({}))?;
        let redb_bytes = status["redbBytes"].as_u64().unwrap_or(0);
        let runs_status = call("runsStatus", &json!({}))?;

        Ok(json!({
            "quotaSampled": quota_sampled,
            "quotaSeen": quota_seen,
            "quotaRejected": quota_rejected,
            "activityReports": activity_reports,
            "activityCellsPut": activity_cells_put,
            "runsRecorded": runs_recorded,
            "runsRefused": runs_refused,
            "runsPasses": runs_passes,
            "redbBytes": redb_bytes,
            "runsStatus": runs_status,
        }))
    })();

    let close_res = engine.close();
    match (result, close_res) {
        (Ok(stats), Ok(())) => Ok(stats),
        (Err(err), _) => Err(err),
        (Ok(_), Err(err)) => Err(format!("close aac-store failed: {err}").into()),
    }
}

/// Generate the full deterministic synthetic fixture and optionally migrate its store.
pub fn generate_memory_fixture(
    repo_root: &Path,
    out_dir: &Path,
    options: &FixtureOptions,
) -> Result<FixtureManifest, Box<dyn Error>> {
    let started = Instant::now();
    let now_ms = match options.now {
        Some(ms) => ms,
        None => {
            let epoch_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            epoch_ms.div_euclid(HOUR_MS) * HOUR_MS
        }
    };
    let root = if out_dir.is_absolute() {
        out_dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(out_dir)
    };
    let home = root.join("home");
    let ccs_dir = home.join(".ccs");
    let reports_dir = root.join("agent-reports");
    let registry_dir = reports_dir.join("pm-mail-registry");
    let usage_logs_dir = root.join("usage-logs");

    if let Ok(meta) = fs::symlink_metadata(&root) {
        if !meta.file_type().is_dir() {
            return Err(format!(
                "Fixture output must be a directory, with no symlink at the destination: {}",
                root.display()
            )
            .into());
        }
        if fs::read_dir(&root)?.next().is_some() {
            return Err(format!(
                "{} already contains a generated fixture; pass an empty directory.",
                root.display()
            )
            .into());
        }
    }

    fs::create_dir_all(&ccs_dir)?;
    set_mode_0700(&ccs_dir)?;
    let mut rng = Rng::new(options.seed);
    let rates = static_rates_map()?;

    write_status_fixture_for_perf(&reports_dir, now_ms)?;
    seed_deterministic_status_answers(&reports_dir, &ccs_dir, options.seed, now_ms)?;
    let base_runs_len = write_scoreboard_fixture_for_perf(repo_root, &reports_dir, now_ms)?;

    let writer_res = generate_writers(
        repo_root,
        &reports_dir,
        &mut rng,
        now_ms,
        options.writers,
        options.run_writers,
    )?;
    let total_runs = generate_runs(
        repo_root,
        &reports_dir,
        &writer_res.run_writers,
        &mut rng,
        now_ms,
        options.runs,
        options.days,
        base_runs_len,
    )?;
    let total_registry =
        generate_mail_registry(&reports_dir, &mut rng, now_ms, options.registry_entries)?;
    let pricing_res = generate_models_dev_cache(
        &ccs_dir,
        &rates,
        &mut rng,
        now_ms,
        options.pricing_providers,
        options.pricing_models_per_provider,
    )?;
    let accounts = build_synthetic_accounts(options.accounts);
    let analytics_res = generate_analytics_usage_logs(
        repo_root,
        &usage_logs_dir,
        &rates,
        &accounts,
        &mut rng,
        now_ms,
        options.days,
        options.quota_samples_per_day,
        options.activity_sessions,
    )?;

    let store_stats = if options.skip_store {
        None
    } else {
        Some(migrate_store_from_fixture(
            &ccs_dir,
            &reports_dir,
            &analytics_res.quota_files,
            &analytics_res.activity_files,
            now_ms,
            options.days,
        )?)
    };

    let manifest = FixtureManifest {
        schema_version: 1,
        seed: options.seed,
        now: now_ms,
        now_iso: iso_sec(now_ms),
        root: root.clone(),
        home,
        ccs_dir,
        reports_dir,
        registry_dir,
        usage_logs_dir,
        counts: FixtureCounts {
            writers: writer_res.total_writers,
            run_writers: writer_res.run_writers.len(),
            runs: total_runs,
            registry_entries: total_registry,
            providers: PROVIDER_SPECS.len(),
            accounts: accounts.len(),
            legacy_source_accounts: analytics_res.legacy_source_accounts,
            days: options.days,
            quota_observations: analytics_res.quota_observations,
            activity_events: analytics_res.activity_events,
            activity_sessions: analytics_res.sessions,
            pricing_providers: pricing_res.providers,
            pricing_models: pricing_res.models,
        },
        store: store_stats,
        generation_ms: started.elapsed().as_millis() as u64,
    };

    write_json_compact(
        &root.join("fixture-manifest.json"),
        &serde_json::to_value(&manifest)?,
        None,
    )?;
    Ok(manifest)
}

struct ParsedFixtureCli {
    help: bool,
    out_dir: Option<PathBuf>,
    options: FixtureOptions,
}

fn parse_fixture_cli(args: &[String]) -> Result<ParsedFixtureCli, Box<dyn Error>> {
    let mut options = FixtureOptions::default();
    let mut out_dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let next = |idx: &mut usize, flag: &str| -> Result<&str, Box<dyn Error>> {
            *idx += 1;
            args.get(*idx)
                .map(String::as_str)
                .ok_or_else(|| format!("Missing value for {flag}").into())
        };
        match arg {
            "--seed" => options.seed = next(&mut i, arg)?.parse()?,
            "--now" => options.now = Some(next(&mut i, arg)?.parse()?),
            "--runs" => options.runs = next(&mut i, arg)?.parse()?,
            "--run-writers" => options.run_writers = next(&mut i, arg)?.parse()?,
            "--writers" => options.writers = next(&mut i, arg)?.parse()?,
            "--registry" => options.registry_entries = next(&mut i, arg)?.parse()?,
            "--days" => options.days = next(&mut i, arg)?.parse()?,
            "--accounts" => options.accounts = next(&mut i, arg)?.parse()?,
            "--quota-per-day" => options.quota_samples_per_day = next(&mut i, arg)?.parse()?,
            "--sessions" => options.activity_sessions = next(&mut i, arg)?.parse()?,
            "--pricing-providers" => options.pricing_providers = next(&mut i, arg)?.parse()?,
            "--pricing-models" => {
                options.pricing_models_per_provider = next(&mut i, arg)?.parse()?;
            }
            "--skip-store" => options.skip_store = true,
            // Accepted for CLI compatibility; Rust links `aac-store` directly.
            "--store-module" => {
                let _ = next(&mut i, arg)?;
            }
            "-h" | "--help" => {
                return Ok(ParsedFixtureCli {
                    help: true,
                    out_dir: None,
                    options,
                });
            }
            other if !other.starts_with('-') && out_dir.is_none() => {
                out_dir = Some(PathBuf::from(other));
            }
            other => return Err(format!("Unknown argument: {other}").into()),
        }
        i += 1;
    }
    Ok(ParsedFixtureCli {
        help: false,
        out_dir,
        options,
    })
}

/// CLI entrypoint for `aac-xtask perf memory-fixture`.
pub fn run_cli(repo_root: &Path, args: &[String]) -> Result<(), Box<dyn Error>> {
    let parsed = parse_fixture_cli(args)?;
    let Some(out_dir) = parsed.out_dir else {
        eprintln!(
            "Usage: aac-xtask perf memory-fixture <out-dir> [--seed 20261125] [--now <epochMs>] [--runs 7500] [--writers 2000] [--registry 4500] [--days 60] [--accounts 19] [--skip-store]"
        );
        if parsed.help {
            return Ok(());
        }
        return Err(Box::new(SilentExit(2)));
    };
    if parsed.help {
        eprintln!(
            "Usage: aac-xtask perf memory-fixture <out-dir> [--seed 20261125] [--now <epochMs>] [--runs 7500] [--writers 2000] [--registry 4500] [--days 60] [--accounts 19] [--skip-store]"
        );
        return Ok(());
    }

    let manifest = generate_memory_fixture(repo_root, &out_dir, &parsed.options)?;
    print!(
        "[OK] Generated synthetic fixture in {} ms at {}\n     writers={} runs={} registry={} providers={} accounts={} days={} quotaObs={} activityEvents={}\n",
        manifest.generation_ms,
        manifest.root.display(),
        manifest.counts.writers,
        manifest.counts.runs,
        manifest.counts.registry_entries,
        manifest.counts.providers,
        manifest.counts.accounts,
        manifest.counts.days,
        manifest.counts.quota_observations,
        manifest.counts.activity_events,
    );
    if let Some(store) = &manifest.store {
        let redb_mib = (store["redbBytes"].as_u64().unwrap_or(0) as f64) / 1_048_576.0;
        println!(
            "     store: redb={redb_mib:.1} MiB, quotaSampled={}, activityCells={}, runsRecorded={}",
            store["quotaSampled"].as_u64().unwrap_or(0),
            store["activityCellsPut"].as_u64().unwrap_or(0),
            store["runsRecorded"].as_u64().unwrap_or(0),
        );
    } else {
        println!("     store: skipped (--skip-store)");
    }
    Ok(())
}
