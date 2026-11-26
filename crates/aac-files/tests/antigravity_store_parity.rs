//! RED/GREEN parity tests for `aac_files::antigravity_store` against the 4 file/platform
//! Antigravity Python workers (`auth_platforms.py`, `native_credential_worker.py`,
//! `desktop_usage.py:antigravity_oauth_client`, `runtime_health.py`) plus `masked_credential_state`.
#![allow(clippy::unwrap_used)]

use aac_files::antigravity_store::{
    AuthErrorKind, MaskedCredentialState, PrivateCredential, ProviderCallError, SecureFileStore,
    UbuntuBackendProbe, UbuntuFileStore, execute_native_bytes, execute_native_packet,
    masked_credential_state, parse_credential, parse_strict_json_object, probe_ubuntu_backend_at,
    read_antigravity_oauth_client, read_masked_credential, refresh_in_memory, runtime_health,
    sha256_hex, verify_identity,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn sample_cred(access: &str, refresh: &str, expiry: &str) -> Vec<u8> {
    json!({
        "auth_method": "consumer",
        "token": {
            "access_token": access,
            "refresh_token": refresh,
            "token_type": "Bearer",
            "expiry": expiry,
        }
    })
    .to_string()
    .into_bytes()
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
fn setup_fallback_home(raw: &[u8]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    set_mode(dir.path(), 0o700);
    let gemini = dir.path().join(".gemini");
    let cli = gemini.join("antigravity-cli");
    fs::create_dir_all(&cli).unwrap();
    set_mode(&gemini, 0o700);
    set_mode(&cli, 0o700);
    let token = cli.join("antigravity-oauth-token");
    fs::write(&token, raw).unwrap();
    set_mode(&token, 0o600);
    dir
}

#[test]
fn strict_json_rejects_duplicate_keys_and_non_objects() {
    assert!(parse_strict_json_object(br#"{"a":1,"a":2}"#).is_err());
    assert!(parse_strict_json_object(br#"{"nested":{"x":1,"x":2}}"#).is_err());
    assert!(parse_strict_json_object(br#"[1,2,3]"#).is_err());
    assert!(parse_strict_json_object(br#"{"a":1e999}"#).is_err());
    let ok = parse_strict_json_object(br#"{"a":1,"b":{"c":"ok"}}"#).unwrap();
    assert_eq!(ok["a"], json!(1));
}

#[test]
fn parse_credential_validates_schema_and_redacts_debug() {
    let raw = sample_cred(
        "tok-access-secret",
        "tok-refresh-secret",
        "2026-11-26T22:00:00Z",
    );
    let parsed = parse_credential(&raw).unwrap();
    assert_eq!(parsed.access_token, "tok-access-secret");
    assert_eq!(parsed.refresh_token, "tok-refresh-secret");
    let cred = PrivateCredential::new(raw.clone()).unwrap();
    assert_eq!(cred.revision(), sha256_hex(&raw));
    let dbg = format!("{cred:?} {parsed:?}");
    assert!(!dbg.contains("tok-access-secret"));
    assert!(!dbg.contains("tok-refresh-secret"));

    // Reject unknown field, bad token_type, control char in token, naive expiry
    assert!(
        parse_credential(
            json!({"auth_method":"consumer","extra":1,"token":{"access_token":"a","refresh_token":"r","token_type":"Bearer","expiry":"2026-11-26T22:00:00Z"}})
                .to_string()
                .as_bytes()
        )
        .is_err()
    );
    assert!(
        parse_credential(
            json!({"auth_method":"consumer","token":{"access_token":"a","refresh_token":"r","token_type":"MAC","expiry":"2026-11-26T22:00:00Z"}})
                .to_string()
                .as_bytes()
        )
        .is_err()
    );
    assert!(parse_credential(&sample_cred("bad token", "r", "2026-11-26T22:00:00Z")).is_err());
    assert!(parse_credential(&sample_cred("a", "r", "2026-11-26T22:00:00")).is_err());
}

#[test]
fn verify_identity_and_refresh_in_memory_match_python_semantics() {
    let raw = sample_cred("acc-1", "ref-1", "2026-11-26T22:00:00Z");
    let cred = PrivateCredential::new(raw).unwrap();
    let now_ms = 1_795_728_000_000i64; // 2026-11-26T21:20:00.000Z
    let id = verify_identity(
        &cred,
        |tok| {
            assert_eq!(tok, "acc-1");
            Ok(json!({"email":"User@Example.COM","id":"1234567890","verified_email":true}))
        },
        now_ms,
    )
    .unwrap();
    assert_eq!(id.email, "user@example.com");
    assert_eq!(id.subject, "1234567890");
    assert_eq!(id.source, "provider-userinfo");
    assert_eq!(id.authority, "Google OAuth2 userinfo");
    assert!(!format!("{id:?}").contains("1234567890"));
    assert_eq!(
        id.identity_key(),
        sha256_hex(br#"["antigravity","user@example.com","1234567890"]"#)
    );

    // Unverified email -> IdentityMismatch
    let err = verify_identity(
        &cred,
        |_| Ok(json!({"email":"user@example.com","id":"123","verified_email":false})),
        now_ms,
    )
    .unwrap_err();
    assert_eq!(err.kind, AuthErrorKind::IdentityMismatch);

    // 401/403 -> NeedsSignIn
    let err = verify_identity(&cred, |_| Err(ProviderCallError::NeedsSignIn), now_ms).unwrap_err();
    assert_eq!(err.kind, AuthErrorKind::NeedsSignIn);

    // Refresh in memory keeps refresh_token unless rotated and updates expiry
    let renewed = refresh_in_memory(
        &cred,
        |rt| {
            assert_eq!(rt, "ref-1");
            Ok(json!({"access_token": "acc-2", "expires_in": 3600}))
        },
        now_ms,
    )
    .unwrap();
    let parsed = parse_credential(renewed.raw()).unwrap();
    assert_eq!(parsed.access_token, "acc-2");
    assert_eq!(parsed.refresh_token, "ref-1");
    assert!(parsed.expiry.ends_with('Z'));
}

#[cfg(target_os = "linux")]
#[test]
fn secure_file_store_install_rollback_and_race_preservation() {
    let raw_a = sample_cred("acc-a", "ref-a", "2026-11-26T22:00:00Z");
    let raw_b = sample_cred("acc-b", "ref-b", "2026-11-26T23:00:00Z");
    let raw_c = sample_cred("acc-c", "ref-c", "2026-11-26T23:30:00Z");
    let dir = setup_fallback_home(&raw_a);
    let store = SecureFileStore::new(dir.path());

    let before = store.read_current().unwrap();
    assert_eq!(before.credential.raw(), raw_a.as_slice());

    let next = PrivateCredential::new(raw_b.clone()).unwrap();
    let receipt = store.install(&next, &before).unwrap();
    assert_eq!(
        store.read_current().unwrap().credential.raw(),
        raw_b.as_slice()
    );

    // Rollback restores A
    store.rollback(&receipt).unwrap();
    assert_eq!(
        store.read_current().unwrap().credential.raw(),
        raw_a.as_slice()
    );

    // Foreign replacement mid-install is preserved, never deleted
    let snap_a = store.read_current().unwrap();
    let target_file = dir
        .path()
        .join(".gemini/antigravity-cli/antigravity-oauth-token");
    let err = store
        .install_with_hook(&next, &snap_a, |_parent| {
            let tmp = target_file.with_extension("foreign");
            fs::write(&tmp, &raw_c).unwrap();
            set_mode(&tmp, 0o600);
            fs::rename(&tmp, &target_file).unwrap();
        })
        .unwrap_err();
    assert_eq!(err.kind, AuthErrorKind::Auth);
    assert_eq!(
        store.read_current().unwrap().credential.raw(),
        raw_c.as_slice()
    );

    // Hard-linked or loose mode file is refused
    set_mode(&target_file, 0o644);
    assert!(store.read_current().is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn ubuntu_file_store_rolls_back_when_secret_service_appears_mid_install() {
    let raw_a = sample_cred("acc-a", "ref-a", "2026-11-26T22:00:00Z");
    let raw_b = sample_cred("acc-b", "ref-b", "2026-11-26T23:00:00Z");
    let dir = setup_fallback_home(&raw_a);

    let step = AtomicUsize::new(0);
    let store = UbuntuFileStore::with_probe(dir.path(), || {
        // read_current calls probe twice (0, 1); install calls probe before (2) and after (3).
        let n = step.fetch_add(1, Ordering::SeqCst);
        if n < 3 {
            UbuntuBackendProbe {
                store: "fallback-file",
                service_running: false,
                unlocked_matches: 0,
                locked_matches: 0,
            }
        } else {
            UbuntuBackendProbe {
                store: "secret-service",
                service_running: true,
                unlocked_matches: 1,
                locked_matches: 0,
            }
        }
    });
    let before = store.read_current().unwrap();
    let next = PrivateCredential::new(raw_b).unwrap();
    let err = store.install(&next, &before).unwrap_err();
    assert!(err.message.contains("rolled back"), "got: {}", err.message);
    assert_eq!(
        SecureFileStore::new(dir.path())
            .read_current()
            .unwrap()
            .credential
            .raw(),
        raw_a.as_slice()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn native_credential_worker_protocol_round_trip() {
    let raw_a = sample_cred("acc-a", "ref-a", "2026-11-26T22:00:00Z");
    let raw_b = sample_cred("acc-b", "ref-b", "2026-11-26T23:00:00Z");
    let dir = setup_fallback_home(&raw_a);
    let ok_probe = || UbuntuBackendProbe {
        store: "fallback-file",
        service_running: false,
        unlocked_matches: 0,
        locked_matches: 0,
    };
    let store = UbuntuFileStore::with_probe(dir.path(), ok_probe);
    let now_ms = 1_795_728_000_000i64;

    // read
    let read = execute_native_packet(
        &json!({"operation": "read"}),
        &store,
        |_| unreachable!(),
        now_ms,
    )
    .unwrap();
    assert_eq!(read["format"], "antigravity-consumer-json");
    assert_eq!(read["revision"], sha256_hex(&raw_a));
    assert_eq!(
        STANDARD
            .decode(read["credentialBase64"].as_str().unwrap())
            .unwrap(),
        raw_a
    );

    // bind-current
    let bound = execute_native_packet(
        &json!({"operation": "bind-current"}),
        &store,
        |_| Ok(json!({"email":"Alice@Example.com","id":"998877","verified_email":true})),
        now_ms,
    )
    .unwrap();
    assert_eq!(bound["email"], "alice@example.com");
    assert_eq!(bound["subject"], "998877");
    assert_eq!(bound["credentialCurrent"], true);
    assert_eq!(bound["nativeSnapshotIdentity"].as_array().unwrap().len(), 4);

    // install + rollback via byte packet interface (including duplicate key rejection)
    assert!(
        execute_native_bytes(
            br#"{"operation":"read","operation":"read"}"#,
            &store,
            |_| unreachable!(),
            now_ms
        )
        .is_err()
    );

    let install_req = json!({
        "operation": "install",
        "credentialBase64": STANDARD.encode(&raw_b),
        "expectedFingerprint": sha256_hex(&raw_a),
    });
    let installed: serde_json::Value = serde_json::from_slice(
        &execute_native_bytes(
            &serde_json::to_vec(&install_req).unwrap(),
            &store,
            |_| unreachable!(),
            now_ms,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(installed["installedFingerprint"], sha256_hex(&raw_b));
    assert!(installed["rollbackState"]["before"]["modifiedNs"].is_string());

    let rb_req = json!({
        "operation": "rollback",
        "receipt": installed["rollbackState"],
        "previousCredentialBase64": STANDARD.encode(&raw_a),
    });
    let restored = execute_native_packet(&rb_req, &store, |_| unreachable!(), now_ms).unwrap();
    assert_eq!(restored["restored"], true);
}

#[cfg(unix)]
#[test]
fn oauth_client_metadata_enforces_strict_int_schema_and_mode() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join(".ccs/account-usage");
    fs::create_dir_all(&folder).unwrap();
    set_mode(&folder, 0o700);
    let file = folder.join("antigravity-oauth-client.json");

    // Valid schemaVersion: 1
    let valid = r#"{"schemaVersion":1,"clientId":"12345-abc.apps.googleusercontent.com","clientSecret":"GOCSPX-abc_123"}"#;
    fs::write(&file, valid).unwrap();
    set_mode(&file, 0o600);
    let client = read_antigravity_oauth_client(dir.path()).unwrap();
    assert_eq!(client.client_id, "12345-abc.apps.googleusercontent.com");
    assert_eq!(client.client_secret, "GOCSPX-abc_123");
    assert!(!format!("{client:?}").contains("GOCSPX-abc_123"));

    // Float schemaVersion: 1.0 must be rejected (production_oauth_metadata_test.py)
    fs::write(
        &file,
        r#"{"schemaVersion":1.0,"clientId":"12345-abc.apps.googleusercontent.com","clientSecret":"GOCSPX-abc_123"}"#,
    )
    .unwrap();
    set_mode(&file, 0o600);
    assert!(read_antigravity_oauth_client(dir.path()).is_err());

    // Duplicate key rejected
    fs::write(
        &file,
        r#"{"schemaVersion":1,"schemaVersion":1,"clientId":"12345-abc.apps.googleusercontent.com","clientSecret":"GOCSPX-abc_123"}"#,
    )
    .unwrap();
    set_mode(&file, 0o600);
    assert!(read_antigravity_oauth_client(dir.path()).is_err());

    // Loose permissions rejected
    fs::write(&file, valid).unwrap();
    set_mode(&file, 0o644);
    assert!(read_antigravity_oauth_client(dir.path()).is_err());
}

#[cfg(unix)]
#[test]
fn masked_credential_state_and_read_match_driver_and_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("antigravity-oauth-token");
    assert_eq!(
        masked_credential_state(&path),
        MaskedCredentialState::Absent
    );

    fs::write(&path, b"{\"auth_method\":\"consumer\"").unwrap();
    set_mode(&path, 0o600);
    assert_eq!(
        masked_credential_state(&path),
        MaskedCredentialState::Partial
    );

    let raw = sample_cred("acc", "ref", "2026-11-26T22:00:00Z");
    fs::write(&path, &raw).unwrap();
    set_mode(&path, 0o600);
    assert_eq!(
        masked_credential_state(&path),
        MaskedCredentialState::Complete
    );
    assert_eq!(read_masked_credential(&path).unwrap(), raw);

    // 0644 -> Unsafe (and read_masked_credential refuses)
    set_mode(&path, 0o644);
    assert_eq!(
        masked_credential_state(&path),
        MaskedCredentialState::Unsafe
    );
    assert!(read_masked_credential(&path).is_err());
}

#[test]
fn runtime_health_parses_pyvenv_normalizes_and_describes() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("venv")).unwrap();
    fs::write(
        dir.path().join("venv/pyvenv.cfg"),
        "home = /usr/bin\nversion_info = 3.13.5\n",
    )
    .unwrap();
    assert_eq!(
        runtime_health::built_for(dir.path()).as_deref(),
        Some("3.13")
    );
    assert_eq!(
        runtime_health::parser_directory(dir.path(), Some((3, 14))),
        dir.path().join("venv/lib/python3.14/site-packages")
    );
    fs::create_dir_all(dir.path().join("parser")).unwrap();
    assert_eq!(
        runtime_health::parser_directory(dir.path(), Some((3, 14))),
        dir.path().join("parser")
    );

    let norm = runtime_health::normalize(Some(&json!({
        "ok": false,
        "reason": "missing-python-module",
        "module": "pyte",
        "python": "3.14",
        "builtFor": "3.13",
    })));
    assert_eq!(
        runtime_health::describe(&norm),
        "missing Python module pyte (system Python is 3.14; the bundle was built for 3.13)"
    );
    assert_eq!(
        runtime_health::describe(&runtime_health::normalize(None)),
        "the parser check could not run"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn probe_ubuntu_backend_speaks_dbus_wire_protocol_over_unix_socket() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("dbus.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    listener.set_nonblocking(true).unwrap();

    // Minimal D-Bus server answering SASL EXTERNAL + Hello + NameHasOwner=false.
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return,
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut buf = [0u8; 4096];
        let mut seen = Vec::new();
        // Wait for AUTH EXTERNAL ... \r\n
        while !seen.windows(2).any(|w| w == b"\r\n") {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                return;
            }
            seen.extend_from_slice(&buf[..n]);
        }
        stream
            .write_all(b"OK 0123456789abcdef0123456789abcdef\r\n")
            .unwrap();
        // Wait for BEGIN\r\n
        seen.clear();
        while !seen.windows(2).any(|w| w == b"\r\n") {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                return;
            }
            seen.extend_from_slice(&buf[..n]);
        }
        // Read two D-Bus messages (Hello serial=1, NameHasOwner serial=2) and reply.
        let mut leftover = seen.split_off(seen.windows(2).position(|w| w == b"\r\n").unwrap() + 2);
        let read_msg = |stream: &mut std::os::unix::net::UnixStream, buf: &mut Vec<u8>| -> u32 {
            while buf.len() < 16 {
                let mut tmp = [0u8; 512];
                let n = stream.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            let body_len = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
            let serial = u32::from_le_bytes(buf[8..12].try_into().unwrap());
            let fields_len = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
            let header_padded = (16 + fields_len + 7) & !7;
            let total = header_padded + body_len;
            while buf.len() < total {
                let mut tmp = [0u8; 512];
                let n = stream.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            buf.drain(..total);
            serial
        };
        let build_reply = |reply_serial: u32, sig: &str, body: &[u8]| -> Vec<u8> {
            // REPLY_SERIAL (code 5), sig "u"
            let mut fields = vec![5u8, 1, b'u', 0];
            fields.extend_from_slice(&reply_serial.to_le_bytes());
            // SIGNATURE (code 8), sig "g"
            while fields.len() % 8 != 0 {
                fields.push(0);
            }
            fields.push(8u8);
            fields.push(1);
            fields.push(b'g');
            fields.push(0);
            fields.push(sig.len() as u8);
            fields.extend_from_slice(sig.as_bytes());
            fields.push(0);
            let mut msg = vec![b'l', 2, 0, 1];
            msg.extend_from_slice(&(body.len() as u32).to_le_bytes());
            msg.extend_from_slice(&(reply_serial + 100).to_le_bytes());
            msg.extend_from_slice(&(fields.len() as u32).to_le_bytes());
            msg.extend_from_slice(&fields);
            while msg.len() % 8 != 0 {
                msg.push(0);
            }
            msg.extend_from_slice(body);
            msg
        };
        let s1 = read_msg(&mut stream, &mut leftover);
        let mut hello_body = Vec::new();
        hello_body.extend_from_slice(&4u32.to_le_bytes());
        hello_body.extend_from_slice(b":1.1\0");
        stream
            .write_all(&build_reply(s1, "s", &hello_body))
            .unwrap();

        let s2 = read_msg(&mut stream, &mut leftover);
        let has_owner_body = 0u32.to_le_bytes(); // false
        stream
            .write_all(&build_reply(s2, "b", &has_owner_body))
            .unwrap();
    });

    let addr = format!("unix:path={}", sock.display());
    let probe = probe_ubuntu_backend_at(
        Some(&addr),
        None,
        aac_files::path::uid(),
        Duration::from_secs(2),
    );
    assert_eq!(
        probe,
        UbuntuBackendProbe {
            store: "fallback-file",
            service_running: false,
            unlocked_matches: 0,
            locked_matches: 0,
        }
    );
    server.join().unwrap();
}
