//! Signed answers (`answer-signing.ts`), ported for Node crypto interop: canonical JSON in
//! the RFC 8785 style (object keys sorted by UTF-16 code unit at every depth, strings and
//! numbers as `JSON.stringify` writes them), Ed25519 over its UTF-8 bytes, PKCS#8 private /
//! SPKI public PEM key files, and the key file's create/retry/repair lifecycle. The signed
//! files are the authority; a reporter mirrors `verify_signed_answer` with the published key.

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use ed25519_dalek::pkcs8::{
    DecodePrivateKey, DecodePublicKey, EncodePublicKey,
};
use pem_rfc7468::LineEnding;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const ANSWER_SCHEMA: &str = "agent-reports/answer@1";

/// The signing key: the private key, its published SPKI PEM, and the key id.
#[derive(Clone)]
pub struct AnswerKey {
    signing: SigningKey,
    pub public_key_pem: String,
    pub key_id: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `JSON.stringify` of a string: the escapes Node writes, no others.
fn json_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `JSON.stringify` of a number: integers up to 2^53 as digits, else `Number#toString`.
fn json_number(number: &serde_json::Number, out: &mut String) {
    const EXACT: u64 = 1 << 53;
    let x = match (number.as_u64(), number.as_i64()) {
        (Some(u), _) if u <= EXACT => {
            out.push_str(&u.to_string());
            return;
        }
        (_, Some(i)) if i.unsigned_abs() <= EXACT => {
            out.push_str(&i.to_string());
            return;
        }
        (Some(u), _) => u as f64,
        (_, Some(i)) => i as f64,
        _ => number.as_f64().unwrap_or(f64::NAN),
    };
    if x.is_finite() {
        out.push_str(&aac_num::ecma::to_string(x));
    } else {
        out.push_str("null");
    }
}

/// Canonical JSON (`canonicalJson`): keys sorted by UTF-16 code unit at every depth, no
/// whitespace, arrays in order. A parsed JSON value always has a canonical text (JavaScript's
/// `undefined`, the one value it cannot hold, never reaches this side).
pub fn canonical_json(value: &Json) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Json, out: &mut String) {
    match value {
        Json::Object(map) => {
            // Every own key counts (JSON has no undefined); sort by UTF-16 code unit.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                json_string(key, out);
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Json::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Json::String(text) => json_string(text, out),
        Json::Number(number) => json_number(number, out),
        Json::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Json::Null => out.push_str("null"),
    }
}

/// First 16 hex characters of the SHA-256 of the public key's DER (SPKI) bytes.
pub fn answer_key_id(spki_der: &[u8]) -> String {
    let digest = Sha256::digest(spki_der);
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// A key file another process is still writing (or a crash left half of) is read again this many times.
const KEY_READ_RETRIES: u32 = 5;
const KEY_READ_PAUSE_MS: u64 = 20;
/// A key file still empty or unreadable this long after its last write is broken: replaced.
const BROKEN_KEY_AGE_MS: u64 = 60 * 1000;

fn read_key(file: &Path) -> Result<Option<SigningKey>, String> {
    let pem = match fs::read_to_string(file) {
        Ok(pem) => pem,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let der = match pem_rfc7468::decode_vec(pem.as_bytes()) {
        Ok((label, der)) if label == "PRIVATE KEY" => der,
        _ => return Ok(None),
    };
    // A well-formed key of another algorithm is an error, not a broken file (Node's check).
    let info = match ed25519_dalek::pkcs8::PrivateKeyInfo::try_from(der.as_slice()) {
        Ok(info) => info,
        Err(_) => return Ok(None),
    };
    if info.algorithm.oid != ed25519_dalek::pkcs8::ALGORITHM_OID {
        return Err("The answer key is not an Ed25519 key.".to_string());
    }
    match SigningKey::from_pkcs8_der(&der) {
        Ok(key) => Ok(Some(key)),
        Err(_) => Ok(None),
    }
}

fn stat_if_there(file: &Path) -> Option<fs::Metadata> {
    match fs::metadata(file) {
        Ok(meta) => Some(meta),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => None,
    }
}

fn random_hex() -> String {
    use rand_core::RngCore;
    let mut bytes = [0u8; 6];
    rand_core::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A new private file, exclusively created (0600 where the platform has modes).
fn create_private(path: &Path) -> std::io::Result<fs::File> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// The PKCS#8 DER prefix of an Ed25519 private key, version 1 (RFC 8410 section 7): the
/// seed only, no public key. Node writes exactly this; ed25519-dalek's own encoder writes
/// version 2 (`MFECAQEw...`, the public key appended), which Bun's BoringSSL refuses with
/// SEQUENCE_LENGTH_MISMATCH, and TS then treats the key file as broken and replaces it.
const PKCS8_V1_ED25519_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// The private key file's text: PKCS#8 v1 PEM, byte for byte what Node's
/// `privateKey.export({ type: 'pkcs8', format: 'pem' })` writes (`MC4CAQAw...`). Reading
/// still accepts v2, so a key an earlier build wrote keeps loading.
pub fn private_key_pem(signing: &SigningKey) -> Result<String, String> {
    let mut der = Vec::with_capacity(48);
    der.extend_from_slice(&PKCS8_V1_ED25519_PREFIX);
    der.extend_from_slice(signing.as_bytes());
    pem_rfc7468::encode_string("PRIVATE KEY", LineEnding::LF, &der).map_err(|e| e.to_string())
}

/// Creates the key file without ever showing another process half a key: whole to a private
/// temp file, synced, then linked into place; where hard links fail, created in place exclusively.
fn create_key_file(dir: &Path, file: &Path) -> Result<(), String> {
    let signing = SigningKey::generate(&mut rand_core::OsRng);
    let created = private_key_pem(&signing)?;
    let temp = dir.join(format!(
        ".answer-key.{}.{}.tmp",
        std::process::id(),
        random_hex()
    ));
    let result = (|| -> Result<(), String> {
        // Written and synced through the handle that created it: a read-only handle cannot
        // be synced on Windows (FlushFileBuffers needs write access).
        let mut handle = create_private(&temp).map_err(|e| e.to_string())?;
        handle.write_all(created.as_bytes()).map_err(|e| e.to_string())?;
        handle.sync_all().map_err(|e| e.to_string())?;
        drop(handle);
        match fs::hard_link(&temp, file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                match create_private(file) {
                    Ok(mut handle) => {
                        handle.write_all(created.as_bytes()).map_err(|e| e.to_string())?;
                        handle.sync_all().map_err(|e| e.to_string())?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
        }
        // The new name is durable only once its folder is.
        if let Ok(folder) = fs::File::open(dir) {
            let _ = folder.sync_all();
        }
        Ok(())
    })();
    let _ = fs::remove_file(&temp);
    result
}

/// `<dir>/answer-key.pem` (PKCS#8, 0600), created on first use; a racing process uses the key
/// the first one wrote. The answer store's writer lock is held before this runs.
pub fn load_or_create_answer_key(dir: &Path) -> Result<AnswerKey, String> {
    let file = dir.join("answer-key.pem");
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    // A crash between writing and cleaning up leaves a private key in a temp file: remove any an hour old.
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(".answer-key.") || !name.ends_with(".tmp") {
                continue;
            }
            let stale = entry.path();
            if let Ok(meta) = fs::symlink_metadata(&stale) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                if now_ms().saturating_sub(mtime) > 60 * 60 * 1000 {
                    let _ = fs::remove_file(&stale);
                }
            }
        }
    }
    let mut attempt: u32 = 0;
    let signing = loop {
        if attempt > KEY_READ_RETRIES + 4 {
            return Err("The answer key file cannot be created or read.".to_string());
        }
        match read_key(&file)? {
            Some(key) => break key,
            None => {
                // No file: create it. A broken file (read_key Ok(None) with a file present)
                // is retried, then moved aside once it is old enough.
                if !file.exists() {
                    create_key_file(dir, &file)?;
                    attempt += 1;
                    continue;
                }
                if attempt < KEY_READ_RETRIES {
                    std::thread::sleep(std::time::Duration::from_millis(KEY_READ_PAUSE_MS));
                    attempt += 1;
                    continue;
                }
                let Some(broken) = stat_if_there(&file) else {
                    attempt += 1;
                    continue;
                };
                let mtime = broken
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                if now_ms().saturating_sub(mtime) < BROKEN_KEY_AGE_MS {
                    return Err("The answer key is being written. Try again in a minute.".to_string());
                }
                // Moved aside only while it is still the same broken file.
                let same = stat_if_there(&file)
                    .map(|_meta| {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::MetadataExt;
                            _meta.ino() == broken.ino()
                        }
                        #[cfg(not(unix))]
                        true
                    })
                    .unwrap_or(false);
                if same {
                    let aside = dir.join(format!(
                        ".answer-key.broken.{}.{}.tmp",
                        std::process::id(),
                        random_hex()
                    ));
                    match fs::rename(&file, &aside) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.to_string()),
                    }
                }
                attempt += 1;
            }
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&file, fs::Permissions::from_mode(0o600));
    }
    let verifying = signing.verifying_key();
    let public_key_pem = verifying
        .to_public_key_pem(LineEnding::LF)
        .map_err(|e| e.to_string())?;
    let spki = verifying
        .to_public_key_der()
        .map_err(|e| e.to_string())?;
    Ok(AnswerKey {
        key_id: answer_key_id(spki.as_bytes()),
        signing,
        public_key_pem,
    })
}

