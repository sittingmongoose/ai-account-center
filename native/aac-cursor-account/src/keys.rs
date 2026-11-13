//! AAC's own Cursor API key, a `credential` in AAC's existing key store (DESIGN 17.3;
//! CONTRACT-registry-lifecycle 3.2, the format `account-key-store.ts` and `key_store.py` use):
//! `<ccs>/account-usage/keys/cursor-<keyId>.json`, 0600 in a 0700 folder, holding
//! `{version: 1, provider, keyId, secret, fingerprint, last4, createdAt}`; on Windows
//! `cursor-<keyId>.dpapi`, CryptProtectData (CurrentUser, entropy `AAC/account-key/v1`) over the
//! UTF-8 bytes of the same record. One Cursor key at a time. Nothing here prints the secret.

use crate::util;
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const PROVIDER: &str = "cursor";
pub const ENTROPY: &[u8] = b"AAC/account-key/v1";
const MAX_FILE_BYTES: u64 = 4096;

#[derive(Debug, PartialEq, Eq)]
pub enum KeyError {
    /// Not 8 to 512 printable ASCII characters without spaces.
    Invalid,
    /// The folder or file could not be written or read safely (a link, wrong owner, loose mode).
    Unsafe,
}

/// A stored key. `secret` stays in memory; `Debug` never shows it.
pub struct Stored {
    pub key_id: String,
    pub secret: String,
    pub fingerprint: String,
    pub last4: String,
}

impl std::fmt::Debug for Stored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Stored {{ key_id: {}, fingerprint: {}, last4: **** }}",
            self.key_id, self.fingerprint
        )
    }
}

pub fn directory(ccs: &Path) -> PathBuf {
    ccs.join("account-usage").join("keys")
}

/// The store's rule for a key (`API_KEY_PATTERN`): 8 to 512 printable ASCII, no whitespace.
pub fn valid_secret(secret: &str) -> bool {
    (8..=512).contains(&secret.len()) && secret.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

pub fn fingerprint(secret: &str) -> String {
    format!("sha256:{}", &util::sha256_hex(secret.as_bytes())[..16])
}

fn last4(secret: &str) -> String {
    secret[secret.len().saturating_sub(4)..].to_string()
}

fn new_key_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    hasher.write_u32(std::process::id());
    format!("{:08x}", hasher.finish() as u32)
}

fn valid_key_id(id: &str) -> bool {
    id.len() == 8 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

const EXTENSION: &str = if cfg!(windows) { "dpapi" } else { "json" };

/// The folder exists at 0700, is no link, and belongs to this user.
fn ensure_directory(dir: &Path) -> Result<(), KeyError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(dir).map_err(|_| KeyError::Unsafe)?;
        let meta = fs::symlink_metadata(dir).map_err(|_| KeyError::Unsafe)?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::getuid() } {
            return Err(KeyError::Unsafe);
        }
        if meta.mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|_| KeyError::Unsafe)?;
        }
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir).map_err(|_| KeyError::Unsafe)?;
        let meta = fs::symlink_metadata(dir).map_err(|_| KeyError::Unsafe)?;
        if !meta.is_dir() {
            return Err(KeyError::Unsafe);
        }
    }
    Ok(())
}

/// The Cursor key files in the folder: (keyId, path, modified).
fn key_files(dir: &Path) -> Vec<(String, PathBuf, std::time::SystemTime)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("cursor-") else {
            continue;
        };
        let Some(id) = rest.strip_suffix(&format!(".{EXTENSION}")) else {
            continue;
        };
        if !valid_key_id(id) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_file() {
            found.push((id.to_string(), path, meta.modified().unwrap_or(std::time::UNIX_EPOCH)));
        }
    }
    found.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    found
}

/// Store a key, replacing any earlier Cursor key. Written to a new 0600 file, synced, renamed.
pub fn put(ccs: &Path, secret: &str) -> Result<Stored, KeyError> {
    if !valid_secret(secret) {
        return Err(KeyError::Invalid);
    }
    let dir = directory(ccs);
    ensure_directory(&dir)?;
    let key_id = new_key_id();
    let stored = Stored {
        key_id: key_id.clone(),
        secret: secret.to_string(),
        fingerprint: fingerprint(secret),
        last4: last4(secret),
    };
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let record = json!({
        "version": 1,
        "provider": PROVIDER,
        "keyId": key_id,
        "secret": secret,
        "fingerprint": stored.fingerprint,
        "last4": stored.last4,
        "createdAt": iso_seconds(created),
    });
    let plain = format!("{record}\n");
    let bytes = protect(plain.as_bytes())?;
    let target = dir.join(format!("cursor-{key_id}.{EXTENSION}"));
    let temporary = dir.join(format!(".cursor-{key_id}.{}.tmp", new_key_id()));
    let written = (|| -> std::io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &target)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(KeyError::Unsafe);
    }
    // One Cursor key at a time: the earlier ones go once the new one is in place.
    for (id, path, _) in key_files(&dir) {
        if id != key_id {
            let _ = fs::remove_file(path);
        }
    }
    Ok(stored)
}

