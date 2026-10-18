use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context as _};
use bytes::Bytes;
use parking_lot::RwLock;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Client, StatusCode};
use reqwest_middleware::ClientWithMiddleware;
use reqwest_retry::{policies::ExponentialBackoff, Jitter, RetryTransientMiddleware};

use crate::{
    context::Context,
    extensions::AnyhowErrorToStringChain,
    hitomi::{self, GalleryInfo, Suggestion},
    types::{Comic, SearchResult, SearchSort},
};

/// Time allowed to establish a connection for an api request
const API_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Time an api response may stall between two reads
const API_READ_TIMEOUT: Duration = Duration::from_secs(15);
/// Time allowed to establish a connection for an image request
const IMG_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Time an image response may stall between two reads
///
/// Without it a connection that stops sending holds its download slot forever,
/// and the retries never get a chance to run.
const IMG_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The server answered an image request with a status other than `200 OK`
#[derive(Debug)]
pub struct UnexpectedStatus {
    pub status: StatusCode,
    pub body: String,
}

impl std::fmt::Display for UnexpectedStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Unexpected status code({}): {}", self.status, self.body)
    }
}

impl std::error::Error for UnexpectedStatus {}

/// Whether `err` says that an image is not where its url points, which a stale `gg.js` causes
pub fn is_stale_url_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<UnexpectedStatus>()
        .is_some_and(|err| matches!(err.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND))
}

/// Download an image with `fetch` from the url `url` builds right before the request
///
/// The url depends on `gg.js`, which Hitomi rotates. A url built from an older copy points
/// at a path that is not there any more. If the server says the image is not there, `refresh`
/// makes the next url come from a fresh `gg.js`, and the url is built once more before giving
/// up. The download and the viewer both fetch their images this way.
pub async fn fetch_with_fresh_url<Url, Fetch, Refresh>(
    url: impl Fn() -> Url,
    fetch: impl Fn(String) -> Fetch,
    refresh: impl FnOnce() -> Refresh,
) -> anyhow::Result<Bytes>
where
    Url: std::future::Future<Output = anyhow::Result<String>>,
    Fetch: std::future::Future<Output = anyhow::Result<Bytes>>,
    Refresh: std::future::Future<Output = ()>,
{
    let first_url = url().await?;
    let err = match fetch(first_url.clone()).await {
        Ok(data) => return Ok(data),
        Err(err) => err,
    };
    if !is_stale_url_error(&err) {
        return Err(err).context(format!("Failed to download `{first_url}`"));
    }

    tracing::debug!(
        url = first_url,
        "Image url looks stale, building it again from a fresh gg.js"
    );
    refresh().await;
    let fresh_url = url().await?;
    fetch(fresh_url.clone())
        .await
        .context(format!("Failed to download `{fresh_url}`"))
}

#[derive(Clone)]
pub struct HitomiClient {
    context: Context,
    api_client: Arc<RwLock<ClientWithMiddleware>>,
    img_client: Arc<RwLock<ClientWithMiddleware>>,
    cover_client: Arc<RwLock<Client>>,
}

impl HitomiClient {
    pub fn new(context: Context) -> Self {
        let api_client = create_api_client(&context);
        let api_client = Arc::new(RwLock::new(api_client));

        let img_client = create_img_client(&context);
        let img_client = Arc::new(RwLock::new(img_client));

        let cover_client = create_cover_client(&context);
        let cover_client = Arc::new(RwLock::new(cover_client));

        Self {
            context,
            api_client,
            img_client,
            cover_client,
        }
    }

    pub fn get_api_client() -> Arc<RwLock<ClientWithMiddleware>> {
        Context::global().hitomi_client().api_client.clone()
    }

    pub fn reload_client(&self) {
        // Versions fetched over the old route, or the failure to fetch them, say nothing
        // about the new one
        hitomi::invalidate_index_versions();

        let api_client = create_api_client(&self.context);
        *self.api_client.write() = api_client;

        let img_client = create_img_client(&self.context);
        *self.img_client.write() = img_client;

        let cover_client = create_cover_client(&self.context);
        *self.cover_client.write() = cover_client;
    }

    pub async fn search(
        &self,
        query: &str,
        page_num: usize,
        sort: SearchSort,
    ) -> anyhow::Result<SearchResult> {
        let ids = hitomi::do_search(query.to_string(), sort).await?;

        let search_result = self.get_page(ids.into_iter().collect(), page_num).await?;

        Ok(search_result)
    }

