use std::sync::LazyLock;

use anyhow::{anyhow, Context};
use regex_lite::Regex;
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::hitomi_client::HitomiClient;

use super::gg::GG;

/// The three hex digits of an image hash that pick its subdomain
static RE_SUBDOMAIN_DIGITS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/[0-9a-f]{61}([0-9a-f]{2})([0-9a-f])").expect("valid regex"));
/// The subdomain part of an image url
static RE_SUBDOMAIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"//..?\.(?:gold-usergeneratedcontent\.net|hitomi\.la)/").expect("valid regex")
});
/// The last three characters of a hash, which make up its directory
static RE_HASH_DIRS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^.*(..)(.)$").expect("valid regex"));
//common.js
pub const PROTOCOL: &str = "https:";
pub const DOMAIN: &str = "ltn.gold-usergeneratedcontent.net";
#[allow(dead_code)]
pub const GALLERY_BLOCK_EXTENSION: &str = ".html";
#[allow(dead_code)]
pub const GALLERY_BLOCK_DIR: &str = "galleryblock";
pub const NOZOMI_EXTENSION: &str = ".nozomi";

#[derive(Debug, Serialize, Deserialize)]
pub struct Artist {
    pub artist: String,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Group {
    pub group: String,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Parody {
    pub parody: String,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Character {
    pub character: String,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct Tag {
    pub tag: String,
    pub url: String,
    #[serde(default, deserialize_with = "string_to_i32")]
    pub female: i32,
    #[serde(default, deserialize_with = "string_to_i32")]
    pub male: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct Language {
    #[serde(default, deserialize_with = "string_to_i32")]
    pub galleryid: i32,
    pub url: String,
    pub language_localname: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct GalleryFiles {
    pub width: i32,
    pub hash: String,
    #[serde(default)]
    pub haswebp: i32,
    #[serde(default)]
    pub hasavif: i32,
    #[serde(default)]
    pub hasjxl: i32,
    pub name: String,
    pub height: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GalleryInfo {
    #[serde(default, deserialize_with = "string_to_i32")]
    pub id: i32,
    pub title: String,
    pub japanese_title: Option<String>,
    pub language: Option<String>,
    pub language_localname: Option<String>,
    #[serde(rename = "type")]
    pub type_field: String,
    pub date: String,
    pub artists: Option<Vec<Artist>>,
    pub groups: Option<Vec<Group>>,
    pub parodys: Option<Vec<Parody>>,
    pub tags: Option<Vec<Tag>>,
    #[serde(default, deserialize_with = "strings_to_i32s")]
    pub related: Vec<i32>,
    #[serde(default)]
    pub languages: Vec<Language>,
    pub characters: Option<Vec<Character>>,
    #[serde(default, deserialize_with = "strings_to_i32s")]
    pub scene_indexes: Vec<i32>,
    #[serde(default)]
    pub files: Vec<GalleryFiles>,
}

fn string_to_i32<'de, D>(d: D) -> Result<i32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde_json::Value;
    let value: Value = serde::Deserialize::deserialize(d)?;

    match value {
        #[allow(clippy::cast_possible_truncation)]
        Value::Number(n) => Ok(n.as_i64().unwrap_or(0) as i32),
        Value::String(s) => Ok(s.parse().unwrap_or(0)),
        _ => Err(serde::de::Error::custom(
            "`string_to_i32` failed, value type is not `Number` or `String`",
        )),
    }
}

/// A list of numbers, which Hitomi sends as numbers or as strings, even within one gallery
///
/// A failure here fails the whole gallery, and with it every comic on the same search page,
/// so `null` counts as an empty list and an entry that is no number is left out.
fn strings_to_i32s<'de, D>(d: D) -> Result<Vec<i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde_json::Value;
    let values: Option<Vec<Value>> = serde::Deserialize::deserialize(d)?;

    let numbers = values
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| match value {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        })
        .filter_map(|n| i32::try_from(n).ok())
        .collect();
    Ok(numbers)
}

#[allow(clippy::cast_sign_loss)]
pub async fn subdomain_from_url(
    url: &str,
    base: Option<&str>,
    dir: Option<&str>,
) -> anyhow::Result<String> {
    let base_is_none_or_empty = match base {
        None => true,
        Some(base) => base.is_empty(),
    };

    let mut retval = String::new();

    if base_is_none_or_empty {
        match dir {
            Some("webp") => retval = "w".to_string(),
            Some("avif") => retval = "a".to_string(),
            _ => {}
        }
    }

    let Some(caps) = RE_SUBDOMAIN_DIGITS.captures(url) else {
        return Ok(String::new());
    };

    let g = i32::from_str_radix(&format!("{}{}", &caps[2], &caps[1]), 16);

    if let Ok(g) = g {
        let mut gg = GG::inst().lock().await;
        let m_result = gg.m(g).await?;

        retval = if base_is_none_or_empty {
            format!("{retval}{}", 1 + m_result)
        } else {
            let c = char::from_u32((97 + m_result) as u32).context("Invalid character value")?;
            format!("{c}{}", base.unwrap_or(""))
        };
    };

    Ok(retval)
}

pub async fn url_from_url(
    url: &str,
    base: Option<&str>,
    dir: Option<&str>,
) -> anyhow::Result<String> {
    let subdomain = subdomain_from_url(url, base, dir).await?;
    let res_url =
        RE_SUBDOMAIN.replace(url, format!("//{subdomain}.gold-usergeneratedcontent.net/"));
    Ok(res_url.into_owned())
}

/// Make the next image url be built from a freshly fetched `gg.js`
pub async fn invalidate_gg() {
    GG::inst().lock().await.invalidate();
}

pub async fn full_path_from_hash(hash: &str) -> anyhow::Result<String> {
    let mut gg = GG::inst().lock().await;
    let b = gg.b().await?;
    let s = gg.s(hash)?;
    Ok(format!("{b}{s}/{hash}"))
}

pub fn real_full_path_from_hash(hash: &str) -> anyhow::Result<String> {
    let real_full_path = RE_HASH_DIRS
        .replace(hash, format!("$2/$1/{hash}"))
        .to_string();
    Ok(real_full_path)
}

pub async fn url_from_hash(
    _gallery_id: i32,
    image: &GalleryFiles,
    dir: Option<&str>,
    ext: Option<&str>,
) -> anyhow::Result<String> {
    let ext = match (ext, dir) {
        (Some(e), _) => e,
        (None, Some(d)) => d,
        (None, None) => image.name.rsplit('.').next().unwrap_or(""),
    };

    let mut url = String::from("https://a.gold-usergeneratedcontent.net/");

    if let Some(dir) = dir {
        if dir != "webp" && dir != "avif" {
            url.push_str(dir);
            url.push('/');
        }
    }

    url.push_str(&full_path_from_hash(&image.hash).await?);
    url.push('.');
    url.push_str(ext);

    Ok(url)
}

pub async fn url_from_url_from_hash(
    gallery_id: i32,
    image: &GalleryFiles,
    dir: Option<&str>,
    ext: Option<&str>,
    base: Option<&str>,
) -> anyhow::Result<String> {
    if base == Some("tn") {
        let real_path = real_full_path_from_hash(&image.hash)?;

        let Some(dir) = dir else {
            return Err(anyhow!(r#"if base is "tn", dir must not be None"#));
        };

        let Some(ext) = ext else {
            return Err(anyhow!(r#"if base is "tn", ext must not be None"#));
        };

        let url = format!("https://a.gold-usergeneratedcontent.net/{dir}/{real_path}.{ext}");
        url_from_url(&url, base, None).await
    } else {
        let url = url_from_hash(gallery_id, image, dir, ext).await?;
        url_from_url(&url, base, dir).await
    }
}

pub enum Ext {
    Webp,
    Avif,
}

pub async fn image_url_from_image(
    gallery_id: i32,
    image: &GalleryFiles,
    ext: Ext,
) -> anyhow::Result<String> {
    match ext {
        Ext::Webp => url_from_url_from_hash(gallery_id, image, Some("webp"), None, None).await,
        Ext::Avif => url_from_url_from_hash(gallery_id, image, Some("avif"), None, None).await,
    }
}

/// Turn the status of a gallery info response into a readable error
///
/// Without this the error page of the server would be handed to the json parser, which then
/// reports that html is not json and buries the actual reason. A removed gallery is the
/// common case: Hitomi keeps listing such ids among the related galleries of a comic.
fn check_gallery_status(gallery_id: i32, status: reqwest::StatusCode) -> anyhow::Result<()> {
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow!(
            "Gallery `{gallery_id}` does not exist on Hitomi, it was most likely removed"
        ));
    }

    if !status.is_success() {
        return Err(anyhow!(
            "Failed to get gallery info for `{gallery_id}`, the server answered with `{status}`"
        ));
    }

    Ok(())
}

pub async fn get_gallery_info(gallery_id: i32) -> anyhow::Result<GalleryInfo> {
    let client = HitomiClient::get_api_client();

    let url = format!("{PROTOCOL}//{DOMAIN}/galleries/{gallery_id}.js");
    let request = client.read().get(&url);
    let http_resp = request.send().await?;

    check_gallery_status(gallery_id, http_resp.status())?;

    let body = http_resp.text().await?;

    let json_str = body.replace("var galleryinfo = ", "");
    let gallery_info: GalleryInfo = serde_json::from_str(&json_str).context(format!(
        // Only the beginning of the body, so an unexpected answer cannot flood the log
        "Failed to parse the gallery info of `{gallery_id}`, the answer started with: {}",
        crate::utils::truncate_to_bytes(json_str.trim(), 200)
    ))?;
    Ok(gallery_info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removed_gallery_says_so() {
        let err = check_gallery_status(2_517_756, reqwest::StatusCode::NOT_FOUND).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("2517756"), "{message}");
        assert!(message.contains("does not exist"), "{message}");
        // The point of the check: no html and no json complaint in the message
        assert!(!message.contains("expected value"), "{message}");
    }

    #[test]
    fn another_failure_names_the_status() {
        let err = check_gallery_status(1, reqwest::StatusCode::INTERNAL_SERVER_ERROR).unwrap_err();

        assert!(err.to_string().contains("500"), "{err}");
    }

    fn gallery_info_with(related: &str, scene_indexes: &str) -> serde_json::Result<GalleryInfo> {
        serde_json::from_str(&format!(
            r#"{{"id": "2524092", "title": "t", "type": "manga", "date": "2023-01-01 00:00:00-05",
                "related": {related}, "scene_indexes": {scene_indexes}}}"#
        ))
    }

    #[test]
    fn related_ids_are_read_as_numbers_or_strings() {
        // the shape of gallery 2524092, which broke the search page it was on
        let gallery = gallery_info_with(r#"["726552", "141551", 2875132]"#, "[]").unwrap();
        assert_eq!(gallery.related, vec![726_552, 141_551, 2_875_132]);
        assert_eq!(gallery.id, 2_524_092);
    }

    #[test]
    fn scene_indexes_are_read_as_numbers_or_strings() {
        let gallery = gallery_info_with("[]", r#"[0, "4", " 12 "]"#).unwrap();
        assert_eq!(gallery.scene_indexes, vec![0, 4, 12]);
    }

    #[test]
    fn id_lists_skip_what_is_no_id() {
        let gallery =
            gallery_info_with(r#"["12", "", "abc", null, 1.5, 9999999999, 34]"#, "null").unwrap();
        assert_eq!(gallery.related, vec![12, 34]);
        assert!(gallery.scene_indexes.is_empty());

        // a missing list is an empty one, as before
        let gallery: GalleryInfo = serde_json::from_str(
            r#"{"id": 1, "title": "t", "type": "manga", "date": "2023-01-01 00:00:00-05"}"#,
        )
        .unwrap();
        assert!(gallery.related.is_empty());
        assert!(gallery.scene_indexes.is_empty());
    }

    #[test]
    fn a_reachable_gallery_passes() {
        assert!(check_gallery_status(1, reqwest::StatusCode::OK).is_ok());
    }

    #[test]
    fn subdomain_digits_come_from_the_end_of_the_hash() {
        let url = format!(
            "https://a.gold-usergeneratedcontent.net/1729083600/890/{}7a3.webp",
            "0".repeat(61)
        );
        let caps = RE_SUBDOMAIN_DIGITS.captures(&url).unwrap();
        assert_eq!((&caps[1], &caps[2]), ("7a", "3"));
    }

    #[test]
    fn subdomain_is_replaced() {
        for url in [
            "https://a.gold-usergeneratedcontent.net/x.webp",
            "https://ab.hitomi.la/x.webp",
        ] {
            let replaced = RE_SUBDOMAIN.replace(url, "//w2.gold-usergeneratedcontent.net/");
            assert_eq!(replaced, "https://w2.gold-usergeneratedcontent.net/x.webp");
        }
    }

    #[test]
    fn real_full_path_nests_the_hash_under_its_tail() {
        let hash = format!("{}7a3", "0".repeat(61));
        assert_eq!(
            real_full_path_from_hash(&hash).unwrap(),
            format!("3/7a/{hash}")
        );
    }
}
