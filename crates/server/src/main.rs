//! The web version: the core behind an HTTP server
//!
//! - `POST /api/<command>`: the commands of the desktop app
//! - `GET /api/events`: their events as Server-Sent Events
//! - `GET /api/comics/{id}/cover`, `GET /api/comics/{id}/pages/{index}`: the images
//! - `GET /api/health`: for a container healthcheck
//! - `/auth/*`: the login page, the login and the session
//! - `/manifest.webmanifest`, `/icons/*`: the web app manifest and the icons, without a login
//! - everything else: the built frontend
//!
//! Everything but the health check, `/auth`, the manifest and the icons needs a login, see
//! [`auth`].
//! `hitomi-server hash-password` prints the hash for `HITOMI_PASSWORD_HASH`.
mod auth;
mod commands;
mod error;
mod events;
mod files;
mod follow;
mod images;
mod kavita;
mod notify;
mod paths;
mod settings;
mod state;
mod volumes;

use std::{io::IsTerminal, net::SocketAddr, path::Path, process::ExitCode, sync::Arc};

use anyhow::{bail, Context as _};
use axum::{http::StatusCode, middleware, routing::get, Json, Router};
use hitomi_core::{
    context::Options,
    download_manager::DownloadTaskState,
    events::DownloadTaskEvent,
    logger,
    subscriptions::{ContextDownloads, HitomiSearch, Subscriptions},
    Context,
};
use serde_json::{json, Value};
use tokio::{
    net::TcpListener,
    sync::{broadcast, watch, Semaphore},
};
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    auth::{Auth, Policy},
    error::ApiError,
    events::{BroadcastSink, CHANNEL_CAPACITY},
    settings::Settings,
    state::{AppState, ComicCache, PinnedDirs},
};

/// PDFs built at the same time, each of them is held in memory while it is built
const MAX_PDFS_AT_ONCE: usize = 2;

#[tokio::main]
async fn main() -> ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        None => run().await,
        Some("hash-password") => hash_password(),
        Some("healthcheck") => healthcheck(),
        Some(other) => Err(anyhow::anyhow!(
            "unknown argument `{other}`, the commands are `hitomi-server hash-password` and `hitomi-server healthcheck`"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // the logger may not be set up yet, so this goes straight to stderr
            eprintln!("hitomi-server failed: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let settings = Settings::from_env()?;

    let (events, _) = broadcast::channel(CHANNEL_CAPACITY);
    let comics = Arc::new(ComicCache::default());
    let context = Context::init_with(
        settings.config_dir.clone(),
        tokio::runtime::Handle::current(),
        BroadcastSink {
            events: events.clone(),
            comics: comics.clone(),
        },
        Options {
            temp_dir: Some(settings.temp_dir.clone()),
            // the downloads of a container that is stopped for an update come back with it
            queue_file: Some(settings.config_dir.join("queue.json")),
        },
    )?;
    let pinned_dirs = PinnedDirs::resolve(
        settings.library_dir.as_ref(),
        settings.export_dir.as_ref(),
        &context.config().read(),
    );
    pin_dirs(&context, &pinned_dirs)?;
    state::check_temp_dir(&settings.temp_dir, &pinned_dirs.download_dir)?;
    check_volumes(&settings, &pinned_dirs)?;
    logger::init(&context, &[env!("CARGO_CRATE_NAME")])?;
    limit_file_names(&settings, &pinned_dirs)?;
    let (shutdown_sender, shutdown) = watch::channel(false);
    let kavita = kavita::Kavita::open(settings.config_dir.join("kavita.json"));
    // subscribed before anything can download, no finished download is missed
    tokio::spawn(kavita::run_scanner(
        kavita.clone(),
        events.subscribe(),
        kavita::Debounce::default(),
        shutdown.clone(),
    ));
    let (notifier, notifications) =
        notify::Notifier::open(settings.config_dir.join("apprise.json"));
    tokio::spawn(notify::run(
        notifier.clone(),
        events.subscribe(),
        notifications,
        {
            let context = context.clone();
            move || context.config().read().language.clone()
        },
        notify::DEBOUNCE,
        shutdown.clone(),
    ));
    // only now: a download that comes back works out its directory from the pinned library
    // and the file name limit, and says so in the log
    context.download_manager().restore_queue();
    let auth = Auth::new(&settings.auth, &settings.config_dir, Policy::default())?;

    let subscriptions = Subscriptions::open(
        settings.config_dir.join("subscriptions.json"),
        Arc::new(HitomiSearch),
        Arc::new(ContextDownloads::new(context.clone())),
        settings.follow_interval,
    );
    subscriptions.on_new_galleries({
        let notifier = notifier.clone();
        Arc::new(move |query: &str, count: usize| {
            notifier.tell(notify::Happened::Followed {
                query: query.to_string(),
                count,
            });
        })
    });
    let state = AppState {
        context: context.clone(),
        pinned_dirs,
        pdf_permits: Arc::new(Semaphore::new(MAX_PDFS_AT_ONCE)),
        events,
        comics,
        subscriptions: subscriptions.clone(),
        kavita,
        notifier,
        shutdown: shutdown.clone(),
    };
    match settings.follow_interval {
        Some(interval) => {
            tracing::info!(
                hours = interval.as_secs() / 3600,
                "Followed searches are checked at this interval"
            );
            tokio::spawn(follow::run_scheduler(subscriptions, shutdown));
        }
        None => tracing::info!("Checks of followed searches are off, they run when asked for"),
    }
    // a crash may have left PDFs of downloads that were running
    files::clear_temp_dir(&files::temp_dir(&state));

    if !settings.web_dir.join("index.html").exists() {
        tracing::warn!(
            "No frontend at `{}`, only the API is served",
            settings.web_dir.display()
        );
    }
    let listener = TcpListener::bind(settings.addr)
        .await
        .with_context(|| format!("Failed to listen on `{}`", settings.addr))?;
    {
        let config = context.config().read();
        tracing::info!(
            addr = %settings.addr,
            config_dir = %settings.config_dir.display(),
            library_dir = %config.download_dir.display(),
            export_dir = %config.export_dir.display(),
            "hitomi-server {} is listening",
            env!("CARGO_PKG_VERSION")
        );
    }

    // the address of the client is what failed logins are counted by
    let app =
        app(state, auth, &settings.web_dir).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            tracing::info!("Shutting down");
            // ends the event streams, which would otherwise keep the server waiting
            let _ = shutdown_sender.send(true);
        })
        .await
        .context("The server failed")?;

    report_interrupted_downloads(&context);
    // writes out what the file logger still buffers
    let logger_result = logger::disable_file_logger();
    // last, a download that ends while the server shuts down must not stay in the file
    context.download_manager().save_queue_now();
    logger_result?;
    Ok(())
}

