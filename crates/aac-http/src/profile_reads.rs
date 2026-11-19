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
        // G6: the runtime's own read once TS stops sending its input. The live-identity proof
        // still arrives with the native driver port, so every row is unselected for now.
        "/api/antigravity/profiles" if bytes.is_empty() => {
            crate::antigravity_runtime::inventory_now(root)
        }
        "/api/antigravity/profiles" => aac_views::antigravity::inventory(envelope(bytes)?).ok(),
        // G6: the quota read once TS stops sending its input. The quota worker is not wired to the
        // route yet, so the service's fallback rows answer (never a fabricated sample).
        "/api/antigravity/profiles/quotas" if bytes.is_empty() => {
            crate::antigravity_quota::quotas_now(root, None, false, &mut |_| None)
        }
        "/api/antigravity/profiles/quotas" => aac_views::antigravity::quotas(envelope(bytes)?).ok(),
        _ => None,
    }
}
