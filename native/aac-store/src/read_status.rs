//! Read-route admission of a snapshot from the existing store writer. This module never opens
//! an Engine, redb, log or lock. Transient writer state cannot be reconstructed by another
//! process; the front door supplies one immutable result of the sole writer's `status` call.
use serde_json::Value;

/// Admit the existing writer's status shape. Unsupported versions/shapes require TS fallback.
/// No defaults, quota readings or health findings are synthesized here.
pub fn from_writer(snapshot: &Value) -> Option<Value> {
    let object = snapshot.as_object()?;
    if object.get("state")?.as_str()? == "unavailable" {
        let reason = object.get("reason")?.as_str()?;
        return (object.len() == 2
            && [
                "disabled",
                "missing",
                "incompatible",
                "load_failed",
                "lock_held",
                "db_unavailable",
            ]
            .contains(&reason))
        .then(|| snapshot.clone());
    }
    if !matches!(object.get("state")?.as_str()?, "ready" | "degraded")
        || object.get("storeSchemaVersion")?.as_u64()? != crate::db::STORE_SCHEMA_VERSION
        || object.get("instanceId")?.as_str()?.len() != 32
    {
        return None;
    }
    for field in ["openedAt", "cacheBytes", "retentionDays", "redbBytes"] {
        if object.get(field)?.as_u64()? > 9_007_199_254_740_991 {
            return None;
        }
    }
    for field in ["logs", "checkpoints", "meta", "redbStats", "capture", "storageHealth"] {
        object.get(field)?.as_object()?;
    }
    object.get("findings")?.as_array()?;
    Some(snapshot.clone())
}
