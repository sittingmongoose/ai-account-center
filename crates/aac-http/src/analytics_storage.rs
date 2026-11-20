//! Resolve the store in the current owner process for the provider cache callbacks.
use crate::store_host::StoreHost;
use aac_store::analytics_cache;
use aac_views::decline::Decline;
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, Weak},
};

static HOSTS: LazyLock<Mutex<HashMap<PathBuf, Weak<StoreHost>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn identity(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_owned())
}

pub(crate) fn attach(root: &Path, host: &Arc<StoreHost>) {
    let mut hosts = HOSTS.lock().unwrap_or_else(|p| p.into_inner());
    hosts.retain(|_, owner| owner.strong_count() > 0);
    hosts
        .entry(identity(root))
        .or_insert_with(|| Arc::downgrade(host));
}

pub(crate) fn host(root: &Path) -> Option<Arc<StoreHost>> {
    HOSTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&identity(root))
        .and_then(Weak::upgrade)
}

pub(crate) fn read(root: &Path, name: &str) -> Result<Option<Option<Value>>, Decline> {
    let Some(host) = host(root) else {
        return Ok(None);
    };
    let engine = host.analytics_engine().ok_or(Decline::Busy)?;
    let bytes = match analytics_cache::get(&engine, &format!("live/{name}"))
        .map_err(|_| Decline::Internal)?
    {
        Some(bytes) => Some(bytes),
        None => analytics_cache::archived(&engine, &format!("legacy/cache/{name}"))
            .map_err(|_| Decline::Internal)?,
    };
    bytes.map_or(Ok(None), |bytes| {
        serde_json::from_slice::<Value>(&bytes)
            .map(|value| Some((!value.is_null()).then_some(value)))
            .map_err(|_| Decline::Shape)
    })
}

pub(crate) fn write(root: &Path, name: &str, bytes: &[u8]) -> Option<bool> {
    let host = host(root)?;
    Some(host.analytics_engine().is_some_and(|engine| {
        analytics_cache::put(&engine, &format!("live/{name}"), bytes).is_ok()
    }))
}
