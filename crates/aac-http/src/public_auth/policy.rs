//! Request facts derived from the accepted socket, never from an arbitrary forwarded address.
use axum::http::{HeaderMap, Method};
use ipnet::IpNet;
use serde_json::{Value, json};
use std::net::IpAddr;
use url::{Host, Url};

const DEFAULT_NETWORKS: &[&str] = &["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fc00::/7"];
const FORWARDED: &[&str] = &[
    "forwarded",
    "via",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-forwarded-port",
    "x-forwarded-server",
    "x-original-forwarded-for",
    "x-real-ip",
    "x-client-ip",
    "x-cluster-client-ip",
    "cf-connecting-ip",
    "true-client-ip",
    "fastly-client-ip",
];

pub(super) fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn unique(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?.to_str().ok()?;
    values.next().is_none().then(|| first.to_owned())
}

fn normalize(ip: IpAddr) -> IpAddr {
    ip.to_canonical()
}

fn host(headers: &HeaderMap) -> Option<Url> {
    Url::parse(&format!("http://{}", unique(headers, "host")?)).ok()
}

fn loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(value)) => value == "localhost" || value.ends_with(".localhost"),
        Some(Host::Ipv4(value)) => value.is_loopback(),
        Some(Host::Ipv6(value)) => normalize(value.into()).is_loopback(),
        None => false,
    }
}

fn same_host(a: &Url, b: &Url) -> bool {
    a.host() == b.host() && a.port() == b.port()
}

pub(super) fn origin_allowed(headers: &HeaderMap) -> bool {
    if !headers.contains_key("origin") {
        return true;
    }
    let Some(raw) = unique(headers, "origin") else {
        return false;
    };
    if raw.is_empty() {
        return true;
    }
    let (Ok(origin), Some(host)) = (Url::parse(&raw), host(headers)) else {
        return false;
    };
    matches!(origin.scheme(), "http" | "https")
        && (same_host(&origin, &host)
            || (loopback_host(&origin) && loopback_host(&host) && origin.port() == host.port()))
}