    pub async fn get_page(&self, ids: Vec<i32>, page_num: usize) -> anyhow::Result<SearchResult> {
        const PAGE_SIZE: usize = 25;

        // Calculate total pages by ceiling division
        let total_page = ids.len().div_ceil(PAGE_SIZE);
        let Some(skipped_pages) = page_num.checked_sub(1) else {
            return Err(anyhow!("There is no page {page_num}, the first page is 1"));
        };

        let get_gallery_info_tasks = ids
            .iter()
            .skip(skipped_pages.saturating_mul(PAGE_SIZE))
            .take(PAGE_SIZE)
            .map(|id| async move {
                hitomi::get_gallery_info(*id)
                    .await
                    .context(format!("Failed to get gallery info for `{id}`"))
            });
        let results = futures::future::join_all(get_gallery_info_tasks).await;
        let gallery_infos = gallery_infos_of_page(results)?;

        let search_result = SearchResult::from_gallery_infos(
            &self.context,
            gallery_infos,
            page_num,
            total_page,
            ids,
        )
        .await?;

        Ok(search_result)
    }

    pub async fn get_comic(&self, id: i32) -> anyhow::Result<Comic> {
        let gallery = hitomi::get_gallery_info(id)
            .await
            .context(format!("Failed to get gallery info for `{id}`"))?;

        let comic = Comic::from_gallery_info(&self.context, gallery).await?;
        Ok(comic)
    }

    pub async fn get_img_data(&self, url: &str) -> anyhow::Result<Bytes> {
        let request = self
            .img_client
            .read()
            .get(url)
            .header("referer", "https://hitomi.la/");
        let http_resp = request.send().await?;
        // check http response status code
        let status = http_resp.status();
        if status == StatusCode::SERVICE_UNAVAILABLE {
            return Err(anyhow!("Failed after multiple retries, try again later"));
        } else if status != StatusCode::OK {
            let body = http_resp.text().await?;
            return Err(UnexpectedStatus { status, body }.into());
        }
        // get image data
        let img_data = http_resp.bytes().await?;
        Ok(img_data)
    }

    pub async fn get_search_suggestions(&self, query: &str) -> anyhow::Result<Vec<Suggestion>> {
        let suggestion = hitomi::get_suggestions_for_query(query).await?;
        Ok(suggestion)
    }

    pub async fn get_cover_data(&self, cover_url: &str) -> anyhow::Result<Bytes> {
        let request = self
            .cover_client
            .read()
            .get(cover_url)
            .header("referer", "https://hitomi.la/");
        let http_resp = request.send().await?;
        // check http response status code
        let status = http_resp.status();
        if status != StatusCode::OK {
            let body = http_resp.text().await?;
            return Err(anyhow!("Unexpected status code({status}): {body}"));
        }
        let cover_data = http_resp.bytes().await?;
        Ok(cover_data)
    }
}

/// The galleries of a search page that could be fetched, skipping the ones that could not
///
/// One removed or unreadable gallery must not hide the other comics on its page. Only when
/// every gallery failed, which points at the connection rather than a gallery, the page fails.
fn gallery_infos_of_page(
    results: Vec<anyhow::Result<GalleryInfo>>,
) -> anyhow::Result<Vec<GalleryInfo>> {
    let (gallery_infos, errs): (Vec<_>, Vec<_>) = results.into_iter().partition(Result::is_ok);
    let gallery_infos: Vec<GalleryInfo> =
        gallery_infos.into_iter().filter_map(Result::ok).collect();
    let mut errs = errs.into_iter().filter_map(Result::err);

    if gallery_infos.is_empty() {
        // The page fails with one error instead of one per gallery, each error is shown once
        return match errs.next() {
            Some(err) => Err(err),
            None => Ok(gallery_infos),
        };
    }
    for err in errs {
        let err_title = "Failed to load a comic of the page, skipping it";
        let string_chain = err.to_string_chain();
        tracing::error!(err_title, message = string_chain);
    }
    Ok(gallery_infos)
}

fn create_api_client(context: &Context) -> ClientWithMiddleware {
    let retry_policy = ExponentialBackoff::builder()
        .base(1)
        .jitter(Jitter::Bounded)
        .build_with_total_retry_duration(Duration::from_secs(5));

    let client = reqwest::ClientBuilder::new()
        .set_proxy(context, "api_client")
        // No limit on the whole request: the nozomi index files are several megabytes,
        // which a slow line or a proxy can not deliver in a few seconds
        .connect_timeout(API_CONNECT_TIMEOUT)
        .read_timeout(API_READ_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    reqwest_middleware::ClientBuilder::new(client)
        .with(RetryTransientMiddleware::new_with_policy(retry_policy))
        .build()
}

fn create_img_client(context: &Context) -> ClientWithMiddleware {
    let retry_policy = ExponentialBackoff::builder()
        .base(1)
        .jitter(Jitter::Bounded)
        .build_with_max_retries(20);

    let client = reqwest::ClientBuilder::new()
        .set_proxy(context, "img_client")
        .connect_timeout(IMG_CONNECT_TIMEOUT)
        .read_timeout(IMG_READ_TIMEOUT)
        .build()
        .unwrap();

    reqwest_middleware::ClientBuilder::new(client)
        .with(RetryTransientMiddleware::new_with_policy(retry_policy))
        .build()
}

fn create_cover_client(context: &Context) -> Client {
    reqwest::ClientBuilder::new()
        .set_proxy(context, "cover_client")
        .connect_timeout(IMG_CONNECT_TIMEOUT)
        .read_timeout(IMG_READ_TIMEOUT)
        .build()
        .unwrap()
}

/// Resolves hostnames with the IPv4 addresses first
///
/// With local DNS the SOCKS5 target is the first address the resolver returns. reqwest hands
/// an IPv6 address on to the proxy as a bracketed hostname, which no SOCKS5 proxy can resolve,
/// so an IPv4 address has to come first whenever the host has one.
struct Ipv4FirstResolver;

impl Resolve for Ipv4FirstResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let addrs: Addrs = Box::new(ipv4_first(addrs).into_iter());
            Ok(addrs)
        })
    }
}

