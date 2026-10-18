use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use anyhow::{anyhow, bail, Context as _};

use hitomi_core::utils::{DEFAULT_MAX_FILE_NAME_BYTES, MIN_MAX_FILE_NAME_BYTES};

use crate::auth::{AuthSettings, PasswordSetting, DEFAULT_SESSION_DAYS};

/// Where the server keeps its files and where it listens, read from the environment
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `config.json` and the logs (`HITOMI_CONFIG_DIR`)
    pub config_dir: PathBuf,
    /// The downloads (`HITOMI_LIBRARY_DIR`), fixed for as long as it is set
    pub library_dir: Option<PathBuf>,
    /// The exports (`HITOMI_EXPORT_DIR`), fixed for as long as it is set
    pub export_dir: Option<PathBuf>,
    /// Where the pages of a running download are kept (`HITOMI_TEMP_DIR`), by default
    /// `downloading` in the config dir. Never the library: a scan of it must not see them.
    pub temp_dir: PathBuf,
    /// Refuse to start when a data directory is not a mount (`HITOMI_REQUIRE_MOUNT`), which
    /// is what the Docker image sets, so nothing is written to the container by mistake
    pub require_mount: bool,
    /// File names are at most this long (`HITOMI_MAX_NAME_BYTES`), for a share that allows
    /// less than the server finds out by itself. Can only lower the limit, never raise it.
    pub max_name_bytes: Option<usize>,
    /// How often the followed searches are checked (`HITOMI_FOLLOW_INTERVAL_HOURS`), none
    /// when the checks are off. A check can still be asked for by hand.
    pub follow_interval: Option<Duration>,
    /// The frontend built by `pnpm build:web` (`HITOMI_WEB_DIR`)
    pub web_dir: PathBuf,
    /// `HITOMI_HOST` and `HITOMI_PORT`
    pub addr: SocketAddr,
    pub auth: AuthSettings,
}

