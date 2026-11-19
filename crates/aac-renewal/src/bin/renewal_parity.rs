//! The live parity runner: reads a case manifest (no expected answers), runs
//! every case in a temporary sandbox of fake credentials, and writes the
//! normalized answers as one JSON object keyed by case name. The TS checker
//! (`tests/spec/renewal-parity.ts`) compares them byte-for-byte with its own.

use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: renewal_parity <manifest.json> <answers-out.json>");
        std::process::exit(2);
    }
    let manifest_path = PathBuf::from(&args[1]);
    let out_path = PathBuf::from(&args[2]);
    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(_) => {
            eprintln!("renewal_parity: cannot read manifest");
            std::process::exit(2);
        }
    };
    let manifest: aac_renewal::parity::Manifest = match serde_json::from_str(&text) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("renewal_parity: invalid manifest: {error}");
            std::process::exit(2);
        }
    };
    let mut answers = serde_json::Map::new();
    for golden in &manifest.cases {
        let answer = aac_renewal::parity::run_case(&golden.case).await;
        answers.insert(golden.case.name.clone(), answer);
    }
    let body =
        serde_json::to_string_pretty(&serde_json::Value::Object(answers)).unwrap_or_default();
    if std::fs::write(&out_path, format!("{body}\n")).is_err() {
        eprintln!("renewal_parity: cannot write answers");
        std::process::exit(2);
    }
}
