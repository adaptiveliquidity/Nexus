//! Configurable SSRF and egress policy for outbound AEON requests.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use url::Url;

use crate::error::{NexusError, Result};
use crate::security::url_guard::is_blocked_ip;

const EGRESS_ALLOWLIST_ENV: &str = "NEXUS_EGRESS_ALLOWLIST";
const EGRESS_ALLOW_PRIVATE_ENV: &str = "NEXUS_EGRESS_ALLOW_PRIVATE";

/// Policy that gates outbound HTTP(S) destinations.
#[derive(Debug, Clone, Default)]
pub struct EgressPolicy {
    /// Host names that are always allowed even when they resolve into blocked ranges.
    pub allow_hosts: HashSet<String>,
    /// Whether private/loopback/link-local/shared ranges are permitted.
    pub allow_private: bool,
}

impl EgressPolicy {
    /// Build policy from environment variables.
    ///
    /// `NEXUS_EGRESS_ALLOWLIST` is parsed as a comma-separated list of host
    /// names and is automatically trimmed and lowercased.
    ///
    /// `NEXUS_EGRESS_ALLOW_PRIVATE` accepts:
    /// - `1`
    /// - `true` (case-insensitive)
    ///   Any other value is rejected.
    pub fn from_env() -> Result<Self> {
        let allow_hosts = match std::env::var(EGRESS_ALLOWLIST_ENV) {
            Ok(value) => value
                .split(',')
                .map(|host| host.trim().to_ascii_lowercase())
                .filter(|host| !host.is_empty())
                .collect(),
            Err(std::env::VarError::NotPresent) => HashSet::new(),
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(NexusError::ConfigError(format!(
                    "{EGRESS_ALLOWLIST_ENV} must be valid Unicode"
                )));
            }
        };

        let allow_private = match std::env::var(EGRESS_ALLOW_PRIVATE_ENV) {
            Ok(value) => {
                let normalized = value.trim().to_ascii_lowercase();
                match normalized.as_str() {
                    "" | "0" | "false" => false,
                    "1" | "true" => true,
                    _ => {
                        return Err(NexusError::ConfigError(
                            "NEXUS_EGRESS_ALLOW_PRIVATE must be one of: 0, 1, true, false".to_string(),
                        ));
                    }
                }
            }
            Err(std::env::VarError::NotPresent) => false,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(NexusError::ConfigError(format!(
                    "{EGRESS_ALLOW_PRIVATE_ENV} must be valid Unicode"
                )));
            }
        };

        Ok(Self {
            allow_hosts,
            allow_private,
        })
    }

    /// Add a host to the explicit allowlist.
    pub fn allow_host(&mut self, host: &str) {
        let host = host.trim().to_ascii_lowercase();
        if !host.is_empty() {
            self.allow_hosts.insert(host);
        }
    }

    /// Check whether a URL may be contacted from this runtime.
    pub fn check_url(&self, url: &Url) -> Result<()> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(NexusError::EgressDenied(format!(
                "URL scheme '{}' is not allowed",
                url.scheme()
            )));
        }

        let host = match url.host_str() {
            Some(host) => host,
            None => {
                return Err(NexusError::EgressDenied("URL does not include a host".to_string()));
            }
        };
        let host = host.trim().to_ascii_lowercase();
        if self.allow_hosts.contains(&host) {
            return Ok(());
        }
        let socket_host = host.trim_start_matches('[').trim_end_matches(']');

        let port = url
            .port_or_known_default()
            .ok_or_else(|| NexusError::EgressDenied(format!("URL '{}' has no known port", url)))?;
        let addresses: Vec<SocketAddr> = if let Ok(ip) = socket_host.parse::<IpAddr>() {
            vec![SocketAddr::new(ip, port)]
        } else {
            match (socket_host, port).to_socket_addrs() {
                Ok(addresses) => addresses.collect(),
                Err(error) => {
                    return Err(NexusError::EgressDenied(format!(
                        "Failed to resolve '{}' for AEON egress: {error}",
                        url
                    )));
                }
            }
        };

        if addresses.is_empty() {
            return Err(NexusError::EgressDenied(format!(
                "URL '{}' did not resolve to any IP address",
                url
            )));
        }

        if addresses.into_iter().any(|address| self.is_blocked_address(address.ip())) {
            return Err(NexusError::EgressDenied(format!(
                "Egress denied to '{}' because destination is restricted",
                url
            )));
        }

        Ok(())
    }

    fn is_blocked_address(&self, ip: IpAddr) -> bool {
        if self.allow_private {
            false
        } else {
            is_blocked_ip(&ip)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_url(raw: &str) -> Url {
        Url::parse(raw).expect("test URL should parse")
    }

    #[test]
    fn deny_metadata_and_private_destinations() {
        let policy = EgressPolicy::default();

        assert!(policy.check_url(&parse_url("http://169.254.169.254/")).is_err());
        assert!(policy.check_url(&parse_url("http://127.0.0.1/")).is_err());
        assert!(policy.check_url(&parse_url("http://10.0.0.1/")).is_err());
        assert!(policy.check_url(&parse_url("http://192.168.1.1/")).is_err());
        assert!(policy.check_url(&parse_url("http://[::1]/")).is_err());
    }

    #[test]
    fn allow_public_ip_destination() {
        let policy = EgressPolicy::default();

        assert!(policy.check_url(&parse_url("http://93.184.216.34/")).is_ok());
    }

    #[test]
    fn allowlisted_host_overrides_loopback() {
        let mut policy = EgressPolicy::default();
        policy.allow_host("localhost");

        assert!(policy.check_url(&parse_url("http://localhost/")).is_ok());
    }

    #[test]
    fn allow_private_enables_private_destinations() {
        let policy = EgressPolicy {
            allow_private: true,
            ..EgressPolicy::default()
        };

        let private_urls = [
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://[::1]/",
            "http://169.254.169.254/",
            "http://100.64.0.1/",
        ];

        for url in private_urls {
            let result = policy.check_url(&parse_url(url));
            assert!(
                result.is_ok(),
                "expected private egress to be allowed: {url}, got {result:?}"
            );
        }
    }

    #[test]
    fn deny_non_http_scheme() {
        let policy = EgressPolicy::default();

        assert!(policy.check_url(&parse_url("file:///etc/passwd")).is_err());
    }
}