fn ipv4_first(addrs: impl IntoIterator<Item = SocketAddr>) -> Vec<SocketAddr> {
    let (mut ipv4_addrs, ipv6_addrs): (Vec<_>, Vec<_>) =
        addrs.into_iter().partition(SocketAddr::is_ipv4);
    ipv4_addrs.extend(ipv6_addrs);
    ipv4_addrs
}

trait ClientBuilderExt {
    fn set_proxy(self, context: &Context, client_name: &str) -> Self;
}

impl ClientBuilderExt for reqwest::ClientBuilder {
    fn set_proxy(self, context: &Context, client_name: &str) -> reqwest::ClientBuilder {
        let proxy_config = context.config().read().proxy.clone();
        // never pick up a proxy from the environment or the OS settings
        let builder = self.no_proxy();
        if !proxy_config.enabled {
            return builder;
        }

        match proxy_config
            .to_url()
            .and_then(|url| Ok(reqwest::Proxy::all(url)?))
        {
            Ok(proxy) if proxy_config.remote_dns => builder.proxy(proxy),
            Ok(proxy) => builder
                .proxy(proxy)
                .dns_resolver(Arc::new(Ipv4FirstResolver)),
            Err(err) => {
                // Route through an unreachable proxy rather than connecting directly,
                // so a broken proxy config fails loudly instead of leaking traffic
                let err_title = format!(
                    "{client_name} failed to set SOCKS5 proxy `{}:{}`, all requests will fail",
                    proxy_config.host, proxy_config.port
                );
                let string_chain = err.to_string_chain();
                tracing::error!(err_title, message = string_chain);
                let blackhole = reqwest::Proxy::all("socks5h://127.0.0.1:0")
                    .expect("static proxy url is valid");
                builder.proxy(blackhole)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gallery_info(id: i32) -> anyhow::Result<GalleryInfo> {
        let json = format!(
            r#"{{"id": {id}, "title": "t", "type": "manga", "date": "2023-01-01 00:00:00-05"}}"#
        );
        Ok(serde_json::from_str(&json)?)
    }

    fn status_error(status: StatusCode) -> anyhow::Error {
        UnexpectedStatus {
            status,
            body: String::new(),
        }
        .into()
    }

    #[test]
    fn missing_image_counts_as_stale_url() {
        assert!(is_stale_url_error(&status_error(StatusCode::NOT_FOUND)));
        assert!(is_stale_url_error(&status_error(StatusCode::FORBIDDEN)));
        // still recognised once a caller added context
        let wrapped = status_error(StatusCode::NOT_FOUND).context("Failed to download");
        assert!(is_stale_url_error(&wrapped));
    }

    #[test]
    fn other_failures_are_not_retried_with_a_fresh_url() {
        assert!(!is_stale_url_error(&status_error(
            StatusCode::INTERNAL_SERVER_ERROR
        )));
        assert!(!is_stale_url_error(&status_error(
            StatusCode::TOO_MANY_REQUESTS
        )));
        assert!(!is_stale_url_error(&anyhow!("connection reset")));
    }

    /// `gg.js` as Hitomi serves it, which rotates, and the images it points at
    struct FakeHitomi {
        /// The copy of `gg.js` the client knows
        known: std::sync::atomic::AtomicU32,
        /// The one on the server, the only one whose urls work
        served: std::sync::atomic::AtomicU32,
        requests: parking_lot::Mutex<Vec<String>>,
    }

    impl FakeHitomi {
        fn new(version: u32) -> Self {
            Self {
                known: version.into(),
                served: version.into(),
                requests: parking_lot::Mutex::new(Vec::new()),
            }
        }

        /// An image fetched as the viewer and the download fetch it
        async fn fetch(&self) -> anyhow::Result<Bytes> {
            fetch_with_fresh_url(
                || async {
                    Ok(format!(
                        "https://a.example/{}/page.webp",
                        self.known.load(SeqCst)
                    ))
                },
                |url| async move {
                    self.requests.lock().push(url.clone());
                    if url.contains(&format!("/{}/", self.served.load(SeqCst))) {
                        Ok(Bytes::from_static(b"image"))
                    } else {
                        Err(UnexpectedStatus {
                            status: StatusCode::NOT_FOUND,
                            body: String::new(),
                        }
                        .into())
                    }
                },
                || async { self.known.store(self.served.load(SeqCst), SeqCst) },
            )
            .await
        }
    }

    use std::sync::atomic::Ordering::SeqCst;

    #[tokio::test]
    async fn an_image_is_fetched_again_from_a_fresh_gg_js_once_it_rotated() {
        let fake = FakeHitomi::new(1);
        assert_eq!(fake.fetch().await.unwrap(), "image");
        // Hitomi rotates gg.js, the copy the client has is stale now
        fake.served.store(2, SeqCst);
        assert_eq!(fake.fetch().await.unwrap(), "image");
        assert_eq!(
            *fake.requests.lock(),
            [
                "https://a.example/1/page.webp",
                "https://a.example/1/page.webp",
                "https://a.example/2/page.webp"
            ]
        );
        // and the fresh copy is kept
        fake.fetch().await.unwrap();
        assert_eq!(fake.requests.lock().len(), 4);
    }

    #[tokio::test]
    async fn an_image_that_is_missing_with_a_fresh_gg_js_too_fails_after_one_more_try() {
        let fake = FakeHitomi::new(1);
        fake.served.store(3, SeqCst);
        // refreshing finds 3, but the image is not there under 3 either
        fake.known.store(1, SeqCst);
        let err = fetch_with_fresh_url(
            || async { Ok("https://a.example/1/page.webp".to_string()) },
            |url| async {
                fake.requests.lock().push(url);
                Err(UnexpectedStatus {
                    status: StatusCode::NOT_FOUND,
                    body: String::new(),
                }
                .into())
            },
            || async {},
        )
        .await
        .unwrap_err();
        assert!(is_stale_url_error(&err));
        assert_eq!(fake.requests.lock().len(), 2);
    }

    #[tokio::test]
    async fn other_failures_are_not_tried_again() {
        let refreshed = std::sync::atomic::AtomicBool::new(false);
        let err = fetch_with_fresh_url(
            || async { Ok("https://a.example/1/page.webp".to_string()) },
            |_| async { Err(anyhow!("connection reset")) },
            || async { refreshed.store(true, SeqCst) },
        )
        .await
        .unwrap_err();
        assert!(err.to_string_chain().contains("connection reset"));
        assert!(!refreshed.load(SeqCst));
    }

    fn ids_of(gallery_infos: &[GalleryInfo]) -> Vec<i32> {
        gallery_infos.iter().map(|gallery| gallery.id).collect()
    }

    #[test]
    fn a_failed_gallery_does_not_hide_the_rest_of_the_page() {
        let results = vec![
            gallery_info(1),
            Err(anyhow!("Gallery `2` does not exist on Hitomi")),
            gallery_info(3),
        ];
        let gallery_infos = gallery_infos_of_page(results).unwrap();
        assert_eq!(ids_of(&gallery_infos), [1, 3]);
    }

    #[test]
    fn a_page_where_every_gallery_failed_fails() {
        let results = vec![Err(anyhow!("timed out 1")), Err(anyhow!("timed out 2"))];
        let err = gallery_infos_of_page(results).unwrap_err();
        assert_eq!(err.to_string(), "timed out 1");
    }

    #[test]
    fn an_empty_page_is_no_failure() {
        assert!(gallery_infos_of_page(Vec::new()).unwrap().is_empty());
    }

    #[test]
    fn ipv4_addresses_come_first_in_their_order() {
        let addrs: Vec<SocketAddr> = [
            "[2604:e880:2:14::11]:0",
            "203.0.113.7:0",
            "[2001:db8::1]:0",
            "198.51.100.2:0",
        ]
        .iter()
        .map(|addr| addr.parse().unwrap())
        .collect();

        let sorted: Vec<String> = ipv4_first(addrs).iter().map(ToString::to_string).collect();
        assert_eq!(
            sorted,
            [
                "203.0.113.7:0",
                "198.51.100.2:0",
                "[2604:e880:2:14::11]:0",
                "[2001:db8::1]:0"
            ]
        );
    }

    #[test]
    fn ipv6_only_hosts_keep_their_addresses() {
        let addr: SocketAddr = "[2001:db8::1]:0".parse().unwrap();
        assert_eq!(ipv4_first([addr]), [addr]);
    }
}
