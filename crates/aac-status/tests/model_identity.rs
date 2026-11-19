//! The shared normalizeModel case table (`tests/spec/model-identity.json`, written by
//! `tests/spec/model-identity-vectors.mjs` from the real TypeScript lookup over the
//! packaged catalog): the Rust port must resolve every case the same way.

use aac_status::model_identity::{normalize_model, AliasTable, Model};
use serde::Deserialize;
use serde_json::{Map, Value as Json};

#[derive(Deserialize)]
struct ModelRow {
    model: String,
    efforts: Vec<String>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    effort: Option<String>,
    #[serde(rename = "useTable")]
    use_table: bool,
    want: Want,
    #[serde(rename = "overrideAliases", default)]
    override_aliases: Option<Map<String, Json>>,
}

#[derive(Deserialize)]
struct Want {
    id: String,
    effort: Option<String>,
}

#[derive(Deserialize)]
struct Corpus {
    models: Vec<ModelRow>,
    aliases: Map<String, Json>,
    #[serde(rename = "effortSuffixes")]
    effort_suffixes: Vec<String>,
    cases: Vec<Case>,
}

#[test]
fn every_case_resolves_as_the_typescript_lookup_does() {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/spec/model-identity.json");
    let text = std::fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("the case table is missing ({}): {error}", file.display()));
    let corpus: Corpus = serde_json::from_str(&text).expect("case table parses");
    let models: Vec<Model<'_>> = corpus
        .models
        .iter()
        .map(|row| Model {
            model: &row.model,
            efforts: &row.efforts,
        })
        .collect();
    let table = AliasTable {
        aliases: &corpus.aliases,
        effort_suffixes: &corpus.effort_suffixes,
    };
    for case in &corpus.cases {
        let override_aliases = case.override_aliases.as_ref();
        let case_table = AliasTable {
            aliases: override_aliases.unwrap_or(&corpus.aliases),
            effort_suffixes: if override_aliases.is_some() {
                &[]
            } else {
                &corpus.effort_suffixes
            },
        };
        let got = normalize_model(
            &case.id,
            case.effort.as_deref(),
            &models,
            case.use_table.then_some(if override_aliases.is_some() {
                &case_table
            } else {
                &table
            }),
        );
        assert_eq!(
            got.id, case.want.id,
            "id {:?} (effort {:?}, table {})",
            case.id, case.effort, case.use_table
        );
        assert_eq!(
            got.effort, case.want.effort,
            "effort for {:?} (effort {:?}, table {})",
            case.id, case.effort, case.use_table
        );
    }
}
