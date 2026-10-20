//! Kavita: a scan of its library after downloads, so new comics show up there right away
//!
//! Optional, and off until a URL, an API key and a library are set in the settings. They live in
//! `kavita.json` in the config directory, readable by its owner only, the API key is a secret.
//! A download that finishes asks for a scan; downloads that finish close to each other share
//! one, and a scan that fails is logged and never touches the download.
//!
//! Kavita takes an API key only to hand out a token (`POST /api/Plugin/authenticate`), the scan
//! (`POST /api/Library/scan`) needs that token and a user with the admin role.
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{bail, Context as _};
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use hitomi_core::{
    download_manager::DownloadTaskState,
    errors::CommandError,
    events::{DownloadTaskEvent, Event},
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};

use crate::{
    error::{ApiError, ApiResult, Args},
    state::AppState,
};

/// The name Kavita lists the API key under when it is used
const PLUGIN_NAME: &str = "hitomi-downloader";
/// How long a call to Kavita may take
const TIMEOUT: Duration = Duration::from_secs(15);

/// When a scan starts after downloads finished
#[derive(Debug, Clone, Copy)]
pub struct Debounce {
    /// that long after the last download that finished
    pub quiet: Duration,
    /// but no later than that after the first, a long queue still shows up while it runs
    pub max_wait: Duration,
}

impl Default for Debounce {
    fn default() -> Self {
        Self {
            quiet: Duration::from_secs(3),
            max_wait: Duration::from_secs(30),
        }
    }
}

/// What `kavita.json` holds
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KavitaSettings {
    /// Where Kavita is reached from the server, `http://kavita:5000` for example
    pub url: String,
    pub api_key: String,
    pub library_id: Option<i64>,
}

impl KavitaSettings {
    /// Whether a scan can be asked for, without all three nothing is sent to Kavita
    pub fn is_set(&self) -> bool {
        !self.url.is_empty() && !self.api_key.is_empty() && self.library_id.is_some()
    }
}

/// The settings as the frontend sees them, the API key only as whether there is one
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KavitaSettingsView {
    url: String,
    library_id: Option<i64>,
    has_api_key: bool,
}

/// A library of Kavita, for the frontend to choose from
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    pub id: i64,
    pub name: String,
}

/// The connection to Kavita, its settings and the scans
pub struct Kavita {
    file: PathBuf,
    settings: RwLock<KavitaSettings>,
    client: reqwest::Client,
}

