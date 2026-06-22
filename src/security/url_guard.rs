//! SSRF guard for HTTP capability URL patterns.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

use crate::error::{NexusError, Result};

/// Validate an HTTP capability URL pattern before it can be granted.
pub fn validate_http_capability_pattern(pattern: &str) -> Result<()> {
    let parse_target = pattern.strip_suffix("/*").unwrap_or(pattern);
    let url = Url::parse(parse_target).map_err(|error| {
        NexusError::InvalidCapability(format!(
            "invalid HTTP capability URL '{}': {}",
            pattern, error
        ))
    })?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(NexusError::InvalidCapability(format!(
            "HTTP capability URL '{}' must use http or https scheme",
            pattern
        )));
    }

    validate_resolved_url(&url).map_err(|_| restricted_pattern_error(pattern))
}

/// Validate the resolved URL host against SSRF-restricted address ranges.
pub fn validate_resolved_url(url: &Url) -> Result<()> {
    match url.host() {
        Some(Host::Ipv4(ip)) => {
            if is_blocked_ip(&IpAddr::V4(ip)) {
                Err(restricted_url_error(url))
            } else {
                Ok(())
            }
        }
        Some(Host::Ipv6(ip)) => {
            if is_blocked_v6(&ip) {
                Err(restricted_url_error(url))
            } else {
                Ok(())
            }
        }
        Some(Host::Domain(name)) => {
            let host = name.trim_end_matches('.').to_ascii_lowercase();
            if host.parse::<IpAddr>().is_ok_and(|ip| is_blocked_ip(&ip))
                || is_blocked_hostname(&host)
            {
                Err(restricted_url_error(url))
            } else {
                Ok(())
            }
        }
        None => Err(NexusError::InvalidCapability(format!(
            "HTTP URL '{}' does not contain a host",
            url
        ))),
    }
}

/// Return true when an IP address is not safe for operator-granted HTTP access.
pub fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_blocked_v4(ip),
        IpAddr::V6(ip) => is_blocked_v6(ip),
    }
}

fn is_blocked_v4(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || (octets[0] == 100 && octets[1] & 0xC0 == 64)
        || (octets[0] == 198 && octets[1] & 0xFE == 18)
        || octets[0] >= 240
}

fn is_blocked_v6(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();
    ip.is_loopback()
        || ip.is_multicast()
        || ip.is_unspecified()
        || segments[0] & 0xFFC0 == 0xFE80
        || segments[0] & 0xFE00 == 0xFC00
        || ip
            .to_ipv4_mapped()
            .or_else(|| ip.to_ipv4())
            .is_some_and(|mapped| is_blocked_ip(&IpAddr::V4(mapped)))
}

fn is_blocked_hostname(host: &str) -> bool {
    matches!(
        host,
        "localhost" | "metadata.google.internal" | "metadata.goog" | "metadata"
    ) || host.starts_with("localhost.")
}

fn restricted_pattern_error(pattern: &str) -> NexusError {
    NexusError::InvalidCapability(format!(
        "HTTP capability URL '{}' targets a restricted host or address",
        pattern
    ))
}

fn restricted_url_error(url: &Url) -> NexusError {
    NexusError::InvalidCapability(format!(
        "HTTP URL '{}' targets a restricted host or address",
        url
    ))
}

// SSRF-TODO(redeem-time): wire validate_resolved_url + is_blocked_ip into the HTTP execution path when wasmtime-wasi-http is added.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_restricted_http_patterns() {
        let blocked = [
            "http://169.254.169.254/latest/meta-data/*",
            "http://metadata.google.internal/computeMetadata/v1/*",
            "http://metadata.goog/computeMetadata/v1/",
            "http://metadata/",
            "http://10.0.0.1/*",
            "http://172.16.0.1/",
            "http://192.168.1.1/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://localhost/",
            "http://LOCALHOST/",
            "http://localhost./",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:169.254.169.254]/",
            "http://169.254.1.1/",
            "http://[fe80::1]/",
            "http://100.64.0.1/",
            "http://224.0.0.1/",
            "http://[ff02::1]/",
            "http://0.0.0.0/",
            "http://[::]/",
            "http://example.com@169.254.169.254/latest/meta-data/",
        ];

        for pattern in blocked {
            assert!(
                validate_http_capability_pattern(pattern).is_err(),
                "expected restricted URL pattern to be rejected: {pattern}"
            );
        }
    }

    #[test]
    fn rejects_bad_schemes_and_missing_hosts() {
        let invalid = [
            "file:///etc/passwd",
            "gopher://example.com/",
            "ftp://example.com/",
        ];

        for pattern in invalid {
            assert!(
                validate_http_capability_pattern(pattern).is_err(),
                "expected invalid URL pattern to be rejected: {pattern}"
            );
        }

        let url_without_host = Url::parse("file:///etc/passwd").unwrap();
        assert!(validate_resolved_url(&url_without_host).is_err());
    }

    #[test]
    fn accepts_public_http_patterns() {
        let allowed = [
            "https://api.openai.com/v1/*",
            "https://example.com/",
            "http://93.184.216.34/",
        ];

        for pattern in allowed {
            validate_http_capability_pattern(pattern).unwrap_or_else(|error| {
                panic!("expected URL pattern to be accepted: {pattern}: {error}")
            });
        }
    }

    #[test]
    fn detects_blocked_ip_ranges_directly() {
        let blocked = [
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            IpAddr::V4(Ipv4Addr::new(198, 18, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6("fc00::1".parse().unwrap()),
            IpAddr::V6("::ffff:127.0.0.1".parse().unwrap()),
        ];

        for ip in blocked {
            assert!(is_blocked_ip(&ip), "expected IP to be blocked: {ip}");
        }

        assert!(!is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))));
    }
}
