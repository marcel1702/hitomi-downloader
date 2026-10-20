//! Notifications through an Apprise API server: downloads that finished or failed, and the
//! galleries the followed searches found
//!
//! Optional, and off until the URL of the Apprise server and at least one Apprise URL
//! (`discord://…`, `tgram://…`, …) are set in the settings. They live in `apprise.json` in the
//! config directory, readable by its owner only, the Apprise URLs hold the tokens of the
//! services. They are sent along with every message to the stateless `POST /notify/` of the
//! Apprise server, which keeps nothing of them.
//!
//! What happens close together is told in one message per kind: 30 seconds after the last of
//! it, at the latest 5 minutes after the first. Censored, a message says how many and never
//! which: no titles of comics, no queries of followed searches.
use std::{
    collections::HashMap,
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
use serde_json::json;
use tokio::sync::{broadcast, mpsc, watch};

use crate::{
    error::{ApiError, ApiResult, Args},
    kavita::{write_private, Debounce},
    state::AppState,
};

/// How long a call to the Apprise server may take, it waits for the services it sends to
const TIMEOUT: Duration = Duration::from_secs(30);
/// How many titles a message names, the rest it counts
const MAX_TITLES: usize = 10;

pub const DEBOUNCE: Debounce = Debounce {
    quiet: Duration::from_secs(30),
    max_wait: Duration::from_secs(5 * 60),
};

/// What `apprise.json` holds
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NotifySettings {
    /// Where the Apprise API server is reached from this server, `http://apprise:8000`
    pub url: String,
    /// The targets in the URL format of Apprise, `discord://WebhookID/Token` for example
    pub urls: Vec<String>,
    pub on_completed: bool,
    pub on_failed: bool,
    pub on_followed: bool,
    /// Tell how many, never which: no titles, no queries
    pub censored: bool,
}

impl Default for NotifySettings {
    fn default() -> Self {
        Self {
            url: String::new(),
            urls: Vec::new(),
            on_completed: true,
            on_failed: true,
            on_followed: true,
            censored: false,
        }
    }
}

impl NotifySettings {
    pub fn is_set(&self) -> bool {
        !self.url.is_empty() && !self.urls.is_empty()
    }
}

/// The settings as the frontend sees them, the Apprise URLs only as how many there are
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifySettingsView {
    url: String,
    url_count: usize,
    on_completed: bool,
    on_failed: bool,
    on_followed: bool,
    censored: bool,
}

/// Something worth telling, as the notifier collects it
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Happened {
    Completed { title: String },
    Failed { title: String },
    Followed { query: String, count: usize },
}

/// A message as it goes to Apprise
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub title: String,
    pub body: String,
    /// `success`, `failure` or `info`, how Apprise shows it
    pub kind: &'static str,
}

pub struct Notifier {
    file: PathBuf,
    settings: RwLock<NotifySettings>,
    client: reqwest::Client,
    /// Hands what happened to the loop that collects it
    sender: mpsc::UnboundedSender<Happened>,
}

