//! GET /api/claude/desktop-profiles*, /api/codex/profiles* and /api/antigravity/profiles* for
//! the shared read-route dispatch (M3 part 4). Account switching is gone, so these routes only
//! read and report: profiles, usage and sign-in health.
//!
//! The Claude desktop manifest is read here, from the existing file under the boot root, with
//! the capped descriptor read; TS remains its only writer. Every other answer is composed from
//! what the guarded TS handler already read from its owners (remote history text, the Codex
//! registry and display-safe login fields, collector rows, renewal notes, the Antigravity
//! runtime). Nothing here scans, refreshes a provider or opens a credential. `None` asks for
//! the logged TS fallback.
use aac_files::read::Read;
use aac_status::read_context::Envelope;
use serde_json::{Value, json};
use std::path::Path;

/// `listClaudeDesktopProfileMetadata`'s rows. TS reads through a symlink and may accept a
/// larger multi-byte manifest, so both stay with the TS reader.
/// The registry row's 1 MiB cap: TS refuses more than 1 MiB of UTF-16 text, and at most 1 MiB of
/// UTF-8 is always within that.
pub(crate) fn desktop_profiles(root: &Path) -> Option<Value> {
    compose_manifest(desktop_manifest(root)?.as_deref())
}

/// The manifest's bytes as the capped read finds them: `Some(None)` when the file is absent, `None` when the read
/// refuses (a link, another owner, too large ...). R1b: Home caches the composition by these exact bytes.
pub(crate) fn desktop_manifest(root: &Path) -> Option<Option<Vec<u8>>> {
    let (file, policy) = crate::registered::file(root, "claude-desktop-profiles")?;
    match aac_files::read::read(&file, &policy) {
        Read::Absent => Some(None),
        Read::Bytes(bytes, _) => Some(Some(bytes)),
        _ => None,
    }
}

/// `listClaudeDesktopProfileMetadata`'s answer for the manifest bytes (`None` read as absent).
pub(crate) fn compose_manifest(bytes: Option<&[u8]>) -> Option<Value> {
    match bytes {
        None => Some(json!({ "profiles": [] })),
        Some(bytes) => aac_views::claude_desktop::profiles(bytes).ok(),
    }
}

/// The process's one installed Antigravity host (TS keeps the same module-wide runtime).
fn antigravity_host(root: &Path) -> Option<&'static crate::antigravity_host::Host> {
    static HOST: std::sync::OnceLock<Option<crate::antigravity_host::Host>> =
        std::sync::OnceLock::new();
    HOST.get_or_init(|| crate::antigravity_host::Host::from_root(root))
        .as_ref()
}

fn now_ms() -> i64 {
    aac_core::UtcMs::now()
        .ok()
        .and_then(|at| i64::try_from(at.get()).ok())
        .unwrap_or(0)
}

/// The empty Ubuntu answers the reference runtime returns when it is not installed. The
/// Antigravity runtime only exists on Linux; every other host answers these without a runtime.
fn empty_inventory() -> Value {
    json!({"schemaVersion": 1, "hostId": "ubuntu", "profiles": []})
}

fn empty_quotas(at: i64) -> Value {
    crate::antigravity_quota::public_quotas(&[], at, at).unwrap_or(Value::Null)
}

/// The public listener's Antigravity inventory (the same installed host, one per process).
pub(crate) fn antigravity_inventory(root: &Path) -> Option<Value> {
    if !cfg!(target_os = "linux") || crate::antigravity_runtime::read_state(root).is_none() {
        return Some(empty_inventory());
    }
    antigravity_host(root).map(|host| host.inventory(now_ms()))
}

/// The public listener's Antigravity quotas, with the route's own refresh selection.
pub(crate) fn antigravity_quotas(root: &Path, refresh: bool, at: i64) -> Option<Value> {
    if !cfg!(target_os = "linux") || crate::antigravity_runtime::read_state(root).is_none() {
        return Some(empty_quotas(at));
    }
    antigravity_host(root).map(|host| host.quotas(refresh, at))
}

fn envelope(bytes: &[u8]) -> Option<Envelope> {
    crate::json_object::from_slice(bytes, &["data", "presentation"]).ok()
}

pub(crate) fn answer(path: &str, root: &Path, bytes: &[u8]) -> Option<Value> {
    match path {
        "/api/claude/desktop-profiles" if bytes.is_empty() => desktop_profiles(root),
        "/api/claude/desktop-profiles/usage" => {
            let input: Value =
                crate::json_object::from_slice(bytes, &["platform", "fetchedAt", "profiles"])
                    .ok()?;
            aac_views::claude_desktop::usage(&input).ok()
        }
        "/api/codex/profiles" => {
            let input: Value = crate::json_object::from_slice(
                bytes,
                &["instancesDir", "env", "registry", "profiles", "activated"],
            )
            .ok()?;
            aac_views::codex_profiles::summary(&input).ok()
        }
        "/api/codex/profiles/quotas" => aac_views::codex_profiles::quotas(envelope(bytes)?).ok(),
        // G6: the installed runtime answers with its own live proof, workers and probes once TS
        // stops sending its input. A host that cannot resolve its trusted paths declines (TS).
        "/api/antigravity/profiles" if bytes.is_empty() => {
            antigravity_host(root).map(|host| host.inventory(now_ms()))
        }
        "/api/antigravity/profiles" => aac_views::antigravity::inventory(envelope(bytes)?).ok(),
        // G6: the quota read once TS stops sending its input, from the installed host's worker.
        "/api/antigravity/profiles/quotas" if bytes.is_empty() => {
            antigravity_host(root).map(|host| host.quotas(false, now_ms()))
        }
        "/api/antigravity/profiles/quotas" => aac_views::antigravity::quotas(envelope(bytes)?).ok(),
        _ => None,
    }
}