fn signature_json(key: &AnswerKey, message: &[u8]) -> Json {
    let value = base64::engine::general_purpose::STANDARD.encode(key.signing.sign(message).to_bytes());
    serde_json::json!({"alg": "ed25519", "keyId": key.key_id, "value": value})
}

/// Signs an answer: the canonical JSON of the unsigned answer, Ed25519, base64.
pub fn sign_answer(unsigned: &Json, key: &AnswerKey) -> Json {
    let mut signed = unsigned.clone();
    if let Some(object) = signed.as_object_mut() {
        object.insert(
            "signature".to_string(),
            signature_json(key, canonical_json(unsigned).as_bytes()),
        );
    }
    signed
}

/// Signs any AAC document the same way (the send summary).
pub fn sign_document(doc: &Json, key: &AnswerKey) -> Json {
    sign_answer(doc, key)
}

/// The same check for any signed AAC document whose `schema` must equal `schema`
/// (`verifySignedDocument`): the fixed refusal words, the key id, the canonical signature.
pub fn verify_signed_document(file_text: &[u8], public_key_pem: &str, schema: &str) -> Result<Json, String> {
    let text = String::from_utf8_lossy(file_text);
    let doc: Json = serde_json::from_str(&text).map_err(|_| "not valid JSON".to_string())?;
    let Some(record) = doc.as_object() else {
        return Err("not an answer".to_string());
    };
    if record.get("schema").and_then(Json::as_str) != Some(schema) {
        return Err("not an answer".to_string());
    }
    let signature = record.get("signature").and_then(Json::as_object);
    let (value, key_id) = match signature {
        Some(sig)
            if sig.get("alg").and_then(Json::as_str) == Some("ed25519")
                && sig.get("value").and_then(Json::as_str).is_some()
                && sig.get("keyId").and_then(Json::as_str).is_some() =>
        {
            (
                sig["value"].as_str().unwrap_or_default(),
                sig["keyId"].as_str().unwrap_or_default(),
            )
        }
        _ => return Err("unsigned".to_string()),
    };
    let der = pem_rfc7468::decode_vec(public_key_pem.as_bytes())
        .map_err(|_| "the public key is not readable".to_string())?;
    if der.0 != "PUBLIC KEY" {
        return Err("the public key is not readable".to_string());
    }
    let verifying = VerifyingKey::from_public_key_der(&der.1)
        .map_err(|_| "the public key is not an Ed25519 key".to_string())?;
    if key_id != answer_key_id(&der.1) {
        return Err("signed by another key".to_string());
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| "bad signature".to_string())?;
    if raw.len() != 64
        || base64::engine::general_purpose::STANDARD.encode(&raw) != value
    {
        return Err("bad signature".to_string());
    }
    let mut unsigned = record.clone();
    unsigned.shift_remove("signature");
    let message = canonical_json(&Json::Object(unsigned));
    let signature = ed25519_dalek::Signature::from_slice(&raw).map_err(|_| "bad signature".to_string())?;
    verifying
        .verify(message.as_bytes(), &signature)
        .map_err(|_| "bad signature".to_string())?;
    Ok(doc)
}

/// Checks one answer file against the published public key (`verifySignedAnswer`).
pub fn verify_signed_answer(file_text: &[u8], public_key_pem: &str) -> Result<Json, String> {
    verify_signed_document(file_text, public_key_pem, ANSWER_SCHEMA)
}


