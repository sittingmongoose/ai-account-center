//! The shared answer-signing case table (`tests/spec/answer-signing.json`, written by
//! `bun tests/spec/answer-signing-vectors.mjs` from the real TypeScript signer): canonical
//! JSON byte for byte, the same key from the same PKCS#8 PEM, the same deterministic Ed25519
//! signatures (which proves Node-interop in both directions), and every fixed refusal word.

use aac_status::answer_signing::{
    canonical_json, load_or_create_answer_key, sign_answer, sign_document, verify_signed_document,
};
use serde::Deserialize;
use serde_json::Value as Json;

#[derive(Deserialize)]
struct CanonicalCase {
    value: Json,
    text: String,
}

#[derive(Deserialize)]
struct SignedCase {
    unsigned: Json,
    signed: Json,
}

#[derive(Deserialize)]
struct Refusal {
    name: String,
    text: String,
    #[serde(rename = "publicKeyPem")]
    public_key_pem: Option<String>,
    schema: String,
    reason: String,
}

#[derive(Deserialize)]
struct Corpus {
    #[serde(rename = "privatePem")]
    private_pem: String,
    #[serde(rename = "publicKeyPem")]
    public_key_pem: String,
    #[serde(rename = "keyId")]
    key_id: String,
    canonical: Vec<CanonicalCase>,
    signed: Vec<SignedCase>,
    summary: SignedCase,
    refusals: Vec<Refusal>,
}

#[test]
fn the_rust_signer_matches_the_typescript_signer_case_for_case() {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/spec/answer-signing.json");
    let text = std::fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("the case table is missing ({}): {error}", file.display()));
    let corpus: Corpus = serde_json::from_str(&text).expect("case table parses");

    for case in &corpus.canonical {
        assert_eq!(
            canonical_json(&case.value),
            case.text,
            "canonical JSON of {}",
            case.value
        );
    }

    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("answer-key.pem"), &corpus.private_pem).expect("key file");
    let key = load_or_create_answer_key(dir.path()).expect("key loads");
    assert_eq!(key.key_id, corpus.key_id, "key id from the SPKI DER");
    assert_eq!(
        key.public_key_pem, corpus.public_key_pem,
        "public PEM as Node exports it"
    );

    for case in &corpus.signed {
        let signed = sign_answer(&case.unsigned, &key);
        assert_eq!(signed, case.signed, "deterministic signature over the canonical bytes");
        let held = verify_signed_document(
            serde_json::to_string_pretty(&case.signed).unwrap().as_bytes(),
            &corpus.public_key_pem,
            "agent-reports/answer@1",
        );
        assert!(held.is_ok(), "verify accepts the TS-signed answer");
    }
    let signed_summary = sign_document(&corpus.summary.unsigned, &key);
    assert_eq!(signed_summary, corpus.summary.signed, "send summary signature");
    assert!(
        verify_signed_document(
            serde_json::to_string_pretty(&corpus.summary.signed).unwrap().as_bytes(),
            &corpus.public_key_pem,
            "agent-reports/send@1",
        )
        .is_ok(),
        "verify accepts the TS-signed summary"
    );

    for refusal in &corpus.refusals {
        let pem = refusal.public_key_pem.as_deref().unwrap_or(&corpus.public_key_pem);
        let held = verify_signed_document(refusal.text.as_bytes(), pem, &refusal.schema);
        assert_eq!(held.err().as_deref(), Some(refusal.reason.as_str()), "{}", refusal.name);
    }
}

/// G16 finding 1: the key file Rust writes is PKCS#8 v1 (`MC4CAQAw...`), the bytes Node
/// writes, never ed25519-dalek's v2 (`MFECAQEw...`) that Bun's BoringSSL refuses.
#[test]
fn the_rust_key_file_is_pkcs8_v1_as_node_writes_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let key = load_or_create_answer_key(dir.path()).expect("key created");
    let text = std::fs::read_to_string(dir.path().join("answer-key.pem")).expect("key file");
    assert!(
        text.starts_with("-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEI"),
        "{text}"
    );
    assert!(text.ends_with("\n-----END PRIVATE KEY-----\n"), "{text}");
    assert_eq!(
        text.len(),
        119,
        "one 64-char base64 line, as Node exports a 48-byte DER"
    );
    // Loading it again gives the same key (no re-creation).
    let again = load_or_create_answer_key(dir.path()).expect("key loads");
    assert_eq!(again.key_id, key.key_id);
}

/// A v2 key an earlier Rust build wrote still loads (same seed, same key id).
#[test]
fn a_pkcs8_v2_key_from_an_earlier_build_still_loads() {
    use ed25519_dalek::pkcs8::EncodePrivateKey;
    let seed = [7u8; 32];
    let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
    let v2 = signing.to_pkcs8_pem(pem_rfc7468::LineEnding::LF).expect("v2 pem");
    assert!(v2.contains("MFECAQEw"), "dalek writes v2: {}", v2.as_str());
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("answer-key.pem"), v2.as_bytes()).expect("key file");
    let from_v2 = load_or_create_answer_key(dir.path()).expect("v2 loads");
    let v1 = aac_status::answer_signing::private_key_pem(&signing).expect("v1 pem");
    let dir1 = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir1.path().join("answer-key.pem"), v1.as_bytes()).expect("key file");
    let from_v1 = load_or_create_answer_key(dir1.path()).expect("v1 loads");
    assert_eq!(from_v2.key_id, from_v1.key_id);
    assert_eq!(from_v2.public_key_pem, from_v1.public_key_pem);
}

/// G16 finding 1 interop: the real TypeScript loader in Bun (tests/spec/answer-key-import.ts)
/// reads the key Rust wrote, gets the same key id and public PEM, exports byte-identical
/// PKCS#8 PEM, and leaves the file in place (no "being written", no move-aside, no new key).
/// Needs `bun` on PATH; set AAC_SKIP_BUN_INTEROP=1 to skip on a host without it.
#[test]
fn bun_imports_the_key_rust_writes() {
    if std::env::var_os("AAC_SKIP_BUN_INTEROP").is_some() {
        eprintln!("skipped: AAC_SKIP_BUN_INTEROP is set");
        return;
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().expect("temp dir");
    let key = load_or_create_answer_key(dir.path()).expect("key created");
    let written = std::fs::read_to_string(dir.path().join("answer-key.pem")).expect("key file");
    let output = std::process::Command::new(if cfg!(windows) { "bun.exe" } else { "bun" })
        .arg(root.join("tests/spec/answer-key-import.ts"))
        .arg(dir.path())
        .current_dir(&root)
        .output()
        .expect("bun runs (set AAC_SKIP_BUN_INTEROP=1 on a host without bun)");
    assert!(
        output.status.success(),
        "bun refused the Rust key: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let seen: Json = serde_json::from_slice(&output.stdout).expect("script prints JSON");
    assert_eq!(seen["runtime"], "bun");
    assert_eq!(seen["keyId"], key.key_id.as_str());
    assert_eq!(seen["publicKeyPem"], key.public_key_pem.as_str());
    assert_eq!(
        seen["exportedPem"],
        written.as_str(),
        "Bun exports the same PKCS#8 bytes"
    );
    assert_eq!(
        seen["fileAfter"],
        written.as_str(),
        "TS left the key file as Rust wrote it"
    );
    assert_eq!(
        seen["entries"],
        serde_json::json!(["answer-key.pem"]),
        "nothing moved aside"
    );
}
