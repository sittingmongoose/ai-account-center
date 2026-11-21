//! Leptos frame, install assets and immutable build packages.
use super::request_log::RequestFailure;
use super::storage::hex;
use super::{Reply, coded_path, decline, decode_component, header, plain, response, set_header};
use aac_files::read::{Ancestors, Read, ReadPolicy};
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::sync::Semaphore;

const FRAME: &str = "app/index.html";
const MANIFEST: &str = "ui-build-manifest.json";
const ASSET_LIMIT: u64 = 64 * 1024 * 1024;
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

include!(concat!(env!("OUT_DIR"), "/ui-assets.rs"));

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
enum Coding {
    Br,
    Gzip,
}

impl Coding {
    fn name(self) -> &'static str {
        match self {
            Self::Br => "br",
            Self::Gzip => "gzip",
        }
    }
    fn suffix(self) -> &'static str {
        match self {
            Self::Br => ".br",
            Self::Gzip => ".gz",
        }
    }
}

#[derive(Deserialize)]
struct Variant {
    path: String,
    file: String,
    bytes: usize,
    sha256: String,
    encoding: Coding,
}

#[derive(Default, Deserialize)]
struct Manifest {
    #[serde(default)]
    precompressed: Vec<serde_json::Value>,
}

#[derive(Clone)]
pub(super) struct AssetResponse;

struct Asset {
    bytes: axum::body::Bytes,
    modified: Option<SystemTime>,
}

#[derive(Clone)]
pub(super) struct StaticUi {
    root: Option<PathBuf>,
    partial_api: bool,
    variants: Arc<HashMap<String, HashMap<Coding, Variant>>>,
    permits: Arc<Semaphore>,
    admission: Arc<Semaphore>,
}

impl StaticUi {
    pub(super) fn load(root: Option<PathBuf>, partial_api: bool) -> Self {
        let mut ui = Self {
            root: root.or_else(|| {
                if !EMBEDDED_UI.is_empty() {
                    return None;
                }
                let binary = crate::identity::executable_path_for_assets().ok()?;
                let directory = binary.parent()?;
                [directory.join("ui"), directory.join("../../dist/ui")]
                    .into_iter()
                    .find(|root| root.join(FRAME).is_file())
            }),
            partial_api,
            variants: Arc::default(),
            permits: Arc::new(Semaphore::new(4)),
            admission: Arc::new(Semaphore::new(64)),
        };
        let manifest = ui
            .read(MANIFEST, 1024 * 1024)
            .ok()
            .and_then(|asset| serde_json::from_slice::<Manifest>(&asset.bytes).ok())
            .unwrap_or_default();
        let mut checked = HashMap::<String, HashMap<Coding, Variant>>::new();
        let mut originals = HashSet::<String>::new();
        for entry in manifest.precompressed.into_iter().take(4096) {
            let Ok(variant) = serde_json::from_value::<Variant>(entry) else {
                continue;
            };
            if safe_relative(&variant.path).as_deref() != Some(&variant.path)
                || hidden(&variant.path)
                || denied(&variant.path)
                || variant.path.len() > 512
                || variant.file != format!("{}{}", variant.path, variant.encoding.suffix())
                || content_type(&variant.path).is_none()
                || variant.bytes == 0
                || variant.bytes as u64 > ASSET_LIMIT
                || !super::storage::is_hex(&variant.sha256, 64)
                || (!originals.contains(&variant.path)
                    && ui.read(&variant.path, ASSET_LIMIT).is_err())
                || ui.variant(&variant).is_none()
            {
                continue;
            }
            originals.insert(variant.path.clone());
            checked
                .entry(variant.path.clone())
                .or_default()
                .insert(variant.encoding, variant);
        }
        ui.variants = Arc::new(checked);
        ui
    }

    fn read(&self, name: &str, cap: u64) -> Result<Asset, Read> {
        if let Some(root) = &self.root {
            let policy = ReadPolicy::capped(cap).ancestors(Ancestors::NoLinkBelow(root));
            return match aac_files::read::read(&root.join(name), &policy) {
                Read::Bytes(bytes, meta) => Ok(Asset {
                    bytes: bytes.into(),
                    modified: meta.modified().ok().filter(|time| {
                        time.duration_since(SystemTime::UNIX_EPOCH)
                            .is_ok_and(|elapsed| elapsed.as_secs() < 253_402_300_800)
                    }),
                }),
                refused => Err(refused),
            };
        }
        let index = EMBEDDED_UI
            .binary_search_by_key(&name, |asset| asset.0)
            .map_err(|_| Read::Absent)?;
        let bytes = EMBEDDED_UI[index].1;
        if bytes.len() as u64 > cap {
            return Err(Read::TooLarge);
        }
        Ok(Asset {
            bytes: axum::body::Bytes::from_static(bytes),
            modified: None,
        })
    }

