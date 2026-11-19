//! The dashboard_tls parser for the proxy CLI, ported from
//! src/web-server/services/dashboard-tls-config.ts and middleware/trusted-networks.ts. The
//! request-time policy (public_auth/policy.rs) applies the same rules; this module keeps the
//! CLI's refusal reasons and canonical spellings. Only the fields the CLI reads are ported
//! (the https_listener block is not used by any proxy subcommand).

use ipnet::IpNet;
use serde_json::Value;
use std::net::IpAddr;

pub const ADDRESSES_MAX: usize = 8;
const LAN_KIND: &str = "lan-https-proxy";
const DEFAULT_NETWORKS: &[&str] = &["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fc00::/7"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProblemReason {
    NotAList,
    Empty,
    TooMany,
    NotAnAddress,
    Unspecified,
    Loopback,
    NotPrivate,
    ThisComputer,
}

#[derive(Clone, Copy, Debug)]
pub struct Problem {
    pub reason: ProblemReason,
    /// The 0-based position of the refused entry, when one entry was refused.
    pub index: Option<usize>,
}

fn problem_text(reason: ProblemReason) -> String {
    match reason {
        ProblemReason::NotAList => "trusted_proxy_addresses is not a list".into(),
        ProblemReason::Empty => "trusted_proxy_addresses is empty".into(),
        ProblemReason::TooMany => {
            format!("trusted_proxy_addresses has more than {ADDRESSES_MAX} entries")
        }
        ProblemReason::NotAnAddress => {
            "an entry is not one exact IP address (no ranges, names or ports)".into()
        }
        ProblemReason::Unspecified => {
            "an entry is the unspecified address (0.0.0.0 or ::)".into()
        }
        ProblemReason::Loopback => "an entry is a loopback address (this computer)".into(),
        ProblemReason::NotPrivate => {
            "an entry is not a private LAN address (10/8, 172.16/12, 192.168/16, fc00::/7)".into()
        }
        ProblemReason::ThisComputer => {
            "an entry is one of this computer's own addresses".into()
        }
    }
}

/// One plain sentence for a refused proxy list (the status command and the log).
pub fn describe(problem: &Problem) -> String {
    match problem.index {
        Some(index) => format!("{} (entry {})", problem_text(problem.reason), index + 1),
        None => problem_text(problem.reason),
    }
}

/// Parse an address as a socket reports it: brackets and a zone are dropped, an IPv4-mapped
/// IPv6 address becomes its IPv4 address. Null when it is not an IP address.
pub fn parse_address(value: &str) -> Option<IpAddr> {
    let mut text = value.trim();
    if text.starts_with('[') && text.ends_with(']') {
        text = &text[1..text.len() - 1];
    }
    if let Some(zone) = text.find('%') {
        text = &text[..zone];
    }
    text.parse::<IpAddr>().ok().map(|ip| ip.to_canonical())
}

/// Every address of this computer's interfaces, loopback included.
pub fn this_computer_addresses() -> Vec<String> {
    if_addrs::get_if_addrs()
        .map(|items| items.iter().map(|item| item.ip().to_string()).collect())
        .unwrap_or_default()
}

fn private_networks() -> Vec<IpNet> {
    DEFAULT_NETWORKS
        .iter()
        .filter_map(|text| text.parse::<IpNet>().ok())
        .collect()
}

/// `dashboard_tls.trusted_proxy_addresses`: 1 to 8 exact private LAN addresses, not loopback,
/// not unspecified, not this computer's own; duplicates dropped, canonical spellings kept in
/// first-seen order. One refused entry refuses the whole list (the kind fails closed).
pub fn parse_trusted_proxy_addresses(
    raw: &Value,
    self_addresses: &[String],
) -> Result<Vec<String>, Problem> {
    let refuse = |reason: ProblemReason, index: Option<usize>| Problem { reason, index };
    let Some(items) = raw.as_array() else {
        return Err(refuse(ProblemReason::NotAList, None));
    };
    if items.is_empty() {
        return Err(refuse(ProblemReason::Empty, None));
    }
    if items.len() > ADDRESSES_MAX {
        return Err(refuse(ProblemReason::TooMany, None));
    }
    let own: Vec<IpAddr> = self_addresses
        .iter()
        .filter_map(|value| parse_address(value))
        .collect();
    let networks = private_networks();
    let mut addresses: Vec<String> = Vec::new();
    for (index, entry) in items.iter().enumerate() {
        let text = match entry.as_str() {
            Some(text) if !text.is_empty() && text.len() <= 64 && !text.contains('%') => text,
            _ => return Err(refuse(ProblemReason::NotAnAddress, Some(index))),
        };
        // net.isIP first: no trimming, brackets or zones are admitted for an entry.
        let Ok(ip) = text.parse::<IpAddr>() else {
            return Err(refuse(ProblemReason::NotAnAddress, Some(index)));
        };
        let ip = ip.to_canonical();
        if ip.is_unspecified() {
            return Err(refuse(ProblemReason::Unspecified, Some(index)));
        }
        if ip.is_loopback() {
            return Err(refuse(ProblemReason::Loopback, Some(index)));
        }
        if !networks.iter().any(|range| range.contains(&ip)) {
            return Err(refuse(ProblemReason::NotPrivate, Some(index)));
        }
        let canonical = ip.to_string();
        if own.iter().any(|address| address.to_string() == canonical) {
            return Err(refuse(ProblemReason::ThisComputer, Some(index)));
        }
        if !addresses.contains(&canonical) {
            addresses.push(canonical);
        }
    }
    Ok(addresses)
}

/// An `https://` origin (scheme, host and optional port only), or null.
pub fn parse_public_origin(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    if text.len() > 512 {
        return None;
    }
    let url = url::Url::parse(text).ok()?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if (url.path() != "/" && url.path() != "") || url.query().is_some() || url.fragment().is_some()
    {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub trusted_proxy: Option<String>,
    pub trusted_proxy_addresses: Vec<String>,
    pub trusted_proxy_problem: Option<Problem>,
    pub public_origin: Option<String>,
}

/// `dashboard_tls`, read-only and normalised; a malformed value is treated as absent.
pub fn parse_settings(raw: &Value, self_addresses: &[String]) -> Settings {
    let Some(block) = raw.as_object() else {
        return Settings::default();
    };
    let mut settings = Settings {
        public_origin: parse_public_origin(block.get("public_origin").unwrap_or(&Value::Null)),
        ..Settings::default()
    };
    let kind = block
        .get("trusted_proxy")
        .and_then(Value::as_str)
        .filter(|kind| {
            matches!(
                *kind,
                "tailscale-serve" | "loopback-https-proxy" | "lan-https-proxy"
            )
        });
    let Some(kind) = kind else {
        return settings;
    };
    if kind == LAN_KIND {
        match parse_trusted_proxy_addresses(
            block.get("trusted_proxy_addresses").unwrap_or(&Value::Null),
            self_addresses,
        ) {
            Ok(addresses) => {
                settings.trusted_proxy = Some(kind.into());
                settings.trusted_proxy_addresses = addresses;
            }
            Err(problem) => settings.trusted_proxy_problem = Some(problem),
        }
    } else {
        settings.trusted_proxy = Some(kind.into());
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn addresses(values: &[&str]) -> Value {
        Value::Array(values.iter().map(|v| json!(v)).collect())
    }

    #[test]
    fn parse_address_normalizes_brackets_zones_and_mapped_forms() {
        assert_eq!(parse_address("[192.168.1.5]").unwrap().to_string(), "192.168.1.5");
        assert_eq!(parse_address("fe80::1%en0").unwrap().to_string(), "fe80::1");
        assert_eq!(parse_address("::ffff:192.168.1.5").unwrap().to_string(), "192.168.1.5");
        assert_eq!(parse_address("::FFFF:0a00:5").unwrap().to_string(), "10.0.0.5");
        assert_eq!(parse_address(" 10.0.0.5 ").unwrap().to_string(), "10.0.0.5");
        assert!(parse_address("10.0.0.5:80").is_none());
        assert!(parse_address("10.0.0.0/8").is_none());
        assert!(parse_address("010.0.0.5").is_none());
        assert!(parse_address("name.test").is_none());
    }

    #[test]
    fn proxy_list_refuses_each_bad_shape_with_its_reason() {
        let own = vec!["192.168.1.10".to_string()];
        let case = |raw: Value, reason: ProblemReason, index: Option<usize>| {
            let problem = parse_trusted_proxy_addresses(&raw, &own).unwrap_err();
            assert_eq!(problem.reason, reason, "{raw}");
            assert_eq!(problem.index, index, "{raw}");
        };
        case(json!("192.168.1.20"), ProblemReason::NotAList, None);
        case(json!([]), ProblemReason::Empty, None);
        case(
            addresses(&["192.168.1.1"; 9]),
            ProblemReason::TooMany,
            None,
        );
        case(addresses(&["192.168.1.20", "name.test"]), ProblemReason::NotAnAddress, Some(1));
        case(addresses(&["192.168.1.20", "10.0.0.5:80"]), ProblemReason::NotAnAddress, Some(1));
        case(addresses(&["192.168.1.20", "fe80::1%en0"]), ProblemReason::NotAnAddress, Some(1));
        case(addresses(&["0.0.0.0"]), ProblemReason::Unspecified, Some(0));
        case(addresses(&["::"]), ProblemReason::Unspecified, Some(0));
        case(addresses(&["127.0.0.1"]), ProblemReason::Loopback, Some(0));
        case(addresses(&["::1"]), ProblemReason::Loopback, Some(0));
        case(addresses(&["8.8.8.8"]), ProblemReason::NotPrivate, Some(0));
        case(addresses(&["192.168.1.10"]), ProblemReason::ThisComputer, Some(0));
        assert_eq!(
            describe(&Problem { reason: ProblemReason::NotPrivate, index: Some(1) }),
            "an entry is not a private LAN address (10/8, 172.16/12, 192.168/16, fc00::/7) (entry 2)"
        );
    }

    #[test]
    fn proxy_list_canonicalizes_and_deduplicates_in_order() {
        let own = vec!["192.168.1.10".to_string()];
        let list = parse_trusted_proxy_addresses(
            &addresses(&["::ffff:10.0.0.5", "10.0.0.5", "fd00::1", "172.16.2.3"]),
            &own,
        )
        .unwrap();
        assert_eq!(list, vec!["10.0.0.5", "fd00::1", "172.16.2.3"]);
    }

    #[test]
    fn public_origin_admits_only_https_origins() {
        assert_eq!(
            parse_public_origin(&json!("https://aac.example.test")).as_deref(),
            Some("https://aac.example.test")
        );
        assert_eq!(
            parse_public_origin(&json!("https://aac.example.test:8443/")).as_deref(),
            Some("https://aac.example.test:8443")
        );
        assert_eq!(
            parse_public_origin(&json!("https://AAC.Example.Test")).as_deref(),
            Some("https://aac.example.test")
        );
        for bad in [
            "http://aac.example.test",
            "https://user:pass@aac.example.test",
            "https://aac.example.test/path",
            "https://aac.example.test/?q=1",
            "https://aac.example.test#frag",
            "not a url",
        ] {
            assert!(parse_public_origin(&json!(bad)).is_none(), "{bad}");
        }
        assert!(parse_public_origin(&json!(format!("https://{}.test", "a".repeat(512)))).is_none());
    }

    #[test]
    fn settings_fail_closed_on_a_bad_lan_list() {
        let own: Vec<String> = vec![];
        let settings = parse_settings(
            &json!({"trusted_proxy": "lan-https-proxy", "trusted_proxy_addresses": ["8.8.8.8"]}),
            &own,
        );
        assert!(settings.trusted_proxy.is_none());
        assert_eq!(
            settings.trusted_proxy_problem.unwrap().reason,
            ProblemReason::NotPrivate
        );
        let settings = parse_settings(
            &json!({"trusted_proxy": "lan-https-proxy", "trusted_proxy_addresses": ["10.0.0.5"], "public_origin": "https://aac.example.test"}),
            &own,
        );
        assert_eq!(settings.trusted_proxy.as_deref(), Some("lan-https-proxy"));
        assert_eq!(settings.trusted_proxy_addresses, vec!["10.0.0.5"]);
        assert_eq!(settings.public_origin.as_deref(), Some("https://aac.example.test"));
        let settings = parse_settings(&json!({"trusted_proxy": "tailscale-serve"}), &own);
        assert_eq!(settings.trusted_proxy.as_deref(), Some("tailscale-serve"));
        let settings = parse_settings(&json!({"trusted_proxy": "unknown-kind"}), &own);
        assert!(settings.trusted_proxy.is_none());
        let settings = parse_settings(&json!(null), &own);
        assert!(settings.trusted_proxy.is_none());
        let settings = parse_settings(&json!([1, 2]), &own);
        assert!(settings.trusted_proxy.is_none());
    }
}