impl Kavita {
    /// The settings from `file`, none if there is no such file or it can not be read
    pub fn open(file: PathBuf) -> Arc<Self> {
        let settings = match std::fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                tracing::warn!(
                    "`{}` is no valid Kavita settings file, Kavita is off until it is saved again: {err}",
                    file.display()
                );
                KavitaSettings::default()
            }),
            Err(_) => KavitaSettings::default(),
        };
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .unwrap_or_default();
        Arc::new(Self {
            file,
            settings: RwLock::new(settings),
            client,
        })
    }

    pub fn settings(&self) -> KavitaSettings {
        self.settings.read().clone()
    }

    fn view(&self) -> KavitaSettingsView {
        let settings = self.settings.read();
        KavitaSettingsView {
            url: settings.url.clone(),
            library_id: settings.library_id,
            has_api_key: !settings.api_key.is_empty(),
        }
    }

    /// Keep `settings` and write them to the file, readable by its owner only
    pub fn save(&self, settings: KavitaSettings) -> anyhow::Result<()> {
        let mut current = self.settings.write();
        write_private(&self.file, &serde_json::to_vec_pretty(&settings)?)?;
        *current = settings;
        Ok(())
    }

    /// The libraries of the Kavita `settings` name, which proves the URL and the API key
    pub async fn libraries(&self, settings: &KavitaSettings) -> anyhow::Result<Vec<Library>> {
        let token = self.token(settings).await?;
        let response = self
            .client
            .get(endpoint(&settings.url, "api/Library/libraries")?)
            .bearer_auth(token)
            .send()
            .await
            .context("Kavita can not be reached")?;
        let body = checked(response, "the list of libraries").await?;
        serde_json::from_slice(&body)
            .context("Kavita answered the list of libraries with something else")
    }

    /// Scan the library of the settings, nothing at all if Kavita is not set up
    pub async fn scan(&self) -> anyhow::Result<bool> {
        let settings = self.settings();
        let Some(library_id) = settings.library_id.filter(|_| settings.is_set()) else {
            return Ok(false);
        };
        let token = self.token(&settings).await?;
        let mut url = endpoint(&settings.url, "api/Library/scan")?;
        url.query_pairs_mut()
            .append_pair("libraryId", &library_id.to_string())
            .append_pair("force", "false");
        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .send()
            .await
            .context("Kavita can not be reached")?;
        checked(response, "the scan of the library").await?;
        Ok(true)
    }

    async fn token(&self, settings: &KavitaSettings) -> anyhow::Result<String> {
        if settings.url.is_empty() {
            bail!("No URL of Kavita is set");
        }
        if settings.api_key.is_empty() {
            bail!("No API key of Kavita is set");
        }
        let mut url = endpoint(&settings.url, "api/Plugin/authenticate")?;
        url.query_pairs_mut()
            .append_pair("apiKey", &settings.api_key)
            .append_pair("pluginName", PLUGIN_NAME);
        let response = self
            .client
            .post(url)
            .send()
            .await
            .context("Kavita can not be reached")?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!("Kavita does not know the API key");
        }
        let body = checked(response, "the API key").await?;

        #[derive(Deserialize)]
        struct User {
            token: String,
        }
        let user: User = serde_json::from_slice(&body)
            .context("Kavita answered the API key with something else than a login")?;
        Ok(user.token)
    }
}

/// `path` below the URL of Kavita, which may itself lie below a path behind a reverse proxy
fn endpoint(base: &str, path: &str) -> anyhow::Result<url::Url> {
    let base = format!("{}/", base.trim_end_matches('/'));
    let base = url::Url::parse(&base).with_context(|| format!("`{base}` is no URL"))?;
    if !matches!(base.scheme(), "http" | "https") {
        bail!("The URL of Kavita has to start with http:// or https://");
    }
    Ok(base.join(path)?)
}

/// The body of a successful answer, an error that names `what` otherwise
async fn checked(response: reqwest::Response, what: &str) -> anyhow::Result<Vec<u8>> {
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        bail!("Kavita refused {what} ({status}), the user of the API key has to be an admin");
    }
    if !status.is_success() {
        bail!("Kavita answered {what} with {status}");
    }
    Ok(response
        .bytes()
        .await
        .context("Kavita broke off its answer")?
        .to_vec())
}

/// Write `bytes` to `file` in one go, with the rights `0600` from the start
fn write_private(file: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;

    let temp = file.with_extension("json.tmp");
    let _ = std::fs::remove_file(&temp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&temp)
        .and_then(|mut out| {
            out.write_all(bytes)?;
            out.sync_all()
        })
        .with_context(|| format!("Failed to write `{}`", temp.display()))?;
    std::fs::rename(&temp, file).with_context(|| format!("Failed to write `{}`", file.display()))
}

/// Scan Kavita after downloads finished, for as long as the server runs
pub async fn run_scanner(
    kavita: Arc<Kavita>,
    mut events: broadcast::Receiver<Event>,
    debounce: Debounce,
    mut shutdown: watch::Receiver<bool>,
) {
    // when the first and the last download of the waiting ones finished
    let mut waiting: Option<(Instant, Instant)> = None;
    loop {
        let due =
            waiting.map(|(first, last)| (last + debounce.quiet).min(first + debounce.max_wait));
        tokio::select! {
            event = events.recv() => {
                let finished = match event {
                    Ok(Event::DownloadTask(DownloadTaskEvent::Update {
                        state: DownloadTaskState::Completed,
                        ..
                    })) => true,
                    // missed events may have held a finished download
                    Err(broadcast::error::RecvError::Lagged(_)) => true,
                    Err(broadcast::error::RecvError::Closed) => return,
                    Ok(_) => false,
                };
                if finished && kavita.settings().is_set() {
                    let now = Instant::now();
                    waiting = Some((waiting.map_or(now, |(first, _)| first), now));
                }
            }
            () = sleep_until(due) => {
                waiting = None;
                match kavita.scan().await {
                    Ok(true) => tracing::info!("Kavita scans its library for the new downloads"),
                    Ok(false) => {}
                    // only logged, the download itself is done
                    Err(err) => tracing::warn!("Kavita could not be told to scan its library: {err:#}"),
                }
            }
            _ = shutdown.changed() => return,
        }
    }
}

