//! The one secret filter is Rust (`secret_policy` + `typed_secrets`): its verdicts are byte-equal
//! to the last TypeScript implementation's on every case of `tests/spec/secret-filter.json`
//! (recorded by `tests/spec/secret-filter-vectors.ts` before the move): the shared refusal
//! fixture in both source forms, and every typed-secret text the Status review suite pins.

use aac_status::secret_policy::{find_secret, find_secret_in};
use aac_status::typed_secrets::find_typed_secret;
use serde::Deserialize;
use serde_json::Value as Json;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Typed {
    text: String,
    typed: Option<String>,
    secret: Option<String>,
    secret_in: Option<String>,
}
#[derive(Deserialize)]
struct Repeat {
    filler: String,
    times: usize,
    suffix: String,
    typed: Option<String>,
    secret: Option<String>,
}
#[derive(Deserialize)]
struct Document {
    name: String,
    doc: Json,
    empty: Option<String>,
    stringified: Option<String>,
    text: Option<String>,
}
#[derive(Deserialize)]
struct Corpus {
    typed: Vec<Typed>,
    repeats: Vec<Repeat>,
    documents: Vec<Document>,
}

fn corpus() -> Corpus {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/spec/secret-filter.json");
    serde_json::from_str(&std::fs::read_to_string(file).expect("corpus")).expect("corpus parses")
}

#[test]
fn typed_secret_verdicts_equal_the_typescript_filter() {
    let corpus = corpus();
    let mut wrong = Vec::new();
    for case in &corpus.typed {
        let got = (
            find_typed_secret(&case.text),
            find_secret(&case.text),
            find_secret_in(&case.text, &Json::String(case.text.clone())),
        );
        let want = (case.typed.as_deref(), case.secret.as_deref(), case.secret_in.as_deref());
        if got != want {
            wrong.push(format!("{:?}: rust {got:?} ts {want:?}", case.text));
        }
    }
    for case in &corpus.repeats {
        let text = case.filler.repeat(case.times) + &case.suffix;
        let started = std::time::Instant::now();
        let got = (find_typed_secret(&text), find_secret(&text));
        assert!(
            started.elapsed().as_secs() < 2,
            "{:?} x{}: not linear",
            case.filler,
            case.times
        );
        if got != (case.typed.as_deref(), case.secret.as_deref()) {
            wrong.push(format!(
                "{:?} x{}{}: rust {got:?}",
                case.filler, case.times, case.suffix
            ));
        }
    }
    assert!(wrong.is_empty(), "{} differ:\n{}", wrong.len(), wrong.join("\n"));
    assert!(corpus.typed.len() > 100 && corpus.repeats.len() > 20);
}

#[test]
fn document_verdicts_equal_the_typescript_filter() {
    let corpus = corpus();
    let mut wrong = Vec::new();
    for case in &corpus.documents {
        // JSON.stringify of the parsed fixture: serde writes the same text for these documents.
        let source = serde_json::to_string(&case.doc).expect("stringify");
        let got = (
            find_secret_in("", &case.doc),
            find_secret_in(&source, &case.doc),
            find_secret(&source),
        );
        let want = (case.empty.as_deref(), case.stringified.as_deref(), case.text.as_deref());
        if got != want {
            wrong.push(format!("{}: rust {got:?} ts {want:?}", case.name));
        }
    }
    assert!(wrong.is_empty(), "{} differ:\n{}", wrong.len(), wrong.join("\n"));
    assert!(corpus.documents.len() > 100);
}