/// Check every directory the server writes to, so a problem shows at the start
fn check_volumes(settings: &Settings, pinned_dirs: &PinnedDirs) -> anyhow::Result<()> {
    let dirs = [
        (&pinned_dirs.download_dir, "library", "HITOMI_LIBRARY_DIR"),
        (
            &pinned_dirs.export_dir,
            "export directory",
            "HITOMI_EXPORT_DIR",
        ),
        (
            &settings.config_dir,
            "config directory",
            "HITOMI_CONFIG_DIR",
        ),
        (
            &settings.temp_dir,
            "directory for the pages of running downloads",
            "HITOMI_TEMP_DIR",
        ),
    ];
    for (dir, what, variable) in dirs {
        volumes::check_writable(dir, what, variable)?;
    }
    if settings.require_mount {
        // the pages directory is in the config volume unless it was placed elsewhere on purpose
        for (dir, what, variable) in &dirs[..3] {
            volumes::check_mounted(dir, std::path::Path::new("/"), what, variable)?;
        }
    }
    Ok(())
}

/// Shorten the names of the comics to what the library and the export directory take
fn limit_file_names(settings: &Settings, pinned_dirs: &PinnedDirs) -> anyhow::Result<()> {
    let limit = volumes::max_name_bytes(
        &[
            (pinned_dirs.download_dir.as_path(), "library"),
            (pinned_dirs.export_dir.as_path(), "export directory"),
        ],
        settings.max_name_bytes,
    )?;
    hitomi_core::utils::set_max_file_name_bytes(limit)?;
    if limit < hitomi_core::utils::DEFAULT_MAX_FILE_NAME_BYTES {
        tracing::warn!(
            limit,
            "File names are limited to {limit} bytes here, long titles are shortened to fit"
        );
    }
    Ok(())
}

/// Ask the running server whether it is up, for the healthcheck of a container
///
/// Exits with success if `GET /api/health` answers 200. Needs no `curl` in the image.
fn healthcheck() -> anyhow::Result<()> {
    use std::io::{Read, Write};
    let port = std::env::var("HITOMI_PORT")
        .ok()
        .and_then(|port| port.trim().parse::<u16>().ok())
        .unwrap_or(8080);
    // a server that listens on one address only is reached there, else on the loopback
    let host = std::env::var("HITOMI_HOST")
        .ok()
        .and_then(|host| host.trim().parse::<std::net::IpAddr>().ok())
        .filter(|host| !host.is_unspecified())
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));

    let timeout = std::time::Duration::from_secs(4);
    let mut stream = std::net::TcpStream::connect_timeout(&SocketAddr::new(host, port), timeout)
        .with_context(|| format!("Failed to connect to {host}:{port}"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(
        stream,
        "GET /api/health HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer)?;
    if answer.starts_with("HTTP/1.1 200") || answer.starts_with("HTTP/1.0 200") {
        Ok(())
    } else {
        bail!(
            "the server answered `{}`",
            answer.lines().next().unwrap_or("nothing")
        )
    }
}

/// Print the hash of a password for `HITOMI_PASSWORD_HASH`
///
/// Asks for the password twice on a terminal, reads one line otherwise:
/// `docker run --rm -i <image> hash-password < password.txt`
fn hash_password() -> anyhow::Result<()> {
    let password = if std::io::stdin().is_terminal() {
        let password = rpassword::prompt_password("Password: ")?;
        if rpassword::prompt_password("Password again: ")? != password {
            bail!("the passwords differ");
        }
        password
    } else {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line.trim_end_matches(['\r', '\n']).to_string()
    };
    if password.is_empty() {
        bail!("the password is empty");
    }
    println!("{}", auth::hash_password(&password)?);
    Ok(())
}

fn app(state: AppState, auth: Auth, web_dir: &Path) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/events", get(events::events))
        .nest("/comics", images::router().merge(files::router()))
        .merge(commands::router())
        .merge(follow::router())
        .merge(kavita::router())
        .merge(notify::router())
        .fallback(api_not_found);

    // Paths the frontend routes itself get its index.html
    let web = ServeDir::new(web_dir).fallback(ServeFile::new(web_dir.join("index.html")));

    let protected = Router::new()
        .nest("/api", api)
        .fallback_service(web)
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            auth.clone(),
            auth::require_session,
        ));

    // The icons and the manifest of the web app, without a login: the login page shows the
    // icon, and a browser asks for these without its cookie for the home screen. Nothing in
    // them is the user's.
    let public = Router::new()
        .route_service(
            "/manifest.webmanifest",
            ServeFile::new(web_dir.join("manifest.webmanifest")),
        )
        .nest_service("/icons", ServeDir::new(web_dir.join("icons")));

    Router::new()
        .nest("/auth", auth.router())
        .merge(public)
        .merge(protected)
}