impl Notifier {
    /// The settings from `file`, and the receiver for [`run`]
    pub fn open(file: PathBuf) -> (Arc<Self>, mpsc::UnboundedReceiver<Happened>) {
        let settings = match std::fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                tracing::warn!(
                    "`{}` is no valid notification settings file, notifications are off until it is saved again: {err}",
                    file.display()
                );
                NotifySettings::default()
            }),
            Err(_) => NotifySettings::default(),
        };
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .unwrap_or_default();
        let (sender, receiver) = mpsc::unbounded_channel();
        let notifier = Arc::new(Self {
            file,
            settings: RwLock::new(settings),
            client,
            sender,
        });
        (notifier, receiver)
    }

    pub fn settings(&self) -> NotifySettings {
        self.settings.read().clone()
    }

    fn view(&self) -> NotifySettingsView {
        let settings = self.settings.read();
        NotifySettingsView {
            url: settings.url.clone(),
            url_count: settings.urls.len(),
            on_completed: settings.on_completed,
            on_failed: settings.on_failed,
            on_followed: settings.on_followed,
            censored: settings.censored,
        }
    }

    pub fn save(&self, settings: NotifySettings) -> anyhow::Result<()> {
        let mut current = self.settings.write();
        write_private(&self.file, &serde_json::to_vec_pretty(&settings)?)?;
        *current = settings;
        Ok(())
    }

    /// Note what happened, if the settings ask for it to be told
    pub fn tell(&self, happened: Happened) {
        let settings = self.settings.read();
        let wanted = settings.is_set()
            && match happened {
                Happened::Completed { .. } => settings.on_completed,
                Happened::Failed { .. } => settings.on_failed,
                Happened::Followed { .. } => settings.on_followed,
            };
        if wanted {
            // fails only once the loop is gone, when the server shuts down
            let _ = self.sender.send(happened);
        }
    }

    /// Send `message` to the targets of `settings`
    pub async fn send(&self, settings: &NotifySettings, message: &Message) -> anyhow::Result<()> {
        if settings.url.is_empty() {
            bail!("No URL of the Apprise server is set");
        }
        if settings.urls.is_empty() {
            bail!("No Apprise URL is set, there is nobody to tell");
        }
        let response = self
            .client
            .post(endpoint(&settings.url)?)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&json!({
                "urls": settings.urls.join(" "),
                "title": message.title,
                "body": message.body,
                "type": message.kind,
                "format": "text",
            }))?)
            .send()
            .await
            .context("The Apprise server can not be reached")?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let text = response.text().await.unwrap_or_default();
        let text = text.trim();
        let reason = match status.as_u16() {
            // what the Apprise API answers when a service refused the message
            424 => "at least one of the services did not take the message, the log of the Apprise server says why",
            400 => "it did not understand the request, check the Apprise URLs",
            _ => "",
        };
        let detail = if text.is_empty() || text.len() > 300 {
            String::new()
        } else {
            format!(": {text}")
        };
        if reason.is_empty() {
            bail!("The Apprise server answered {status}{detail}");
        }
        bail!("The Apprise server answered {status}, {reason}{detail}");
    }
}