async fn sleep_until(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due.into()).await,
        None => std::future::pending().await,
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/get_kavita_settings", post(get_kavita_settings))
        .route("/save_kavita_settings", post(save_kavita_settings))
        .route("/test_kavita", post(test_kavita))
}

/// Settings as the frontend sends them, an API key of `None` keeps the one there is
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsArgs {
    url: String,
    api_key: Option<String>,
    library_id: Option<i64>,
}

impl SettingsArgs {
    fn into_settings(self, current: &KavitaSettings) -> KavitaSettings {
        KavitaSettings {
            url: self.url.trim().trim_end_matches('/').to_string(),
            api_key: self
                .api_key
                .map_or_else(|| current.api_key.clone(), |key| key.trim().to_string()),
            library_id: self.library_id,
        }
    }
}

fn api_error(status: StatusCode, err: &anyhow::Error) -> ApiError {
    ApiError::new(
        status,
        CommandError {
            err_title: "Kavita".to_string(),
            err_message: format!("{err:#}"),
        },
    )
}

async fn get_kavita_settings(State(state): State<AppState>) -> Json<KavitaSettingsView> {
    Json(state.kavita.view())
}

async fn save_kavita_settings(
    State(state): State<AppState>,
    Args(args): Args<SettingsArgs>,
) -> ApiResult<Json<KavitaSettingsView>> {
    let settings = args.into_settings(&state.kavita.settings());
    if !settings.url.is_empty() {
        endpoint(&settings.url, "").map_err(|err| api_error(StatusCode::BAD_REQUEST, &err))?;
    }
    state.kavita.save(settings).map_err(|err| {
        tracing::error!("Failed to save the Kavita settings: {err:#}");
        api_error(StatusCode::INTERNAL_SERVER_ERROR, &err)
    })?;
    Ok(Json(state.kavita.view()))
}

