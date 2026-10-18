use std::{
    ops::RangeInclusive,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::types::{DownloadFormat, ProxyConfig};

/// The languages the interface ships translations for
///
/// Kept in step with the locale files under `src/locales`.
const SUPPORTED_LANGUAGES: [&str; 2] = ["en-US", "de-DE"];

/// The language the interface uses unless the user picks another one
const DEFAULT_LANGUAGE: &str = "en-US";

/// How many comics may download at the same time
///
/// Kept low, Hitomi throttles clients that open too many connections.
pub const COMIC_CONCURRENCY_RANGE: RangeInclusive<u32> = 1..=8;
/// How many images may download at the same time, across all comics
pub const IMG_CONCURRENCY_RANGE: RangeInclusive<u32> = 1..=16;

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub download_dir: PathBuf,
    pub export_dir: PathBuf,
    pub enable_file_logger: bool,
    pub download_format: DownloadFormat,
    pub dir_fmt: String,
    pub proxy: ProxyConfig,
    pub comic_concurrency: u32,
    pub img_concurrency: u32,
    pub language: String,
}

impl Config {
    /// Load the config from `config.json` in `app_data_dir`, or create it there
    pub fn new(app_data_dir: &Path) -> anyhow::Result<Config> {
        let config_path = app_data_dir.join("config.json");

        let mut config = if config_path.exists() {
            let config_string = std::fs::read_to_string(config_path)?;
            match serde_json::from_str(&config_string) {
                // if deserialization succeeds, use the deserialized Config
                Ok(config) => config,
                // Otherwise, merge the default configuration with the configuration already in the file
                // to avoid resetting all configuration items when new configuration items are added in the new version
                // after the user upgrades to the new version
                Err(_) => Config::merge_config(&config_string, app_data_dir),
            }
        } else {
            Config::default(app_data_dir)
        };
        // A config written by an older version can name a language that is no longer
        // shipped, which would leave the interface without a selected language
        if !SUPPORTED_LANGUAGES.contains(&config.language.as_str()) {
            config.language = DEFAULT_LANGUAGE.to_string();
        }
        config.clamp_concurrency();
        config.save(app_data_dir)?;
        Ok(config)
    }

    /// Check that the concurrency limits lie in their allowed ranges
    pub fn validate_concurrency(&self) -> anyhow::Result<()> {
        let comic_range = COMIC_CONCURRENCY_RANGE;
        if !comic_range.contains(&self.comic_concurrency) {
            return Err(anyhow!(
                "the number of comics downloading at once must be between {} and {}",
                comic_range.start(),
                comic_range.end()
            ));
        }
        let img_range = IMG_CONCURRENCY_RANGE;
        if !img_range.contains(&self.img_concurrency) {
            return Err(anyhow!(
                "the number of images downloading at once must be between {} and {}",
                img_range.start(),
                img_range.end()
            ));
        }
        Ok(())
    }

    /// Pull concurrency limits from a hand edited config file back into their ranges
    ///
    /// A limit of 0 would never let a download start.
    fn clamp_concurrency(&mut self) {
        self.comic_concurrency = self.comic_concurrency.clamp(
            *COMIC_CONCURRENCY_RANGE.start(),
            *COMIC_CONCURRENCY_RANGE.end(),
        );
        self.img_concurrency = self
            .img_concurrency
            .clamp(*IMG_CONCURRENCY_RANGE.start(), *IMG_CONCURRENCY_RANGE.end());
    }

    pub fn save(&self, app_data_dir: &Path) -> anyhow::Result<()> {
        let config_path = app_data_dir.join("config.json");
        let config_string = serde_json::to_string_pretty(self)?;
        std::fs::write(config_path, config_string)?;
        Ok(())
    }

    fn merge_config(config_string: &str, app_data_dir: &Path) -> Config {
        let Ok(mut json_value) = serde_json::from_str::<serde_json::Value>(config_string) else {
            return Config::default(app_data_dir);
        };
        let serde_json::Value::Object(ref mut map) = json_value else {
            return Config::default(app_data_dir);
        };
        let Ok(default_config_value) = serde_json::to_value(Config::default(app_data_dir)) else {
            return Config::default(app_data_dir);
        };
        let serde_json::Value::Object(default_map) = default_config_value else {
            return Config::default(app_data_dir);
        };
        for (key, value) in default_map {
            map.entry(key).or_insert(value);
        }
        let Ok(config) = serde_json::from_value(json_value) else {
            return Config::default(app_data_dir);
        };
        config
    }

    fn default(app_data_dir: &Path) -> Config {
        Config {
            download_dir: app_data_dir.join("download"),
            export_dir: app_data_dir.join("export"),
            enable_file_logger: true,
            download_format: DownloadFormat::Webp,
            dir_fmt: "{artists}/{title} ({id}) - {language}".to_string(),
            proxy: ProxyConfig::default(),
            comic_concurrency: 2,
            img_concurrency: 4,
            language: DEFAULT_LANGUAGE.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_with_legacy_proxy_fields_keeps_other_settings() {
        let legacy = r#"{
            "downloadDir": "/comics",
            "exportDir": "/exports",
            "enableFileLogger": false,
            "downloadFormat": "Avif",
            "dirFmt": "{id}",
            "proxyHost": "127.0.0.1",
            "proxyMode": "Custom",
            "proxyPort": 7890,
            "language": "de-DE"
        }"#;
        let config = Config::merge_config(legacy, Path::new("/app-data"));
        assert_eq!(config.download_dir, PathBuf::from("/comics"));
        assert_eq!(config.download_format, DownloadFormat::Avif);
        assert_eq!(config.dir_fmt, "{id}");
        assert_eq!(config.language, "de-DE");
        assert_eq!(config.proxy, ProxyConfig::default());
        // a key the old config does not have is filled in from the defaults
        let without_format =
            Config::merge_config(r#"{ "language": "de-DE" }"#, Path::new("/app-data"));
        assert_eq!(
            without_format.dir_fmt,
            "{artists}/{title} ({id}) - {language}"
        );

        let saved = serde_json::to_value(&config).unwrap();
        assert!(saved.get("proxyMode").is_none());
    }

    #[test]
    fn config_without_concurrency_gets_the_defaults() {
        let older = r#"{
            "downloadDir": "/comics",
            "exportDir": "/exports",
            "enableFileLogger": true,
            "downloadFormat": "Webp",
            "dirFmt": "{id}",
            "proxy": {
                "enabled": false, "host": "127.0.0.1", "port": 1080,
                "username": "", "password": "", "remoteDns": true
            },
            "language": "en-US"
        }"#;
        let config = Config::merge_config(older, Path::new("/app-data"));
        assert_eq!(config.download_dir, PathBuf::from("/comics"));
        assert_eq!((config.comic_concurrency, config.img_concurrency), (2, 4));
    }

    #[test]
    fn concurrency_out_of_range_is_rejected_and_clamped() {
        let mut config = Config::default(Path::new("/app-data"));
        assert!(config.validate_concurrency().is_ok());

        config.comic_concurrency = 0;
        config.img_concurrency = 99;
        assert!(config.validate_concurrency().is_err());

        config.clamp_concurrency();
        assert_eq!((config.comic_concurrency, config.img_concurrency), (1, 16));
        assert!(config.validate_concurrency().is_ok());
    }
}
