use anyhow::{anyhow, Context, Result};
use std::net::{IpAddr, Ipv4Addr};
use url::Url;

pub fn validate_rpc_url(rpc_url: &str) -> Result<Url> {
    let url: Url = rpc_url
        .parse()
        .with_context(|| format!("parsing RPC URL: {}", rpc_url))?;

    match url.scheme() {
        "http" | "https" => {}
        s => return Err(anyhow!("unsupported RPC URL scheme: {}", s)),
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(anyhow!("RPC URL must not include username/password"));
    }

    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("RPC URL missing host"))?;
    if is_blocked_hostname(host) {
        return Err(anyhow!("RPC URL host is not allowed: {}", host));
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return Err(anyhow!("RPC URL IP is not allowed: {}", ip));
        }
    }

    if let Some(allowlist) = parse_allowlist_env() {
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow!("RPC URL missing port"))?;
        if !allowlist_matches(&allowlist, host, port) {
            return Err(anyhow!("RPC URL host is not in allowlist: {}", host));
        }
    }

    Ok(url)
}

fn parse_allowlist_env() -> Option<Vec<String>> {
    let v = std::env::var("EVM_DEBUGGER_RPC_ALLOWLIST").ok()?;
    let list: Vec<String> = v
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    if list.is_empty() {
        None
    } else {
        Some(list)
    }
}

fn allowlist_matches(allowlist: &[String], host: &str, port: u16) -> bool {
    let h = host.to_ascii_lowercase();
    let hp = format!("{}:{}", h, port);
    allowlist.iter().any(|e| e == &h || e == &hp)
}

fn is_blocked_hostname(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local")
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4 == Ipv4Addr::BROADCAST
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{allowlist_matches, is_blocked_hostname, is_blocked_ip, validate_rpc_url};
    use std::net::IpAddr;

    #[test]
    fn reject_non_http_scheme() {
        let e = validate_rpc_url("file:///etc/passwd").unwrap_err();
        assert!(e.to_string().contains("unsupported"));
    }

    #[test]
    fn reject_localhost_hostname() {
        assert!(is_blocked_hostname("localhost"));
        assert!(validate_rpc_url("http://localhost:8545").is_err());
    }

    #[test]
    fn reject_private_ip() {
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(is_blocked_ip(ip));
        assert!(validate_rpc_url("http://127.0.0.1:8545").is_err());
    }

    #[test]
    fn allow_public_host_without_allowlist() {
        let url = validate_rpc_url("https://example.com").unwrap();
        assert_eq!(url.scheme(), "https");
    }

    #[test]
    fn allowlist_host_match() {
        let allow = vec!["example.com".to_string(), "example.com:1234".to_string()];
        assert!(allowlist_matches(&allow, "example.com", 80));
        assert!(allowlist_matches(&allow, "example.com", 1234));
        assert!(!allowlist_matches(&allow, "evil.com", 80));
    }
}
