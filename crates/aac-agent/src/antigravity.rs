//! Host-side `"antigravity"` agent verb covering native credential store operations
//! (`read`, `install`, `rollback`, `bind-current`) and runtime bundle health inspection.

use aac_core::{Cause, Failure, UtcMs};
use aac_files::antigravity_store::{
    ProviderCallError, UbuntuFileStore, execute_native_packet, parse_strict_json_object,
    runtime_health,
};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

const USERINFO_ENDPOINT: &str = "https://www.googleapis.com/oauth2/v2/userinfo";
const USERINFO_MAX_BYTES: usize = 16_384;
const USERINFO_TIMEOUT_SECS: u64 = 5;

fn refused(reason: &'static str) -> Failure {
    Failure::new(Cause::Refused, reason)
}

async fn fetch_userinfo(endpoint: &str, access_token: &str) -> Result<Value, ProviderCallError> {
    if access_token.is_empty() || access_token.len() > USERINFO_MAX_BYTES {
        return Err(ProviderCallError::Unavailable);
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(USERINFO_TIMEOUT_SECS))
        .build()
        .map_err(|_| ProviderCallError::Unavailable)?;
    let mut resp = client
        .get(endpoint)
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .await
        .map_err(|_| ProviderCallError::Unavailable)?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(ProviderCallError::NeedsSignIn);
    }
    if !(200..300).contains(&status) {
        return Err(ProviderCallError::Unavailable);
    }
    let mut raw = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|_| ProviderCallError::Unavailable)?
    {
        if raw.len().saturating_add(chunk.len()) > USERINFO_MAX_BYTES {
            return Err(ProviderCallError::Unavailable);
        }
        raw.extend_from_slice(&chunk);
    }
    let map = parse_strict_json_object(&raw).map_err(|_| ProviderCallError::Unavailable)?;
    Ok(Value::Object(map))
}

/// Dispatch one `"antigravity"` verb request.
pub async fn run(args: &Value) -> Result<Value, Failure> {
    let Some(obj) = args.as_object() else {
        return Err(refused("invalid antigravity args"));
    };
    let Some(action) = obj.get("action").and_then(Value::as_str) else {
        return Err(refused("missing antigravity action"));
    };
    match action {
        "runtime-health" => {
            if obj
                .keys()
                .any(|k| !matches!(k.as_str(), "action" | "bundle" | "python"))
            {
                return Err(refused("unexpected antigravity runtime-health field"));
            }
            let Some(bundle_str) = obj.get("bundle").and_then(Value::as_str) else {
                return Err(refused("missing bundle path"));
            };
            let bundle = PathBuf::from(bundle_str);
            if !bundle.is_absolute() {
                return Err(refused("bundle path must be absolute"));
            }
            let python = match obj.get("python") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(_) => return Err(refused("invalid python minor")),
            };
            crate::blocking(move || Ok(runtime_health::check_bundle(&bundle, python.as_deref())))
                .await
        }
        "native-credential" => {
            if obj
                .keys()
                .any(|k| !matches!(k.as_str(), "action" | "home" | "packet"))
            {
                return Err(refused("unexpected antigravity native-credential field"));
            }
            let Some(home_str) = obj.get("home").and_then(Value::as_str) else {
                return Err(refused("missing home path"));
            };
            let home = PathBuf::from(home_str);
            if !home.is_absolute() {
                return Err(refused("home path must be absolute"));
            }
            let packet = obj
                .get("packet")
                .filter(|v| v.is_object())
                .cloned()
                .ok_or_else(|| refused("missing native credential packet"))?;
            let now_ms = i64::try_from(UtcMs::now()?.get()).unwrap_or(0);
            let is_bind = packet.get("operation").and_then(Value::as_str) == Some("bind-current");
            if is_bind {
                let home_for_read = home.clone();
                let (before_key, access_token) = crate::blocking(move || {
                    let store = UbuntuFileStore::new(&home_for_read);
                    let snap = store.read_current().map_err(|_| {
                        Failure::new(
                            Cause::Refused,
                            "Antigravity private store request failed safely.",
                        )
                    })?;
                    let parsed =
                        aac_files::antigravity_store::parse_credential(snap.credential.raw())
                            .map_err(|_| {
                                Failure::new(
                                    Cause::Refused,
                                    "Antigravity private store request failed safely.",
                                )
                            })?;
                    let key = (
                        snap.device,
                        snap.inode,
                        snap.modified_ns,
                        snap.credential.revision().to_owned(),
                    );
                    Ok((key, parsed.access_token))
                })
                .await?;
                let userinfo_res = fetch_userinfo(USERINFO_ENDPOINT, &access_token).await;
                crate::blocking(move || {
                    let store = UbuntuFileStore::new(&home);
                    let after = store.read_current().map_err(|_| {
                        Failure::new(
                            Cause::Refused,
                            "Antigravity private store request failed safely.",
                        )
                    })?;
                    let after_key = (
                        after.device,
                        after.inode,
                        after.modified_ns,
                        after.credential.revision().to_owned(),
                    );
                    if before_key != after_key {
                        return Err(Failure::new(
                            Cause::Refused,
                            "Antigravity private store request failed safely.",
                        ));
                    }
                    execute_native_packet(&packet, &store, |_| userinfo_res, now_ms).map_err(|_| {
                        Failure::new(
                            Cause::Refused,
                            "Antigravity private store request failed safely.",
                        )
                    })
                })
                .await
            } else {
                crate::blocking(move || {
                    let store = UbuntuFileStore::new(&home);
                    execute_native_packet(
                        &packet,
                        &store,
                        |_| Err(ProviderCallError::Unavailable),
                        now_ms,
                    )
                    .map_err(|_| {
                        Failure::new(
                            Cause::Refused,
                            "Antigravity private store request failed safely.",
                        )
                    })
                })
                .await
            }
        }
        _ => Err(refused("unsupported antigravity action")),
    }
}
