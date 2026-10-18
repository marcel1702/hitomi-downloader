//! The web version: the core behind an HTTP server
//!
//! - `POST /api/<command>`: the commands of the desktop app
//! - `GET /api/events`: their events as Server-Sent Events
//! - `GET /api/comics/{id}/cover`, `GET /api/comics/{id}/pages/{index}`: the images
//! - `GET /api/health`: for a container healthcheck
//! - `/auth/*`: the login page, the login and the session
//! - everything else: the built frontend
//!
//! Everything but the health check and `/auth` needs a login, see [`auth`].
//! `hitomi-server hash-password` prints the hash for `HITOMI_PASSWORD_HASH`.
mod auth;
mod commands;
mod error;
mod events;
mod images;
mod paths;
mod settings;
mod state;

use std::{io::IsTerminal, net::SocketAddr, path::Path, process::ExitCode, sync::Arc};

use anyhow::{bail, Context as _};
use axum::{http::StatusCode, middleware, routing::get, Json, Router};
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
    auth::{Auth, Policy},
    error::ApiError,
    events::{BroadcastSink, CHANNEL_CAPACITY},
    settings::Settings,
    state::{AppState, ComicCache, PinnedDirs},
};

#[tokio::main]
async fn main() -> ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        None => run().await,
        Some("hash-password") => hash_password(),
        Some(other) => Err(anyhow::anyhow!(
            "unknown argument `{other}`, the only command is `hitomi-server hash-password`"
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
    let auth = Auth::new(&settings.auth, &settings.config_dir, Policy::default())?;

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
    logger::disable_file_logger()?;
    Ok(())
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
        .nest("/comics", images::router())
        .merge(commands::router())
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

    Router::new().nest("/auth", auth.router()).merge(protected)
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
                    |axum::extract::State(auth): axum::extract::State<Auth>| async move {
                        let identity = auth::Identity {
                            user: "tester".to_string(),
                            method: "test".to_string(),
                        };
                        let cookie = auth.start_session(&identity).unwrap();
                        ([(axum::http::header::SET_COOKIE, cookie)], "ok")
                    },
                ),
            )
        }
    }

    /// A login with the password method and the test method, sessions in a fresh directory
    fn new_auth(policy: Policy) -> Auth {
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
            false,
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
            let cookie = auth.start_session(&identity).unwrap();
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

    #[tokio::test]
    async fn without_a_session_only_health_and_login_are_reachable() {
        for (method, uri) in [
            ("POST", "/api/get_config"),
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
            for command in ["export_cbz", "export_pdf"] {
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
}