    fn variant(&self, variant: &Variant) -> Option<Asset> {
        let asset = self.read(&variant.file, variant.bytes as u64).ok()?;
        (asset.bytes.len() == variant.bytes && hex(&Sha256::digest(&asset.bytes)) == variant.sha256)
            .then_some(asset)
    }

    pub(super) async fn handle(self, request: Request) -> Reply {
        let (parts, _) = request.into_parts();
        if let Some(reply) = api_fallback(parts.uri.path(), self.partial_api) {
            return reply;
        }
        if !matches!(parts.method, Method::GET | Method::HEAD) {
            return unknown_method(&parts.method, parts.uri.path());
        }
        // A cold page and its worker can request many assets together. Keep the
        // blocking read budget while allowing a bounded number to wait briefly.
        let Ok(admitted) = self.admission.clone().try_acquire_owned() else {
            return plain(503, "The dashboard is busy. Try again later.");
        };
        let Ok(Ok(permit)) =
            tokio::time::timeout(Duration::from_secs(5), self.permits.clone().acquire_owned())
                .await
        else {
            return plain(503, "The dashboard is busy. Try again later.");
        };
        tokio::task::spawn_blocking(move || {
            let _admitted = admitted;
            let _permit = permit;
            self.answer(
                parts.uri.path(),
                &parts.headers,
                parts.method == Method::HEAD,
            )
        })
        .await
        .unwrap_or_else(|_| plain(500, "The request could not be completed safely."))
    }

    fn answer(&self, path: &str, headers: &HeaderMap, head: bool) -> Reply {
        let decoded = decode_component(path);
        if decoded.as_ref().is_some_and(|decoded| {
            decoded.split(['/', '\\']).any(|segment| segment == "..")
                || decode_component(decoded).is_some_and(|decoded| {
                    decoded.split(['/', '\\']).any(|segment| segment == "..")
                })
        }) {
            return headless(plain(404, "Not found."), head);
        }
        let relative = icon(path)
            .map(str::to_owned)
            .or_else(|| decoded.as_deref().and_then(safe_relative));
        if let Some(relative) = relative {
            if denied(&relative) {
                return headless(plain(404, "Not found."), head);
            }
            if path.ends_with('/') && !relative.is_empty() || hidden(&relative) {
                // Page dispatch still sees the request's original address spelling.
            } else if let Ok(mut reply) = self.file(&relative, headers, head) {
                reply.extensions_mut().insert(AssetResponse);
                return reply;
            }
        }
        if page(path) {
            return match self.file(FRAME, headers, head) {
                Ok(reply) => reply,
                Err(Read::Directory | Read::Unreadable(std::io::ErrorKind::IsADirectory)) => {
                    let mut reply = response(
                        500,
                        json!({"error":"The request could not be completed safely."}),
                    );
                    reply.extensions_mut().insert(RequestFailure("EISDIR"));
                    headless(reply, head)
                }
                Err(_) => headless(
                    (StatusCode::NOT_FOUND, "This build has no dashboard page.").into_response(),
                    head,
                ),
            };
        }
        let (status, location) = if let Some(alias) = alias(path) {
            (StatusCode::MOVED_PERMANENTLY, alias)
        } else if let Some(short) = path
            .strip_suffix('/')
            .filter(|value| page(value) || alias(value).is_some())
        {
            (StatusCode::MOVED_PERMANENTLY, short)
        } else {
            (StatusCode::FOUND, "/")
        };
        redirect(status, location, headers, head)
    }

