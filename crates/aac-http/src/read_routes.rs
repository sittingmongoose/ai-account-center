//! Read-route candidates. Inputs come only from guarded TS handlers over the existing source
//! owners. Supported raw inputs compose here; anything else is a named decline (L1), which the
//! bridge logs per route and reason before the TS handler answers. Routing is by the table's
//! handler, then one typed parse of the input (no `Value` probe first).
use crate::table::Handler;
use aac_views::decline::Decline;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    snapshot: Value,
}

fn snapshot(bytes: &[u8]) -> Result<Snapshot, Decline> {
    crate::json_object::from_slice(bytes, &["snapshot"]).map_err(|_| Decline::Shape)
}

/// `root` is the service's own canonical boot root (never a path from the TS input). Runs on the
/// blocking pool.
pub(crate) fn answer(
    handler: Handler,
    path: &str,
    query: bool,
    root: &Path,
    bytes: &[u8],
    cache: Option<&crate::account_usage::AccountUsageCache>,
) -> Result<Value, Decline> {
    match handler {
        // D7: Home's view from the page's context and the three answers it reads, in the envelope.
        Handler::Home => {
            if query {
                return Err(Decline::Unsupported);
            }
            let envelope = crate::json_object::from_slice(bytes, &["data", "presentation"])
                .map_err(|_| Decline::Shape)?;
            aac_views::home::compose_named(envelope)
        }
        // TS refused any query before forwarding; anything else is not the read it proved.
        Handler::Accounts => {
            if query {
                return Err(Decline::Unsupported);
            }
            crate::account_reads::answer(path, root, bytes).ok_or(Decline::Unsupported)
        }
        // TS consumed its query (platform, refresh) before forwarding the bare route.
        Handler::Profiles => {
            if query {
                return Err(Decline::Unsupported);
            }
            crate::profile_reads::answer(path, root, bytes, cache).ok_or(Decline::Unsupported)
        }
        Handler::Files => crate::read_files::answer(path, bytes).ok_or(Decline::Unsupported),
        // Status and Scoreboard take the presentation envelope (compose) or the reader snapshot.
        Handler::Status | Handler::Scoreboard => {
            if let Ok(envelope) = crate::json_object::from_slice(bytes, &["data", "presentation"]) {
                return if handler == Handler::Status {
                    aac_status::read_status::compose(envelope)
                } else {
                    aac_status::read_scoreboard::compose(envelope)
                }
                .ok_or(Decline::Unsupported);
            }
            let input = snapshot(bytes)?;
            if handler == Handler::Status {
                aac_status::http_views::status(&input.snapshot)
            } else {
                aac_status::http_views::scoreboard(&input.snapshot)
            }
            .ok_or(Decline::Unsupported)
        }
        Handler::StoreStatus => aac_store::read_status::from_writer(&snapshot(bytes)?.snapshot)
            .ok_or(Decline::Unsupported),
        _ => Err(Decline::Unsupported),
    }
}
