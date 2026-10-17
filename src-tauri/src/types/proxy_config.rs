use anyhow::{anyhow, Context};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use specta::Type;

/// RFC 1929 stores the length of the username and the password in a single byte each
const MAX_CREDENTIAL_LEN: usize = 255;

/// Connection settings for the optional SOCKS5 proxy all requests are routed through
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// Let the proxy resolve hostnames (`socks5h`) so no DNS query leaves the machine directly
    pub remote_dns: bool,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".to_string(),
            port: 1080,
            username: String::new(),
            password: String::new(),
            remote_dns: true,
        }
    }
}

impl ProxyConfig {
    /// Build the proxy URL, with the credentials percent-encoded into its userinfo
    pub fn to_url(&self) -> anyhow::Result<Url> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err(anyhow!("proxy host is empty"));
        }
        if self.port == 0 {
            return Err(anyhow!("proxy port must be between 1 and 65535"));
        }
        if self.username.len() > MAX_CREDENTIAL_LEN {
            return Err(anyhow!(
                "proxy username is longer than {MAX_CREDENTIAL_LEN} bytes"
            ));
        }
        if self.password.len() > MAX_CREDENTIAL_LEN {
            return Err(anyhow!(
                "proxy password is longer than {MAX_CREDENTIAL_LEN} bytes"
            ));
        }
        // RFC 1929 has no username-only variant and reqwest drops a username without password
        if self.username.is_empty() != self.password.is_empty() {
            return Err(anyhow!("proxy username and password must be set together"));
        }

        let scheme = if self.remote_dns { "socks5h" } else { "socks5" };
        // a bare IPv6 address has to be bracketed to be parsed as a URL host
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        };

        let mut url = Url::parse(&format!("{scheme}://placeholder"))?;
        url.set_host(Some(&host))
            .with_context(|| format!("invalid proxy host `{host}`"))?;
        url.set_port(Some(self.port))
            .map_err(|()| anyhow!("can not set proxy port"))?;
        if !self.username.is_empty() {
            url.set_username(&self.username)
                .map_err(|()| anyhow!("can not set proxy username"))?;
            url.set_password(Some(&self.password))
                .map_err(|()| anyhow!("can not set proxy password"))?;
        }
        Ok(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_resolves_dns_through_proxy() {
        let url = ProxyConfig::default().to_url().unwrap();
        assert_eq!(url.as_str(), "socks5h://127.0.0.1:1080");
    }

    #[test]
    fn local_dns_uses_socks5_scheme() {
        let config = ProxyConfig {
            remote_dns: false,
            port: 9050,
            ..ProxyConfig::default()
        };
        assert_eq!(config.to_url().unwrap().as_str(), "socks5://127.0.0.1:9050");
    }

    #[test]
    fn credentials_are_percent_encoded() {
        let config = ProxyConfig {
            host: "proxy.example.com".to_string(),
            username: "us@er".to_string(),
            password: "p:a/s s".to_string(),
            ..ProxyConfig::default()
        };
        let url = config.to_url().unwrap();
        assert_eq!(url.host_str(), Some("proxy.example.com"));
        assert_eq!(url.username(), "us%40er");
        assert_eq!(url.password(), Some("p%3Aa%2Fs%20s"));
    }

    #[test]
    fn bare_ipv6_host_is_bracketed() {
        let config = ProxyConfig {
            host: "::1".to_string(),
            ..ProxyConfig::default()
        };
        assert_eq!(config.to_url().unwrap().as_str(), "socks5h://[::1]:1080");
    }

    #[test]
    fn invalid_configs_are_rejected() {
        let invalid = [
            ProxyConfig {
                host: "  ".to_string(),
                ..ProxyConfig::default()
            },
            ProxyConfig {
                host: "bad host".to_string(),
                ..ProxyConfig::default()
            },
            ProxyConfig {
                port: 0,
                ..ProxyConfig::default()
            },
            ProxyConfig {
                password: "secret".to_string(),
                ..ProxyConfig::default()
            },
            ProxyConfig {
                username: "user".to_string(),
                ..ProxyConfig::default()
            },
            ProxyConfig {
                username: "u".repeat(256),
                password: "secret".to_string(),
                ..ProxyConfig::default()
            },
            ProxyConfig {
                username: "user".to_string(),
                password: "p".repeat(256),
                ..ProxyConfig::default()
            },
        ];
        for config in invalid {
            assert!(config.to_url().is_err(), "{config:?} should be rejected");
        }
    }
}
