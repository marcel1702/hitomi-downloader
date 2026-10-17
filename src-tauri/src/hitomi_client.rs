use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use bytes::Bytes;
use parking_lot::RwLock;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Client, StatusCode};
use reqwest_middleware::ClientWithMiddleware;
use reqwest_retry::{policies::ExponentialBackoff, Jitter, RetryTransientMiddleware};
use tauri::{AppHandle, Manager};

use crate::{
    config::Config,
    extensions::AnyhowErrorToStringChain,
    hitomi::{self, GalleryInfo, Suggestion},
    types::{Comic, SearchResult, SearchSort},
    utils::get_app_handle,
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

#[derive(Clone)]
pub struct HitomiClient {
    app: AppHandle,
    api_client: Arc<RwLock<ClientWithMiddleware>>,
    img_client: Arc<RwLock<ClientWithMiddleware>>,
    cover_client: Arc<RwLock<Client>>,
}

impl HitomiClient {
    pub fn new(app: AppHandle) -> Self {
        let api_client = create_api_client(&app);
        let api_client = Arc::new(RwLock::new(api_client));

        let img_client = create_img_client(&app);
        let img_client = Arc::new(RwLock::new(img_client));

        let cover_client = create_cover_client(&app);
        let cover_client = Arc::new(RwLock::new(cover_client));

        Self {
            app,
            api_client,
            img_client,
            cover_client,
        }
    }

    pub fn get_api_client() -> Arc<RwLock<ClientWithMiddleware>> {
        let app = get_app_handle();
        let hitomi_client = app.state::<HitomiClient>();
        hitomi_client.api_client.clone()
    }

    pub fn reload_client(&self) {
        // Versions fetched over the old route, or the failure to fetch them, say nothing
        // about the new one
        hitomi::invalidate_index_versions();

        let api_client = create_api_client(&self.app);
        *self.api_client.write() = api_client;

        let img_client = create_img_client(&self.app);
        *self.img_client.write() = img_client;

        let cover_client = create_cover_client(&self.app);
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

        let get_gallery_info_tasks = ids
            .iter()
            .skip((page_num - 1) * PAGE_SIZE)
            .take(PAGE_SIZE)
            .map(|id| async move {
                hitomi::get_gallery_info(*id)
                    .await
                    .context(format!("Failed to get gallery info for `{id}`"))
            });
        let results = futures::future::join_all(get_gallery_info_tasks).await;
        let gallery_infos = gallery_infos_of_page(results)?;

        let search_result =
            SearchResult::from_gallery_infos(&self.app, gallery_infos, page_num, total_page, ids)
                .await?;

        Ok(search_result)
    }

    pub async fn get_comic(&self, id: i32) -> anyhow::Result<Comic> {
        let gallery = hitomi::get_gallery_info(id)
            .await
            .context(format!("Failed to get gallery info for `{id}`"))?;

        let comic = Comic::from_gallery_info(&self.app, gallery).await?;
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

fn create_api_client(app: &AppHandle) -> ClientWithMiddleware {
    let retry_policy = ExponentialBackoff::builder()
        .base(1)
        .jitter(Jitter::Bounded)
        .build_with_total_retry_duration(Duration::from_secs(5));

    let client = reqwest::ClientBuilder::new()
        .set_proxy(app, "api_client")
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

fn create_img_client(app: &AppHandle) -> ClientWithMiddleware {
    let retry_policy = ExponentialBackoff::builder()
        .base(1)
        .jitter(Jitter::Bounded)
        .build_with_max_retries(20);

    let client = reqwest::ClientBuilder::new()
        .set_proxy(app, "img_client")
        .connect_timeout(IMG_CONNECT_TIMEOUT)
        .read_timeout(IMG_READ_TIMEOUT)
        .build()
        .unwrap();

    reqwest_middleware::ClientBuilder::new(client)
        .with(RetryTransientMiddleware::new_with_policy(retry_policy))
        .build()
}

fn create_cover_client(app: &AppHandle) -> Client {
    reqwest::ClientBuilder::new()
        .set_proxy(app, "cover_client")
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
    fn set_proxy(self, app: &AppHandle, client_name: &str) -> Self;
}

impl ClientBuilderExt for reqwest::ClientBuilder {
    fn set_proxy(self, app: &AppHandle, client_name: &str) -> reqwest::ClientBuilder {
        let proxy_config = app.state::<RwLock<Config>>().read().proxy.clone();
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
