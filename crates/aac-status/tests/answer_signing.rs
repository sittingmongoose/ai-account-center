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
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/spec/answer-signing.json");
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
    assert_eq!(key.public_key_pem, corpus.public_key_pem, "public PEM as Node exports it");

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

#[test]
fn new_answer_keys_use_v1_and_existing_v2_keys_keep_their_identity() {
    use ed25519_dalek::SigningKey;
    use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey, PrivateKeyInfo};
    use pkcs8::Version;
    use pem_rfc7468::LineEnding;

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("answer-key.pem");
    let created = load_or_create_answer_key(dir.path()).expect("create key");
    let pem = std::fs::read_to_string(&path).expect("read created key");
    let (_, der) = pem_rfc7468::decode_vec(pem.as_bytes()).expect("decode PEM");
    let info = PrivateKeyInfo::try_from(der.as_slice()).expect("decode PKCS#8");
    assert_eq!(info.version(), Version::V1);

    let signing = SigningKey::from_pkcs8_der(&der).expect("load private key");
    let old_pem = signing.to_pkcs8_pem(LineEnding::LF).expect("encode v2");
    std::fs::write(path, old_pem.as_bytes()).expect("write old key");
    let existing = load_or_create_answer_key(dir.path()).expect("reload existing key");
    assert_eq!(existing.key_id, created.key_id);
    assert_eq!(existing.public_key_pem, created.public_key_pem);
}