/// `POST /notify/` below the URL of the Apprise server, which may lie below a path itself
fn endpoint(base: &str) -> anyhow::Result<url::Url> {
    let base = format!("{}/", base.trim_end_matches('/'));
    let parsed = url::Url::parse(&base).with_context(|| format!("`{base}` is no URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("The URL of the Apprise server has to start with http:// or https://");
    }
    Ok(parsed.join("notify/")?)
}

/// The Apprise URLs of `text`: one per line, blank lines left out
fn parse_urls(lines: &[String]) -> anyhow::Result<Vec<String>> {
    let urls: Vec<String> = lines
        .iter()
        .flat_map(|line| line.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    for url in &urls {
        let scheme = url.split_once("://").map(|(scheme, _)| scheme);
        if !scheme.is_some_and(|scheme| {
            !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        }) {
            bail!(
                "`{}` is no Apprise URL, they look like discord://WebhookID/Token or slack://TokenA/TokenB/TokenC",
                shorten(url)
            );
        }
    }
    Ok(urls)
}

/// The start of `text`, an error message must not repeat a whole token
fn shorten(text: &str) -> String {
    let start: String = text.chars().take(12).collect();
    if start.len() < text.len() {
        format!("{start}…")
    } else {
        start
    }
}

// ---- The messages

/// The words of a message in one language of the interface
struct Words {
    downloaded: fn(usize) -> String,
    failed: fn(usize) -> String,
    found: fn(usize) -> String,
    downloaded_body: &'static str,
    failed_body: &'static str,
    found_body: &'static str,
    more: fn(usize) -> String,
    /// with the name of the app in it, `{app}`
    test_title: &'static str,
    test_body: &'static str,
}

const ENGLISH: Words = Words {
    downloaded: |n| {
        if n == 1 {
            "1 comic downloaded".into()
        } else {
            format!("{n} comics downloaded")
        }
    },
    failed: |n| {
        if n == 1 {
            "1 download failed".into()
        } else {
            format!("{n} downloads failed")
        }
    },
    found: |n| {
        if n == 1 {
            "1 new gallery from the followed searches".into()
        } else {
            format!("{n} new galleries from the followed searches")
        }
    },
    downloaded_body: "They are in the library now.",
    failed_body: "The downloads tab of the web version has them, to try again.",
    found_body: "They are downloading.",
    more: |n| format!("and {n} more"),
    test_title: "{app}: test",
    test_body: "If this arrived, the notifications of {app} work.",
};

const GERMAN: Words = Words {
    downloaded: |n| {
        if n == 1 {
            "1 Comic heruntergeladen".into()
        } else {
            format!("{n} Comics heruntergeladen")
        }
    },
    failed: |n| {
        if n == 1 {
            "1 Download fehlgeschlagen".into()
        } else {
            format!("{n} Downloads fehlgeschlagen")
        }
    },
    found: |n| {
        if n == 1 {
            "1 neue Galerie aus den gefolgten Suchen".into()
        } else {
            format!("{n} neue Galerien aus den gefolgten Suchen")
        }
    },
    downloaded_body: "Sie sind jetzt in der Bibliothek.",
    failed_body: "Im Download-Tab der Web-Version lassen sie sich erneut versuchen.",
    found_body: "Sie werden heruntergeladen.",
    more: |n| format!("und {n} weitere"),
    test_title: "{app}: Test",
    test_body: "Wenn das angekommen ist, funktionieren die Benachrichtigungen von {app}.",
};

fn words(language: &str) -> &'static Words {
    if language.starts_with("de") {
        &GERMAN
    } else {
        &ENGLISH
    }
}

/// The lines of a list, at most `MAX_TITLES` of them and the count of the rest
fn listed(items: &[String], words: &Words) -> String {
    let mut lines: Vec<String> = items
        .iter()
        .take(MAX_TITLES)
        .map(|item| format!("• {item}"))
        .collect();
    if items.len() > MAX_TITLES {
        lines.push((words.more)(items.len() - MAX_TITLES));
    }
    lines.join("\n")
}

/// The messages for what happened, one per kind, in the order completed, failed, followed
pub fn messages(happened: &[Happened], censored: bool, language: &str) -> Vec<Message> {
    let words = words(language);
    let mut completed = Vec::new();
    let mut failed = Vec::new();
    // the count per query, in the order they came
    let mut followed: Vec<(String, usize)> = Vec::new();
    for item in happened {
        match item {
            Happened::Completed { title } => completed.push(title.clone()),
            Happened::Failed { title } => failed.push(title.clone()),
            Happened::Followed { query, count } => {
                match followed.iter_mut().find(|(known, _)| known == query) {
                    Some((_, total)) => *total += count,
                    None => followed.push((query.clone(), *count)),
                }
            }
        }
    }

    let mut messages = Vec::new();
    if !completed.is_empty() {
        messages.push(Message {
            title: (words.downloaded)(completed.len()),
            body: if censored {
                words.downloaded_body.to_string()
            } else {
                listed(&completed, words)
            },
            kind: "success",
        });
    }
    if !failed.is_empty() {
        messages.push(Message {
            title: (words.failed)(failed.len()),
            body: if censored {
                words.failed_body.to_string()
            } else {
                format!("{}\n\n{}", listed(&failed, words), words.failed_body)
            },
            kind: "failure",
        });
    }
    if !followed.is_empty() {
        let total = followed.iter().map(|(_, count)| count).sum();
        let per_query: Vec<String> = followed
            .iter()
            .map(|(query, count)| format!("{query}: {count}"))
            .collect();
        messages.push(Message {
            title: (words.found)(total),
            body: if censored {
                words.found_body.to_string()
            } else {
                format!("{}\n\n{}", listed(&per_query, words), words.found_body)
            },
            kind: "info",
        });
    }
    messages
}

/// The name of the app in a message, censored only the short one that says less
fn app_name(censored: bool) -> &'static str {
    if censored {
        "hi-dl"
    } else {
        "hitomi-downloader"
    }
}

fn test_message(language: &str, censored: bool) -> Message {
    let words = words(language);
    let app = app_name(censored);
    Message {
        title: words.test_title.replace("{app}", app),
        body: words.test_body.replace("{app}", app),
        kind: "info",
    }
}

