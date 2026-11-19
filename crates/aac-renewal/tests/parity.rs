//! The checked-in TS goldens: every case the renewal parity manifest captures,
//! run against the Rust behaviour with the same fake-credential sandboxes.
//! Regenerate with `bun tests/spec/renewal-parity.ts --regenerate` and review
//! the diff; the live checker (`bun tests/spec/renewal-parity.ts`) also compares
//! TS and Rust answers byte-for-byte on identical inputs.

const MANIFEST: &str = include_str!("fixtures/renewal-parity.json");

#[tokio::test]
async fn renewal_parity_goldens() {
    let manifest: aac_renewal::parity::Manifest =
        serde_json::from_str(MANIFEST).expect("parity manifest parses");
    assert_eq!(manifest.version, 1);
    assert!(!manifest.cases.is_empty(), "manifest has cases");
    let results = aac_renewal::parity::run_goldens(&manifest).await;
    let failed: Vec<&(String, bool, String)> = results.iter().filter(|(_, ok, _)| !ok).collect();
    for (name, _, detail) in &failed {
        eprintln!("--- {name}\n{detail}");
    }
    assert!(
        failed.is_empty(),
        "{} of {} renewal parity cases differ from the TS goldens",
        failed.len(),
        results.len()
    );
}
