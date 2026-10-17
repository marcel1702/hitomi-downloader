use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use anyhow::{anyhow, Context as _};

/// Where the server keeps its files and where it listens, read from the environment
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `config.json` and the logs (`HITOMI_CONFIG_DIR`)
    pub config_dir: PathBuf,
    /// The downloads (`HITOMI_LIBRARY_DIR`), fixed for as long as it is set
    pub library_dir: Option<PathBuf>,
    /// The exports (`HITOMI_EXPORT_DIR`), fixed for as long as it is set
    pub export_dir: Option<PathBuf>,
    /// The built frontend (`HITOMI_WEB_DIR`)
    pub web_dir: PathBuf,
    /// `HITOMI_HOST` and `HITOMI_PORT`
    pub addr: SocketAddr,
}

/// Only reachable from this machine unless `HITOMI_HOST` says otherwise, the server has no
/// login of its own yet
const DEFAULT_HOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const DEFAULT_PORT: u16 = 8080;

impl Settings {
    pub fn from_env() -> anyhow::Result<Settings> {
        Settings::from_vars(|name| std::env::var(name).ok())
    }

    /// The settings from the variables `var` returns, `None` meaning unset
    fn from_vars(var: impl Fn(&str) -> Option<String>) -> anyhow::Result<Settings> {
        // an empty variable counts as unset, which is what `VAR=` in a compose file means
        let var = |name: &str| var(name).filter(|value| !value.trim().is_empty());

        let host = match var("HITOMI_HOST") {
            Some(host) => host
                .trim()
                .parse()
                .with_context(|| format!("HITOMI_HOST `{host}` is no IP address"))?,
            None => DEFAULT_HOST,
        };
        let port = match var("HITOMI_PORT") {
            Some(port) => port
                .trim()
                .parse()
                .ok()
                .filter(|port| *port != 0)
                .ok_or_else(|| anyhow!("HITOMI_PORT `{port}` is no port between 1 and 65535"))?,
            None => DEFAULT_PORT,
        };

        Ok(Settings {
            config_dir: var("HITOMI_CONFIG_DIR")
                .map_or_else(|| PathBuf::from("config"), PathBuf::from),
            library_dir: var("HITOMI_LIBRARY_DIR").map(PathBuf::from),
            export_dir: var("HITOMI_EXPORT_DIR").map(PathBuf::from),
            web_dir: var("HITOMI_WEB_DIR").map_or_else(|| PathBuf::from("dist"), PathBuf::from),
            addr: SocketAddr::new(host, port),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn settings(vars: &[(&str, &str)]) -> anyhow::Result<Settings> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        Settings::from_vars(|name| vars.get(name).cloned())
    }

    #[test]
    fn defaults_listen_on_localhost_only() {
        let settings = settings(&[]).unwrap();
        assert_eq!(settings.addr, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(settings.config_dir, PathBuf::from("config"));
        assert_eq!(settings.web_dir, PathBuf::from("dist"));
        assert_eq!(settings.library_dir, None);
        assert_eq!(settings.export_dir, None);
    }

    #[test]
    fn variables_override_the_defaults() {
        let settings = settings(&[
            ("HITOMI_HOST", "0.0.0.0"),
            ("HITOMI_PORT", "9000"),
            ("HITOMI_CONFIG_DIR", "/config"),
            ("HITOMI_LIBRARY_DIR", "/data/library"),
            ("HITOMI_EXPORT_DIR", "/data/export"),
            ("HITOMI_WEB_DIR", "/app/web"),
        ])
        .unwrap();
        assert_eq!(settings.addr, "0.0.0.0:9000".parse().unwrap());
        assert_eq!(settings.config_dir, PathBuf::from("/config"));
        assert_eq!(settings.library_dir, Some(PathBuf::from("/data/library")));
        assert_eq!(settings.export_dir, Some(PathBuf::from("/data/export")));
        assert_eq!(settings.web_dir, PathBuf::from("/app/web"));
    }

    #[test]
    fn empty_variables_count_as_unset() {
        let settings = settings(&[("HITOMI_LIBRARY_DIR", ""), ("HITOMI_PORT", " ")]).unwrap();
        assert_eq!(settings.library_dir, None);
        assert_eq!(settings.addr.port(), DEFAULT_PORT);
    }

    #[test]
    fn invalid_host_or_port_is_an_error() {
        assert!(settings(&[("HITOMI_HOST", "localhost:80")]).is_err());
        assert!(settings(&[("HITOMI_PORT", "0")]).is_err());
        assert!(settings(&[("HITOMI_PORT", "70000")]).is_err());
        assert!(settings(&[("HITOMI_PORT", "http")]).is_err());
    }
}
