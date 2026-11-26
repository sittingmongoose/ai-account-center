//! R1: `GET /api/views/home` from fed generations. The request carries no body: the front door sends the
//! generation ids it read and pushed, its clock and the transport facts in `x-aac-feed`, and the page's context
//! (validated by TS, URI-encoded JSON) in `x-aac-home-context` when the page sent a readable one. The service reads
//! the desktop manifest itself (part 4's capped read under its boot root) and answers the final bytes; any
//! generation it does not hold as named is `stale`, and TS answers.
use crate::feed::{Feeds, Generation, HOME_ANTIGRAVITY, HOME_DASHBOARD, HOME_OPENS, Prepared};
use aac_api::home::HomeContext;
use aac_views::decline::Decline;
use aac_views::home::fed;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

/// The front door's per-request facts.
pub(crate) const FEED_HEADER: &str = "x-aac-feed";
/// The page's context as TS read it.
pub(crate) const CONTEXT_HEADER: &str = "x-aac-home-context";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Feed {
    /// `mac` or `windows` (the query TS validated).
    platform: String,
    /// `isSecureTransport(req)`: the dashboard generation differs by it.
    secure: bool,
    /// The per-response pass's clock (`markPassedResets`).
    now: i64,
    /// The answer's `updatedAt`.
    updated_at: String,
    dashboard: String,
    opens: String,
    antigravity: String,
}

/// `decodeURIComponent` of what `encodeURIComponent` wrote: `%XX` escapes of UTF-8 bytes. Anything else declines.
fn uri_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn context(text: &str, platform: &str) -> Result<HomeContext, Decline> {
    let json = uri_decode(text).ok_or(Decline::Shape)?;
    let ctx: HomeContext = serde_json::from_str(&json).map_err(|_| Decline::Shape)?;
    if ctx.platform == platform {
        Ok(ctx)
    } else {
        Err(Decline::Shape)
    }
}

/// R1b: the profiles of the last manifest text and Open generation: the manifest's composition, its rows with each
/// Open's progress, and their JSON text. Kept by the exact manifest bytes (read on every request, compared whole) and
/// the Open generation's digest, so a changed file or a new Open is never answered from it.
struct Profiles {
    manifest: Option<Vec<u8>>,
    opens: [u8; 32],
    profiles: Arc<Value>,
    text: Arc<Vec<u8>>,
    digest: [u8; 32],
}

static PROFILES: Mutex<Option<Profiles>> = Mutex::new(None);

/// One request's profiles: the rows, their JSON text and the manifest's digest (for the `ETag`).
struct Rows {
    profiles: Arc<Value>,
    text: Arc<Vec<u8>>,
    digest: [u8; 32],
}

/// This request's profiles and their text, from the cache when the manifest bytes and the Open generation are the
/// last request's.
fn profiles(
    manifest: Option<Vec<u8>>,
    opens: &Generation,
    held: &fed::Opens,
) -> Result<Rows, Decline> {
    if let Ok(kept) = PROFILES.lock()
        && let Some(kept) = kept.as_ref()
        && kept.opens == opens.digest
        && kept.manifest == manifest
    {
        return Ok(Rows {
            profiles: kept.profiles.clone(),
            text: kept.text.clone(),
            digest: kept.digest,
        });
    }
    // GET /api/claude/desktop-profiles's Rust read; a manifest it refuses stays with the TS reader.
    let composed =
        crate::profile_reads::compose_manifest(manifest.as_deref()).ok_or(Decline::Unsupported)?;
    let rows = Arc::new(fed::profiles_with_opens(&composed, held)?);
    let text = Arc::new(aac_views::js_json::write(&rows));
    let mut hash = Sha256::new();
    match &manifest {
        Some(bytes) => {
            hash.update(b"manifest");
            hash.update(bytes);
        }
        None => hash.update(b"absent"),
    }
    let digest: [u8; 32] = hash.finalize().into();
    if let Ok(mut kept) = PROFILES.lock() {
        *kept = Some(Profiles {
            manifest,
            opens: opens.digest,
            profiles: rows.clone(),
            text: text.clone(),
            digest,
        });
    }
    Ok(Rows {
        profiles: rows,
        text,
        digest,
    })
}

