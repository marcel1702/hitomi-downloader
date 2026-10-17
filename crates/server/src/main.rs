//! The web version: the core behind an HTTP server
//!
//! - `POST /api/<command>`: the commands of the desktop app
//! - `GET /api/events`: their events as Server-Sent Events
//! - `GET /api/comics/{id}/cover`, `GET /api/comics/{id}/pages/{index}`: the images
//! - `GET /api/health`: for a container healthcheck
//! - everything else: the built frontend
mod commands;
mod error;
mod events;
mod images;
mod settings;
mod state;

use std::{path::Path, process::ExitCode, sync::Arc};

use anyhow::Context as _;
use axum::{http::StatusCode, routing::get, Json, Router};
use hitomi_core::{
    download_manager::DownloadTaskState, events::DownloadTaskEvent, logger, Context,
};
use serde_json::{json, Value};
use tokio::{
    net::TcpListener,
    sync::{broadcast, watch},
};
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    error::ApiError,
    events::{BroadcastSink, CHANNEL_CAPACITY},
    settings::Settings,
    state::{AppState, ComicCache, PinnedDirs},
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
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
    let context = Context::init(
        settings.config_dir.clone(),
        tokio::runtime::Handle::current(),
        BroadcastSink {
            events: events.clone(),
            comics: comics.clone(),
        },
    )?;
    let pinned_dirs = PinnedDirs {
        download_dir: settings.library_dir.clone(),
        export_dir: settings.export_dir.clone(),
    };
    pin_dirs(&context, &pinned_dirs)?;
    logger::init(&context, &[env!("CARGO_CRATE_NAME")])?;

    let (shutdown_sender, shutdown) = watch::channel(false);
    let state = AppState {
        context: context.clone(),
        pinned_dirs,
        events,
        comics,
        shutdown,
    };

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

    axum::serve(listener, app(state, &settings.web_dir))
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
    logger::disable_file_logger()?;
    Ok(())
}

fn app(state: AppState, web_dir: &Path) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/events", get(events::events))
        .nest("/comics", images::router())
        .merge(commands::router())
        .fallback(api_not_found);

    // Paths the frontend routes itself get its index.html
    let web = ServeDir::new(web_dir).fallback(ServeFile::new(web_dir.join("index.html")));

    Router::new()
        .nest("/api", api)
        .fallback_service(web)
        .with_state(state)
}

/// Make the download and export dir from the environment the ones in the config
fn pin_dirs(context: &Context, pinned_dirs: &PinnedDirs) -> anyhow::Result<()> {
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

/// Downloads are not kept across a restart yet, say which ones stopped
///
/// Their finished images stay in the temporary download directory, so downloading the
/// comic again only fetches the rest.
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
                "Download interrupted by the shutdown at {downloaded_img_count}/{total_img_count} images, downloading it again continues from there"
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

            let (events, _) = broadcast::channel(16);
            let comics = Arc::new(ComicCache::default());
            let context = Context::init(
                root.join("config"),
                runtime.handle().clone(),
                BroadcastSink {
                    events: events.clone(),
                    comics: comics.clone(),
                },
            )
            .unwrap();
            let pinned_dirs = PinnedDirs {
                download_dir: Some(root.join("library")),
                export_dir: Some(root.join("export")),
            };
            pin_dirs(&context, &pinned_dirs).unwrap();
            let (_, shutdown) = watch::channel(false);
            let state = AppState {
                context,
                pinned_dirs,
                events,
                comics,
                shutdown,
            };
            (state, web_dir)
        })
    }

    async fn request(method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Vec<u8>) {
        let (state, web_dir) = test_state();
        let request = Request::builder().method(method).uri(uri);
        let request = match body {
            Some(body) => request
                .header("content-type", "application/json")
                .body(Body::from(body.to_string())),
            None => request.body(Body::empty()),
        }
        .unwrap();
        let response: Response<Body> = app(state.clone(), web_dir).oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, body.to_vec())
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

        // a frontend that tries to move the library is overruled, the rest is saved
        config["downloadDir"] = json!("/somewhere/else");
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
        assert_eq!(saved["dirFmt"], json!("{id}"));
        // and so is the file on disk
        let on_disk: Value = serde_json::from_slice(
            &std::fs::read(root.join("config").join("config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(on_disk["downloadDir"], json!(root.join("library")));
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
}
