use std::{
    collections::HashMap,
    sync::{LazyLock, OnceLock},
};

use chrono::Utc;
use regex_lite::Regex;

use crate::hitomi_client::HitomiClient;

static RE_DEFAULT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"var o = (\d)").expect("valid regex"));
static RE_O: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"o = (\d); break;").expect("valid regex"));
static RE_CASE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"case (\d+):").expect("valid regex"));
static RE_B: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"b: '(.+)'").expect("valid regex"));
static RE_HASH_TAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(..)(.)$").expect("valid regex"));

pub struct GG {
    pub last_retrieval: Option<i64>,
    pub m_default: i32,
    pub m_map: HashMap<i32, i32>,
    pub b: String,
}

impl GG {
    fn new() -> Self {
        Self {
            last_retrieval: None,
            m_default: 0,
            m_map: HashMap::new(),
            b: String::new(),
        }
    }

    pub fn inst() -> &'static tokio::sync::Mutex<GG> {
        static INSTANCE: OnceLock<tokio::sync::Mutex<GG>> = OnceLock::new();
        INSTANCE.get_or_init(|| tokio::sync::Mutex::new(GG::new()))
    }

    pub async fn refresh(&mut self) -> anyhow::Result<()> {
        if let Some(last_retrieval) = self.last_retrieval {
            if last_retrieval + 60000 >= Utc::now().timestamp_millis() {
                return Ok(());
            }
        }

        let client = HitomiClient::get_api_client();

        let request = client
            .read()
            .get("https://ltn.gold-usergeneratedcontent.net/gg.js");
        // An error page must not be parsed, that would keep the stale values for another minute
        let body = request.send().await?.error_for_status()?.text().await?;

        self.apply(&body)?;

        self.last_retrieval = Some(Utc::now().timestamp_millis());
        Ok(())
    }

    /// Take over the values from the source of `gg.js`
    fn apply(&mut self, body: &str) -> anyhow::Result<()> {
        if let Some(cap) = RE_DEFAULT.captures(body) {
            self.m_default = cap[1].parse()?;
        }

        if let Some(cap) = RE_O.captures(body) {
            let o: i32 = cap[1].parse()?;
            self.m_map.clear();

            for cap in RE_CASE.captures_iter(body) {
                let case: i32 = cap[1].parse()?;
                self.m_map.insert(case, o);
            }
        }

        if let Some(cap) = RE_B.captures(body) {
            self.b = cap[1].to_string();
        }

        Ok(())
    }

    /// Make the next lookup fetch `gg.js` again, unless it was fetched in the last few seconds
    ///
    /// Hitomi rotates the values in `gg.js`, so an image url built from an old copy points
    /// at a path that no longer exists. Several images fail at once when that happens, the
    /// grace period keeps them from fetching `gg.js` once each.
    pub fn invalidate(&mut self) {
        const GRACE_PERIOD_MILLIS: i64 = 5000;
        if let Some(last_retrieval) = self.last_retrieval {
            if last_retrieval + GRACE_PERIOD_MILLIS < Utc::now().timestamp_millis() {
                self.last_retrieval = None;
            }
        }
    }

    pub async fn m(&mut self, g: i32) -> anyhow::Result<i32> {
        self.refresh().await?;
        Ok(self.m_map.get(&g).copied().unwrap_or(self.m_default))
    }

    pub async fn b(&mut self) -> anyhow::Result<String> {
        self.refresh().await?;
        Ok(self.b.clone())
    }

    #[allow(clippy::unused_self)]
    pub fn s(&self, h: &str) -> anyhow::Result<String> {
        if let Some(caps) = RE_HASH_TAIL.captures(h) {
            let combined = format!("{}{}", &caps[2], &caps[1]);
            let num = i32::from_str_radix(&combined, 16)?;
            Ok(num.to_string())
        } else {
            Err(anyhow::anyhow!("Invalid hash format"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gg_fetched_millis_ago(millis: i64) -> GG {
        GG {
            last_retrieval: Some(Utc::now().timestamp_millis() - millis),
            ..GG::new()
        }
    }

    #[test]
    fn old_copy_is_invalidated() {
        let mut gg = gg_fetched_millis_ago(10_000);
        gg.invalidate();
        assert_eq!(gg.last_retrieval, None);
    }

    #[test]
    fn fresh_copy_survives_invalidation() {
        // several images failing at once must not each fetch gg.js again
        let mut gg = gg_fetched_millis_ago(1_000);
        gg.invalidate();
        assert!(gg.last_retrieval.is_some());
    }

    #[test]
    fn never_fetched_stays_unfetched() {
        let mut gg = GG::new();
        gg.invalidate();
        assert_eq!(gg.last_retrieval, None);
    }

    /// Shaped like the `gg.js` Hitomi serves, shortened to a few cases
    const GG_JS: &str = "'use strict';
gg = { m: function(g) { var o = 0; switch (g) {
case 1234:
case 3021:
case 87:
o = 1; break;
} return o; },
s: function(h) { var m = /(..)(.)$/.exec(h); return parseInt(m[2]+m[1], 16).toString(10); },
b: '1729083600/'
};";

    #[test]
    fn values_are_read_from_gg_js() {
        let mut gg = GG::new();
        gg.apply(GG_JS).unwrap();

        assert_eq!(gg.m_default, 0);
        assert_eq!(gg.m_map.len(), 3);
        assert_eq!(gg.m_map.get(&3021), Some(&1));
        assert_eq!(gg.m_map.get(&87), Some(&1));
        assert_eq!(gg.b, "1729083600/");
    }

    #[test]
    fn s_is_read_from_the_end_of_the_hash() {
        // the last character followed by the two before it, read as hex: 0x37a
        let hash = format!("{}7a3", "0".repeat(61));
        assert_eq!(GG::new().s(&hash).unwrap(), "890");
    }
}