/// One fed Home answer: its rendered JSON bytes and content-derived weak ETag.
pub(crate) struct Answer {
    pub(crate) bytes: Vec<u8>,
    /// Content-derived ETag (`W/"<sha256>"`) over build commit, generation digests, manifest digest,
    /// page context, and the rendered bytes (excluding the per-request millisecond `now` header so
    /// consecutive identical renders share an ETag and answer `304 Not Modified`, while any
    /// `resetPassed` flip or input change changes `bytes` and moves the ETag).
    pub(crate) etag: String,
}

/// The answer of one fed Home request. Runs on the blocking pool (it reads the manifest).
pub(crate) fn answer(
    feeds: &Feeds,
    root: &Path,
    feed: &str,
    page: Option<&str>,
) -> Result<Answer, Decline> {
    let header: Feed = crate::json_object::from_slice(
        feed.as_bytes(),
        &[
            "platform",
            "secure",
            "now",
            "updatedAt",
            "dashboard",
            "opens",
            "antigravity",
        ],
    )
    .map_err(|_| Decline::Shape)?;
    if header.platform != "mac" && header.platform != "windows" {
        return Err(Decline::Shape);
    }
    let source = format!(
        "{HOME_DASHBOARD}{}-{}",
        header.platform,
        u8::from(header.secure)
    );
    let dashboard_gen = feeds.current(&source, &header.dashboard)?;
    let opens_gen = feeds.current(HOME_OPENS, &header.opens)?;
    let antigravity_gen = feeds.current(HOME_ANTIGRAVITY, &header.antigravity)?;
    let (
        Prepared::Dashboard(dashboard),
        Prepared::Opens(opens),
        Prepared::Antigravity(antigravity),
    ) = (
        &dashboard_gen.prepared,
        &opens_gen.prepared,
        &antigravity_gen.prepared,
    )
    else {
        return Err(Decline::Internal);
    };
    let dashboard = dashboard.as_ref().map_err(|d| *d)?;
    let opens = opens.as_ref().map_err(|d| *d)?;
    let antigravity = antigravity.as_ref().map_err(|d| *d)?;
    let context = page
        .map(|text| context(text, &header.platform))
        .transpose()?;
    let manifest = crate::profile_reads::desktop_manifest(root).ok_or(Decline::Unsupported)?;
    let rows = profiles(manifest, &opens_gen, opens)?;
    let manifest_digest = rows.digest;
    let bytes = fed::respond(fed::Request {
        dashboard,
        antigravity,
        profiles: &rows.profiles,
        profiles_text: &rows.text,
        context: context.as_ref(),
        now: header.now,
        updated_at: &header.updated_at,
    })?;
    let mut hash = Sha256::new();
    for part in [
        concat!("home@3\0", env!("AAC_BUILD_COMMIT"), "\0").as_bytes(),
        &dashboard_gen.digest,
        &opens_gen.digest,
        &antigravity_gen.digest,
        &manifest_digest,
        b"\0",
        page.unwrap_or("-").as_bytes(),
        b"\0",
        bytes.as_slice(),
    ] {
        hash.update(part);
    }
    let digest: [u8; 32] = hash.finalize().into();
    let etag = format!("W/\"{}\"", crate::view_reply::hex_digest(&digest));
    Ok(Answer { bytes, etag })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_context_is_what_encode_uri_component_wrote() {
        // Node: encodeURIComponent('{"a":"Jan 5 12:07 AM é"}')
        let text = "%7B%22a%22%3A%22Jan%205%E2%80%AF12%3A07%20AM%20%C3%A9%22%7D";
        assert_eq!(
            uri_decode(text).as_deref(),
            Some("{\"a\":\"Jan 5\u{202f}12:07 AM é\"}")
        );
        for bad in ["%", "%4", "%zz", "%C3"] {
            assert_eq!(uri_decode(bad), None, "{bad}");
        }
    }
}
