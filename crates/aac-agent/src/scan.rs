//! Discovery adapter: the standalone scanner remains the sole parser and reader.
use aac_core::{Cause, Failure};
use aac_usage::{discover, protocol};
use serde_json::value::RawValue;

/// Deserialize the scan request directly from the raw stdin frame bytes (borrowing `args`
/// without allocating a second `Box<RawValue>` copy or expanding a `serde_json::Value` tree),
/// release the stdin buffer immediately, then execute the scan and return pre-serialized JSON
/// response bytes (R4).
pub(crate) async fn run_frame(frame_bytes: Vec<u8>) -> Result<Box<RawValue>, Failure> {
    #[derive(serde::Deserialize)]
    struct BorrowedArgs<'a> {
        #[serde(borrow)]
        args: &'a RawValue,
    }
    let parsed = {
        let envelope: BorrowedArgs<'_> = serde_json::from_slice(&frame_bytes)
            .map_err(|_| Failure::new(Cause::Damaged, "invalid agent request JSON"))?;
        #[cfg(feature = "parity-fixtures")]
        {
            protocol::parse_request_with_fixture(envelope.args.get().as_bytes())
                .map_err(|_| Failure::new(Cause::Refused, "invalid scan request"))?
        }
        #[cfg(not(feature = "parity-fixtures"))]
        {
            protocol::parse_request(envelope.args.get().as_bytes())
                .map_err(|_| Failure::new(Cause::Refused, "invalid scan request"))?
        }
    };
    // Release the raw stdin frame immediately before running the scan.
    drop(frame_bytes);
    #[cfg(feature = "parity-fixtures")]
    let (request, fixture) = parsed;
    #[cfg(not(feature = "parity-fixtures"))]
    let request = parsed;

    let env = discover::env_from(
        std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?))),
    );
    let home = aac_usage::home_dir(&env);
    #[cfg(feature = "parity-fixtures")]
    let (home, env) = if let Some(fixture) = fixture {
        let home = fixture
            .get("home")
            .and_then(serde_json::Value::as_str)
            .map(std::path::PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or_else(|| Failure::new(Cause::Refused, "invalid scan fixture home"))?;
        let vars: std::collections::HashMap<String, String> = serde_json::from_value(
            fixture
                .get("env")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        )
        .map_err(|_| Failure::new(Cause::Refused, "invalid scan fixture environment"))?;
        (Some(home), discover::env_from(vars))
    } else {
        (home, env)
    };
    // Do filesystem work off the async executor, including a home on a hung mount. Main exits
    // the process after the terminal event, so blocked scanner threads cannot delay cancellation.
    let wait =
        std::time::Duration::from_secs_f64(request.budget + (request.budget * 0.2).clamp(1.0, 4.0));
    let stalled = protocol::stalled_answer(&request);
    let worker = tokio::task::spawn_blocking(move || {
        aac_usage::lower_priority();
        let home = home
            .filter(|home| home.is_dir())
            .ok_or_else(|| Failure::new(Cause::NotInstalled, "scan home is unavailable"))?;
        let mut bytes = protocol::scan_persistent_owned_bytes(request, &home, &env);
        // `RawValue::from_string` copies a text whose capacity exceeds its length; shrinking
        // first keeps the answer in one buffer (R4).
        bytes.shrink_to_fit();
        let text = String::from_utf8(bytes)
            .map_err(|_| Failure::new(Cause::Bug, "scan encoding failed"))?;
        RawValue::from_string(text).map_err(|_| Failure::new(Cause::Bug, "scan encoding failed"))
    });
    match tokio::time::timeout(wait, worker).await {
        Ok(result) => result.map_err(|_| Failure::new(Cause::Bug, "scan worker failed"))?,
        Err(_) => serde_json::value::to_raw_value(&stalled)
            .map_err(|_| Failure::new(Cause::Bug, "stalled scan encoding failed")),
    }
}
