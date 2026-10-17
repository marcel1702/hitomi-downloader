use std::{
    collections::HashMap,
    io::{Cursor, Read},
    ops::Range,
    sync::LazyLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::anyhow;
use byteorder::{BigEndian, ReadBytesExt};
use indexmap::IndexSet;
use parking_lot::Mutex;
use regex_lite::Regex;
use reqwest::{header::RANGE, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;

use crate::hitomi_client::HitomiClient;

use super::{DOMAIN, NOZOMI_EXTENSION, PROTOCOL};

//searchlib.js
const SEPARATOR: &str = "-";
const EXTENSION: &str = ".html";
const INDEX_DIR: &str = "tagindex";
const GALLERIES_INDEX_DIR: &str = "galleriesindex";
const MAX_NODE_SIZE: u64 = 464;
const B: usize = 16;
const COMPRESSED_NOZOMI_PREFIX: &str = "n";
const TAG_INDEX_DOMAIN: &str = "tagindex.hitomi.la";

/// How long a fetched index version is trusted
///
/// Hitomi renews its search index now and then and the web page fetches the version on
/// every visit, so the app must not hold on to one version for the whole session.
const INDEX_VERSION_TTL: Duration = Duration::from_secs(10 * 60);

/// The index versions fetched so far, with the time they were fetched, keyed by index directory
///
/// Only versions that were fetched successfully end up here, a failed fetch is tried again
/// by the next search instead of breaking the search until the app restarts.
static INDEX_VERSIONS: LazyLock<Mutex<HashMap<&'static str, (String, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The statuses Hitomi answers with when it refuses a request because of where it comes from
///
/// Hitomi blocks its web page for some regions, the EU among them, and answers with 404
/// there. The data servers the app talks to are not blocked at the moment, but would most
/// likely answer the same way if they were.
const REGION_BLOCK_STATUSES: [StatusCode; 3] = [
    StatusCode::FORBIDDEN,
    StatusCode::NOT_FOUND,
    StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
];
const REGION_BLOCK_HINT: &str =
    "Hitomi may be blocking access from your region, try a SOCKS5 proxy in the settings";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Node {
    keys: Vec<Vec<u8>>,
    datas: Vec<(i64, i32)>,
    sub_node_addresses: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct Suggestion {
    s: String,
    t: i32,
    u: String,
    n: String,
}

fn sha256(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

fn hash_term(term: &str) -> Vec<u8> {
    sha256(term.as_bytes())[..4].to_vec()
}

fn sanitize(input: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[/#]").expect("valid regex"));
    RE.replace_all(input, "").to_string()
}

async fn get_index_version(name: &str) -> anyhow::Result<String> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let client = HitomiClient::get_api_client();
    let url = format!("{PROTOCOL}//{DOMAIN}/{name}/version?_={timestamp}");
    let request = client.read().get(&url);
    let http_resp = request.send().await?;
    check_index_version_status(name, http_resp.status())?;
    parse_index_version(&http_resp.text().await?)
}

/// Check the status of the answer to an index version request
///
/// The version file always exists, so a 404 for it is no missing file but most likely a block.
fn check_index_version_status(name: &str, status: StatusCode) -> anyhow::Result<()> {
    if status == StatusCode::OK {
        Ok(())
    } else if REGION_BLOCK_STATUSES.contains(&status) {
        Err(anyhow!(
            "Failed to get the version of the search index `{name}`, the server answered with `{status}`. {REGION_BLOCK_HINT}"
        ))
    } else {
        Err(anyhow!(
            "Failed to get the version of the search index `{name}`, the server answered with `{status}`"
        ))
    }
}

/// Read the index version from the answer, which is nothing but a number
///
/// Anything else, an error page above all, must never end up in an index url.
fn parse_index_version(body: &str) -> anyhow::Result<String> {
    let version = body.trim();
    if version.is_empty() || !version.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(anyhow!(
            "The search index version is not a number, the answer started with: {}",
            crate::utils::truncate_to_bytes(version, 100)
        ));
    }
    Ok(version.to_string())
}

/// Check if an index version fetched at `fetched_at` can still be used at `now`
fn is_fresh(fetched_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(fetched_at) < INDEX_VERSION_TTL
}

/// The version of the search index in the directory `name`, fetched again once it is too old
async fn index_version(name: &'static str) -> anyhow::Result<String> {
    if let Some(version) = cached_index_version(name, Instant::now()) {
        return Ok(version);
    }
    let version = get_index_version(name).await?;
    INDEX_VERSIONS
        .lock()
        .insert(name, (version.clone(), Instant::now()));
    Ok(version)
}

fn cached_index_version(name: &str, now: Instant) -> Option<String> {
    let versions = INDEX_VERSIONS.lock();
    let (version, fetched_at) = versions.get(name)?;
    is_fresh(*fetched_at, now).then(|| version.clone())
}

/// Forget the fetched index versions, so the next search fetches them again
///
/// Called when the index answers that a file is gone, which happens after Hitomi renewed it,
/// and when the proxy changes, so the versions are fetched over the new route.
pub fn invalidate_index_versions() {
    INDEX_VERSIONS.lock().clear();
}

fn encode_search_query_for_url(s: char) -> String {
    match s {
        ' ' => "_".to_string(),
        '/' => "slash".to_string(),
        '.' => "dot".to_string(),
        _ => s.to_string(),
    }
}

async fn get_url_at_range(url: &str, range: Range<u64>) -> anyhow::Result<Vec<u8>> {
    let start = range.start;
    let end = range.end - 1;
    let range_header = format!("bytes={start}-{end}");

    let client = HitomiClient::get_api_client();

    let request = client.read().get(url).header(RANGE, range_header);
    let http_resp = request.send().await?;
    let status = http_resp.status();
    if matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::RANGE_NOT_SATISFIABLE
    ) {
        // The file of the version in use is gone, the next search has to fetch the new version
        invalidate_index_versions();
    }
    let body = http_resp.bytes().await?;

    range_body(url, status, &body, range)
}

/// The requested `range` of `url` out of the answer to a range request
///
/// A server is free to ignore the range and send the whole file with `200 OK`. Reading the
/// start of that file instead of the requested node would hand the search the root node
/// over and over again, so the range is cut out here instead.
fn range_body(
    url: &str,
    status: StatusCode,
    body: &[u8],
    range: Range<u64>,
) -> anyhow::Result<Vec<u8>> {
    match status {
        StatusCode::PARTIAL_CONTENT => Ok(body.to_vec()),
        StatusCode::OK => {
            let len = body.len();
            let start = usize::try_from(range.start).unwrap_or(usize::MAX);
            let end = usize::try_from(range.end).unwrap_or(usize::MAX).min(len);
            if start >= len {
                return Err(anyhow!(
                    "`{url}` has only {len} bytes, it has nothing at byte {start}"
                ));
            }
            Ok(body[start..end].to_vec())
        }
        StatusCode::NOT_FOUND | StatusCode::RANGE_NOT_SATISFIABLE => Err(anyhow!(
            "`{url}` answered with `{status}`, the search index was probably renewed in the meantime, please search again. If it keeps failing: {REGION_BLOCK_HINT}"
        )),
        status if REGION_BLOCK_STATUSES.contains(&status) => Err(anyhow!(
            "`{url}` answered with `{status}`. {REGION_BLOCK_HINT}"
        )),
        status => Err(anyhow!("`{url}` answered with `{status}`")),
    }
}

fn decode_node(data: &[u8]) -> anyhow::Result<Node> {
    let mut cursor = Cursor::new(data);

    let number_of_keys = cursor.read_i32::<BigEndian>()?;
    let mut keys = Vec::new();

    for _ in 0..number_of_keys {
        let key_size = cursor.read_i32::<BigEndian>()?;
        if key_size == 0 || key_size > 32 {
            return Err(anyhow!("fatal: !keySize || keySize > 32"));
        }
        #[allow(clippy::cast_sign_loss)]
        let mut key = vec![0u8; key_size as usize];
        cursor.read_exact(&mut key)?;
        keys.push(key);
    }

    let number_of_datas = cursor.read_i32::<BigEndian>()?;
    let mut datas = Vec::new();

    for _ in 0..number_of_datas {
        let offset = cursor.read_i64::<BigEndian>()?;
        let length = cursor.read_i32::<BigEndian>()?;
        datas.push((offset, length));
    }

    let mut sub_node_addresses = Vec::new();
    for _ in 0..=B {
        let address = cursor.read_i64::<BigEndian>()?;
        sub_node_addresses.push(address);
    }

    Ok(Node {
        keys,
        datas,
        sub_node_addresses,
    })
}

async fn get_node_at_address(field: &str, address: i64) -> anyhow::Result<Option<Node>> {
    // Only the version of the index the field lives in is needed
    let url = match field {
        "galleries" | "languages" | "nozomiurl" => {
            let galleries_index_version = index_version(GALLERIES_INDEX_DIR).await?;
            format!(
                "{PROTOCOL}//{DOMAIN}/{GALLERIES_INDEX_DIR}/{field}.{galleries_index_version}.index"
            )
        }
        _ => {
            let tag_index_version = index_version(INDEX_DIR).await?;
            format!("{PROTOCOL}//{DOMAIN}/{INDEX_DIR}/{field}.{tag_index_version}.index")
        }
    };

    #[allow(clippy::cast_sign_loss)]
    let nodedata = get_url_at_range(&url, address as u64..(address as u64 + MAX_NODE_SIZE)).await?;
    Ok(Some(decode_node(&nodedata)?))
}

fn compare_arrays(a: &[u8], b: &[u8]) -> i32 {
    let top = std::cmp::min(a.len(), b.len());
    for i in 0..top {
        #[allow(clippy::comparison_chain)]
        if a[i] < b[i] {
            return -1;
        } else if a[i] > b[i] {
            return 1;
        }
    }
    0
}

fn locate_key(key: &[u8], node: &Node) -> (bool, usize) {
    for (i, node_key) in node.keys.iter().enumerate() {
        let cmp_result = compare_arrays(key, node_key);
        if cmp_result <= 0 {
            return (cmp_result == 0, i);
        }
    }
    (false, node.keys.len())
}

fn is_leaf(node: &Node) -> bool {
    node.sub_node_addresses.iter().all(|&addr| addr == 0)
}

async fn b_search(field: &str, key: &[u8], node: &Node) -> anyhow::Result<Option<(i64, i32)>> {
    if node.keys.is_empty() {
        return Ok(None);
    }

    let (there, where_idx) = locate_key(key, node);
    if there {
        return Ok(Some(node.datas[where_idx]));
    } else if is_leaf(node) {
        return Ok(None);
    }

    if let Some(next_node) = get_node_at_address(field, node.sub_node_addresses[where_idx]).await? {
        // use Box::pin to wrap the Future of the recursive call
        return Box::pin(b_search(field, key, &next_node)).await;
    }
    Ok(None)
}

async fn get_gallery_ids_from_data(data: (i64, i32)) -> anyhow::Result<IndexSet<i32>> {
    let galleries_index_version = index_version(GALLERIES_INDEX_DIR).await?;

    let url = format!(
        "{PROTOCOL}//{DOMAIN}/{GALLERIES_INDEX_DIR}/galleries.{galleries_index_version}.data"
    );
    let (offset, length) = data;

    if length > 100_000_000 || length <= 0 {
        return Err(anyhow!("length `{length}` is too long"));
    }

    #[allow(clippy::cast_sign_loss)]
    #[allow(clippy::cast_lossless)]
    let inbuf = get_url_at_range(&url, offset as u64..((offset + length as i64) as u64)).await?;

    let mut cursor = Cursor::new(inbuf);
    let number_of_gallery_ids = cursor.read_i32::<BigEndian>()?;

    if number_of_gallery_ids > 10_000_000 || number_of_gallery_ids <= 0 {
        return Err(anyhow!(
            "number_of_galleryids `{number_of_gallery_ids}` is too long"
        ));
    }

    let mut gallery_ids = IndexSet::new();
    for _ in 0..number_of_gallery_ids {
        gallery_ids.insert(cursor.read_i32::<BigEndian>()?);
    }

    Ok(gallery_ids)
}

/// Check if the server delivered the nozomi file at `nozomi_address`
///
/// A tag nobody used has no nozomi file, which is a search without results.
/// Any other failure must not pass for "no results", it fails the search instead.
fn nozomi_file_exists(nozomi_address: &str, status: StatusCode) -> anyhow::Result<bool> {
    if status == StatusCode::NOT_FOUND {
        Ok(false)
    } else if status == StatusCode::OK {
        Ok(true)
    } else {
        Err(anyhow!(
            "Failed to get `{nozomi_address}`, the server answered with `{status}`"
        ))
    }
}

pub(crate) async fn get_gallery_ids_from_nozomi(
    area: Option<&str>,
    tag: &str,
    language: &str,
) -> anyhow::Result<IndexSet<i32>> {
    let nozomi_address = match area {
        None => format!(
            "{PROTOCOL}//{DOMAIN}/{COMPRESSED_NOZOMI_PREFIX}/{tag}-{language}{NOZOMI_EXTENSION}"
        ),
        Some(area) => format!(
            "{PROTOCOL}//{DOMAIN}/{COMPRESSED_NOZOMI_PREFIX}/{area}/{tag}-{language}{NOZOMI_EXTENSION}"
        ),
    };

    let client = HitomiClient::get_api_client();

    let request = client.read().get(&nozomi_address);
    let http_resp = request.send().await?;
    if !nozomi_file_exists(&nozomi_address, http_resp.status())? {
        return Ok(IndexSet::new());
    }

    let bytes = http_resp.bytes().await?;

    let mut cursor = Cursor::new(bytes);
    let mut nozomi = IndexSet::new();

    while let Ok(id) = cursor.read_i32::<BigEndian>() {
        nozomi.insert(id);
    }

    Ok(nozomi)
}

pub(crate) async fn get_gallery_ids_for_query(query: &str) -> anyhow::Result<IndexSet<i32>> {
    let query = query.replace('_', " ");

    if let Some(colon_idx) = query.find(':') {
        let (ns, tag) = query.split_at(colon_idx);
        let tag = &tag[1..];

        let (area, language, tag) = match ns {
            "female" | "male" => (Some("tag"), "all", query.to_string()),
            "language" => (None, tag, "index".to_string()),
            _ => (Some(ns), "all", tag.to_string()),
        };

        return get_gallery_ids_from_nozomi(area, &tag, language).await;
    }

    let key = hash_term(&query);
    let field = "galleries";

    if let Some(node) = get_node_at_address(field, 0).await? {
        if let Some(data) = b_search(field, &key, &node).await? {
            return get_gallery_ids_from_data(data).await;
        }
    }

    Ok(IndexSet::new())
}

pub async fn get_suggestions_for_query(query: &str) -> anyhow::Result<Vec<Suggestion>> {
    let query = query.replace('_', " ");
    let (field, term) = if let Some(colon_idx) = query.find(':') {
        let (field, term) = query.split_at(colon_idx);
        (field, &term[1..])
    } else {
        ("global", query.as_str())
    };

    let chars_path_segment = term
        .chars()
        .map(encode_search_query_for_url)
        .collect::<Vec<String>>()
        .join("/");

    let url = if chars_path_segment.is_empty() {
        format!("{PROTOCOL}//{TAG_INDEX_DOMAIN}/{field}.json")
    } else {
        format!("{PROTOCOL}//{TAG_INDEX_DOMAIN}/{field}/{chars_path_segment}.json")
    };

    let client = HitomiClient::get_api_client();
    let request = client.read().get(&url);

    let http_resp = request.send().await?;
    let status = http_resp.status();
    if status == StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    } else if status != StatusCode::OK {
        let body = http_resp.text().await?;
        return Err(anyhow!("Unexpected status code({status}): {body}"));
    }
    let body = http_resp.text().await?;

    let suggestions: serde_json::Value = serde_json::from_str(&body)?;

    let mut result = Vec::new();
    if let serde_json::Value::Array(suggestions) = suggestions {
        for suggestion in suggestions {
            if let serde_json::Value::Array(suggestion) = suggestion {
                if suggestion.len() < 3 {
                    continue;
                }

                let ns = suggestion[2].as_str().unwrap_or("").to_string();
                let tagname = sanitize(suggestion[0].as_str().unwrap_or(""));

                let url = match ns.as_str() {
                    "female" | "male" => format!("/tag/{ns}:{tagname}{SEPARATOR}1{EXTENSION}"),
                    "language" => format!("/index-{tagname}{SEPARATOR}1{EXTENSION}"),
                    _ => format!("/{ns}/{tagname}{SEPARATOR}all{SEPARATOR}1{EXTENSION}"),
                };

                result.push(Suggestion {
                    s: suggestion[0].as_str().unwrap_or("").to_string(),
                    #[allow(clippy::cast_possible_truncation)]
                    t: suggestion[1].as_i64().unwrap_or(0) as i32,
                    u: url,
                    n: ns,
                });
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "https://example.test/tag/unknown-all.nozomi";

    #[test]
    fn unknown_tag_has_no_results() {
        assert!(!nozomi_file_exists(ADDRESS, StatusCode::NOT_FOUND).unwrap());
    }

    #[test]
    fn delivered_file_is_read() {
        assert!(nozomi_file_exists(ADDRESS, StatusCode::OK).unwrap());
    }

    #[test]
    fn server_failure_fails_the_search() {
        for status in [
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let err = nozomi_file_exists(ADDRESS, status).unwrap_err();
            assert!(err.to_string().contains(status.as_str()), "{err}");
        }
    }

    #[test]
    fn index_version_is_a_number() {
        assert_eq!(parse_index_version("1729083912\n").unwrap(), "1729083912");
        assert_eq!(parse_index_version("  42 ").unwrap(), "42");
    }

    #[test]
    fn anything_else_is_no_index_version() {
        for body in [
            "",
            "  \n",
            "<!DOCTYPE html><html>Not Found</html>",
            "12a",
            "-5",
        ] {
            assert!(parse_index_version(body).is_err(), "`{body}` was accepted");
        }
    }

    #[test]
    fn blocked_version_request_points_at_the_proxy() {
        assert!(check_index_version_status("galleriesindex", StatusCode::OK).is_ok());

        for status in REGION_BLOCK_STATUSES {
            let err = check_index_version_status("galleriesindex", status).unwrap_err();
            let message = err.to_string();
            assert!(message.contains(status.as_str()), "{message}");
            assert!(message.contains("SOCKS5"), "{message}");
        }

        let err = check_index_version_status("galleriesindex", StatusCode::BAD_GATEWAY)
            .unwrap_err()
            .to_string();
        assert!(err.contains("502") && !err.contains("SOCKS5"), "{err}");
    }

    const INDEX_URL: &str = "https://example.test/galleriesindex/galleries.1.index";

    #[test]
    fn partial_content_is_used_as_it_is() {
        let body = range_body(INDEX_URL, StatusCode::PARTIAL_CONTENT, b"node", 464..928).unwrap();
        assert_eq!(body, b"node");
    }

    #[test]
    fn ignored_range_is_cut_out_of_the_whole_file() {
        let file: Vec<u8> = (0..=255).collect();

        let body = range_body(INDEX_URL, StatusCode::OK, &file, 16..20).unwrap();
        assert_eq!(body, vec![16, 17, 18, 19]);
        // a node at the end of the file is shorter than the requested range
        let body = range_body(INDEX_URL, StatusCode::OK, &file, 250..714).unwrap();
        assert_eq!(body, vec![250, 251, 252, 253, 254, 255]);
        assert!(range_body(INDEX_URL, StatusCode::OK, &file, 256..720).is_err());
    }

    #[test]
    fn missing_index_file_asks_to_search_again() {
        for status in [StatusCode::NOT_FOUND, StatusCode::RANGE_NOT_SATISFIABLE] {
            let err = range_body(INDEX_URL, status, b"<html>", 0..464)
                .unwrap_err()
                .to_string();
            assert!(err.contains("renewed"), "{err}");
        }
    }

    #[test]
    fn refused_index_request_points_at_the_proxy() {
        let err = range_body(INDEX_URL, StatusCode::FORBIDDEN, b"<html>", 0..464)
            .unwrap_err()
            .to_string();
        assert!(err.contains("403") && err.contains("SOCKS5"), "{err}");

        let err = range_body(INDEX_URL, StatusCode::INTERNAL_SERVER_ERROR, b"", 0..464)
            .unwrap_err()
            .to_string();
        assert!(err.contains("500") && !err.contains("SOCKS5"), "{err}");
    }

    #[test]
    fn index_version_expires() {
        let fetched_at = Instant::now();
        assert!(is_fresh(fetched_at, fetched_at + Duration::from_secs(60)));
        assert!(!is_fresh(
            fetched_at,
            fetched_at + Duration::from_secs(11 * 60)
        ));
    }

    /// One test for everything touching the shared cache, so parallel tests can not interfere
    #[test]
    fn cached_index_version_is_used_until_it_expires_or_is_invalidated() {
        let fetched_at = Instant::now();
        INDEX_VERSIONS
            .lock()
            .insert(GALLERIES_INDEX_DIR, ("1729083912".to_string(), fetched_at));

        assert_eq!(
            cached_index_version(GALLERIES_INDEX_DIR, fetched_at + Duration::from_secs(60)),
            Some("1729083912".to_string())
        );
        assert_eq!(
            cached_index_version(
                GALLERIES_INDEX_DIR,
                fetched_at + Duration::from_secs(11 * 60)
            ),
            None
        );
        // the other index has its own version
        assert_eq!(cached_index_version(INDEX_DIR, fetched_at), None);

        invalidate_index_versions();
        assert_eq!(cached_index_version(GALLERIES_INDEX_DIR, fetched_at), None);
    }

    #[test]
    fn sanitize_drops_url_separators() {
        assert_eq!(sanitize("fate/stay night #1"), "fatestay night 1");
        assert_eq!(sanitize("plain tag"), "plain tag");
    }
}