    fn file(&self, relative: &str, headers: &HeaderMap, head: bool) -> Result<Reply, Read> {
        let entries = self.variants.get(relative);
        let encoding =
            entries.and_then(|entries| negotiate(header(headers, "accept-encoding")?, entries));
        let compressed = encoding
            .and_then(|coding| entries?.get(&coding))
            .and_then(|variant| Some((self.variant(variant)?, variant)));
        let (asset, coding, etag) = if let Some((asset, variant)) = compressed {
            (
                asset,
                Some(variant.encoding),
                format!("\"{}-{}\"", &variant.sha256[..16], variant.encoding.name()),
            )
        } else {
            let asset = self.read(relative, ASSET_LIMIT)?;
            let etag = match asset
                .modified
                .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            {
                Some(time) => format!("W/\"{:x}-{:x}\"", asset.bytes.len(), time.as_millis()),
                None => format!(
                    "W/\"{:x}-{}\"",
                    asset.bytes.len(),
                    hex(&Sha256::digest(&asset.bytes))
                ),
            };
            (asset, None, etag)
        };
        let modified = asset.modified.filter(|_| coding.is_none());
        let fresh = fresh(headers, &etag, modified);
        let size = asset.bytes.len();
        let (status, bytes, range_header) = if fresh {
            (StatusCode::NOT_MODIFIED, axum::body::Bytes::new(), None)
        } else if coding.is_none() && range_allowed(headers, &etag, modified) {
            match range(header(headers, "range"), size) {
                Range::Slice(start, end) => (
                    StatusCode::PARTIAL_CONTENT,
                    asset.bytes.slice(start..=end),
                    Some(format!("bytes {start}-{end}/{size}")),
                ),
                Range::Unsatisfiable => return Ok(headless(unsatisfiable(size), head)),
                Range::Full => (StatusCode::OK, asset.bytes, None),
            }
        } else {
            (StatusCode::OK, asset.bytes, None)
        };
        let length = bytes.len();
        let mut reply = (
            status,
            if head {
                Body::empty()
            } else {
                Body::from(bytes)
            },
        )
            .into_response();
        set_header(
            &mut reply,
            "cache-control",
            if versioned(relative) {
                IMMUTABLE
            } else {
                "no-cache"
            },
        );
        set_header(&mut reply, "etag", &etag);
        if status != StatusCode::NOT_MODIFIED || coding.is_some() {
            set_header(
                &mut reply,
                "content-type",
                content_type(relative).unwrap_or("application/octet-stream"),
            );
        }
        if let Some(modified) = modified {
            set_header(
                &mut reply,
                "last-modified",
                &httpdate::fmt_http_date(modified),
            );
        }
        if coding.is_none() {
            set_header(&mut reply, "accept-ranges", "bytes");
        }
        if let Some(range) = range_header {
            set_header(&mut reply, "content-range", &range);
        }
        if status != StatusCode::NOT_MODIFIED {
            set_header(&mut reply, "content-length", &length.to_string());
            if let Some(coding) = coding {
                set_header(&mut reply, "content-encoding", coding.name());
            }
        }
        if entries.is_some() {
            set_header(&mut reply, "vary", "Accept-Encoding");
        }
        if relative == FRAME {
            set_header(&mut reply, "x-content-type-options", "nosniff");
            set_header(&mut reply, "referrer-policy", "same-origin");
            set_header(
                &mut reply,
                "content-security-policy",
                "frame-ancestors 'self'",
            );
            set_header(&mut reply, "x-frame-options", "SAMEORIGIN");
        }
        Ok(reply)
    }
}

fn api_fallback(path: &str, partial: bool) -> Option<Reply> {
    let lower = path.to_ascii_lowercase();
    if !super::api_path(&lower) {
        return None;
    }
    // These router trees are still being composed. Keep L's explicit decline for them.
    if partial
        && matches!(
            lower.split('/').nth(2),
            Some(
                "accounts"
                    | "app-updates"
                    | "claude"
                    | "views"
                    | "codex"
                    | "antigravity"
                    | "bar"
                    | "store"
                    | "usage"
                    | "status"
                    | "scoreboard"
                    | "diag"
            )
        )
    {
        return Some(decline());
    }
    Some(if coded_path(path) {
        response(
            404,
            json!({"error":"API endpoint was not found.","code":"not_found"}),
        )
    } else {
        plain(404, "API endpoint was not found.")
    })
}

fn content_type(name: &str) -> Option<&'static str> {
    Some(
        match Path::new(name)
            .extension()?
            .to_str()?
            .to_ascii_lowercase()
            .as_str()
        {
            "wasm" => "application/wasm",
            "js" | "mjs" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "html" => "text/html; charset=utf-8",
            "svg" => "image/svg+xml",
            "json" | "map" => "application/json; charset=utf-8",
            "txt" => "text/plain; charset=utf-8",
            "webmanifest" => "application/manifest+json",
            "png" => "image/png",
            "ico" => "image/vnd.microsoft.icon",
            "webp" => "image/webp",
            "jpg" | "jpeg" => "image/jpeg",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            _ => return None,
        },
    )
}

fn safe_relative(path: &str) -> Option<String> {
    if path.contains(['\\', '\0', ':']) {
        return None;
    }
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .try_fold(String::new(), |mut joined, part| {
            if part == ".." || part.ends_with(['.', ' ']) {
                return None;
            }
            if !joined.is_empty() {
                joined.push('/');
            }
            joined.push_str(part);
            Some(joined)
        })
}

