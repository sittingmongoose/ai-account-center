//! History-only Kimi Code usage from OMP session logs.
//!
//! Jared used Kimi only through OMP. These logs are ordinary OMP JSONL (`type=message`,
//! assistant `usage`); the model or route names Kimi Code, so the row is attributed to
//! `kimi-code` instead of OMP. Keep in sync with `aac_store::collect::is_kimi_code_usage`
//! and `aac_views::analytics_activity`.

/// Dashboard route written onto a Kimi Code row.
pub const KIMI_CODE_ROUTE: &str = "kimi-code";

/// True when an OMP (or zcode) usage row is Kimi Code: the logged route, or the model id.
pub fn is_kimi_code_usage(model: Option<&str>, route: Option<&str>) -> bool {
    let route = route.unwrap_or("").trim().to_ascii_lowercase();
    let route = route.strip_prefix("builtin:").unwrap_or(&route);
    if matches!(route, "kimi-code" | "kimi") {
        return true;
    }
    let model = model.unwrap_or("").trim().to_ascii_lowercase();
    let name = model.rsplit('/').next().unwrap_or(&model);
    name == "k3" || name == "kimi-k3" || name == "kimi-for-coding" || name == "kimi-code" || name.starts_with("kimi-")
}