/// Only reachable from this machine unless `HITOMI_HOST` says otherwise, reaching it from
/// the network is a decision to make on purpose
const DEFAULT_HOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_FOLLOW_INTERVAL_HOURS: u64 = 6;
const MAX_FOLLOW_INTERVAL_HOURS: u64 = 24 * 30;

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

        let password = match (var("HITOMI_PASSWORD_HASH"), var("HITOMI_PASSWORD")) {
            (Some(_), Some(_)) => {
                bail!("HITOMI_PASSWORD_HASH and HITOMI_PASSWORD are both set, set only one of them")
            }
            (Some(hash), None) => Some(PasswordSetting::Hash(hash.trim().to_string())),
            // a password may well start or end with a space, so it is taken as it is
            (None, Some(plain)) => Some(PasswordSetting::Plain(plain)),
            (None, None) => None,
        };
        let session_days = match var("HITOMI_SESSION_DAYS") {
            Some(days) => days
                .trim()
                .parse()
                .ok()
                .filter(|days| (1..=3650).contains(days))
                .ok_or_else(|| {
                    anyhow!("HITOMI_SESSION_DAYS `{days}` is no number of days between 1 and 3650")
                })?,
            None => DEFAULT_SESSION_DAYS,
        };
        let public_url = var("HITOMI_PUBLIC_URL")
            .map(|url| url.trim().trim_end_matches('/').to_string())
            .map(|url| {
                if url.starts_with("https://") || url.starts_with("http://") {
                    Ok(url)
                } else {
                    Err(anyhow!(
                        "HITOMI_PUBLIC_URL `{url}` does not start with https:// or http://"
                    ))
                }
            })
            .transpose()?;
        let flag = |name: &str| -> anyhow::Result<bool> {
            match var(name).map(|value| value.trim().to_lowercase()) {
                None => Ok(false),
                Some(value) if ["1", "true", "yes", "on"].contains(&value.as_str()) => Ok(true),
                Some(value) if ["0", "false", "no", "off"].contains(&value.as_str()) => Ok(false),
                Some(value) => bail!("{name} `{value}` is neither true nor false"),
            }
        };
        let trust_forwarded_for = flag("HITOMI_TRUST_FORWARDED_FOR")?;
        let require_mount = flag("HITOMI_REQUIRE_MOUNT")?;
        let max_name_bytes = match var("HITOMI_MAX_NAME_BYTES") {
            Some(bytes) => Some(
                bytes
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .filter(|bytes| {
                        (MIN_MAX_FILE_NAME_BYTES..=DEFAULT_MAX_FILE_NAME_BYTES).contains(bytes)
                    })
                    .ok_or_else(|| {
                        anyhow!(
                            "HITOMI_MAX_NAME_BYTES `{bytes}` is no number between {MIN_MAX_FILE_NAME_BYTES} and {DEFAULT_MAX_FILE_NAME_BYTES}"
                        )
                    })?,
            ),
            None => None,
        };

        // 0 switches the checks off, a check more often than every hour would hammer Hitomi
        let follow_interval = match var("HITOMI_FOLLOW_INTERVAL_HOURS") {
            Some(hours) => {
                let hours: u64 = hours
                    .trim()
                    .parse()
                    .ok()
                    .filter(|hours| *hours <= MAX_FOLLOW_INTERVAL_HOURS)
                    .ok_or_else(|| {
                        anyhow!(
                            "HITOMI_FOLLOW_INTERVAL_HOURS `{hours}` is no number of hours between 1 and {MAX_FOLLOW_INTERVAL_HOURS}, or 0 for no checks"
                        )
                    })?;
                (hours > 0).then(|| Duration::from_secs(hours * 3600))
            }
            None => Some(Duration::from_secs(DEFAULT_FOLLOW_INTERVAL_HOURS * 3600)),
        };

        let config_dir =
            var("HITOMI_CONFIG_DIR").map_or_else(|| PathBuf::from("config"), PathBuf::from);

        Ok(Settings {
            config_dir: config_dir.clone(),
            library_dir: var("HITOMI_LIBRARY_DIR").map(PathBuf::from),
            export_dir: var("HITOMI_EXPORT_DIR").map(PathBuf::from),
            require_mount,
            max_name_bytes,
            temp_dir: var("HITOMI_TEMP_DIR")
                .map_or_else(|| config_dir.join("downloading"), PathBuf::from),
            follow_interval,
            web_dir: var("HITOMI_WEB_DIR").map_or_else(|| PathBuf::from("dist-web"), PathBuf::from),
            addr: SocketAddr::new(host, port),
            auth: AuthSettings {
                username: var("HITOMI_USERNAME").map(|username| username.trim().to_string()),
                password,
                session_days,
                public_url,
                trust_forwarded_for,
            },
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
        assert_eq!(settings.web_dir, PathBuf::from("dist-web"));
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
    fn the_pages_of_downloads_default_to_the_config_dir() {
        let settings = settings(&[("HITOMI_CONFIG_DIR", "/config")]).unwrap();
        assert_eq!(settings.temp_dir, PathBuf::from("/config/downloading"));
        let settings = super::Settings::from_vars(|_| None).unwrap();
        assert_eq!(settings.temp_dir, PathBuf::from("config/downloading"));

        let settings = settings_with(&[
            ("HITOMI_CONFIG_DIR", "/config"),
            ("HITOMI_TEMP_DIR", "/scratch"),
        ]);
        assert_eq!(settings.temp_dir, PathBuf::from("/scratch"));
    }

    fn settings_with(vars: &[(&str, &str)]) -> Settings {
        settings(vars).unwrap()
    }

    #[test]
    fn the_checks_of_the_volumes_are_set_by_the_environment() {
        let defaults = super::Settings::from_vars(|_| None).unwrap();
        assert!(!defaults.require_mount);
        assert_eq!(defaults.max_name_bytes, None);

        let set = settings(&[
            ("HITOMI_REQUIRE_MOUNT", "true"),
            ("HITOMI_MAX_NAME_BYTES", "143"),
        ])
        .unwrap();
        assert!(set.require_mount);
        assert_eq!(set.max_name_bytes, Some(143));

        assert!(!settings_with(&[("HITOMI_REQUIRE_MOUNT", "no")]).require_mount);
        for bad in ["maybe", "2"] {
            assert!(settings(&[("HITOMI_REQUIRE_MOUNT", bad)]).is_err(), "{bad}");
        }
        for bad in ["99", "256", "long", "-1", "143.5"] {
            assert!(
                settings(&[("HITOMI_MAX_NAME_BYTES", bad)]).is_err(),
                "{bad}"
            );
        }
        assert!(settings(&[("HITOMI_MAX_NAME_BYTES", "100")]).is_ok());
        assert!(settings(&[("HITOMI_MAX_NAME_BYTES", "255")]).is_ok());
    }

    #[test]
    fn the_followed_searches_are_checked_every_six_hours_unless_told_otherwise() {
        let hours = |settings: Settings| settings.follow_interval.map(|d| d.as_secs() / 3600);
        assert_eq!(hours(settings_with(&[])), Some(6));
        assert_eq!(
            hours(settings_with(&[("HITOMI_FOLLOW_INTERVAL_HOURS", "1")])),
            Some(1)
        );
        assert_eq!(
            hours(settings_with(&[("HITOMI_FOLLOW_INTERVAL_HOURS", " 24 ")])),
            Some(24)
        );
        assert_eq!(
            hours(settings_with(&[("HITOMI_FOLLOW_INTERVAL_HOURS", "720")])),
            Some(720)
        );
        // 0 switches them off
        assert_eq!(
            hours(settings_with(&[("HITOMI_FOLLOW_INTERVAL_HOURS", "0")])),
            None
        );
        for bad in ["721", "-1", "1.5", "often", "6h"] {
            assert!(
                settings(&[("HITOMI_FOLLOW_INTERVAL_HOURS", bad)]).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn empty_variables_count_as_unset() {
        let settings = settings(&[("HITOMI_LIBRARY_DIR", ""), ("HITOMI_PORT", " ")]).unwrap();
        assert_eq!(settings.library_dir, None);
        assert_eq!(settings.addr.port(), DEFAULT_PORT);
    }

    #[test]
    fn login_settings_come_from_the_environment() {
        let settings = settings(&[
            ("HITOMI_USERNAME", " marcel "),
            ("HITOMI_PASSWORD", " with spaces "),
            ("HITOMI_SESSION_DAYS", "7"),
            ("HITOMI_PUBLIC_URL", "https://hitomi.example.com/"),
            ("HITOMI_TRUST_FORWARDED_FOR", "true"),
        ])
        .unwrap();
        assert_eq!(settings.auth.username.as_deref(), Some("marcel"));
        assert_eq!(
            settings.auth.password,
            Some(PasswordSetting::Plain(" with spaces ".to_string()))
        );
        assert_eq!(settings.auth.session_days, 7);
        assert_eq!(
            settings.auth.public_url.as_deref(),
            Some("https://hitomi.example.com")
        );
        assert!(settings.auth.trust_forwarded_for);

        let defaults = super::Settings::from_vars(|_| None).unwrap().auth;
        assert_eq!(defaults.session_days, DEFAULT_SESSION_DAYS);
        assert!(!defaults.trust_forwarded_for);
        assert_eq!(defaults.password, None);
    }

    #[test]
    fn contradicting_or_invalid_login_settings_are_an_error() {
        assert!(settings(&[
            ("HITOMI_PASSWORD_HASH", "$argon2id$x"),
            ("HITOMI_PASSWORD", "x")
        ])
        .is_err());
        assert!(settings(&[("HITOMI_SESSION_DAYS", "0")]).is_err());
        assert!(settings(&[("HITOMI_SESSION_DAYS", "forever")]).is_err());
        assert!(settings(&[("HITOMI_PUBLIC_URL", "hitomi.example.com")]).is_err());
        assert!(settings(&[("HITOMI_TRUST_FORWARDED_FOR", "maybe")]).is_err());
    }

    #[test]
    fn invalid_host_or_port_is_an_error() {
        assert!(settings(&[("HITOMI_HOST", "localhost:80")]).is_err());
        assert!(settings(&[("HITOMI_PORT", "0")]).is_err());
        assert!(settings(&[("HITOMI_PORT", "70000")]).is_err());
        assert!(settings(&[("HITOMI_PORT", "http")]).is_err());
    }
}
