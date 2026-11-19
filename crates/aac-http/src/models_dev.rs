//! The production models.dev fetch (cut gap G15): the HTTP GET `refreshModelsDevRegistry`
//! performs — a 3-second timeout and `Accept: application/json` — behind the coalesced
//! [`RegistryRefresh`] point, exactly as the TS activity service calls
//! `startModelsDevRegistryRefresh` over its one module cache.
//!
//! Nothing schedules this yet: the TS activity service keeps refreshing until the cut, and two
//! writers would race on the cache file, so G14's Rust collection scheduling wires this into the
//! service at that point. Cross-platform on purpose (the pricing module tests run on Windows too).
use std::path::Path;
use std::time::Duration;

use aac_views::pricing::{Fetched, RegistryRefresh};
use serde_json::{Map, Value};

/// One service-held refresh point: the shared cache plus the coalesced live refresh.
pub struct ModelsDevRefresh {
    refresh: RegistryRefresh,
}

impl ModelsDevRefresh {
    /// A refresh point over `<ccs_dir>/models-dev-registry-cache.json`.
    pub fn new(ccs_dir: &Path) -> Self {
        ModelsDevRefresh { refresh: RegistryRefresh::new(ccs_dir) }
    }

    /// `startModelsDevRegistryRefresh` against the live registry: one fetch per coalesced batch,
    /// a fresh cache short-circuits unless `force`, and any failure keeps the stale cache.
    pub fn refresh(&self, force: bool, now_ms: f64) -> Option<Map<String, Value>> {
        self.refresh_from(aac_views::pricing::MODELS_DEV_API_URL, force, now_ms)
    }

    /// The same refresh against an explicit URL (tests point it at a recorded local server).
    pub fn refresh_from(&self, url: &str, force: bool, now_ms: f64) -> Option<Map<String, Value>> {
        self.refresh.start(force, now_ms, || fetch(url))
    }

    /// The bound cache, for a `Resolver` over the same file.
    pub fn cache(&self) -> &std::sync::Mutex<aac_views::pricing::RegistryCache> {
        self.refresh.cache()
    }
}

/// The live fetch. `Err` is any transport failure (timeout included); the refresh flow discards
/// the reason and answers from the stale cache, exactly like the TS `catch`.
#[allow(clippy::result_unit_err)]
pub fn fetch(url: &str) -> Result<Fetched, ()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(
            aac_views::pricing::LIVE_FETCH_TIMEOUT_MS,
        ))
        .build()
        .map_err(|_| ())?;
    let response = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .map_err(|_| ())?;
    let ok = response.status().is_success();
    let body = response.bytes().map_err(|_| ())?.to_vec();
    Ok(Fetched { ok, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Serve one HTTP request on a loopback port and answer it with `status` and `body`.
    fn one_shot_server(status: &str, body: &str) -> String {
        let status = status.to_owned();
        let body = body.to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        let address = listener.local_addr().expect("local address");
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut request = [0u8; 4096];
            let read = socket.read(&mut request).unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..read]).to_string();
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(reply.as_bytes());
            let _ = socket.flush();
            request
        });
        format!("http://{address}/api.json")
    }

    #[test]
    fn fetch_reads_a_json_registry_over_http() {
        let url = one_shot_server(
            "200 OK",
            r#"{"openai":{"id":"openai","models":{"gpt-5.5":{"id":"gpt-5.5","cost":{"input":5,"output":30}}}}}"#,
        );
        let root = tempfile::tempdir().expect("tempdir");
        let refresh = ModelsDevRefresh::new(root.path());
        let providers = refresh.refresh_from(&url, true, 123.0).expect("providers");
        assert_eq!(providers.len(), 1);
        let cache_bytes =
            std::fs::read(root.path().join("models-dev-registry-cache.json")).expect("cache file");
        assert_eq!(
            String::from_utf8_lossy(&cache_bytes),
            r#"{"version":1,"fetchedAt":123,"providers":{"openai":{"id":"openai","models":{"gpt-5.5":{"id":"gpt-5.5","cost":{"input":5,"output":30}}}}}}"#
        );
    }

    #[test]
    fn a_non_ok_response_keeps_the_stale_cache() {
        let stale_url = one_shot_server(
            "200 OK",
            r#"{"openai":{"id":"openai","models":{"gpt-5.5":{"id":"gpt-5.5","cost":{"input":5,"output":30}}}}}"#,
        );
        let root = tempfile::tempdir().expect("tempdir");
        let refresh = ModelsDevRefresh::new(root.path());
        refresh
            .refresh_from(&stale_url, true, 100.0)
            .expect("stale setup");
        let bad_url = one_shot_server("503 Service Unavailable", "{}");
        let providers = refresh
            .refresh_from(&bad_url, true, 1_000_000_000.0)
            .expect("stale answer");
        assert!(providers.contains_key("openai"));
    }
}
