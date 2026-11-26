//! macOS login Keychain reads with user interaction disabled, and CommonCrypto AES-128-CBC
//! (`_mac_keychain_read_bytes`, `_mac_keychain_unlocked`, `_mac_aes128_cbc_decrypt`).
//! Other platforms have no Keychain: every item is absent.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

pub(crate) const NOT_FOUND: i32 = -25300;
#[cfg(target_os = "macos")]
const ACL_OR_LOCKED: [i32; 2] = [-25293, -25308];

/// `_mac_keychain_account`: $USER, else the OS user name, else Claude Code's own default.
#[allow(unsafe_code)]
pub(crate) fn account() -> String {
    let valid = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    };
    if let Ok(user) = std::env::var("USER")
        && !user.is_empty()
    {
        return if valid(&user) {
            user
        } else {
            "claude-code-user".into()
        };
    }
    #[cfg(unix)]
    {
        // SAFETY: getpwuid returns a pointer into static storage or null; it is read at once.
        let name = unsafe {
            let pw = libc::getpwuid(libc::getuid());
            if pw.is_null() || (*pw).pw_name.is_null() {
                None
            } else {
                std::ffi::CStr::from_ptr((*pw).pw_name)
                    .to_str()
                    .ok()
                    .map(str::to_owned)
            }
        };
        if let Some(name) = name.filter(|n| valid(n)) {
            return name;
        }
    }
    "claude-code-user".into()
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod ffi {
    use std::ffi::c_void;
    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        pub fn SecKeychainSetUserInteractionAllowed(state: u8) -> i32;
        pub fn SecKeychainGetStatus(keychain: *const c_void, status: *mut u32) -> i32;
        pub fn SecKeychainFindGenericPassword(
            keychain: *const c_void,
            service_len: u32,
            service: *const u8,
            account_len: u32,
            account: *const u8,
            password_len: *mut u32,
            password: *mut *mut c_void,
            item: *mut *mut c_void,
        ) -> i32;
        pub fn SecKeychainItemFreeContent(attrs: *mut c_void, data: *mut c_void) -> i32;
    }
    unsafe extern "C" {
        pub fn CCCrypt(
            op: u32,
            alg: u32,
            options: u32,
            key: *const c_void,
            key_len: usize,
            iv: *const c_void,
            input: *const c_void,
            input_len: usize,
            output: *mut c_void,
            output_cap: usize,
            moved: *mut usize,
        ) -> i32;
    }
}

/// Whether the default login Keychain is unlocked in this security session, without UI.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(crate) fn unlocked() -> bool {
    // SAFETY: plain C calls with valid out pointers.
    unsafe {
        if ffi::SecKeychainSetUserInteractionAllowed(0) != 0 {
            return false;
        }
        let mut flags = 0u32;
        ffi::SecKeychainGetStatus(std::ptr::null(), &mut flags) == 0 && flags & 1 != 0
    }
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn unlocked() -> bool {
    false
}

fn over_ssh() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some_and(|v| !v.is_empty())
        || std::env::var_os("SSH_CLIENT").is_some_and(|v| !v.is_empty())
}

/// One generic password: `Ok(bytes)`, `Err(NOT_FOUND)` when absent, `Err(status)` otherwise.
/// Only Claude Code items (`allow_cli`) may use `/usr/bin/security`, and only in an unlocked,
/// non-SSH desktop session after the API refused for ACL or lock reasons.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(crate) fn read(service: &str, account: &str, allow_cli: bool) -> Result<Vec<u8>, i32> {
    use std::ffi::c_void;
    let max = super::desktop::MAX_BYTES as u32;
    // SAFETY: the service/account buffers outlive the call; the returned data is freed below.
    let rc = unsafe {
        if ffi::SecKeychainSetUserInteractionAllowed(0) != 0 {
            return Err(-1);
        }
        let mut len = 0u32;
        let mut data: *mut c_void = std::ptr::null_mut();
        let rc = ffi::SecKeychainFindGenericPassword(
            std::ptr::null(),
            service.len() as u32,
            service.as_ptr(),
            account.len() as u32,
            account.as_ptr(),
            &mut len,
            &mut data,
            std::ptr::null_mut(),
        );
        if rc == 0 {
            let out = (!data.is_null() && len > 0 && len <= max)
                .then(|| std::slice::from_raw_parts(data as *const u8, len as usize).to_vec());
            if !data.is_null() {
                ffi::SecKeychainItemFreeContent(std::ptr::null_mut(), data);
            }
            return out.ok_or(-1);
        }
        rc
    };
    if rc == NOT_FOUND {
        return Err(NOT_FOUND);
    }
    if allow_cli && !account.is_empty() && ACL_OR_LOCKED.contains(&rc) && unlocked() && !over_ssh()
    {
        let output = std::process::Command::new("/usr/bin/security")
            .args(["find-generic-password", "-a", account, "-s", service, "-w"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        if let Ok(output) = output
            && output.status.success()
        {
            let mut raw = output.stdout;
            while raw.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                raw.pop();
            }
            if !raw.is_empty() && raw.len() <= max as usize {
                return Ok(raw);
            }
        }
    }
    Err(rc)
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn read(_: &str, _: &str, _: bool) -> Result<Vec<u8>, i32> {
    Err(NOT_FOUND)
}

/// Electron safeStorage AES-128-CBC with PKCS#7 padding through CommonCrypto.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(crate) fn aes128_cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    let max = super::desktop::MAX_BYTES;
    if data.len() > max {
        return None;
    }
    let mut out = vec![0u8; data.len() + 16];
    let mut moved = 0usize;
    // SAFETY: every buffer is valid for its stated length. kCCDecrypt=1, AES128=0, PKCS7=1.
    let rc = unsafe {
        ffi::CCCrypt(
            1,
            0,
            1,
            key.as_ptr().cast(),
            16,
            iv.as_ptr().cast(),
            data.as_ptr().cast(),
            data.len(),
            out.as_mut_ptr().cast(),
            out.len(),
            &mut moved,
        )
    };
    if rc != 0 || moved == 0 || moved > max {
        return None;
    }
    out.truncate(moved);
    Some(out)
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn aes128_cbc_decrypt(_: &[u8; 16], _: &[u8; 16], _: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Electron's macOS safeStorage round trip: CommonCrypto encrypt, then this decrypt.
    #[allow(unsafe_code)]
    #[test]
    fn safe_storage_cbc_round_trip() {
        let key = super::super::desktop::safe_storage_key(b"secret");
        let iv = [b' '; 16];
        let plain = br#"{"acct":"x"}"#;
        let mut sealed = vec![0u8; plain.len() + 16];
        let mut moved = 0usize;
        // SAFETY: valid buffers; kCCEncrypt=0, AES128=0, PKCS7=1.
        let rc = unsafe {
            ffi::CCCrypt(
                0,
                0,
                1,
                key.as_ptr().cast(),
                16,
                iv.as_ptr().cast(),
                plain.as_ptr().cast(),
                plain.len(),
                sealed.as_mut_ptr().cast(),
                sealed.len(),
                &mut moved,
            )
        };
        assert_eq!(rc, 0);
        sealed.truncate(moved);
        assert_eq!(aes128_cbc_decrypt(&key, &iv, &sealed).unwrap(), plain);
        assert!(aes128_cbc_decrypt(&[0u8; 16], &iv, &sealed).is_none_or(|p| p != plain));
        assert_eq!(
            read("aac-synthetic-missing-item", "nobody", false),
            Err(NOT_FOUND)
        );
    }
}