// ---- The loop

/// Collect what happened and tell it, for as long as the server runs
///
/// `events` brings the downloads, `happened` what [`Notifier::tell`] was handed, the followed
/// searches. `language` is asked at the time a message goes out.
pub async fn run(
    notifier: Arc<Notifier>,
    mut events: broadcast::Receiver<Event>,
    mut happened: mpsc::UnboundedReceiver<Happened>,
    language: impl Fn() -> String + Send + 'static,
    debounce: Debounce,
    mut shutdown: watch::Receiver<bool>,
) {
    // the titles of the downloads that are known, the update of a state only names the id
    let mut titles: HashMap<i32, String> = HashMap::new();
    let mut waiting: Vec<Happened> = Vec::new();
    let mut since: Option<(Instant, Instant)> = None;
    loop {
        let due = since.map(|(first, last)| (last + debounce.quiet).min(first + debounce.max_wait));
        tokio::select! {
            event = events.recv() => match event {
                Ok(Event::DownloadTask(DownloadTaskEvent::Create { comic, .. })) => {
                    titles.insert(comic.id, comic.title.clone());
                }
                Ok(Event::DownloadTask(DownloadTaskEvent::Update { comic_id, state, .. })) => {
                    let title = || titles.get(&comic_id).cloned().unwrap_or_else(|| format!("#{comic_id}"));
                    match state {
                        DownloadTaskState::Completed => notifier.tell(Happened::Completed { title: title() }),
                        DownloadTaskState::Failed => notifier.tell(Happened::Failed { title: title() }),
                        _ => {}
                    }
                }
                Ok(_) => {}
                // a few updates are lost then, the next ones come through
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
            item = happened.recv() => {
                let Some(item) = item else { return };
                waiting.push(item);
                let now = Instant::now();
                since = Some((since.map_or(now, |(first, _)| first), now));
            }
            () = sleep_until(due) => {
                since = None;
                let settings = notifier.settings();
                let batch = std::mem::take(&mut waiting);
                for message in messages(&batch, settings.censored, &language()) {
                    // only logged, nothing else depends on it
                    if let Err(err) = notifier.send(&settings, &message).await {
                        tracing::warn!("A notification could not be sent: {err:#}");
                    }
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

// ---- The commands

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/get_notify_settings", post(get_notify_settings))
        .route("/save_notify_settings", post(save_notify_settings))
        .route("/test_notify", post(test_notify))
}

/// Settings as the frontend sends them, `urls` of `None` keeps the Apprise URLs there are
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsArgs {
    url: String,
    urls: Option<Vec<String>>,
    on_completed: bool,
    on_failed: bool,
    on_followed: bool,
    censored: bool,
}

impl SettingsArgs {
    fn into_settings(self, current: &NotifySettings) -> anyhow::Result<NotifySettings> {
        let url = self.url.trim().trim_end_matches('/').to_string();
        if !url.is_empty() {
            endpoint(&url)?;
        }
        let urls = match self.urls {
            Some(lines) => parse_urls(&lines)?,
            None => current.urls.clone(),
        };
        Ok(NotifySettings {
            url,
            urls,
            on_completed: self.on_completed,
            on_failed: self.on_failed,
            on_followed: self.on_followed,
            censored: self.censored,
        })
    }
}

fn api_error(status: StatusCode, err: &anyhow::Error) -> ApiError {
    ApiError::new(
        status,
        CommandError {
            err_title: "Notifications".to_string(),
            err_message: format!("{err:#}"),
        },
    )
}

async fn get_notify_settings(State(state): State<AppState>) -> Json<NotifySettingsView> {
    Json(state.notifier.view())
}

async fn save_notify_settings(
    State(state): State<AppState>,
    Args(args): Args<SettingsArgs>,
) -> ApiResult<Json<NotifySettingsView>> {
    let settings = args
        .into_settings(&state.notifier.settings())
        .map_err(|err| api_error(StatusCode::BAD_REQUEST, &err))?;
    state.notifier.save(settings).map_err(|err| {
        tracing::error!("Failed to save the notification settings: {err:#}");
        api_error(StatusCode::INTERNAL_SERVER_ERROR, &err)
    })?;
    Ok(Json(state.notifier.view()))
}

/// Send a test message with the settings of the dialog, saved or not
///
/// Not logged: a wrong address while the settings are filled in is no error of the server.
async fn test_notify(
    State(state): State<AppState>,
    Args(args): Args<SettingsArgs>,
) -> ApiResult<Json<()>> {
    let settings = args
        .into_settings(&state.notifier.settings())
        .map_err(|err| api_error(StatusCode::BAD_REQUEST, &err))?;
    let language = state.context.config().read().language.clone();
    state
        .notifier
        .send(&settings, &test_message(&language, settings.censored))
        .await
        .map(Json)
        .map_err(|err| api_error(StatusCode::BAD_GATEWAY, &err))
}

#[cfg(test)]
mod tests {
    use axum::routing::post as post_route;

    use super::*;

    fn completed(title: &str) -> Happened {
        Happened::Completed {
            title: title.into(),
        }
    }

    #[test]
    fn a_batch_is_one_message_per_kind() {
        let happened = [
            completed("Alpha"),
            Happened::Failed {
                title: "Beta".into(),
            },
            completed("Gamma"),
            Happened::Followed {
                query: "artist:x".into(),
                count: 2,
            },
            Happened::Followed {
                query: "series:y".into(),
                count: 1,
            },
            Happened::Followed {
                query: "artist:x".into(),
                count: 3,
            },
        ];
        let messages = messages(&happened, false, "en-US");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].title, "2 comics downloaded");
        assert_eq!(messages[0].body, "• Alpha\n• Gamma");
        assert_eq!(messages[0].kind, "success");
        assert_eq!(messages[1].title, "1 download failed");
        assert!(messages[1].body.starts_with("• Beta\n\n"));
        assert_eq!(messages[1].kind, "failure");
        assert_eq!(
            messages[2].title,
            "6 new galleries from the followed searches"
        );
        assert!(
            messages[2]
                .body
                .starts_with("• artist:x: 5\n• series:y: 1\n\n"),
            "{}",
            messages[2].body
        );
    }

    #[test]
    fn censored_messages_name_no_title_and_no_query() {
        let happened = [
            completed("Secret Title"),
            Happened::Failed {
                title: "Other Secret".into(),
            },
            Happened::Followed {
                query: "artist:secret".into(),
                count: 4,
            },
        ];
        for language in ["en-US", "de-DE"] {
            for message in messages(&happened, true, language) {
                let text = format!("{} {}", message.title, message.body).to_lowercase();
                assert!(!text.contains("secret"), "{language}: {text}");
            }
        }
        let german = messages(&happened, true, "de-DE");
        assert_eq!(german[0].title, "1 Comic heruntergeladen");
        assert_eq!(german[2].title, "4 neue Galerien aus den gefolgten Suchen");
    }

    #[test]
    fn censored_the_app_is_only_hi_dl() {
        for language in ["en-US", "de-DE"] {
            let censored = test_message(language, true);
            let text = format!("{} {}", censored.title, censored.body);
            assert!(text.contains("hi-dl") && !text.contains("hitomi"), "{text}");
            let open = test_message(language, false);
            assert!(
                open.title.starts_with("hitomi-downloader:"),
                "{}",
                open.title
            );
        }
        // the messages of the downloads never named the app, censored or not
        let happened = [
            completed("A"),
            Happened::Followed {
                query: "q".into(),
                count: 1,
            },
        ];
        for message in messages(&happened, true, "en-US") {
            assert!(!format!("{} {}", message.title, message.body).contains("hitomi"));
        }
    }

    #[test]
    fn a_long_list_is_cut_and_counted() {
        let happened: Vec<Happened> = (1..=13).map(|n| completed(&format!("T{n}"))).collect();
        let message = &messages(&happened, false, "en-US")[0];
        assert_eq!(message.title, "13 comics downloaded");
        assert_eq!(message.body.lines().count(), MAX_TITLES + 1);
        assert!(message.body.ends_with("and 3 more"));
    }

    #[test]
    fn apprise_urls_are_checked_and_never_quoted_whole() {
        let lines = vec![
            "discord://WebhookID/Token\n\n  slack://A/B/C  ".to_string(),
            "tgram://bot/chat".into(),
        ];
        assert_eq!(
            parse_urls(&lines).unwrap(),
            [
                "discord://WebhookID/Token",
                "slack://A/B/C",
                "tgram://bot/chat"
            ]
        );
        let err =
            parse_urls(&["https-webhook-without-scheme/very-secret-token".into()]).unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("discord://") && !text.contains("very-secret-token"),
            "{text}"
        );
        assert!(endpoint("apprise:8000").is_err());
        assert_eq!(
            endpoint("http://apprise:8000").unwrap().as_str(),
            "http://apprise:8000/notify/"
        );
        assert_eq!(
            endpoint("https://x.lan/apprise/").unwrap().as_str(),
            "https://x.lan/apprise/notify/"
        );
    }

    /// An Apprise API that records what it was sent, and answers with `status`
    async fn fake_apprise(
        status: StatusCode,
    ) -> (String, Arc<parking_lot::Mutex<Vec<serde_json::Value>>>) {
        let seen: Arc<parking_lot::Mutex<Vec<serde_json::Value>>> = Arc::default();
        let sink = seen.clone();
        let app = Router::new().route(
            "/apprise/notify/",
            post_route(move |Json(body): Json<serde_json::Value>| async move {
                sink.lock().push(body);
                status
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/apprise"), seen)
    }

    fn temp_file() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hitomi-notify-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("apprise.json")
    }

    fn settings(url: &str) -> NotifySettings {
        NotifySettings {
            url: url.into(),
            urls: vec!["discord://id/token".into(), "tgram://bot/chat".into()],
            ..NotifySettings::default()
        }
    }

    #[tokio::test]
    async fn a_message_goes_to_the_stateless_endpoint_with_every_url() {
        let (url, seen) = fake_apprise(StatusCode::OK).await;
        let (notifier, _) = Notifier::open(temp_file());
        notifier
            .send(&settings(&url), &test_message("en-US", false))
            .await
            .unwrap();
        let body = seen.lock()[0].clone();
        assert_eq!(body["urls"], "discord://id/token tgram://bot/chat");
        assert_eq!(body["title"], "hitomi-downloader: test");
        assert_eq!(body["type"], "info");
    }

    #[tokio::test]
    async fn the_answers_of_apprise_are_explained() {
        let (url, _) = fake_apprise(StatusCode::FAILED_DEPENDENCY).await;
        let (notifier, _) = Notifier::open(temp_file());
        let err = notifier
            .send(&settings(&url), &test_message("en-US", false))
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("did not take the message"),
            "{err:#}"
        );
        let err = notifier
            .send(
                &settings("http://127.0.0.1:9"),
                &test_message("en-US", false),
            )
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("can not be reached"), "{err:#}");
        let empty = NotifySettings {
            urls: Vec::new(),
            ..settings(&url)
        };
        let err = notifier
            .send(&empty, &test_message("en-US", false))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("No Apprise URL"), "{err:#}");
    }

    #[test]
    fn the_settings_are_kept_for_the_owner_only_and_the_urls_never_shown() {
        let file = temp_file();
        let (notifier, _) = Notifier::open(file.clone());
        assert!(!notifier.settings().is_set());
        let saved = NotifySettings {
            censored: true,
            on_failed: false,
            ..settings("http://apprise:8000")
        };
        notifier.save(saved.clone()).unwrap();
        assert_eq!(Notifier::open(file.clone()).0.settings(), saved);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let view = serde_json::to_value(notifier.view()).unwrap();
        assert_eq!(
            view,
            json!({ "url": "http://apprise:8000", "urlCount": 2, "onCompleted": true,
                    "onFailed": false, "onFollowed": true, "censored": true })
        );
        assert!(!view.to_string().contains("token"));
    }

    fn update(comic_id: i32, state: DownloadTaskState) -> Event {
        Event::DownloadTask(DownloadTaskEvent::Update {
            comic_id,
            state,
            downloaded_img_count: 1,
            total_img_count: 1,
        })
    }

    fn create(id: i32, title: &str) -> Event {
        Event::DownloadTask(DownloadTaskEvent::Create {
            state: DownloadTaskState::Pending,
            comic: Box::new(hitomi_core::types::Comic {
                id,
                title: title.into(),
                ..Default::default()
            }),
            downloaded_img_count: 0,
            total_img_count: 1,
        })
    }

    const FAST: Debounce = Debounce {
        quiet: Duration::from_millis(150),
        max_wait: Duration::from_millis(600),
    };

    /// The loop on its own events, with `settings`, and what reached Apprise after `events`
    async fn sent_after(
        settings: NotifySettings,
        seen: &Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
        events: Vec<Event>,
        followed: Vec<(&str, usize)>,
    ) -> Vec<serde_json::Value> {
        let (notifier, happened) = Notifier::open(temp_file());
        notifier.save(settings).unwrap();
        let (sender, receiver) = broadcast::channel(64);
        let (stop, shutdown) = watch::channel(false);
        let looping = tokio::spawn(run(
            notifier.clone(),
            receiver,
            happened,
            || "en-US".to_string(),
            FAST,
            shutdown,
        ));
        for event in events {
            sender.send(event).unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        for (query, count) in followed {
            notifier.tell(Happened::Followed {
                query: query.into(),
                count,
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        stop.send(true).unwrap();
        looping.await.unwrap();
        seen.lock().drain(..).collect()
    }

    #[tokio::test]
    async fn downloads_close_together_are_one_message_with_their_titles() {
        let (url, seen) = fake_apprise(StatusCode::OK).await;
        let events = vec![
            create(1, "Alpha"),
            create(2, "Beta"),
            create(3, "Gamma"),
            update(1, DownloadTaskState::Downloading),
            update(1, DownloadTaskState::Completed),
            update(2, DownloadTaskState::Completed),
            update(3, DownloadTaskState::Failed),
        ];
        let sent = sent_after(settings(&url), &seen, events, vec![("artist:x", 2)]).await;
        let titles: Vec<&str> = sent
            .iter()
            .map(|body| body["title"].as_str().unwrap())
            .collect();
        assert_eq!(
            titles,
            [
                "2 comics downloaded",
                "1 download failed",
                "2 new galleries from the followed searches"
            ]
        );
        assert_eq!(sent[0]["body"], "• Alpha\n• Beta");
        assert!(sent[1]["body"].as_str().unwrap().contains("Gamma"));
        assert_eq!(sent[1]["type"], "failure");
    }

    #[tokio::test]
    async fn the_switches_and_censoring_decide_what_is_told() {
        let (url, seen) = fake_apprise(StatusCode::OK).await;
        let events = || {
            vec![
                create(1, "Secret"),
                update(1, DownloadTaskState::Completed),
                create(2, "Hidden"),
                update(2, DownloadTaskState::Failed),
            ]
        };

        let only_failed = NotifySettings {
            on_completed: false,
            on_followed: false,
            ..settings(&url)
        };
        let sent = sent_after(only_failed, &seen, events(), vec![("artist:x", 1)]).await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["title"], "1 download failed");

        let censored = NotifySettings {
            censored: true,
            ..settings(&url)
        };
        let sent = sent_after(censored, &seen, events(), vec![("artist:secret", 1)]).await;
        assert_eq!(sent.len(), 3);
        for body in &sent {
            let text = body.to_string().to_lowercase();
            assert!(
                !text.contains("secret") && !text.contains("hidden"),
                "{text}"
            );
        }
    }

    #[tokio::test]
    async fn without_settings_nothing_is_sent() {
        let (url, seen) = fake_apprise(StatusCode::OK).await;
        let no_urls = NotifySettings {
            urls: Vec::new(),
            ..settings(&url)
        };
        let sent = sent_after(
            no_urls,
            &seen,
            vec![create(1, "A"), update(1, DownloadTaskState::Completed)],
            vec![("q", 1)],
        )
        .await;
        assert!(sent.is_empty());
    }
}