fn hidden(relative: &str) -> bool {
    relative.split('/').any(|part| part.starts_with('.'))
}

fn denied(relative: &str) -> bool {
    let relative = relative.to_ascii_lowercase();
    relative == MANIFEST || relative.ends_with(".br") || relative.ends_with(".gz")
}

fn versioned(relative: &str) -> bool {
    relative
        .strip_prefix("pkg/")
        .and_then(|tail| tail.split_once('/'))
        .is_some_and(|(hash, _)| {
            super::storage::is_hex(hash, 12) && !hash.bytes().any(|byte| byte.is_ascii_uppercase())
        })
}

fn icon(path: &str) -> Option<&'static str> {
    matches!(
        path,
        "/favicon.ico"
            | "/apple-touch-icon.png"
            | "/apple-touch-icon-precomposed.png"
            | "/apple-touch-icon-180x180.png"
            | "/apple-touch-icon-180x180-precomposed.png"
            | "/apple-touch-icon-120x120.png"
            | "/apple-touch-icon-120x120-precomposed.png"
            | "/apple-touch-icon-152x152.png"
            | "/apple-touch-icon-152x152-precomposed.png"
            | "/apple-touch-icon-167x167.png"
            | "/apple-touch-icon-167x167-precomposed.png"
            | "/apple-touch-icon-180.png"
    )
    .then_some("icons/apple-touch-icon-180-v2.png")
}

fn page(path: &str) -> bool {
    matches!(
        path,
        "/" | "/login"
            | "/analytics"
            | "/accounts"
            | "/status"
            | "/scoreboard"
            | "/app/home"
            | "/app/analytics"
            | "/app/accounts"
            | "/app/status"
            | "/app/scoreboard"
    ) || path
        .strip_prefix("/accounts/")
        .is_some_and(aac_providers::is_dashboard_provider)
}

fn alias(path: &str) -> Option<&'static str> {
    match path {
        "/settings" => Some("/accounts"),
        "/home" => Some("/"),
        _ => None,
    }
}

fn quality(parameters: impl Iterator<Item = impl AsRef<str>>) -> u16 {
    let mut quality = 1000;
    for parameter in parameters {
        let parameter = parameter.as_ref().trim();
        let (name, raw) = parameter.split_once('=').unwrap_or((parameter, ""));
        if !name.trim().eq_ignore_ascii_case("q") {
            continue;
        }
        let (whole, fraction) = raw.trim().split_once('.').unwrap_or((raw.trim(), ""));
        quality = if fraction.len() <= 3 && fraction.bytes().all(|ch| ch.is_ascii_digit()) {
            match whole {
                "0" => {
                    fraction.parse::<u16>().unwrap_or(0) * 10_u16.pow((3 - fraction.len()) as u32)
                }
                "1" if fraction.bytes().all(|ch| ch == b'0') => 1000,
                _ => 0,
            }
        } else {
            0
        };
    }
    quality
}

fn negotiate(raw: &str, entries: &HashMap<Coding, Variant>) -> Option<Coding> {
    if raw.len() > 2048 {
        return None;
    }
    let mut qualities = HashMap::new();
    for part in raw.split(',') {
        let mut parts = part.split(';');
        let coding = parts.next()?.trim().to_ascii_lowercase();
        qualities.entry(coding).or_insert_with(|| quality(parts));
    }
    let mut selected = None;
    let mut best = 0;
    for coding in [Coding::Br, Coding::Gzip] {
        let score = qualities
            .get(coding.name())
            .or_else(|| {
                (coding == Coding::Gzip)
                    .then(|| qualities.get("x-gzip"))
                    .flatten()
            })
            .or_else(|| qualities.get("*"))
            .copied()
            .unwrap_or(0);
        if entries.contains_key(&coding) && score > best {
            selected = Some(coding);
            best = score;
        }
    }
    selected
}

fn weak_etag(value: &str) -> &str {
    value.trim().strip_prefix("W/").unwrap_or(value.trim())
}

pub(super) fn fresh(headers: &HeaderMap, etag: &str, modified: Option<SystemTime>) -> bool {
    if header(headers, "cache-control").is_some_and(|value| {
        value
            .split(',')
            .any(|directive| directive.trim().eq_ignore_ascii_case("no-cache"))
    }) {
        return false;
    }
    if let Some(value) = header(headers, "if-none-match") {
        return value
            .split(',')
            .any(|candidate| candidate.trim() == "*" || weak_etag(candidate) == weak_etag(etag));
    }
    modified
        .zip(
            header(headers, "if-modified-since")
                .and_then(|time| httpdate::parse_http_date(time).ok()),
        )
        .is_some_and(|(modified, since)| {
            httpdate::HttpDate::from(modified) <= httpdate::HttpDate::from(since)
        })
}

