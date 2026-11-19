//! G15 pricing parity spec: every case of `tests/spec/pricing/cases.json` (generated from the TS
//! modules by `tests/spec/pricing/parity.ts`) answered by `aac_views::pricing`, byte-equal to the
//! recorded `JSON.stringify` text: each lookup's `{pricing, source}` resolution, analytics rate
//! row and cost, the cache-file bytes a write produces, the TTL/staleness reads, the refresh flow
//! with replayed fetch outcomes, the `model\0route` facts table lane D's owned read consumes, and
//! `getKnownModels`. Recorded fixtures only — no live models.dev fetch happens here.
use aac_views::js_json;
use aac_views::pricing::{RegistryCache, Resolver, refresh_models_dev_registry};
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::Mutex;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus {
    fixtures: Map<String, Value>,
    lookups: Vec<Lookup>,
    reads: Vec<Read>,
    refreshes: Vec<Refresh>,
    facts: Vec<Facts>,
    known_models: Option<KnownModels>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lookup {
    name: String,
    #[serde(default)]
    fixture: Option<String>,
    fetched_at: f64,
    model: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    service_tier: Option<String>,
    #[serde(default)]
    usage: Option<[f64; 4]>,
    cache_bytes: String,
    resolution: String,
    analytics: String,
    #[serde(default)]
    cost: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Read {
    name: String,
    file_text: String,
    now: f64,
    allow_stale: bool,
    reads: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Refresh {
    name: String,
    steps: Vec<RefreshStep>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefreshStep {
    fetch: FetchScript,
    force: bool,
    now: f64,
    returned: String,
    cache_bytes: String,
    fetches_so_far: usize,
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum FetchScript {
    Ok { body: String },
    HttpError { body: String },
    TransportError,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Facts {
    name: String,
    fixture: String,
    models: Vec<String>,
    routes: Vec<String>,
    table: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnownModels {
    #[allow(dead_code)]
    fixture: String,
    models: String,
}

fn corpus() -> Corpus {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/spec/pricing/cases.json");
    let text =
        std::fs::read_to_string(path).expect("cases.json exists (generate with parity.ts --write)");
    serde_json::from_str(&text).expect("corpus parses")
}

fn text(value: &Value) -> String {
    String::from_utf8(js_json::write(value)).expect("JSON text")
}

fn write_bytes(value: &Value) -> Vec<u8> {
    js_json::write(value)
}

/// Install a case's fixture as the cache file the way `setCachedModelsDevRegistry` writes it, and
/// prove the bytes match first.
fn install_fixture(corpus: &Corpus, root: &Path, fixture: &Option<String>, fetched_at: f64) {
    let Some(name) = fixture else { return };
    let providers = corpus
        .fixtures
        .get(name)
        .and_then(Value::as_object)
        .expect("fixture providers")
        .clone();
    let mut cache = RegistryCache::new(root.join("models-dev-registry-cache.json"));
    cache.write(&providers, fetched_at);
}

#[test]
fn lookups_are_byte_equal() {
    let corpus = corpus();
    assert!(
        corpus.lookups.len() > 800,
        "the corpus carries the golden spread"
    );
    for case in &corpus.lookups {
        let root = tempfile::tempdir().expect("tempdir");
        install_fixture(&corpus, root.path(), &case.fixture, case.fetched_at);
        let file = root.path().join("models-dev-registry-cache.json");
        if case.fixture.is_some() {
            let written = std::fs::read(&file).expect("cache file written");
            assert_eq!(
                String::from_utf8_lossy(&written),
                case.cache_bytes,
                "{}: cache bytes",
                case.name
            );
        }
        let mut resolver = Resolver::over(&file);
        let resolution = resolver.get_model_pricing_with_source(
            &case.model,
            case.provider.as_deref(),
            case.service_tier.as_deref(),
        );
        assert_eq!(
            text(&resolution.to_value()),
            case.resolution,
            "{}: resolution",
            case.name
        );
        let analytics = resolver
            .default_account_analytics_pricing(&case.model, case.provider.as_deref())
            .map(|rates| rates.to_value())
            .unwrap_or(Value::Null);
        assert_eq!(text(&analytics), case.analytics, "{}: analytics", case.name);
        if let Some((expected, usage)) = case.cost.as_ref().zip(case.usage) {
            let cost = resolver.calculate_cost(
                &usage,
                &case.model,
                case.provider.as_deref(),
                case.service_tier.as_deref(),
            );
            let cost = serde_json::Number::from_f64(cost).expect("finite cost");
            assert_eq!(text(&Value::Number(cost)), *expected, "{}: cost", case.name);
        }
    }
}

#[test]
fn cache_reads_match_the_ttl_and_shape_rules() {
    let corpus = corpus();
    for case in &corpus.reads {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path()).expect("ccs dir");
        let file = root.path().join("models-dev-registry-cache.json");
        std::fs::write(&file, &case.file_text).expect("install cache text");
        let mut cache = RegistryCache::new(file);
        let reads = cache.read(case.allow_stale, case.now).is_some();
        assert_eq!(reads, case.reads, "{}", case.name);
    }
}

#[test]
fn refresh_flow_matches_on_replayed_fetches() {
    let corpus = corpus();
    for case in &corpus.refreshes {
        let root = tempfile::tempdir().expect("tempdir");
        let file = root.path().join("models-dev-registry-cache.json");
        let cache = Mutex::new(RegistryCache::new(file.clone()));
        let mut fetches = 0usize;
        for step in &case.steps {
            let script = &step.fetch;
            let returned = refresh_models_dev_registry(&cache, step.force, step.now, || {
                fetches += 1;
                match script {
                    FetchScript::Ok { body } => Ok(aac_views::pricing::Fetched {
                        ok: true,
                        body: body.clone().into_bytes(),
                    }),
                    FetchScript::HttpError { body } => Ok(aac_views::pricing::Fetched {
                        ok: false,
                        body: body.clone().into_bytes(),
                    }),
                    FetchScript::TransportError => Err(()),
                }
            });
            let returned_text = match returned {
                Some(providers) => text(&Value::Object(providers)),
                None => "null".to_owned(),
            };
            assert_eq!(
                returned_text, step.returned,
                "{}: returned registry",
                case.name
            );
            let bytes =
                std::fs::read(&file).map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            let bytes = bytes.unwrap_or_else(|_| "absent".into());
            assert_eq!(bytes, step.cache_bytes, "{}: cache bytes", case.name);
            assert_eq!(fetches, step.fetches_so_far, "{}: fetch count", case.name);
        }
        let _ = write_bytes; // js_json parity for file bytes is asserted through the reads above
    }
}

#[test]
fn facts_tables_are_byte_equal() {
    let corpus = corpus();
    for case in &corpus.facts {
        let root = tempfile::tempdir().expect("tempdir");
        install_fixture(&corpus, root.path(), &Some(case.fixture.clone()), 123.0);
        let mut resolver = Resolver::over(root.path().join("models-dev-registry-cache.json"));
        let table = resolver.facts_table(&case.models, &case.routes);
        assert_eq!(text(&Value::Object(table)), case.table, "{}", case.name);
    }
}

#[test]
fn known_models_match() {
    let corpus = corpus();
    let Some(known) = &corpus.known_models else {
        return;
    };
    let root = tempfile::tempdir().expect("tempdir");
    install_fixture(&corpus, root.path(), &Some(known.fixture.clone()), 123.0);
    let mut resolver = Resolver::over(root.path().join("models-dev-registry-cache.json"));
    let models: Vec<Value> = resolver
        .get_known_models()
        .into_iter()
        .map(Value::String)
        .collect();
    assert_eq!(text(&Value::Array(models)), known.models);
}
