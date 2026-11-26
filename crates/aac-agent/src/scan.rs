//! Discovery adapter: the standalone scanner remains the sole parser and reader.
use aac_core::{Cause, Failure};
use aac_usage::{discover, protocol};
use serde_json::Value;

pub(crate) async fn run(args: &Value) -> Result<Value, Failure> {
    #[cfg(feature = "parity-fixtures")]
    let (input, fixture) = {
        let mut input = args.clone();
        let fixture = input
            .as_object_mut()
            .and_then(|map| map.remove("_scanFixture"));
        (input, fixture)
    };
    #[cfg(not(feature = "parity-fixtures"))]
    let input = args.clone();
    // No new Python/JSON emulator: the already-shipped Rust parser owns this grammar.
    let raw = serde_json::to_vec(&input)
        .map_err(|_| Failure::new(Cause::Refused, "invalid scan request"))?;
    let request = protocol::parse_request(&raw)
        .map_err(|_| Failure::new(Cause::Refused, "invalid scan request"))?;
    let env = discover::env_from(
        std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?))),
    );
    let home = aac_usage::home_dir(&env);
    #[cfg(feature = "parity-fixtures")]
    let (home, env) = if let Some(fixture) = fixture {
        let home = fixture
            .get("home")
            .and_then(Value::as_str)
            .map(std::path::PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or_else(|| Failure::new(Cause::Refused, "invalid scan fixture home"))?;
        let vars: std::collections::HashMap<String, String> =
            serde_json::from_value(fixture.get("env").cloned().unwrap_or(serde_json::json!({})))
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
        Ok(protocol::scan_persistent(&request, &home, &env))
    });
    match tokio::time::timeout(wait, worker).await {
        Ok(result) => result.map_err(|_| Failure::new(Cause::Bug, "scan worker failed"))?,
        Err(_) => Ok(stalled),
    }
}