fn range_allowed(headers: &HeaderMap, etag: &str, modified: Option<SystemTime>) -> bool {
    header(headers, "if-range").is_none_or(|value| {
        if value.contains('"') {
            !etag.starts_with("W/") && value == etag
        } else {
            modified
                .zip(httpdate::parse_http_date(value).ok())
                .is_some_and(|(modified, since)| {
                    httpdate::HttpDate::from(modified) <= httpdate::HttpDate::from(since)
                })
        }
    })
}

enum Range {
    Full,
    Unsatisfiable,
    Slice(usize, usize),
}

fn range(header: Option<&str>, size: usize) -> Range {
    let Some((start, end)) = header
        .and_then(|raw| raw.strip_prefix("bytes="))
        .filter(|value| !value.contains(','))
        .and_then(|value| value.trim().split_once('-'))
    else {
        return Range::Full;
    };
    let bounds = if start.is_empty() {
        end.parse::<usize>()
            .ok()
            .filter(|suffix| *suffix > 0)
            .map(|suffix| (size.saturating_sub(suffix), size.saturating_sub(1)))
    } else if let Ok(start) = start.parse() {
        if end.is_empty() {
            Some((start, size.saturating_sub(1)))
        } else {
            end.parse::<usize>()
                .ok()
                .map(|end| (start, end.min(size.saturating_sub(1))))
        }
    } else {
        None
    };
    if let Some((start, end)) = bounds.filter(|(start, end)| start <= end && *start < size) {
        Range::Slice(start, end)
    } else {
        Range::Unsatisfiable
    }
}

fn unsatisfiable(size: usize) -> Reply {
    let mut reply = plain(416, "The request could not be processed.");
    set_header(&mut reply, "content-range", &format!("bytes */{size}"));
    reply
}

fn headless(mut reply: Reply, head: bool) -> Reply {
    if head {
        *reply.body_mut() = Body::empty();
    }
    reply
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn redirect(status: StatusCode, location: &str, headers: &HeaderMap, head: bool) -> Reply {
    let reason = status.canonical_reason().unwrap_or("Found");
    let accept = header(headers, "accept").unwrap_or("*/*");
    let mut choices = [(0, 0, usize::MAX); 2];
    for (index, choice) in accept.split(',').enumerate() {
        let mut parts = choice.split(';');
        let name = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
        let quality = quality(parts);
        for (kind, choice) in ["text/plain", "text/html"].into_iter().zip(&mut choices) {
            let specificity = match name.as_str() {
                "*/*" => 1,
                "text/*" => 2,
                name if name == kind => 3,
                _ => 0,
            };
            if specificity > choice.0 {
                *choice = (specificity, quality, index);
            }
        }
    }
    let [plain, html] = choices;
    let (kind, body) = if html.1 > 0
        && (html.1, html.0, std::cmp::Reverse(html.2))
            > (plain.1, plain.0, std::cmp::Reverse(plain.2))
    {
        let link = escape_html(location);
        (
            "text/html; charset=utf-8",
            format!("<p>{reason}. Redirecting to {link}</p>"),
        )
    } else if plain.1 > 0 {
        (
            "text/plain; charset=utf-8",
            format!("{reason}. Redirecting to {location}"),
        )
    } else {
        ("", String::new())
    };
    let length = body.len();
    let mut reply = (status, if head { String::new() } else { body }).into_response();
    if kind.is_empty() {
        reply.headers_mut().remove("content-type");
    } else {
        set_header(&mut reply, "content-type", kind);
    }
    set_header(&mut reply, "content-length", &length.to_string());
    set_header(&mut reply, "location", location);
    set_header(&mut reply, "vary", "Accept");
    reply
}

fn unknown_method(method: &Method, path: &str) -> Reply {
    let path = escape_html(path);
    let mut reply = (StatusCode::NOT_FOUND, format!("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Error</title>\n</head>\n<body>\n<pre>Cannot {method} {path}</pre>\n</body>\n</html>\n")).into_response();
    set_header(&mut reply, "content-type", "text/html; charset=utf-8");
    set_header(&mut reply, "content-security-policy", "default-src 'none'");
    set_header(&mut reply, "x-content-type-options", "nosniff");
    reply
}