/// The libraries of the Kavita the arguments name, saved or not
///
/// Not logged: a wrong URL or key while the settings are filled in is no error of the server,
/// the dialog shows the answer.
async fn test_kavita(
    State(state): State<AppState>,
    Args(args): Args<SettingsArgs>,
) -> ApiResult<Json<Vec<Library>>> {
    let settings = args.into_settings(&state.kavita.settings());
    state
        .kavita
        .libraries(&settings)
        .await
        .map(Json)
        .map_err(|err| api_error(StatusCode::BAD_GATEWAY, &err))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{
        extract::{Query, State as AxumState},
        http::HeaderMap,
        routing::get,
    };

    use super::*;

    const KEY: &str = "the-api-key";
    const TOKEN: &str = "the-token";

    /// What the fake Kavita saw
    #[derive(Default)]
    struct Seen {
        logins: AtomicUsize,
        scans: parking_lot::Mutex<Vec<(String, String)>>,
        fail_scans: std::sync::atomic::AtomicBool,
    }

    /// A Kavita that knows `KEY`, lists two libraries and records the scans
    async fn fake_kavita() -> (String, Arc<Seen>) {
        let seen = Arc::new(Seen::default());
        let authorized = |headers: &HeaderMap| {
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                == Some(&format!("Bearer {TOKEN}"))
        };
        let app = Router::new()
            .route(
                "/kavita/api/Plugin/authenticate",
                post(
                    |AxumState(seen): AxumState<Arc<Seen>>,
                     Query(query): Query<std::collections::HashMap<String, String>>| async move {
                        seen.logins.fetch_add(1, Ordering::SeqCst);
                        if query.get("apiKey").map(String::as_str) != Some(KEY)
                            || query.get("pluginName").map(String::as_str) != Some(PLUGIN_NAME)
                        {
                            return Err(StatusCode::UNAUTHORIZED);
                        }
                        Ok(Json(serde_json::json!({ "username": "admin", "token": TOKEN })))
                    },
                ),
            )
            .route(
                "/kavita/api/Library/libraries",
                get(move |headers: HeaderMap| async move {
                    if !authorized(&headers) {
                        return Err(StatusCode::UNAUTHORIZED);
                    }
                    Ok(Json(serde_json::json!([
                        { "id": 1, "name": "Manga", "type": 0 },
                        { "id": 7, "name": "Hitomi", "type": 0 }
                    ])))
                }),
            )
            .route(
                "/kavita/api/Library/scan",
                post(
                    move |AxumState(seen): AxumState<Arc<Seen>>,
                          headers: HeaderMap,
                          Query(query): Query<std::collections::HashMap<String, String>>| async move {
                        if !authorized(&headers) {
                            return StatusCode::UNAUTHORIZED;
                        }
                        if seen.fail_scans.load(Ordering::SeqCst) {
                            return StatusCode::INTERNAL_SERVER_ERROR;
                        }
                        seen.scans.lock().push((
                            query.get("libraryId").cloned().unwrap_or_default(),
                            query.get("force").cloned().unwrap_or_default(),
                        ));
                        StatusCode::OK
                    },
                ),
            )
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        // behind a path, as behind a reverse proxy, and with the slash a user may type
        (format!("http://{addr}/kavita/"), seen)
    }

    fn temp_file() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hitomi-kavita-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("kavita.json")
    }

    fn settings(url: &str, key: &str, library_id: Option<i64>) -> KavitaSettings {
        KavitaSettings {
            url: url.trim_end_matches('/').to_string(),
            api_key: key.to_string(),
            library_id,
        }
    }

    #[tokio::test]
    async fn the_libraries_prove_url_and_key() {
        let (url, _) = fake_kavita().await;
        let kavita = Kavita::open(temp_file());
        let libraries = kavita.libraries(&settings(&url, KEY, None)).await.unwrap();
        assert_eq!(
            libraries,
            [
                Library {
                    id: 1,
                    name: "Manga".into()
                },
                Library {
                    id: 7,
                    name: "Hitomi".into()
                }
            ]
        );

        let err = kavita
            .libraries(&settings(&url, "wrong", None))
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("does not know the API key"),
            "{err:#}"
        );
        let err = kavita
            .libraries(&settings("ftp://kavita", KEY, None))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("http://"), "{err:#}");
        // nothing listens on port 9 of the loopback
        let err = kavita
            .libraries(&settings("http://127.0.0.1:9", KEY, None))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("can not be reached"), "{err:#}");
    }

    #[tokio::test]
    async fn the_settings_are_kept_for_the_owner_only() {
        let file = temp_file();
        let kavita = Kavita::open(file.clone());
        assert_eq!(kavita.settings(), KavitaSettings::default());
        let saved = settings("http://kavita:5000", KEY, Some(7));
        kavita.save(saved.clone()).unwrap();

        assert_eq!(Kavita::open(file.clone()).settings(), saved);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let view = serde_json::to_value(kavita.view()).unwrap();
        assert_eq!(
            view,
            serde_json::json!({ "url": "http://kavita:5000", "libraryId": 7, "hasApiKey": true })
        );
        assert!(!view.to_string().contains(KEY));
    }

    #[test]
    fn an_api_key_that_is_not_sent_stays() {
        let current = settings("http://old", KEY, Some(1));
        let args = |api_key: Option<&str>| SettingsArgs {
            url: " http://kavita:5000/ ".into(),
            api_key: api_key.map(str::to_string),
            library_id: Some(7),
        };
        assert_eq!(
            args(None).into_settings(&current),
            settings("http://kavita:5000", KEY, Some(7))
        );
        assert_eq!(args(Some(" new ")).into_settings(&current).api_key, "new");
        assert_eq!(args(Some("")).into_settings(&current).api_key, "");
    }

    fn completed(comic_id: i32) -> Event {
        Event::DownloadTask(DownloadTaskEvent::Update {
            comic_id,
            state: DownloadTaskState::Completed,
            downloaded_img_count: 1,
            total_img_count: 1,
        })
    }

    const FAST: Debounce = Debounce {
        quiet: Duration::from_millis(150),
        max_wait: Duration::from_millis(600),
    };

    /// A scanner on its own events, and what it scanned after `events` went out `gap` apart
    async fn scans_after(
        kavita_settings: Option<KavitaSettings>,
        seen: &Arc<Seen>,
        events: Vec<Event>,
        gap: Duration,
    ) -> Vec<(String, String)> {
        let kavita = Kavita::open(temp_file());
        if let Some(kavita_settings) = kavita_settings {
            kavita.save(kavita_settings).unwrap();
        }
        let (sender, receiver) = broadcast::channel(16);
        let (stop, shutdown) = watch::channel(false);
        let scanner = tokio::spawn(run_scanner(kavita, receiver, FAST, shutdown));
        for event in events {
            sender.send(event).unwrap();
            tokio::time::sleep(gap).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        stop.send(true).unwrap();
        scanner.await.unwrap();
        seen.scans.lock().drain(..).collect()
    }

    #[tokio::test]
    async fn a_batch_of_downloads_asks_for_one_scan() {
        let (url, seen) = fake_kavita().await;
        let scans = scans_after(
            Some(settings(&url, KEY, Some(7))),
            &seen,
            vec![completed(1), completed(2), completed(3)],
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(scans, [("7".to_string(), "false".to_string())]);
    }

    #[tokio::test]
    async fn a_long_queue_is_scanned_while_it_runs() {
        let (url, seen) = fake_kavita().await;
        // a download every 100 ms for 1.2 s: never quiet, so the longest wait decides
        let scans = scans_after(
            Some(settings(&url, KEY, Some(7))),
            &seen,
            (0..12).map(completed).collect(),
            Duration::from_millis(100),
        )
        .await;
        assert!((2..=3).contains(&scans.len()), "{scans:?}");
    }

    #[tokio::test]
    async fn other_events_and_no_settings_send_nothing() {
        let (url, seen) = fake_kavita().await;
        let not_done = Event::DownloadTask(DownloadTaskEvent::Update {
            comic_id: 1,
            state: DownloadTaskState::Downloading,
            downloaded_img_count: 0,
            total_img_count: 1,
        });
        let scans = scans_after(
            Some(settings(&url, KEY, Some(7))),
            &seen,
            vec![not_done],
            Duration::ZERO,
        )
        .await;
        assert!(scans.is_empty());

        // without a library, and without any settings, Kavita is never called
        for kavita_settings in [Some(settings(&url, KEY, None)), None] {
            let logins = seen.logins.load(Ordering::SeqCst);
            let scans =
                scans_after(kavita_settings, &seen, vec![completed(1)], Duration::ZERO).await;
            assert!(scans.is_empty());
            assert_eq!(seen.logins.load(Ordering::SeqCst), logins);
        }
    }

    #[tokio::test]
    async fn a_failed_scan_is_only_logged() {
        let (url, seen) = fake_kavita().await;
        seen.fail_scans.store(true, Ordering::SeqCst);
        let kavita = Kavita::open(temp_file());
        kavita.save(settings(&url, KEY, Some(7))).unwrap();
        let err = kavita.scan().await.unwrap_err();
        assert!(format!("{err:#}").contains("500"), "{err:#}");

        // the scanner goes on, and scans again for the next download once Kavita is back
        let (sender, receiver) = broadcast::channel(16);
        let (stop, shutdown) = watch::channel(false);
        let scanner = tokio::spawn(run_scanner(kavita, receiver, FAST, shutdown));
        sender.send(completed(1)).unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        seen.fail_scans.store(false, Ordering::SeqCst);
        sender.send(completed(2)).unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        stop.send(true).unwrap();
        scanner.await.unwrap();
        assert_eq!(seen.scans.lock().len(), 1);
    }
}
