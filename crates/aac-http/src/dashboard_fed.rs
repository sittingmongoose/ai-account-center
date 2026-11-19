//! G2: the Accounts and tray response from the dashboard generation shared with Home.
use crate::feed::{Feeds, HOME_DASHBOARD, Prepared};
use aac_views::decline::Decline;
use serde::Deserialize;

pub(crate) const VIEW: &str = "accounts-dashboard";
pub(crate) const VERSION: u32 = 1;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    platform: String,
    secure: bool,
    now: i64,
    updated_at: String,
    dashboard: String,
}

pub(crate) fn answer(feeds: &Feeds, header: &str) -> Result<(Vec<u8>, Vec<u8>), Decline> {
    let request: Request = crate::json_object::from_slice(
        header.as_bytes(),
        &["platform", "secure", "now", "updatedAt", "dashboard"],
    )
    .map_err(|_| Decline::Shape)?;
    if !["mac", "windows"].contains(&request.platform.as_str()) {
        return Err(Decline::Shape);
    }
    let source = format!(
        "{HOME_DASHBOARD}{}-{}",
        request.platform,
        u8::from(request.secure)
    );
    let generation = feeds.current(&source, &request.dashboard)?;
    let Prepared::Dashboard(dashboard) = &generation.prepared else {
        return Err(Decline::Internal);
    };
    let dashboard = dashboard.as_ref().map_err(|reason| *reason)?;
    let mut identity = Vec::with_capacity(64 + header.len());
    identity.extend_from_slice(env!("AAC_BUILD_COMMIT").as_bytes());
    identity.extend_from_slice(&generation.digest);
    identity.extend_from_slice(header.as_bytes());
    Ok((dashboard.render(&request.updated_at, request.now), identity))
}