/// The stored key, or None when there is none. A file that is a link, too large, readable by
/// others, or not a valid record is never used (and makes the store unsafe, not empty).
pub fn load(ccs: &Path) -> Result<Option<Stored>, KeyError> {
    let dir = directory(ccs);
    let Ok(meta) = fs::symlink_metadata(&dir) else {
        return Ok(None);
    };
    if !meta.is_dir() {
        return Err(KeyError::Unsafe);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 {
            return Err(KeyError::Unsafe);
        }
    }
    let Some((key_id, path, _)) = key_files(&dir).into_iter().next() else {
        return Ok(None);
    };
    let meta = fs::symlink_metadata(&path).map_err(|_| KeyError::Unsafe)?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES * 4 {
        return Err(KeyError::Unsafe);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 {
            return Err(KeyError::Unsafe);
        }
    }
    let mut raw = Vec::new();
    fs::File::open(&path)
        .and_then(|mut file| file.read_to_end(&mut raw))
        .map_err(|_| KeyError::Unsafe)?;
    let plain = unprotect(&raw)?;
    if plain.len() as u64 > MAX_FILE_BYTES {
        return Err(KeyError::Unsafe);
    }
    let record: Value = serde_json::from_slice(&plain).map_err(|_| KeyError::Unsafe)?;
    let secret = record.get("secret").and_then(Value::as_str).unwrap_or("");
    if record.get("version") != Some(&json!(1))
        || record.get("provider").and_then(Value::as_str) != Some(PROVIDER)
        || record.get("keyId").and_then(Value::as_str) != Some(key_id.as_str())
        || !valid_secret(secret)
        || record
            .get("fingerprint")
            .and_then(Value::as_str)
            .is_some_and(|f| f != fingerprint(secret))
    {
        return Err(KeyError::Unsafe);
    }
    Ok(Some(Stored {
        key_id,
        secret: secret.to_string(),
        fingerprint: fingerprint(secret),
        last4: last4(secret),
    }))
}

/// Remove every Cursor key; how many files went.
pub fn delete(ccs: &Path) -> usize {
    key_files(&directory(ccs))
        .into_iter()
        .filter(|(_, path, _)| fs::remove_file(path).is_ok())
        .count()
}

fn iso_seconds(secs: i64) -> String {
    let hour = util::hour_label(secs * 1000).unwrap_or_default();
    let rem = secs.rem_euclid(3600);
    format!("{}T{}:{:02}:{:02}Z", &hour[..10], &hour[11..13], rem / 60, rem % 60)
}

#[cfg(not(windows))]
fn protect(plain: &[u8]) -> Result<Vec<u8>, KeyError> {
    Ok(plain.to_vec())
}

#[cfg(not(windows))]
fn unprotect(stored: &[u8]) -> Result<Vec<u8>, KeyError> {
    Ok(stored.to_vec())
}

#[cfg(windows)]
fn protect(plain: &[u8]) -> Result<Vec<u8>, KeyError> {
    dpapi(plain, true)
}

#[cfg(windows)]
fn unprotect(stored: &[u8]) -> Result<Vec<u8>, KeyError> {
    dpapi(stored, false)
}

/// CryptProtectData / CryptUnprotectData, CurrentUser scope, the store's entropy, never a prompt.
#[cfg(windows)]
fn dpapi(input: &[u8], encrypt: bool) -> Result<Vec<u8>, KeyError> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    let data_in = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: every pointer refers to a live buffer of the stated length for the call's duration;
    // the output buffer is copied, then freed with LocalFree as the API requires.
    let ok = unsafe {
        if encrypt {
            CryptProtectData(
                &data_in,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        } else {
            CryptUnprotectData(
                &data_in,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
    };
    if ok == 0 || out.pbData.is_null() {
        return Err(KeyError::Unsafe);
    }
    // SAFETY: the API returned `cbData` valid bytes at `pbData`.
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe {
        LocalFree(out.pbData as _);
    }
    Ok(bytes)
}
