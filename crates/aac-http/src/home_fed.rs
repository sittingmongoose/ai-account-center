//! R1: `GET /api/views/home` from fed generations. The request carries no body: the front door sends the
//! generation ids it read and pushed, its clock and the transport facts in `x-aac-feed`, and the page's context
//! (validated by TS, URI-encoded JSON) in `x-aac-home-context` when the page sent a readable one. The service reads
//! the desktop manifest itself (part 4's capped read under its boot root) and answers the final bytes; any
//! generation it does not hold as named is `stale`, and TS answers.
use crate::feed::{Feeds, HOME_ANTIGRAVITY, HOME_DASHBOARD, HOME_OPENS, Prepared};
use aac_api::home::HomeContext;
use aac_views::decline::Decline;
use aac_views::home::fed;
use serde::Deserialize;
use std::path::Path;

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

/// The answer of one fed Home request. Runs on the blocking pool (it reads the manifest).
pub(crate) fn answer(
    feeds: &Feeds,
    root: &Path,
    feed: &str,
    page: Option<&str>,
) -> Result<fed::Answer, Decline> {
    let feed: Feed = crate::json_object::from_slice(
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
    if feed.platform != "mac" && feed.platform != "windows" {
        return Err(Decline::Shape);
    }
    let source = format!(
        "{HOME_DASHBOARD}{}-{}",
        feed.platform,
        u8::from(feed.secure)
    );
    let dashboard = feeds.current(&source, &feed.dashboard)?;
    let opens = feeds.current(HOME_OPENS, &feed.opens)?;
    let antigravity = feeds.current(HOME_ANTIGRAVITY, &feed.antigravity)?;
    let (
        Prepared::Dashboard(dashboard),
        Prepared::Opens(opens),
        Prepared::Antigravity(antigravity),
    ) = (&dashboard.prepared, &opens.prepared, &antigravity.prepared)
    else {
        return Err(Decline::Internal);
    };
    let dashboard = dashboard.as_ref().map_err(|d| *d)?;
    let opens = opens.as_ref().map_err(|d| *d)?;
    let antigravity = antigravity.as_ref().map_err(|d| *d)?;
    let page = page.map(|text| context(text, &feed.platform)).transpose()?;
    // GET /api/claude/desktop-profiles's Rust read; a manifest it refuses stays with the TS reader.
    let manifest = crate::profile_reads::desktop_profiles(root).ok_or(Decline::Unsupported)?;
    fed::respond(fed::Request {
        dashboard,
        opens,
        antigravity,
        manifest: &manifest,
        context: page.as_ref(),
        now: feed.now,
        updated_at: &feed.updated_at,
    })
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