pub(super) fn local_access(headers: &HeaderMap, peer: IpAddr) -> bool {
    normalize(peer).is_loopback()
        && host(headers).is_some_and(|url| loopback_host(&url))
        && origin_allowed(headers)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Transport {
    Encrypted,
    Loopback,
    Plain,
}

impl Transport {
    pub(super) fn word(self) -> &'static str {
        match self {
            Self::Encrypted => "encrypted",
            Self::Loopback => "loopback",
            Self::Plain => "plain",
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Network {
    pub(super) trust: bool,
    pub(super) ranges: Vec<IpNet>,
    pub(super) origin: Option<String>,
    proxy: Proxy,
}

#[derive(Clone, Default)]
enum Proxy {
    #[default]
    None,
    Loopback,
    Lan(Vec<IpAddr>),
}

fn parse_range(raw: &str) -> Option<IpNet> {
    if raw.trim() != raw || raw.len() > 64 {
        return None;
    }
    let range = match raw.split_once('/') {
        Some((address, prefix)) => {
            let ip: IpAddr = address.parse().ok()?;
            if prefix.is_empty()
                || prefix.len() > 3
                || !prefix.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            let prefix: u8 = prefix.parse().ok()?;
            match ip {
                IpAddr::V6(ip) if prefix >= 96 && ip.to_ipv4_mapped().is_some() => {
                    IpNet::new(IpAddr::V4(ip.to_ipv4_mapped()?), prefix - 96).ok()?
                }
                _ => IpNet::new(ip, prefix).ok()?,
            }
        }
        None => IpNet::from(normalize(raw.parse().ok()?)),
    }
    .trunc();
    let prefix = range.prefix_len();
    match range.addr() {
        IpAddr::V4(ip) if prefix >= 8 && !ip.is_loopback() => Some(range),
        IpAddr::V6(ip)
            if !ip.is_loopback()
                && ip.to_ipv4_mapped().is_none()
                && (prefix >= 48 || (prefix >= 7 && ip.octets()[0] & 0xfe == 0xfc)) =>
        {
            Some(range)
        }
        _ => None,
    }
}

fn default_ranges() -> impl Iterator<Item = IpNet> {
    DEFAULT_NETWORKS
        .iter()
        .filter_map(|value| parse_range(value))
}

fn lan_proxy(raw: &Value) -> Option<Proxy> {
    let values = raw.as_array()?;
    if !(1..=8).contains(&values.len()) {
        return None;
    }
    // An unknown interface inventory must not confer proxy trust.
    let own = if_addrs::get_if_addrs().ok()?;
    let addresses = values
        .iter()
        .map(|value| {
            let ip = normalize(value.as_str()?.parse().ok()?);
            (!ip.is_unspecified()
                && !ip.is_loopback()
                && default_ranges().any(|range| range.contains(&ip))
                && !own.iter().any(|item| normalize(item.ip()) == ip))
            .then_some(ip)
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Proxy::Lan(addresses))
}

impl Network {
    pub(super) fn from_config(network: &Value, tls: &Value) -> Self {
        let mut ranges = Vec::new();
        match &network["trusted_networks"] {
            Value::Null => ranges.extend(default_ranges()),
            Value::Array(values) => {
                for range in values
                    .iter()
                    .take(64)
                    .filter_map(|value| parse_range(value.as_str()?))
                {
                    if !ranges.contains(&range) {
                        ranges.push(range);
                    }
                }
            }
            Value::String(value) => ranges.extend(parse_range(value)),
            _ => {}
        }
        let origin = tls["public_origin"]
            .as_str()
            .filter(|value| value.len() <= 512)
            .and_then(|value| Url::parse(value).ok())
            .filter(|url| {
                url.scheme() == "https"
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.path() == "/"
                    && url.query().is_none()
                    && url.fragment().is_none()
            })
            .map(|url| url.origin().ascii_serialization());
        let proxy = match tls["trusted_proxy"].as_str() {
            Some("tailscale-serve" | "loopback-https-proxy") => Proxy::Loopback,
            Some("lan-https-proxy") => {
                lan_proxy(&tls["trusted_proxy_addresses"]).unwrap_or_default()
            }
            _ => Proxy::None,
        };
        Self {
            trust: network["trust_local_network"] == true,
            ranges,
            origin,
            proxy,
        }
    }

    pub(super) fn connection(&self, headers: &HeaderMap, peer: IpAddr, tls: bool) -> Connection {
        let peer = normalize(peer);
        let local = peer.is_loopback();
        let lan_proxy = matches!(&self.proxy, Proxy::Lan(addresses) if addresses.contains(&peer));
        let proxy_hop = lan_proxy || (local && matches!(self.proxy, Proxy::Loopback));
        let forwarded = FORWARDED.iter().any(|name| headers.contains_key(*name));
        let client_header = headers
            .get_all("x-forwarded-for")
            .iter()
            .next_back()
            .and_then(|value| value.to_str().ok());
        let via_proxy = proxy_hop && headers.contains_key("x-forwarded-for");
        let client = client_header
            .and_then(|value| value.rsplit(',').next()?.trim().parse::<IpAddr>().ok())
            .map(normalize);
        let proto = unique(headers, "x-forwarded-proto").as_deref() == Some("https");
        let direct_loopback =
            local && !forwarded && host(headers).is_some_and(|url| loopback_host(&url));
        let trusted = self.trust
            && !local
            && !lan_proxy
            && !forwarded
            && self.ranges.iter().any(|range| range.contains(&peer));
        let lan_encrypted = proto && client.is_some();
        let encrypted = tls
            || if lan_proxy {
                lan_encrypted
            } else {
                proxy_hop && proto
            };
        let transport = if encrypted {
            Transport::Encrypted
        } else if direct_loopback {
            Transport::Loopback
        } else {
            Transport::Plain
        };
        Connection {
            peer,
            client,
            lan_proxy,
            via_proxy,
            direct_loopback,
            trusted,
            transport,
            secure: if lan_proxy {
                tls || lan_encrypted
            } else {
                encrypted || trusted || direct_loopback
            },
            lan_encrypted,
        }
    }
}

pub(super) struct Connection {
    pub(super) peer: IpAddr,
    client: Option<IpAddr>,
    pub(super) lan_proxy: bool,
    pub(super) via_proxy: bool,
    pub(super) direct_loopback: bool,
    trusted: bool,
    pub(super) transport: Transport,
    pub(super) secure: bool,
    lan_encrypted: bool,
}

impl Connection {
    pub(super) fn description(&self) -> Value {
        if self.lan_proxy {
            return json!({"peer": self.client.map_or_else(|| "unknown".into(), |ip| ip.to_string()),
                "trusted": false, "proxied": true});
        }
        json!({"peer":self.peer.to_string(),"trusted":self.trusted})
    }

    pub(super) fn client_address(&self) -> Option<String> {
        if self.via_proxy {
            Some(
                self.client
                    .map_or_else(|| "invalid".into(), |ip| ip.to_string()),
            )
        } else {
            Some(self.peer.to_string())
        }
    }

    pub(super) fn credential_allowed(&self, recorded: Option<&str>) -> bool {
        !self.lan_proxy
            || (self.lan_encrypted && matches!(recorded, Some("encrypted" | "loopback")))
    }

    pub(super) fn client_key(&self) -> String {
        let address = if self.via_proxy {
            self.client
        } else {
            Some(self.peer)
        };
        let key = match address {
            Some(IpAddr::V6(ip)) if !ip.is_loopback() => {
                let network = u128::from(ip) & (u128::MAX << 72);
                format!("{}/56", std::net::Ipv6Addr::from(network))
            }
            Some(ip) => ip.to_string(),
            None => "invalid".into(),
        };
        if self.via_proxy {
            format!("proxy:{key}")
        } else {
            key
        }
    }
}

pub(super) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = header(headers, "authorization")?.trim();
    let end = value.find(char::is_whitespace).unwrap_or(value.len());
    value[..end]
        .eq_ignore_ascii_case("bearer")
        .then(|| value[end..].trim())
}

pub(super) fn get(method: &Method) -> bool {
    *method == Method::GET || *method == Method::HEAD
}

pub(super) fn public(method: &Method, path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    (get(method)
        && matches!(
            lower.as_str(),
            "/api/health" | "/api/auth/check" | "/api/auth/setup"
        ))
        || (*method == Method::POST
            && matches!(
                lower.as_str(),
                "/api/auth/login" | "/api/auth/setup" | "/api/auth/devices/pair"
            ))
}

pub(super) fn device_scope(method: &Method, path: &str) -> bool {
    if get(method) {
        if matches!(
            path,
            "/api/accounts/dashboard"
                | "/api/claude/desktop-profiles"
                | "/api/antigravity/profiles"
                | "/api/bar/auth"
                | "/api/auth/devices/me"
                | "/api/store/status"
        ) {
            return true;
        }
        return ["/api/usage/", "/api/cost/"].iter().any(|prefix| {
            path.strip_prefix(prefix).is_some_and(|value| {
                value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                    && value.len() <= 32
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
            })
        });
    }
    if *method == Method::POST {
        return path == "/api/auth/devices/me/rotate"
            || path
                .strip_prefix("/api/claude/desktop-profiles/")
                .and_then(|suffix| suffix.strip_suffix("/open"))
                .is_some_and(|id| !id.is_empty() && !id.contains('/'));
    }
    *method == Method::DELETE && path == "/api/auth/devices/me"
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn origins_allow_loopback_aliases_only_on_the_same_port() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "127.0.0.1:1234".parse().unwrap());
        for (value, expected) in [
            ("http://localhost:1234", true),
            ("https://a.localhost:1234", true),
            ("http://localhost:4444", false),
            ("file://localhost:1234", false),
            ("null", false),
            ("https://other.example:1234", false),
        ] {
            headers.insert("origin", value.parse().unwrap());
            assert_eq!(origin_allowed(&headers), expected, "{value}");
        }
    }

    #[test]
    fn plain_network_trust_does_not_follow_forwarded_headers() {
        let network = Network::from_config(&json!({"trust_local_network":true}), &Value::Null);
        let mut headers = HeaderMap::new();
        headers.insert("host", "aac.test".parse().unwrap());
        let peer = "192.168.0.4".parse().unwrap();
        let direct = network.connection(&headers, peer, false);
        assert!(direct.secure);
        assert_eq!(direct.transport, Transport::Plain);
        headers.insert("x-forwarded-for", "127.0.0.1".parse().unwrap());
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        let forwarded = network.connection(&headers, peer, false);
        assert!(!forwarded.secure);
        assert_eq!(forwarded.client_key(), "192.168.0.4");
    }

    #[test]
    fn trusted_proxy_uses_rightmost_client_and_never_admits_plain_credentials() {
        let network = Network {
            proxy: Proxy::Lan(vec!["192.168.1.9".parse().unwrap()]),
            ..Network::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "127.0.0.1, 192.0.2.46".parse().unwrap());
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        let peer = "192.168.1.9".parse().unwrap();
        let connection = network.connection(&headers, peer, false);
        assert_eq!(connection.client_key(), "proxy:192.0.2.46");
        assert!(connection.secure && connection.credential_allowed(Some("loopback")));
        assert!(!connection.credential_allowed(Some("plain")));
        headers.insert("x-forwarded-proto", "https, http".parse().unwrap());
        assert!(!network.connection(&headers, peer, false).secure);
    }

    #[test]
    fn tray_scope_does_not_expand_with_public_case_matching() {
        assert!(public(&Method::POST, "/API/AUTH/LOGIN"));
        assert!(!public(&Method::DELETE, "/api/auth/devices/pair"));
        assert!(device_scope(&Method::HEAD, "/api/auth/devices/me"));
        assert!(!device_scope(&Method::GET, "/api/Auth/devices/me"));
        assert!(!device_scope(&Method::POST, "/api/accounts/dashboard"));
    }
}