/// Check the library and export dir, and make them the ones in the config
fn pin_dirs(context: &Context, pinned_dirs: &PinnedDirs) -> anyhow::Result<()> {
    pinned_dirs.check()?;
    let mut config = context.config().write();
    let before = (config.download_dir.clone(), config.export_dir.clone());
    pinned_dirs.apply(&mut config);
    if (config.download_dir.clone(), config.export_dir.clone()) != before {
        config
            .save(context.data_dir())
            .context("Failed to save the directories from the environment")?;
    }
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn api_not_found() -> ApiError {
    ApiError::with_status(
        StatusCode::NOT_FOUND,
        "unknown endpoint",
        anyhow::anyhow!("the API has no such endpoint"),
    )
}

/// Resolves on Ctrl+C, and on SIGTERM, which is how `docker stop` asks to shut down
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

/// Say which downloads the shutdown stopped
///
/// They are in the queue file and come back at the next start. Their finished images stay in
/// the temporary download directory, so only the rest is fetched.
fn report_interrupted_downloads(context: &Context) {
    for event in context.download_manager().task_snapshots() {
        let DownloadTaskEvent::Create {
            state,
            comic,
            downloaded_img_count,
            total_img_count,
        } = event
        else {
            continue;
        };
        if matches!(
            state,
            DownloadTaskState::Pending | DownloadTaskState::Downloading | DownloadTaskState::Paused
        ) {
            tracing::warn!(
                id = comic.id,
                comic_title = comic.title,
                "Download interrupted by the shutdown at {downloaded_img_count}/{total_img_count} images, it continues from there at the next start"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use axum::{
        body::Body,
        http::{Request, Response},
    };
    use tower::ServiceExt;

    use super::*;

    /// The context exists once per process, so the tests share one, with a runtime of its own
    /// that outlives every test
    fn test_state() -> &'static (AppState, std::path::PathBuf) {
        static STATE: OnceLock<(AppState, std::path::PathBuf)> = OnceLock::new();
        static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        STATE.get_or_init(|| {
            let runtime = RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap());
            let root =
                std::env::temp_dir().join(format!("hitomi-server-test-{}", std::process::id()));
            let web_dir = root.join("web");
            std::fs::create_dir_all(&web_dir).unwrap();
            std::fs::write(web_dir.join("index.html"), "<!doctype html>app").unwrap();
            std::fs::write(
                web_dir.join("manifest.webmanifest"),
                r#"{"short_name":"HI-DL"}"#,
            )
            .unwrap();
            std::fs::create_dir_all(web_dir.join("icons")).unwrap();
            std::fs::write(web_dir.join("icons").join("icon-32.png"), b"\x89PNG").unwrap();

            let (events, _) = broadcast::channel(16);
            let comics = Arc::new(ComicCache::default());
            let context = Context::init_with(
                root.join("config"),
                runtime.handle().clone(),
                BroadcastSink {
                    events: events.clone(),
                    comics: comics.clone(),
                },
                Options {
                    temp_dir: Some(root.join("config").join("downloading")),
                    queue_file: None,
                },
            )
            .unwrap();
            let pinned_dirs = PinnedDirs {
                download_dir: root.join("library"),
                export_dir: root.join("export"),
            };
            pin_dirs(&context, &pinned_dirs).unwrap();
            let (_, shutdown) = watch::channel(false);
            let subscriptions = Subscriptions::open(
                root.join("config").join("subscriptions.json"),
                follow_fakes().search.clone(),
                follow_fakes().downloads.clone(),
                Some(std::time::Duration::from_secs(6 * 3600)),
            );
            let state = AppState {
                context,
                pinned_dirs,
                pdf_permits: Arc::new(Semaphore::new(MAX_PDFS_AT_ONCE)),
                events,
                comics,
                subscriptions,
                kavita: kavita::Kavita::open(root.join("config").join("kavita.json")),
                notifier: notify::Notifier::open(root.join("config").join("apprise.json")).0,
                shutdown,
            };
            (state, web_dir)
        })
    }

    /// What the followed searches find and queue in the tests, instead of Hitomi
    #[derive(Default)]
    struct FollowFakes {
        search: Arc<FakeSearch>,
        downloads: Arc<FakeDownloads>,
    }

    fn follow_fakes() -> &'static FollowFakes {
        static FAKES: OnceLock<FollowFakes> = OnceLock::new();
        FAKES.get_or_init(FollowFakes::default)
    }

    #[derive(Default)]
    struct FakeSearch {
        results: parking_lot::Mutex<std::collections::HashMap<String, Result<Vec<i32>, String>>>,
    }

    impl FakeSearch {
        fn set(&self, query: &str, ids: &[i32]) {
            self.results
                .lock()
                .insert(query.to_string(), Ok(ids.to_vec()));
        }

        fn fail(&self, query: &str) {
            self.results
                .lock()
                .insert(query.to_string(), Err("Hitomi is not there".to_string()));
        }
    }

    impl hitomi_core::subscriptions::SearchSource for FakeSearch {
        fn ids<'a>(
            &'a self,
            query: &'a str,
        ) -> futures::future::BoxFuture<'a, anyhow::Result<Vec<i32>>> {
            Box::pin(async move {
                match self.results.lock().get(query) {
                    Some(Ok(ids)) => Ok(ids.clone()),
                    Some(Err(message)) => Err(anyhow::anyhow!(message.clone())),
                    None => Ok(Vec::new()),
                }
            })
        }
    }

    #[derive(Default)]
    struct FakeDownloads {
        queued: parking_lot::Mutex<Vec<i32>>,
    }

    impl hitomi_core::subscriptions::Downloads for FakeDownloads {
        fn library_ids(&self) -> std::collections::HashSet<i32> {
            std::collections::HashSet::new()
        }

        fn is_active(&self, _id: i32) -> bool {
            false
        }

        fn queue<'a>(&'a self, id: i32) -> futures::future::BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                self.queued.lock().push(id);
                Ok(())
            })
        }
    }

    /// Wait for what a check in the background does
    async fn eventually(what: &str, done: impl Fn() -> bool) {
        for _ in 0..100 {
            if done() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("{what} did not happen");
    }

    async fn post_json(path: &str, body: Value) -> (StatusCode, Value) {
        let (status, bytes) = request("POST", path, Some(body)).await;
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn subscription_of(list: &Value, query: &str) -> Option<Value> {
        list["subscriptions"]
            .as_array()?
            .iter()
            .find(|subscription| subscription["query"] == query)
            .cloned()
    }

    #[tokio::test]
    async fn the_kavita_settings_never_hand_out_the_api_key() {
        test_state();
        let (status, empty) = post_json("/api/get_kavita_settings", json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            empty,
            json!({ "url": "", "libraryId": null, "hasApiKey": false })
        );

        let (status, saved) = post_json(
            "/api/save_kavita_settings",
            json!({ "url": "http://kavita:5000/", "apiKey": "secret-key", "libraryId": 3 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(
            saved,
            json!({ "url": "http://kavita:5000", "libraryId": 3, "hasApiKey": true })
        );

        // without an API key in the arguments, the one there is stays
        let (_, again) = post_json(
            "/api/save_kavita_settings",
            json!({ "url": "http://kavita:5000", "apiKey": null, "libraryId": 4 }),
        )
        .await;
        assert_eq!(again["hasApiKey"], json!(true));
        assert_eq!(again["libraryId"], json!(4));
        let (_, read) = post_json("/api/get_kavita_settings", json!({})).await;
        assert!(!read.to_string().contains("secret-key"), "{read}");

        // a URL that is none is refused and changes nothing
        let (status, refused) = post_json(
            "/api/save_kavita_settings",
            json!({ "url": "kavita:5000", "apiKey": null, "libraryId": 4 }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(refused["err_title"], json!("Kavita"));
        let (_, read) = post_json("/api/get_kavita_settings", json!({})).await;
        assert_eq!(read["url"], json!("http://kavita:5000"));

        // a Kavita that can not be reached is the answer of the test, not an error of the server
        let (status, tested) = post_json(
            "/api/test_kavita",
            json!({ "url": "http://127.0.0.1:9", "apiKey": null, "libraryId": null }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(
            tested["err_message"]
                .as_str()
                .unwrap()
                .contains("can not be reached"),
            "{tested}"
        );

        // cleared again, Kavita is off
        let (_, cleared) = post_json(
            "/api/save_kavita_settings",
            json!({ "url": "", "apiKey": "", "libraryId": null }),
        )
        .await;
        assert_eq!(
            cleared,
            json!({ "url": "", "libraryId": null, "hasApiKey": false })
        );
    }

    #[tokio::test]
    async fn the_notification_settings_never_hand_out_the_apprise_urls() {
        test_state();
        let args = |urls: Value, url: &str| {
            json!({ "url": url, "urls": urls, "onCompleted": true, "onFailed": false,
                    "onFollowed": true, "censored": true })
        };
        let (status, empty) = post_json("/api/get_notify_settings", json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(empty["urlCount"], json!(0));
        assert_eq!(empty["censored"], json!(false));

        let (status, saved) = post_json(
            "/api/save_notify_settings",
            args(
                json!(["discord://id/secret-token", "tgram://bot/chat"]),
                "http://apprise:8000/",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(
            saved,
            json!({ "url": "http://apprise:8000", "urlCount": 2, "onCompleted": true,
                    "onFailed": false, "onFollowed": true, "censored": true })
        );

        // without URLs in the arguments, the ones there are stay; they are never read back
        let (_, kept) = post_json(
            "/api/save_notify_settings",
            args(Value::Null, "http://apprise:8000"),
        )
        .await;
        assert_eq!(kept["urlCount"], json!(2));
        let (_, read) = post_json("/api/get_notify_settings", json!({})).await;
        assert!(!read.to_string().contains("secret-token"), "{read}");

        // an Apprise URL that is none, and a server URL without http, are refused
        let (status, refused) = post_json(
            "/api/save_notify_settings",
            args(json!(["not-a-url"]), "http://apprise:8000"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(refused["err_title"], json!("Notifications"));
        let (status, _) = post_json(
            "/api/save_notify_settings",
            args(Value::Null, "apprise:8000"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // an Apprise server that can not be reached is the answer of the test
        let (status, tested) =
            post_json("/api/test_notify", args(Value::Null, "http://127.0.0.1:9")).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(
            tested["err_message"]
                .as_str()
                .unwrap()
                .contains("can not be reached"),
            "{tested}"
        );

        let (_, cleared) = post_json("/api/save_notify_settings", args(json!([]), "")).await;
        assert_eq!(cleared["urlCount"], json!(0));
    }

    #[tokio::test]
    async fn a_search_is_followed_from_now_and_listed() {
        test_state();
        let fakes = follow_fakes();
        fakes.search.set("artist:http one", &[3, 2, 1]);

        let (status, preview) = post_json(
            "/api/preview_subscription",
            json!({ "query": "Artist:HTTP  One" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(preview["query"], json!("artist:http one"));
        assert_eq!(preview["matches"], json!(3));
        assert_eq!(preview["toDownload"], json!(3));
        assert_eq!(preview["alreadyFollowed"], json!(false));
        assert_eq!(preview["canDownloadAll"], json!(true));

        let (status, created) = post_json(
            "/api/create_subscription",
            json!({ "query": "artist:http one", "firstRun": "fromNow" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert_eq!(created["status"], json!("active"));
        assert_eq!(created["matches"], json!(3));

        let (status, list) = post_json("/api/list_subscriptions", json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["intervalHours"], json!(6));
        let listed = subscription_of(&list, "artist:http one").expect("listed");
        assert_eq!(listed["id"], created["id"]);
        assert_eq!(listed["status"], json!("active"));
        // from now on queues nothing
        assert!(!fakes.downloads.queued.lock().contains(&1));

        // it is followed now, and says so
        let (_, preview) = post_json(
            "/api/preview_subscription",
            json!({ "query": "artist:http one" }),
        )
        .await;
        assert_eq!(preview["alreadyFollowed"], json!(true));
        let (status, error) = post_json(
            "/api/create_subscription",
            json!({ "query": "ARTIST:http one", "firstRun": "fromNow" }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(error["err_message"]
            .as_str()
            .unwrap()
            .contains("followed already"));
    }

    #[tokio::test]
    async fn following_everything_queues_in_the_background() {
        test_state();
        let fakes = follow_fakes();
        fakes.search.set("series:http two", &[9102, 9101]);

        let (status, created) = post_json(
            "/api/create_subscription",
            json!({ "query": "series:http two", "firstRun": "everything" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        eventually("the galleries being queued", || {
            let queued = fakes.downloads.queued.lock();
            queued.contains(&9101) && queued.contains(&9102)
        })
        .await;
        let id = created["id"].clone();

        // a new gallery is found by a check asked for by hand, which answers at once
        fakes.search.set("series:http two", &[9103, 9102, 9101]);
        let (status, _) = post_json("/api/check_subscription", json!({ "id": id })).await;
        assert_eq!(status, StatusCode::OK);
        eventually("the new gallery being queued", || {
            fakes.downloads.queued.lock().contains(&9103)
        })
        .await;
        let queued = fakes.downloads.queued.lock().clone();
        assert_eq!(
            queued.iter().filter(|id| **id == 9102).count(),
            1,
            "{queued:?}"
        );

        // the check is over once the list shows what it found
        let mut listed = Value::Null;
        for _ in 0..100 {
            let (_, list) = post_json("/api/list_subscriptions", json!({})).await;
            listed = subscription_of(&list, "series:http two").expect("listed");
            if listed["status"] == json!("active") && listed["lastNew"] == json!(1) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(listed["matches"], json!(3), "{listed}");
        assert_eq!(listed["lastNew"], json!(1), "{listed}");
    }

    #[tokio::test]
    async fn a_followed_search_can_be_paused_checked_and_deleted() {
        test_state();
        let fakes = follow_fakes();
        fakes.search.set("tag:http three", &[1]);
        let (_, created) = post_json(
            "/api/create_subscription",
            json!({ "query": "tag:http three", "firstRun": "fromNow" }),
        )
        .await;
        let id = created["id"].clone();

        let (status, _) = post_json(
            "/api/set_subscription_paused",
            json!({ "id": id, "paused": true }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, list) = post_json("/api/list_subscriptions", json!({})).await;
        let listed = subscription_of(&list, "tag:http three").unwrap();
        assert_eq!(listed["status"], json!("paused"));
        assert_eq!(listed["nextCheckAt"], Value::Null);

        let (status, _) = post_json(
            "/api/set_subscription_paused",
            json!({ "id": id, "paused": false }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = post_json("/api/delete_subscription", json!({ "id": id })).await;
        assert_eq!(status, StatusCode::OK);
        let (_, list) = post_json("/api/list_subscriptions", json!({})).await;
        assert!(subscription_of(&list, "tag:http three").is_none());

        // there is nothing to act on any more
        for path in ["/api/delete_subscription", "/api/check_subscription"] {
            let (status, error) = post_json(path, json!({ "id": id })).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(error["err_message"]
                .as_str()
                .unwrap()
                .contains("no such subscription"));
        }
        let (status, _) = post_json(
            "/api/set_subscription_paused",
            json!({ "id": id, "paused": true }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_query_that_can_not_be_followed_is_refused_with_the_reason() {
        test_state();
        let fakes = follow_fakes();

        let (status, error) = post_json(
            "/api/create_subscription",
            json!({ "query": "   ", "firstRun": "fromNow" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["err_message"].as_str().unwrap().contains("empty"));

        fakes.search.fail("artist:http broken");
        let (status, error) = post_json(
            "/api/preview_subscription",
            json!({ "query": "artist:http broken" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(error["err_message"]
            .as_str()
            .unwrap()
            .contains("Hitomi is not there"));

        // the arguments are checked like those of every command
        let (status, _) = post_json(
            "/api/create_subscription",
            json!({ "query": "a", "firstRun": "sometimes" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let many: Vec<i32> = (50_000..51_001).collect();
        fakes.search.set("language:http many", &many);
        let (status, error) = post_json(
            "/api/create_subscription",
            json!({ "query": "language:http many", "firstRun": "everything" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["err_message"]
            .as_str()
            .unwrap()
            .contains("at most 1000"));
        let (_, preview) = post_json(
            "/api/preview_subscription",
            json!({ "query": "language:http many" }),
        )
        .await;
        assert_eq!(preview["canDownloadAll"], json!(false));
    }

    const PEER: [u8; 4] = [192, 0, 2, 1];
    const USERNAME: &str = "marcel";
    const PASSWORD: &str = "correct horse battery staple";

    /// A second login method, only here to show that one plugs in without touching the
    /// session or the protected routes
    struct TestMethod;

    impl auth::AuthMethod for TestMethod {
        fn descriptor(&self) -> auth::MethodDescriptor {
            auth::MethodDescriptor::Redirect {
                id: "test".to_string(),
                label: "Test".to_string(),
                url: "/auth/test/login".to_string(),
            }
        }

        fn router(self: Arc<Self>) -> Router<Auth> {
            Router::new().route(
                "/test/login",
                get(
                    |axum::extract::State(auth): axum::extract::State<Auth>,
                     headers: axum::http::HeaderMap| async move {
                        let identity = auth::Identity {
                            user: "tester".to_string(),
                            method: "test".to_string(),
                        };
                        let cookie = auth.start_session(&identity, &headers).unwrap();
                        ([(axum::http::header::SET_COOKIE, cookie)], "ok")
                    },
                ),
            )
        }
    }

    /// A login with the password method and the test method, sessions in a fresh directory
    fn new_auth(policy: Policy) -> Auth {
        new_auth_behind_proxy(policy, false)
    }

    /// [`new_auth`] that trusts the `X-Forwarded-*` headers of a reverse proxy, or not
    fn new_auth_behind_proxy(policy: Policy, trust_forwarded_for: bool) -> Auth {
        let dir = std::env::temp_dir().join(format!("hitomi-auth-test-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let methods: Vec<Arc<dyn auth::AuthMethod>> = vec![
            Arc::new(auth::PasswordMethod::new(
                USERNAME.to_string(),
                auth::hash_password(PASSWORD).unwrap(),
                auth::Throttle::new(policy),
            )),
            Arc::new(TestMethod),
        ];
        Auth::with_methods(
            methods,
            &dir,
            std::time::Duration::from_secs(3600),
            false,
            trust_forwarded_for,
        )
        .unwrap()
    }

    /// The login most tests share, and the cookie of a session in it
    fn shared_auth() -> &'static (Auth, String) {
        static AUTH: OnceLock<(Auth, String)> = OnceLock::new();
        AUTH.get_or_init(|| {
            let auth = new_auth(Policy::default());
            let identity = auth::Identity {
                user: USERNAME.to_string(),
                method: "password".to_string(),
            };
            let cookie = auth
                .start_session(&identity, &axum::http::HeaderMap::new())
                .unwrap();
            (auth, cookie_pair(cookie.to_str().unwrap()))
        })
    }

    /// `name=value` out of a `Set-Cookie` value
    fn cookie_pair(set_cookie: &str) -> String {
        set_cookie.split(';').next().unwrap().to_string()
    }

    struct TestResponse {
        status: StatusCode,
        headers: axum::http::HeaderMap,
        body: Vec<u8>,
    }

    async fn send(
        auth: &Auth,
        method: &str,
        uri: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        let (state, web_dir) = test_state();
        let mut request = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = match body {
            Some(body) => request
                .header("content-type", "application/json")
                .body(Body::from(body.to_string())),
            None => request.body(Body::empty()),
        }
        .unwrap();
        let app = app(state.clone(), auth.clone(), web_dir).layer(
            axum::extract::connect_info::MockConnectInfo(SocketAddr::from((PEER, 40000))),
        );
        let response: Response<Body> = app.oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        TestResponse {
            status,
            headers,
            body: body.to_vec(),
        }
    }

    /// A request of a logged in client
    async fn request(method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Vec<u8>) {
        let (auth, cookie) = shared_auth();
        let response = send(auth, method, uri, body, &[("cookie", cookie)]).await;
        (response.status, response.body)
    }

    /// A request without a session
    async fn anonymous(method: &str, uri: &str, body: Option<Value>) -> TestResponse {
        send(&shared_auth().0, method, uri, body, &[]).await
    }

    fn json_of(body: &[u8]) -> Value {
        serde_json::from_slice(body).unwrap()
    }

    #[tokio::test]
    async fn health_reports_the_version() {
        let (status, body) = request("GET", "/api/health", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            json_of(&body),
            json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") })
        );
    }

    #[tokio::test]
    async fn the_environment_decides_the_directories() {
        let (_, root_web) = test_state();
        let root = root_web.parent().unwrap();

        let (status, body) = request("POST", "/api/get_config", None).await;
        assert_eq!(status, StatusCode::OK);
        let mut config = json_of(&body);
        assert_eq!(config["downloadDir"], json!(root.join("library")));
        assert_eq!(config["exportDir"], json!(root.join("export")));

        // a frontend that tries to move the library or the exports is overruled, the rest is saved
        config["downloadDir"] = json!("/somewhere/else");
        config["exportDir"] = json!("/etc");
        config["dirFmt"] = json!("{id}");
        let (status, body) = request(
            "POST",
            "/api/save_config",
            Some(json!({ "config": config })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

        let (_, body) = request("POST", "/api/get_config", None).await;
        let saved = json_of(&body);
        assert_eq!(saved["downloadDir"], json!(root.join("library")));
        assert_eq!(saved["exportDir"], json!(root.join("export")));
        assert_eq!(saved["dirFmt"], json!("{id}"));
        // and so is the file on disk
        let on_disk: Value = serde_json::from_slice(
            &std::fs::read(root.join("config").join("config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(on_disk["downloadDir"], json!(root.join("library")));
        assert_eq!(on_disk["exportDir"], json!(root.join("export")));
    }

    #[tokio::test]
    async fn invalid_arguments_answer_with_a_command_error() {
        let (status, body) = request(
            "POST",
            "/api/get_comic",
            Some(json!({ "id": "not a number" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let error = json_of(&body);
        assert_eq!(error["err_title"], json!("invalid arguments"));
        assert!(
            error["err_message"].as_str().unwrap().contains("id"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn desktop_only_commands_say_so() {
        let (status, body) = request(
            "POST",
            "/api/update_tray_menu_labels",
            Some(json!({ "showWindow": "Show", "quit": "Quit" })),
        )
        .await;
        assert_eq!((status, json_of(&body)), (StatusCode::OK, Value::Null));

        let (status, _) = request(
            "POST",
            "/api/show_path_in_file_manager",
            Some(json!({ "path": "/" })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);

        let (status, _) = request("POST", "/api/get_image_data", Some(json!({}))).await;
        assert_eq!(status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn unknown_api_paths_are_no_frontend_routes() {
        let (status, body) = request("POST", "/api/no_such_command", Some(json!({}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json_of(&body)["err_title"], json!("unknown endpoint"));
    }

    #[tokio::test]
    async fn other_paths_get_the_frontend() {
        for path in ["/", "/index.html", "/some/route/of/the/frontend"] {
            let (status, body) = request("GET", path, None).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(body, b"<!doctype html>app", "{path}");
        }
    }

    #[tokio::test]
    async fn page_zero_of_a_search_is_an_error_and_no_panic() {
        // answered before Hitomi is asked, so no request leaves the test
        for (uri, body) in [
            (
                "/api/search",
                json!({ "query": "language:english", "pageNum": 0, "sort": "Date" }),
            ),
            ("/api/get_page", json!({ "ids": [1, 2, 3], "pageNum": 0 })),
        ] {
            let (status, body) = request("POST", uri, Some(body)).await;
            assert!(
                status.is_server_error() || status.is_client_error(),
                "{uri}"
            );
            assert!(
                String::from_utf8_lossy(&body).contains("the first page is 1"),
                "{uri}: {}",
                String::from_utf8_lossy(&body)
            );
        }
    }

    #[tokio::test]
    async fn a_page_past_the_end_is_not_found() {
        let (state, _) = test_state();
        state.comics.insert(&hitomi_core::types::Comic {
            id: 42,
            ..Default::default()
        });
        let (status, body) = request("GET", "/api/comics/42/pages/0", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(String::from_utf8_lossy(&body).contains("has 0 pages"));
    }

    #[tokio::test]
    async fn without_a_session_only_health_and_login_are_reachable() {
        for (method, uri) in [
            ("POST", "/api/get_config"),
            ("POST", "/api/list_subscriptions"),
            ("POST", "/api/create_subscription"),
            ("POST", "/api/get_kavita_settings"),
            ("POST", "/api/save_kavita_settings"),
            ("POST", "/api/test_kavita"),
            ("POST", "/api/get_notify_settings"),
            ("POST", "/api/save_notify_settings"),
            ("POST", "/api/test_notify"),
            ("GET", "/api/events"),
            ("GET", "/api/comics/3651691/cover"),
            ("POST", "/api/no_such_command"),
        ] {
            let response = anonymous(method, uri, None).await;
            assert_eq!(response.status, StatusCode::UNAUTHORIZED, "{method} {uri}");
            assert_eq!(json_of(&response.body)["err_title"], json!("not logged in"));
        }

        // a page of the frontend sends the browser to the login, which brings it back
        for (uri, next) in [("/", "%2F"), ("/reader?id=1", "%2Freader%3Fid%3D1")] {
            let response = anonymous("GET", uri, None).await;
            assert_eq!(response.status, StatusCode::SEE_OTHER, "{uri}");
            assert_eq!(
                response.headers["location"],
                format!("/auth/login?next={next}").replace("%2F", "/")
            );
        }

        assert_eq!(
            anonymous("GET", "/api/health", None).await.status,
            StatusCode::OK
        );
        let login = anonymous("GET", "/auth/login", None).await;
        assert_eq!(login.status, StatusCode::OK);
        assert_eq!(login.headers["x-frame-options"], "DENY");
        assert!(String::from_utf8_lossy(&login.body).contains("/auth/methods"));
    }

    #[tokio::test]
    async fn the_icons_and_the_manifest_need_no_login() {
        let manifest = anonymous("GET", "/manifest.webmanifest", None).await;
        assert_eq!(manifest.status, StatusCode::OK);
        assert_eq!(json_of(&manifest.body)["short_name"], "HI-DL");

        let icon = anonymous("GET", "/icons/icon-32.png", None).await;
        assert_eq!(icon.status, StatusCode::OK);
        assert_eq!(icon.headers["content-type"], "image/png");
        assert_eq!(icon.body, b"\x89PNG");

        // only what is in there: a missing icon is missing, and nothing else is reached
        for uri in [
            "/icons/none.png",
            "/icons/../index.html",
            "/icons/%2e%2e/index.html",
        ] {
            let response = anonymous("GET", uri, None).await;
            assert_eq!(response.status, StatusCode::NOT_FOUND, "{uri}");
            assert!(
                !String::from_utf8_lossy(&response.body).contains("app"),
                "{uri}"
            );
        }
        // the app itself still needs the login
        assert_eq!(
            anonymous("GET", "/index.html", None).await.status,
            StatusCode::SEE_OTHER
        );
    }

    #[tokio::test]
    async fn the_login_page_lists_every_method() {
        let response = anonymous("GET", "/auth/methods", None).await;
        assert_eq!(
            json_of(&response.body),
            json!([
                { "kind": "password", "id": "password" },
                { "kind": "redirect", "id": "test", "label": "Test", "url": "/auth/test/login" }
            ])
        );
    }

    #[tokio::test]
    async fn the_password_login_starts_a_session_and_logout_ends_it() {
        let auth = new_auth(Policy::default());
        let credentials =
            |password: &str| Some(json!({ "username": USERNAME, "password": password }));

        let login = send(&auth, "POST", "/auth/login", credentials(PASSWORD), &[]).await;
        assert_eq!(login.status, StatusCode::OK);
        let set_cookie = login.headers["set-cookie"].to_str().unwrap().to_string();
        for flag in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=3600"] {
            assert!(set_cookie.contains(flag), "{set_cookie}");
        }
        assert!(
            !set_cookie.contains("Secure"),
            "plain http, no https public URL"
        );
        let cookie = cookie_pair(&set_cookie);

        let config = send(
            &auth,
            "POST",
            "/api/get_config",
            None,
            &[("cookie", &cookie)],
        )
        .await;
        assert_eq!(config.status, StatusCode::OK);
        let session = send(&auth, "GET", "/auth/session", None, &[("cookie", &cookie)]).await;
        assert_eq!(
            json_of(&session.body),
            json!({ "user": USERNAME, "method": "password" })
        );
        // logged in, the login page sends the browser on
        let page = send(&auth, "GET", "/auth/login", None, &[("cookie", &cookie)]).await;
        assert_eq!(page.headers["location"], "/");

        let logout = send(&auth, "POST", "/auth/logout", None, &[("cookie", &cookie)]).await;
        assert!(logout.headers["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0"));
        let after = send(
            &auth,
            "POST",
            "/api/get_config",
            None,
            &[("cookie", &cookie)],
        )
        .await;
        assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn wrong_credentials_are_rejected_and_lock_the_client_out() {
        let auth = new_auth(Policy {
            max_failures: 3,
            delay: std::time::Duration::from_millis(10),
            ..Policy::default()
        });
        let login = |username: &str, password: &str| {
            let auth = auth.clone();
            let body = json!({ "username": username, "password": password });
            async move { send(&auth, "POST", "/auth/login", Some(body), &[]).await }
        };

        for (username, password) in [(USERNAME, "wrong"), ("someone", PASSWORD)] {
            let response = login(username, password).await;
            assert_eq!(response.status, StatusCode::UNAUTHORIZED);
            assert!(!response.headers.contains_key("set-cookie"));
            assert_eq!(
                json_of(&response.body)["err_message"],
                json!("wrong user name or password")
            );
        }
        assert_eq!(
            login(USERNAME, "wrong again").await.status,
            StatusCode::UNAUTHORIZED
        );

        // the third failure locked the client out, now even the right password waits
        let locked = login(USERNAME, PASSWORD).await;
        assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(locked.headers["retry-after"], "900");
        assert!(!locked.headers.contains_key("set-cookie"));

        // X-Forwarded-For is not trusted unless the server is told to, so it frees no one
        let spoofed = send(
            &auth,
            "POST",
            "/auth/login",
            Some(json!({ "username": USERNAME, "password": PASSWORD })),
            &[("x-forwarded-for", "203.0.113.50")],
        )
        .await;
        assert_eq!(spoofed.status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn parallel_wrong_logins_get_no_more_tries_than_the_limit() {
        let auth = new_auth(Policy {
            max_failures: 3,
            delay: std::time::Duration::from_millis(10),
            ..Policy::default()
        });
        let attempts = (0..12).map(|_| {
            let auth = auth.clone();
            let body = json!({ "username": USERNAME, "password": "wrong" });
            tokio::spawn(async move { send(&auth, "POST", "/auth/login", Some(body), &[]).await })
        });
        let mut statuses = Vec::new();
        for attempt in attempts.collect::<Vec<_>>() {
            statuses.push(attempt.await.unwrap().status);
        }
        let checked = statuses
            .iter()
            .filter(|status| **status == StatusCode::UNAUTHORIZED)
            .count();
        assert_eq!(checked, 3, "{statuses:?}");
        assert!(statuses.iter().all(|status| [
            StatusCode::UNAUTHORIZED,
            StatusCode::TOO_MANY_REQUESTS
        ]
        .contains(status)));
        // and the right password waits now as well
        let locked = send(
            &auth,
            "POST",
            "/auth/login",
            Some(json!({ "username": USERNAME, "password": PASSWORD })),
            &[],
        )
        .await;
        assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn the_cookie_is_secure_when_a_trusted_proxy_was_reached_over_https() {
        let body = || Some(json!({ "username": USERNAME, "password": PASSWORD }));
        let https = [("x-forwarded-proto", "https")];
        let set_cookie = |response: TestResponse| {
            assert_eq!(response.status, StatusCode::OK);
            response.headers["set-cookie"].to_str().unwrap().to_string()
        };

        let trusted = new_auth_behind_proxy(Policy::default(), true);
        let login = send(&trusted, "POST", "/auth/login", body(), &https).await;
        let cookie = set_cookie(login);
        assert!(cookie.contains("; Secure"), "{cookie}");
        let logout = send(
            &trusted,
            "POST",
            "/auth/logout",
            None,
            &[https[0], ("cookie", &cookie_pair(&cookie))],
        )
        .await;
        assert!(set_cookie(logout).contains("; Secure"));
        let plain = set_cookie(send(&trusted, "POST", "/auth/login", body(), &[]).await);
        assert!(!plain.contains("Secure"), "{plain}");

        // a proxy that is not trusted can not say so, any client could
        let untrusted = new_auth(Policy::default());
        let cookie = set_cookie(send(&untrusted, "POST", "/auth/login", body(), &https).await);
        assert!(!cookie.contains("Secure"), "{cookie}");
    }

    #[tokio::test]
    async fn the_login_page_only_takes_a_target_on_this_server() {
        // what the address bar holds, a browser drops the tab and the line break of the first two
        for next in [
            "/%09/evil.com",
            "/%0A/evil.com",
            "/%0D%0A/evil.com",
            "//evil.com",
            "/%5Cevil.com",
            "%2F%2Fevil.com",
            "https://evil.com/",
            "javascript:alert(1)",
        ] {
            let response = anonymous("GET", &format!("/auth/login?next={next}"), None).await;
            assert_eq!(response.status, StatusCode::SEE_OTHER, "{next}");
            assert_eq!(response.headers["location"], "/auth/login", "{next}");
        }
        for next in ["/", "/reader%3Fid%3D1", "/settings"] {
            let response = anonymous("GET", &format!("/auth/login?next={next}"), None).await;
            assert_eq!(response.status, StatusCode::OK, "{next}");
        }
    }

    #[tokio::test]
    async fn another_login_method_plugs_in_without_touching_the_session() {
        let auth = new_auth(Policy::default());
        let login = send(&auth, "GET", "/auth/test/login", None, &[]).await;
        let cookie = cookie_pair(login.headers["set-cookie"].to_str().unwrap());

        let session = send(&auth, "GET", "/auth/session", None, &[("cookie", &cookie)]).await;
        assert_eq!(
            json_of(&session.body),
            json!({ "user": "tester", "method": "test" })
        );
        let config = send(
            &auth,
            "POST",
            "/api/get_config",
            None,
            &[("cookie", &cookie)],
        )
        .await;
        assert_eq!(config.status, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_comic_path_outside_the_library_is_refused() {
        let (_, web_dir) = test_state();
        let root = web_dir.parent().unwrap();
        let library = root.join("library");
        std::fs::create_dir_all(library.join("Some Comic - 1")).unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();

        for dir in [
            root.join("secret"),
            library.join("../secret"),
            std::path::PathBuf::from("/etc"),
        ] {
            let comic = json!({ "comic": comic_json(1, Some(&dir)) });
            for command in ["export_cbz", "export_pdf", "delete_comic"] {
                let (status, body) =
                    request("POST", &format!("/api/{command}"), Some(comic.clone())).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{command} {}", dir.display());
                assert_eq!(
                    json_of(&body)["err_title"],
                    json!("the comic is not in the library")
                );
            }
        }
        assert!(!root
            .join("export")
            .join("..")
            .join("secret")
            .join("x.cbz")
            .exists());

        // a sync never hands back a path the library does not have
        let comic = json!({ "comic": comic_json(1, Some(&root.join("secret"))) });
        let (status, body) = request("POST", "/api/get_synced_comic", Some(comic)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_of(&body)["comicDownloadDir"], Value::Null);
    }

    /// A comic as the frontend sends it
    fn comic_json(id: i32, download_dir: Option<&std::path::Path>) -> Value {
        let mut comic = serde_json::to_value(hitomi_core::types::Comic {
            id,
            title: "Some Comic".to_string(),
            ..Default::default()
        })
        .unwrap();
        comic["comicDownloadDir"] = json!(download_dir);
        comic["isDownloaded"] = json!(download_dir.is_some());
        comic
    }

    /// 1x1 lossless webp, a page the PDF export can read
    const PAGE: [u8; 34] = [
        0x52, 0x49, 0x46, 0x46, 0x1a, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38,
        0x4c, 0x0d, 0x00, 0x00, 0x00, 0x2f, 0x00, 0x00, 0x00, 0x10, 0x07, 0x10, 0x11, 0x11, 0x88,
        0x88, 0xfe, 0x07, 0x00,
    ];

    /// A comic in the library, put in the comic cache the way a search would
    fn library_comic(id: i32, dir_name: &str, files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let (state, web_dir) = test_state();
        let dir = web_dir.parent().unwrap().join("library").join(dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        state.comics.insert(&hitomi_core::types::Comic {
            id,
            title: dir_name.to_string(),
            comic_download_dir: Some(dir.clone()),
            is_downloaded: Some(true),
            ..Default::default()
        });
        dir
    }

    async fn download(id: i32, format: &str) -> TestResponse {
        let (auth, cookie) = shared_auth();
        send(
            auth,
            "GET",
            &format!("/api/comics/{id}/download/{format}"),
            None,
            &[("cookie", cookie)],
        )
        .await
    }

    /// The PDF tests look at the shared temporary directory, so they run one after the other
    static PDF_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn temp_pdfs() -> Vec<std::path::PathBuf> {
        let (state, _) = test_state();
        std::fs::read_dir(files::temp_dir(state))
            .map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn a_comic_is_deleted_from_the_library_and_forgotten_by_the_cache() {
        let (state, _) = test_state();
        let metadata = serde_json::to_vec(&hitomi_core::types::Comic {
            id: 9201,
            ..Default::default()
        })
        .unwrap();
        let name = "Delete Me - 9201";
        let dir = library_comic(
            9201,
            name,
            &[
                ("metadata.json", metadata.as_slice()),
                (&format!("{name}.cbz"), b"archive"),
            ],
        );
        assert!(state.comics.ids().contains(&9201));

        let comic = json!({ "comic": comic_json(9201, Some(&dir)) });
        let (status, body) = request("POST", "/api/delete_comic", Some(comic.clone())).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert!(!dir.exists());
        assert!(!state.comics.ids().contains(&9201));

        // gone is an error, and says so
        let (status, _) = request("POST", "/api/delete_comic", Some(comic)).await;
        assert_ne!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_directory_that_is_not_that_comic_is_not_deleted() {
        let metadata = serde_json::to_vec(&hitomi_core::types::Comic {
            id: 9202,
            ..Default::default()
        })
        .unwrap();
        let dir = library_comic(
            9203,
            "Other Comic - 9202",
            &[("metadata.json", metadata.as_slice())],
        );
        // the client asks to delete the comic 9203 in the folder of the comic 9202
        let comic = json!({ "comic": comic_json(9203, Some(&dir)) });
        let (status, _) = request("POST", "/api/delete_comic", Some(comic)).await;
        assert_ne!(status, StatusCode::OK);
        assert!(dir.join("metadata.json").exists());
    }

    #[tokio::test]
    async fn the_cbz_of_a_comic_is_served_as_the_archive_with_its_real_name() {
        let name = "La habitación 日本 - 9101";
        let archive = b"PK\x03\x04 an archive, as far as the download is concerned";
        library_comic(9101, name, &[(&format!("{name}.cbz"), archive.as_slice())]);

        let response = download(9101, "cbz").await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body, archive);
        assert_eq!(
            response.headers["content-type"],
            "application/vnd.comicbook+zip"
        );
        assert_eq!(
            response.headers["content-length"],
            archive.len().to_string().as_str()
        );
        let disposition = response.headers["content-disposition"].to_str().unwrap();
        assert!(
            disposition.starts_with("attachment; filename=\"La habitaci_n __ - 9101.cbz\""),
            "{disposition}"
        );
        assert!(
            disposition.ends_with(
                "filename*=UTF-8''La%20habitaci%C3%B3n%20%E6%97%A5%E6%9C%AC%20-%209101.cbz"
            ),
            "{disposition}"
        );
        assert_eq!(response.headers["cache-control"], "private, no-store");
    }

    #[tokio::test]
    async fn the_pdf_of_a_comic_is_built_streamed_and_leaves_nothing_behind() {
        let _serial = PDF_TESTS.lock().await;
        let name = "Pdf Comic - 9102";
        library_comic(9102, name, &[("0001.webp", &PAGE), ("0002.webp", &PAGE)]);

        let response = download(9102, "pdf").await;
        assert_eq!(
            response.status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        assert!(
            response.body.starts_with(b"%PDF-"),
            "{:?}",
            &response.body[..8.min(response.body.len())]
        );
        assert_eq!(response.headers["content-type"], "application/pdf");
        assert_eq!(
            response.headers["content-length"],
            response.body.len().to_string().as_str()
        );
        assert!(response.headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("Pdf%20Comic%20-%209102.pdf"));
        // the body is gone, so is the temporary file it came from
        assert_eq!(temp_pdfs(), Vec::<std::path::PathBuf>::new());
    }

    #[tokio::test]
    async fn an_aborted_pdf_download_deletes_its_temporary_file_too() {
        let _serial = PDF_TESTS.lock().await;
        let name = "Aborted Pdf - 9103";
        library_comic(9103, name, &[("0001.webp", &PAGE)]);

        let (auth, cookie) = shared_auth();
        let (state, web_dir) = test_state();
        let request = Request::builder()
            .uri("/api/comics/9103/download/pdf")
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap();
        let app = app(state.clone(), auth.clone(), web_dir).layer(
            axum::extract::connect_info::MockConnectInfo(SocketAddr::from((PEER, 40000))),
        );
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            temp_pdfs().len(),
            1,
            "the file exists while the browser has not read it"
        );
        // the browser gives up without reading a byte
        drop(response);
        assert_eq!(temp_pdfs(), Vec::<std::path::PathBuf>::new());
    }

    #[tokio::test]
    async fn a_failed_pdf_leaves_no_temporary_file() {
        let _serial = PDF_TESTS.lock().await;
        // a page that is no image, which makes the PDF fail halfway
        library_comic(9104, "Broken Pdf - 9104", &[("0001.webp", b"not an image")]);
        let response = download(9104, "pdf").await;
        assert_eq!(response.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            json_of(&response.body)["err_title"],
            json!("Failed to create the PDF")
        );
        assert_eq!(temp_pdfs(), Vec::<std::path::PathBuf>::new());
    }

    #[tokio::test]
    async fn a_download_without_archive_or_outside_the_library_is_refused_clearly() {
        // downloaded by an old version as loose pages, there is no cbz to serve
        library_comic(9105, "Loose Pages - 9105", &[("0001.webp", &PAGE)]);
        let response = download(9105, "cbz").await;
        assert_eq!(response.status, StatusCode::NOT_FOUND);
        assert!(String::from_utf8_lossy(&response.body).contains("has no cbz archive"));

        // a comic that is not downloaded
        let (state, _) = test_state();
        state.comics.insert(&hitomi_core::types::Comic {
            id: 9106,
            ..Default::default()
        });
        let response = download(9106, "cbz").await;
        assert_eq!(response.status, StatusCode::NOT_FOUND);
        assert!(String::from_utf8_lossy(&response.body).contains("is not downloaded"));

        // a directory outside the library, which no client can plant in the cache, is not served
        let outside = state.context.data_dir().join("outside - 9107");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("outside - 9107.cbz"), b"secret").unwrap();
        state.comics.insert(&hitomi_core::types::Comic {
            id: 9107,
            comic_download_dir: Some(outside),
            is_downloaded: Some(true),
            ..Default::default()
        });
        let response = download(9107, "cbz").await;
        assert_eq!(response.status, StatusCode::NOT_FOUND);
        assert!(!String::from_utf8_lossy(&response.body).contains("secret"));

        assert_eq!(download(9101, "zip").await.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn downloads_need_a_login() {
        let response = anonymous("GET", "/api/comics/9101/download/cbz", None).await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    }
}
