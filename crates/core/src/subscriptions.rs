//! Following searches: the server looks for new galleries by itself
//!
//! A subscription is a search query. Every so often the query is run, and the galleries it
//! finds that the subscription has not dealt with yet are handed to the download queue. A
//! check is one index download for the query and no request per gallery, only the galleries
//! that are new cost one request each.
//!
//! What a subscription has dealt with is stored, besides the library being looked at: a comic
//! that was deleted from the library must not come back at the next check. The search and the
//! download queue are behind traits, so that all of it runs in tests without Hitomi.
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::{
    context::Context, extensions::AnyhowErrorToStringChain, hitomi, types::SearchSort, utils,
};

/// How many searches can be followed
pub const MAX_SUBSCRIPTIONS: usize = 50;
/// How many galleries "download everything" takes at most, more than that only follows
/// from now on, so that a search that is too broad does not flood the queue
pub const MAX_DOWNLOAD_EVERYTHING: usize = 1000;
/// A check that failed is tried again after this, or after the interval if that is shorter
const ERROR_RETRY: Duration = Duration::from_secs(30 * 60);
/// The pause after a gallery was handed to the queue, so that a long list of new galleries
/// does not hammer Hitomi with requests
const QUEUE_PAUSE: Duration = Duration::from_millis(500);
const FILE_VERSION: u32 = 1;

/// Where the ids of a search come from
pub trait SearchSource: Send + Sync {
    /// The ids of the galleries that match `query`, the newest first
    fn ids<'a>(&'a self, query: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<i32>>>;
}

/// The downloads of the app, as far as a subscription needs them
pub trait Downloads: Send + Sync {
    /// The ids of the comics in the library
    fn library_ids(&self) -> HashSet<i32>;
    /// Whether the comic `id` is being downloaded
    fn is_active(&self, id: i32) -> bool;
    /// Put the comic `id` into the download queue
    fn queue<'a>(&'a self, id: i32) -> BoxFuture<'a, anyhow::Result<()>>;
}

/// The search of Hitomi, sorted by date
pub struct HitomiSearch;

impl SearchSource for HitomiSearch {
    fn ids<'a>(&'a self, query: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<i32>>> {
        Box::pin(async move {
            let ids = hitomi::do_search(query.to_string(), SearchSort::Date).await?;
            Ok(ids.into_iter().collect())
        })
    }
}

/// The library and the download queue of a [`Context`]
pub struct ContextDownloads {
    context: Context,
}

impl ContextDownloads {
    pub fn new(context: Context) -> Self {
        Self { context }
    }
}

impl Downloads for ContextDownloads {
    fn library_ids(&self) -> HashSet<i32> {
        let library = self.context.config().read().download_dir.clone();
        library_ids(&library)
    }

    fn is_active(&self, id: i32) -> bool {
        self.context.download_manager().is_active(id)
    }

    fn queue<'a>(&'a self, id: i32) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let comic = self.context.hitomi_client().get_comic(id).await?;
            self.context
                .download_manager()
                .create_download_task(comic)?;
            tokio::time::sleep(QUEUE_PAUSE).await;
            Ok(())
        })
    }
}

/// The ids of the comics in the library at `library`, from their `metadata.json`
pub fn library_ids(library: &Path) -> HashSet<i32> {
    #[derive(Deserialize)]
    struct IdOnly {
        id: i32,
    }
    WalkDir::new(library)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == "metadata.json")
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|json| serde_json::from_str::<IdOnly>(&json).ok())
        .map(|comic| comic.id)
        .collect()
}

/// What a new subscription does with the galleries that match already
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FirstRun {
    /// Only what is new after this
    FromNow,
    /// Everything that matches and is not in the library
    Everything,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Active,
    Paused,
    /// The last check failed, or could not queue every gallery
    Error,
    Checking,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Subscription {
    id: String,
    query: String,
    #[serde(default)]
    paused: bool,
    created_at: i64,
    /// When it was last looked at, whether that worked or not
    #[serde(default)]
    last_check_at: Option<i64>,
    /// How many galleries the last check that worked put into the queue
    #[serde(default)]
    last_new: usize,
    #[serde(default)]
    matches: usize,
    #[serde(default)]
    last_error: Option<String>,
    /// Every gallery up to this id counts as dealt with: what matched when it was made to
    /// follow from now on
    ///
    /// Hitomi numbers its galleries in the order they are added, so one number stands for
    /// all of them, a search with ten thousand matches would store ten thousand ids
    /// otherwise. A gallery that is that old and only matches later, a tag added to it,
    /// counts as old: it is not new on Hitomi.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    handled_up_to: Option<i32>,
    /// The ids above `handled_up_to` this subscription has dealt with, sorted: queued by it,
    /// or found in the library or the queue
    ///
    /// They have to be remembered, a comic that was deleted from the library must not be
    /// queued again. So the list grows with what the subscription downloads, and no faster.
    #[serde(default)]
    handled: Vec<i32>,
}

#[derive(Serialize, Deserialize)]
struct SubscriptionsFile {
    version: u32,
    subscriptions: Vec<Subscription>,
}

/// A subscription as the frontend shows it
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionView {
    pub id: String,
    pub query: String,
    pub status: Status,
    pub created_at: i64,
    pub last_check_at: Option<i64>,
    pub last_new: usize,
    pub matches: usize,
    pub last_error: Option<String>,
    /// When the next check is due, none for a paused one or when checks are off
    pub next_check_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionList {
    /// The interval between checks, none when they are off
    pub interval_hours: Option<u64>,
    pub next_check_at: Option<i64>,
    pub subscriptions: Vec<SubscriptionView>,
}

/// What a search would give, shown before it is followed
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub query: String,
    pub matches: usize,
    pub in_library: usize,
    /// What "download everything" would download
    pub to_download: usize,
    pub already_followed: bool,
    pub can_download_all: bool,
    pub limit: usize,
}

#[derive(Debug)]
pub enum SubscriptionError {
    /// The query has no word in it
    Empty,
    Duplicate,
    TooMany,
    TooManyMatches {
        count: usize,
        limit: usize,
    },
    NotFound,
    Search(anyhow::Error),
}

impl std::fmt::Display for SubscriptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "the search is empty"),
            Self::Duplicate => write!(f, "this search is followed already"),
            Self::TooMany => write!(f, "at most {MAX_SUBSCRIPTIONS} searches can be followed"),
            Self::TooManyMatches { count, limit } => write!(
                f,
                "{count} galleries would be downloaded, \"download everything\" takes at most {limit}"
            ),
            Self::NotFound => write!(f, "there is no such subscription"),
            Self::Search(err) => write!(f, "the search failed: {}", err.to_string_chain()),
        }
    }
}

impl std::error::Error for SubscriptionError {}

/// What a check found
struct Found {
    matches: usize,
    /// Handed to the download queue
    queued: usize,
    /// Dealt with now, queued or already there
    handled: Vec<i32>,
    /// Could not be put into the queue, they are tried again at the next check
    failed: usize,
}

/// The subscriptions of the app and what looks after them
pub struct Subscriptions {
    file: utils::AtomicFile,
    source: Arc<dyn SearchSource>,
    downloads: Arc<dyn Downloads>,
    /// How often a subscription is checked, none when the checks are off. A check that is
    /// asked for by hand works either way.
    interval: Option<Duration>,
    subscriptions: Mutex<Vec<Subscription>>,
    checking: Mutex<HashSet<String>>,
    /// One check at a time, whoever asks, so that Hitomi sees one client doing one thing
    turn: tokio::sync::Mutex<()>,
    /// Whether [`Subscriptions::check_all`] runs
    checking_all: AtomicBool,
    /// Told after a check that queued new galleries, see [`Subscriptions::on_new_galleries`]
    on_new: Mutex<Option<NewGalleries>>,
}

/// What [`Subscriptions::on_new_galleries`] calls, with the query and how many it queued
pub type NewGalleries = Arc<dyn Fn(&str, usize) + Send + Sync>;

/// The current time as seconds since 1970
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// `query` as it is stored and compared: words separated by one space, in lower case, which
/// is how the search reads it
fn normalize(query: &str) -> String {
    let query = query.trim();
    let query = query.strip_prefix('?').unwrap_or(query);
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// How many of `ids` are not in `library`
fn count_new(ids: &[i32], library: &HashSet<i32>) -> usize {
    ids.iter().filter(|id| !library.contains(id)).count()
}

/// Marks a subscription as being checked for as long as it lives
struct CheckingFlag<'a> {
    set: &'a Mutex<HashSet<String>>,
    id: String,
}

impl<'a> CheckingFlag<'a> {
    fn new(set: &'a Mutex<HashSet<String>>, id: &str) -> Self {
        set.lock().insert(id.to_string());
        Self {
            set,
            id: id.to_string(),
        }
    }
}

impl Drop for CheckingFlag<'_> {
    fn drop(&mut self) {
        self.set.lock().remove(&self.id);
    }
}

/// Sets the flag back when it is dropped, also when the future that holds it is
struct RunningFlag<'a>(&'a AtomicBool);

impl Drop for RunningFlag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Subscriptions {
    /// The subscriptions stored in `path`, none if there is no file
    pub fn open(
        path: PathBuf,
        source: Arc<dyn SearchSource>,
        downloads: Arc<dyn Downloads>,
        interval: Option<Duration>,
    ) -> Arc<Subscriptions> {
        let subscriptions = load(&path);
        Arc::new(Subscriptions {
            file: utils::AtomicFile::new(path),
            source,
            downloads,
            interval,
            subscriptions: Mutex::new(subscriptions),
            checking: Mutex::new(HashSet::new()),
            turn: tokio::sync::Mutex::new(()),
            checking_all: AtomicBool::new(false),
            on_new: Mutex::new(None),
        })
    }

    /// Call `on_new` after every check that queued new galleries, the notifications of the
    /// server listen there
    pub fn on_new_galleries(&self, on_new: NewGalleries) {
        *self.on_new.lock() = Some(on_new);
    }

    pub fn list(&self, now: i64) -> SubscriptionList {
        let subscriptions = self.subscriptions.lock();
        let views: Vec<SubscriptionView> = subscriptions
            .iter()
            .map(|subscription| self.view(subscription, now))
            .collect();
        SubscriptionList {
            interval_hours: self.interval.map(|interval| interval.as_secs() / 3600),
            next_check_at: views.iter().filter_map(|view| view.next_check_at).min(),
            subscriptions: views,
        }
    }

    fn view(&self, subscription: &Subscription, now: i64) -> SubscriptionView {
        let status = if self.checking.lock().contains(&subscription.id) {
            Status::Checking
        } else if subscription.paused {
            Status::Paused
        } else if subscription.last_error.is_some() {
            Status::Error
        } else {
            Status::Active
        };
        SubscriptionView {
            id: subscription.id.clone(),
            query: subscription.query.clone(),
            status,
            created_at: subscription.created_at,
            last_check_at: subscription.last_check_at,
            last_new: subscription.last_new,
            matches: subscription.matches,
            last_error: subscription.last_error.clone(),
            next_check_at: self.due_at(subscription).map(|due| due.max(now)),
        }
    }

    /// When `subscription` is due for a check, none if it never is
    ///
    /// One that was never checked is due at once.
    fn due_at(&self, subscription: &Subscription) -> Option<i64> {
        let interval = self.interval?;
        if subscription.paused {
            return None;
        }
        let delay = if subscription.last_error.is_some() {
            interval.min(ERROR_RETRY)
        } else {
            interval
        };
        Some(
            subscription
                .last_check_at
                .map_or(i64::MIN, |last| last + delay.as_secs() as i64),
        )
    }

    /// What following `query` would do, for the dialog that asks how to follow
    pub async fn preview(&self, query: &str) -> Result<Preview, SubscriptionError> {
        let query = normalize(query);
        if query.is_empty() {
            return Err(SubscriptionError::Empty);
        }
        let ids = self
            .source
            .ids(&query)
            .await
            .map_err(SubscriptionError::Search)?;
        let library = self.library_ids().await;
        let to_download = count_new(&ids, &library);
        let already_followed = self
            .subscriptions
            .lock()
            .iter()
            .any(|subscription| subscription.query == query);
        Ok(Preview {
            query,
            matches: ids.len(),
            in_library: ids.len() - to_download,
            to_download,
            already_followed,
            can_download_all: to_download <= MAX_DOWNLOAD_EVERYTHING,
            limit: MAX_DOWNLOAD_EVERYTHING,
        })
    }

    /// Follow `query`
    ///
    /// A subscription that follows from now on takes every gallery that matches as dealt
    /// with. One that downloads everything has not dealt with any, and its first check
    /// queues them: call [`Subscriptions::check`] for it.
    pub async fn create(
        &self,
        query: &str,
        first_run: FirstRun,
        now: i64,
    ) -> Result<SubscriptionView, SubscriptionError> {
        let query = normalize(query);
        if query.is_empty() {
            return Err(SubscriptionError::Empty);
        }
        self.ensure_room_for(&query)?;
        let ids = self
            .source
            .ids(&query)
            .await
            .map_err(SubscriptionError::Search)?;

        let (handled_up_to, last_check_at) = match first_run {
            FirstRun::FromNow => (ids.iter().max().copied(), Some(now)),
            FirstRun::Everything => {
                let library = self.library_ids().await;
                let count = count_new(&ids, &library);
                if count > MAX_DOWNLOAD_EVERYTHING {
                    return Err(SubscriptionError::TooManyMatches {
                        count,
                        limit: MAX_DOWNLOAD_EVERYTHING,
                    });
                }
                (None, None)
            }
        };
        let subscription = Subscription {
            id: uuid::Uuid::new_v4().to_string(),
            query: query.clone(),
            paused: false,
            created_at: now,
            last_check_at,
            last_new: 0,
            matches: ids.len(),
            last_error: None,
            handled_up_to,
            handled: Vec::new(),
        };
        let view = {
            let mut subscriptions = self.subscriptions.lock();
            // checked again: another request may have followed it while the search ran
            Self::ensure_room(&subscriptions, &query)?;
            subscriptions.push(subscription.clone());
            self.view(&subscription, now)
        };
        self.save();
        tracing::info!(query, "Following a search");
        Ok(view)
    }

    fn ensure_room_for(&self, query: &str) -> Result<(), SubscriptionError> {
        Self::ensure_room(&self.subscriptions.lock(), query)
    }

    fn ensure_room(subscriptions: &[Subscription], query: &str) -> Result<(), SubscriptionError> {
        if subscriptions
            .iter()
            .any(|subscription| subscription.query == query)
        {
            return Err(SubscriptionError::Duplicate);
        }
        if subscriptions.len() >= MAX_SUBSCRIPTIONS {
            return Err(SubscriptionError::TooMany);
        }
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<(), SubscriptionError> {
        {
            let mut subscriptions = self.subscriptions.lock();
            let before = subscriptions.len();
            subscriptions.retain(|subscription| subscription.id != id);
            if subscriptions.len() == before {
                return Err(SubscriptionError::NotFound);
            }
        }
        self.save();
        Ok(())
    }

    pub fn set_paused(&self, id: &str, paused: bool) -> Result<(), SubscriptionError> {
        {
            let mut subscriptions = self.subscriptions.lock();
            let subscription = subscriptions
                .iter_mut()
                .find(|subscription| subscription.id == id)
                .ok_or(SubscriptionError::NotFound)?;
            subscription.paused = paused;
        }
        self.save();
        Ok(())
    }

    /// Whether there is a subscription `id`
    pub fn exists(&self, id: &str) -> bool {
        self.subscriptions
            .lock()
            .iter()
            .any(|subscription| subscription.id == id)
    }

    /// Whether the subscription `id` is being checked now
    pub fn is_checking(&self, id: &str) -> bool {
        self.checking.lock().contains(id)
    }

    /// The ids of the subscriptions that are not paused
    pub fn active_ids(&self) -> Vec<String> {
        self.subscriptions
            .lock()
            .iter()
            .filter(|subscription| !subscription.paused)
            .map(|subscription| subscription.id.clone())
            .collect()
    }

    /// Check the subscription `id` now: look for new galleries and queue them
    ///
    /// It waits for its turn, one check runs at a time. A check that fails is not an error
    /// of this call, it is recorded in the subscription, where the list shows it.
    pub async fn check(&self, id: &str, now: i64) -> Result<(), SubscriptionError> {
        let _turn = self.turn.lock().await;
        let subscription = self
            .subscriptions
            .lock()
            .iter()
            .find(|subscription| subscription.id == id)
            .cloned()
            .ok_or(SubscriptionError::NotFound)?;
        let _checking = CheckingFlag::new(&self.checking, id);

        let outcome = self.look_for_new(&subscription).await;
        match &outcome {
            Ok(found) => tracing::info!(
                query = subscription.query,
                matches = found.matches,
                queued = found.queued,
                failed = found.failed,
                "Checked a followed search"
            ),
            Err(err) => tracing::error!(
                query = subscription.query,
                message = err.to_string_chain(),
                "Failed to check a followed search"
            ),
        }
        let queued = outcome.as_ref().map_or(0, |found| found.queued);
        self.record(id, now, outcome);
        if queued > 0 {
            // taken out of the lock first, the callback may take its time
            let on_new = self.on_new.lock().clone();
            if let Some(on_new) = on_new {
                on_new(&subscription.query, queued);
            }
        }
        Ok(())
    }

    async fn look_for_new(&self, subscription: &Subscription) -> anyhow::Result<Found> {
        let ids = self.source.ids(&subscription.query).await?;
        let library = self.library_ids().await;
        let handled: HashSet<i32> = subscription.handled.iter().copied().collect();

        let mut found = Found {
            matches: ids.len(),
            queued: 0,
            handled: Vec::new(),
            failed: 0,
        };
        // the oldest first, which is the order a series is read in
        for id in ids.into_iter().rev() {
            if subscription.handled_up_to.is_some_and(|up_to| id <= up_to) || handled.contains(&id)
            {
                continue;
            }
            // in the library, or about to be: another subscription may have queued it
            if library.contains(&id) || self.downloads.is_active(id) {
                found.handled.push(id);
                continue;
            }
            match self.downloads.queue(id).await {
                Ok(()) => {
                    found.queued += 1;
                    found.handled.push(id);
                }
                Err(err) => {
                    found.failed += 1;
                    tracing::error!(
                        id,
                        message = err.to_string_chain(),
                        "Failed to queue a gallery of a followed search, it is tried again"
                    );
                }
            }
        }
        Ok(found)
    }

    fn record(&self, id: &str, now: i64, outcome: anyhow::Result<Found>) {
        {
            let mut subscriptions = self.subscriptions.lock();
            // deleted while it was checked
            let Some(subscription) = subscriptions
                .iter_mut()
                .find(|subscription| subscription.id == id)
            else {
                return;
            };
            subscription.last_check_at = Some(now);
            match outcome {
                Ok(found) => {
                    subscription.last_new = found.queued;
                    subscription.matches = found.matches;
                    subscription.handled.extend(found.handled);
                    subscription.handled.sort_unstable();
                    subscription.handled.dedup();
                    subscription.last_error = (found.failed > 0).then(|| {
                        format!(
                            "{} galleries could not be queued, they are tried again at the next check",
                            found.failed
                        )
                    });
                }
                Err(err) => subscription.last_error = Some(err.to_string_chain()),
            }
        }
        self.save();
    }

    /// Check every subscription that is due, one after the other with `pause` between them
    ///
    /// `now` is asked again for each check, they can take long.
    pub async fn run_due(&self, now: impl Fn() -> i64, pause: Duration) {
        let mut due: Vec<(i64, String)> = {
            let current = now();
            self.subscriptions
                .lock()
                .iter()
                .filter_map(|subscription| {
                    let due = self.due_at(subscription)?;
                    (due <= current).then(|| (due, subscription.id.clone()))
                })
                .collect()
        };
        // the one that waited longest first
        due.sort();
        self.check_in_turn(due.into_iter().map(|(_, id)| id).collect(), now, pause)
            .await;
    }

    /// Check every subscription that is not paused, as `run_due` does
    ///
    /// Not while it runs already: the second run would wait behind the first, and go through
    /// all of them once more right after it. `false` when it did not run for that reason.
    pub async fn check_all(&self, now: impl Fn() -> i64, pause: Duration) -> bool {
        if self.checking_all.swap(true, Ordering::SeqCst) {
            return false;
        }
        let _running = RunningFlag(&self.checking_all);
        self.check_in_turn(self.active_ids(), now, pause).await;
        true
    }

    async fn check_in_turn(&self, ids: Vec<String>, now: impl Fn() -> i64, pause: Duration) {
        let count = ids.len();
        for (index, id) in ids.into_iter().enumerate() {
            // paused or deleted while it waited
            let skip = self
                .subscriptions
                .lock()
                .iter()
                .find(|subscription| subscription.id == id)
                .is_none_or(|subscription| subscription.paused);
            if skip {
                continue;
            }
            let _ = self.check(&id, now()).await;
            if index + 1 < count {
                tokio::time::sleep(pause).await;
            }
        }
    }

    async fn library_ids(&self) -> HashSet<i32> {
        let downloads = self.downloads.clone();
        // a walk over the whole library, which must not block the async runtime
        tokio::task::spawn_blocking(move || downloads.library_ids())
            .await
            .unwrap_or_default()
    }

    /// Write the subscriptions as they are once the other writes of the file are done, so
    /// that a write with an older state, a check that ends, never lands after a newer one
    fn save(&self) {
        let result = self.file.write(|| {
            let file = SubscriptionsFile {
                version: FILE_VERSION,
                subscriptions: self.subscriptions.lock().clone(),
            };
            Ok(serde_json::to_vec(&file)?)
        });
        if let Err(err) = result {
            tracing::error!(
                message = err.to_string_chain(),
                "Failed to save the followed searches"
            );
        }
    }
}

fn load(path: &Path) -> Vec<Subscription> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::error!(
                "Failed to read the followed searches `{}`: {err}",
                path.display()
            );
            return Vec::new();
        }
    };
    match serde_json::from_str::<SubscriptionsFile>(&text) {
        Ok(file) if file.version == FILE_VERSION => file.subscriptions,
        Ok(file) => {
            utils::set_aside(
                path,
                "The followed searches",
                &format!(
                    "its version is {}, this one reads {FILE_VERSION}",
                    file.version
                ),
            );
            Vec::new()
        }
        Err(err) => {
            utils::set_aside(path, "The followed searches", &err.to_string());
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    const T0: i64 = 1_000_000;
    const HOUR: i64 = 3600;

    /// The search, with answers the test sets
    #[derive(Default)]
    struct FakeSearch {
        results: Mutex<HashMap<String, Result<Vec<i32>, String>>>,
        delay: Duration,
        running: AtomicUsize,
        most_running: AtomicUsize,
        calls: AtomicUsize,
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

    impl SearchSource for FakeSearch {
        fn ids<'a>(&'a self, query: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<i32>>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
                self.most_running.fetch_max(running, Ordering::SeqCst);
                tokio::time::sleep(self.delay).await;
                self.running.fetch_sub(1, Ordering::SeqCst);
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
        library: Mutex<HashSet<i32>>,
        active: Mutex<HashSet<i32>>,
        failing: Mutex<HashSet<i32>>,
        queued: Mutex<Vec<i32>>,
    }

    impl Downloads for FakeDownloads {
        fn library_ids(&self) -> HashSet<i32> {
            self.library.lock().clone()
        }

        fn is_active(&self, id: i32) -> bool {
            self.active.lock().contains(&id)
        }

        fn queue<'a>(&'a self, id: i32) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                if self.failing.lock().contains(&id) {
                    anyhow::bail!("no gallery info for {id}");
                }
                self.queued.lock().push(id);
                Ok(())
            })
        }
    }

    struct Fixture {
        dir: PathBuf,
        search: Arc<FakeSearch>,
        downloads: Arc<FakeDownloads>,
        subscriptions: Arc<Subscriptions>,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with(Some(Duration::from_secs(6 * 3600)), Duration::ZERO)
        }

        fn with(interval: Option<Duration>, delay: Duration) -> Self {
            let dir = std::env::temp_dir().join(format!("hitomi-subs-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let search = Arc::new(FakeSearch {
                delay,
                ..FakeSearch::default()
            });
            let downloads = Arc::new(FakeDownloads::default());
            let subscriptions = Subscriptions::open(
                dir.join("subscriptions.json"),
                search.clone(),
                downloads.clone(),
                interval,
            );
            Self {
                dir,
                search,
                downloads,
                subscriptions,
            }
        }

        fn reopen(&self) -> Arc<Subscriptions> {
            Subscriptions::open(
                self.dir.join("subscriptions.json"),
                self.search.clone(),
                self.downloads.clone(),
                Some(Duration::from_secs(6 * 3600)),
            )
        }

        fn queued(&self) -> Vec<i32> {
            self.downloads.queued.lock().clone()
        }

        fn view(&self, id: &str) -> SubscriptionView {
            self.subscriptions
                .list(T0)
                .subscriptions
                .into_iter()
                .find(|view| view.id == id)
                .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn a_query_is_compared_by_its_words_in_lower_case() {
        assert_eq!(
            normalize("  Artist:Beater   LANGUAGE:english "),
            "artist:beater language:english"
        );
        assert_eq!(normalize("?artist:beater"), "artist:beater");
        assert_eq!(normalize("   "), "");
    }

    #[tokio::test]
    async fn following_from_now_takes_what_matches_as_dealt_with() {
        let fixture = Fixture::new();
        fixture.search.set("artist:beater", &[5, 4, 3]);
        let view = fixture
            .subscriptions
            .create("artist:beater", FirstRun::FromNow, T0)
            .await
            .unwrap();
        assert_eq!(view.matches, 3);
        assert_eq!(view.status, Status::Active);

        // nothing new: nothing is queued
        fixture
            .subscriptions
            .check(&view.id, T0 + HOUR)
            .await
            .unwrap();
        assert_eq!(fixture.queued(), Vec::<i32>::new());

        // a new gallery shows up
        fixture.search.set("artist:beater", &[6, 5, 4, 3]);
        fixture
            .subscriptions
            .check(&view.id, T0 + 7 * HOUR)
            .await
            .unwrap();
        assert_eq!(fixture.queued(), [6]);
        assert_eq!(fixture.view(&view.id).last_new, 1);
        assert_eq!(fixture.view(&view.id).matches, 4);

        // and is never queued a second time
        fixture
            .subscriptions
            .check(&view.id, T0 + 13 * HOUR)
            .await
            .unwrap();
        assert_eq!(fixture.queued(), [6]);
        assert_eq!(fixture.view(&view.id).last_new, 0);
    }

    #[tokio::test]
    async fn a_check_that_queued_new_galleries_says_so() {
        let fixture = Fixture::new();
        let told: Arc<Mutex<Vec<(String, usize)>>> = Arc::default();
        let sink = told.clone();
        fixture
            .subscriptions
            .on_new_galleries(Arc::new(move |query: &str, count: usize| {
                sink.lock().push((query.to_string(), count));
            }));
        fixture.search.set("artist:beater", &[5, 4, 3]);
        let view = fixture
            .subscriptions
            .create("artist:beater", FirstRun::FromNow, T0)
            .await
            .unwrap();

        // nothing new, nothing is said
        fixture
            .subscriptions
            .check(&view.id, T0 + HOUR)
            .await
            .unwrap();
        assert!(told.lock().is_empty());

        fixture.search.set("artist:beater", &[7, 6, 5, 4, 3]);
        fixture
            .subscriptions
            .check(&view.id, T0 + 7 * HOUR)
            .await
            .unwrap();
        assert_eq!(*told.lock(), [("artist:beater".to_string(), 2)]);
    }

    #[tokio::test]
    async fn following_everything_queues_what_is_not_there_oldest_first() {
        let fixture = Fixture::new();
        fixture.search.set("series:x", &[5, 4, 3, 2]);
        // 3 is in the library, 4 is being downloaded by something else
        fixture.downloads.library.lock().insert(3);
        fixture.downloads.active.lock().insert(4);
        let view = fixture
            .subscriptions
            .create("series:x", FirstRun::Everything, T0)
            .await
            .unwrap();
        // it has not been looked at yet, the first check does it
        assert_eq!(view.last_check_at, None);
        assert_eq!(fixture.queued(), Vec::<i32>::new());

        fixture.subscriptions.check(&view.id, T0).await.unwrap();
        assert_eq!(fixture.queued(), [2, 5]);
        assert_eq!(fixture.view(&view.id).last_new, 2);
    }

    #[tokio::test]
    async fn a_comic_deleted_from_the_library_is_not_downloaded_again() {
        let fixture = Fixture::new();
        fixture.search.set("series:x", &[3, 2]);
        fixture.downloads.library.lock().insert(2);
        let view = fixture
            .subscriptions
            .create("series:x", FirstRun::Everything, T0)
            .await
            .unwrap();
        fixture.subscriptions.check(&view.id, T0).await.unwrap();
        assert_eq!(fixture.queued(), [3]);

        // 3 arrived in the library, and the user deleted both comics
        fixture.downloads.library.lock().clear();
        fixture
            .subscriptions
            .check(&view.id, T0 + 7 * HOUR)
            .await
            .unwrap();
        assert_eq!(fixture.queued(), [3], "nothing is queued again");
    }

    #[tokio::test]
    async fn download_everything_is_refused_above_the_limit_but_following_is_not() {
        let fixture = Fixture::new();
        let many: Vec<i32> = (1..=1001).collect();
        fixture.search.set("language:english", &many);

        let preview = fixture
            .subscriptions
            .preview("language:english")
            .await
            .unwrap();
        assert_eq!(preview.matches, 1001);
        assert_eq!(preview.to_download, 1001);
        assert!(!preview.can_download_all);

        let err = fixture
            .subscriptions
            .create("language:english", FirstRun::Everything, T0)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            SubscriptionError::TooManyMatches {
                count: 1001,
                limit: 1000
            }
        ));
        // from now on has no limit, nothing is downloaded
        let view = fixture
            .subscriptions
            .create("language:english", FirstRun::FromNow, T0)
            .await
            .unwrap();
        assert_eq!(view.matches, 1001);

        // what is in the library does not count against the limit
        fixture.search.set("type:manga", &many);
        fixture.downloads.library.lock().extend(1..=50);
        let preview = fixture.subscriptions.preview("type:manga").await.unwrap();
        assert_eq!((preview.in_library, preview.to_download), (50, 951));
        assert!(preview.can_download_all);
    }

    #[tokio::test]
    async fn the_same_search_can_not_be_followed_twice_and_only_so_many_can() {
        let fixture = Fixture::new();
        fixture
            .subscriptions
            .create("artist:beater language:english", FirstRun::FromNow, T0)
            .await
            .unwrap();
        // the words and their case do not matter
        let err = fixture
            .subscriptions
            .create("  ARTIST:Beater   language:english", FirstRun::FromNow, T0)
            .await
            .unwrap_err();
        assert!(matches!(err, SubscriptionError::Duplicate));
        assert!(
            fixture
                .subscriptions
                .preview("artist:beater  language:english")
                .await
                .unwrap()
                .already_followed
        );

        for index in 1..MAX_SUBSCRIPTIONS {
            fixture
                .subscriptions
                .create(&format!("tag:t{index}"), FirstRun::FromNow, T0)
                .await
                .unwrap();
        }
        let err = fixture
            .subscriptions
            .create("tag:one too many", FirstRun::FromNow, T0)
            .await
            .unwrap_err();
        assert!(matches!(err, SubscriptionError::TooMany));
        assert!(matches!(
            fixture
                .subscriptions
                .create("   ", FirstRun::FromNow, T0)
                .await,
            Err(SubscriptionError::Empty)
        ));
    }

    #[tokio::test]
    async fn a_gallery_that_could_not_be_queued_is_tried_again() {
        let fixture = Fixture::new();
        fixture.search.set("series:x", &[9, 8, 7]);
        fixture.downloads.failing.lock().insert(8);
        let view = fixture
            .subscriptions
            .create("series:x", FirstRun::Everything, T0)
            .await
            .unwrap();
        fixture.subscriptions.check(&view.id, T0).await.unwrap();
        // the others went through
        assert_eq!(fixture.queued(), [7, 9]);
        let shown = fixture.view(&view.id);
        assert_eq!(shown.status, Status::Error);
        assert!(shown.last_error.unwrap().contains("1 galleries"));

        fixture.downloads.failing.lock().clear();
        fixture
            .subscriptions
            .check(&view.id, T0 + HOUR)
            .await
            .unwrap();
        assert_eq!(fixture.queued(), [7, 9, 8], "only the one that failed");
        let shown = fixture.view(&view.id);
        assert_eq!(shown.status, Status::Active);
        assert_eq!(shown.last_error, None);
    }

    #[tokio::test]
    async fn a_failed_search_is_recorded_and_keeps_the_state() {
        let fixture = Fixture::new();
        fixture.search.set("artist:beater", &[3, 2, 1]);
        let view = fixture
            .subscriptions
            .create("artist:beater", FirstRun::FromNow, T0)
            .await
            .unwrap();

        fixture.search.fail("artist:beater");
        fixture
            .subscriptions
            .check(&view.id, T0 + HOUR)
            .await
            .unwrap();
        let shown = fixture.view(&view.id);
        assert_eq!(shown.status, Status::Error);
        assert_eq!(shown.last_check_at, Some(T0 + HOUR));
        assert_eq!(shown.matches, 3, "what it knew stays");
        assert!(shown.last_error.unwrap().contains("Hitomi is not there"));

        // it works again, and nothing was lost or queued twice
        fixture.search.set("artist:beater", &[4, 3, 2, 1]);
        fixture
            .subscriptions
            .check(&view.id, T0 + 2 * HOUR)
            .await
            .unwrap();
        assert_eq!(fixture.queued(), [4]);
        assert_eq!(fixture.view(&view.id).status, Status::Active);
    }

    #[tokio::test]
    async fn a_search_that_does_not_work_can_not_be_followed() {
        let fixture = Fixture::new();
        fixture.search.fail("artist:beater");
        assert!(matches!(
            fixture
                .subscriptions
                .create("artist:beater", FirstRun::FromNow, T0)
                .await,
            Err(SubscriptionError::Search(_))
        ));
        assert!(fixture.subscriptions.list(T0).subscriptions.is_empty());
    }

    #[tokio::test]
    async fn a_subscription_is_due_after_the_interval_and_sooner_after_an_error() {
        let fixture = Fixture::new();
        fixture.search.set("a", &[1]);
        let view = fixture
            .subscriptions
            .create("a", FirstRun::FromNow, T0)
            .await
            .unwrap();
        // made at T0 and looked at then: due six hours later
        assert_eq!(view.next_check_at, Some(T0 + 6 * HOUR));
        let due = |now: i64| fixture.subscriptions.list(now).next_check_at;
        assert_eq!(due(T0 + HOUR), Some(T0 + 6 * HOUR));
        // overdue is now
        assert_eq!(due(T0 + 9 * HOUR), Some(T0 + 9 * HOUR));

        fixture.search.fail("a");
        fixture
            .subscriptions
            .check(&view.id, T0 + 6 * HOUR)
            .await
            .unwrap();
        assert_eq!(
            fixture.view(&view.id).next_check_at,
            Some(T0 + 6 * HOUR + 1800),
            "a failed check is tried again in half an hour"
        );
    }

    #[tokio::test]
    async fn run_due_checks_only_what_is_due_the_longest_waiting_first() {
        let fixture = Fixture::new();
        for query in ["old", "older", "fresh", "paused"] {
            fixture.search.set(query, &[1]);
        }
        let mut ids = HashMap::new();
        for (query, at) in [
            ("old", T0),
            ("older", T0 - HOUR),
            ("fresh", T0 + 5 * HOUR),
            ("paused", T0 - 9 * HOUR),
        ] {
            let view = fixture
                .subscriptions
                .create(query, FirstRun::FromNow, at)
                .await
                .unwrap();
            ids.insert(query, view.id);
        }
        fixture
            .subscriptions
            .set_paused(&ids["paused"], true)
            .unwrap();
        // new galleries everywhere, to see which subscription was checked
        for query in ["old", "older", "fresh", "paused"] {
            fixture.search.set(query, &[1, 100 + query.len() as i32]);
        }
        // "older" is due since T0 + 5h, "old" since T0 + 6h, "fresh" is due at T0 + 11h
        fixture
            .subscriptions
            .run_due(|| T0 + 7 * HOUR, Duration::ZERO)
            .await;
        assert_eq!(
            fixture.queued(),
            [105, 103],
            "older (5 letters) before old (3)"
        );
        assert_eq!(fixture.view(&ids["fresh"]).last_new, 0);
        assert_eq!(fixture.view(&ids["paused"]).status, Status::Paused);

        // nothing is due right after
        fixture
            .subscriptions
            .run_due(|| T0 + 7 * HOUR + 60, Duration::ZERO)
            .await;
        assert_eq!(fixture.queued().len(), 2);
    }

    #[tokio::test]
    async fn without_an_interval_nothing_is_due_but_a_check_by_hand_works() {
        let fixture = Fixture::with(None, Duration::ZERO);
        fixture.search.set("a", &[1]);
        let view = fixture
            .subscriptions
            .create("a", FirstRun::FromNow, T0)
            .await
            .unwrap();
        assert_eq!(view.next_check_at, None);
        assert_eq!(fixture.subscriptions.list(T0).interval_hours, None);

        fixture.search.set("a", &[2, 1]);
        fixture
            .subscriptions
            .run_due(|| T0 + 100 * HOUR, Duration::ZERO)
            .await;
        assert_eq!(fixture.queued(), Vec::<i32>::new());

        fixture
            .subscriptions
            .check(&view.id, T0 + 100 * HOUR)
            .await
            .unwrap();
        assert_eq!(fixture.queued(), [2]);
    }

    #[tokio::test]
    async fn check_all_skips_the_paused_ones() {
        let fixture = Fixture::new();
        let mut ids = Vec::new();
        for query in ["a", "b"] {
            fixture.search.set(query, &[1]);
            ids.push(
                fixture
                    .subscriptions
                    .create(query, FirstRun::FromNow, T0)
                    .await
                    .unwrap()
                    .id,
            );
        }
        fixture.subscriptions.set_paused(&ids[1], true).unwrap();
        fixture.search.set("a", &[10, 1]);
        fixture.search.set("b", &[20, 1]);
        fixture
            .subscriptions
            .check_all(|| T0 + HOUR, Duration::ZERO)
            .await;
        assert_eq!(fixture.queued(), [10]);
    }

    #[tokio::test]
    async fn check_all_does_not_start_while_it_runs() {
        let fixture = Fixture::with(None, Duration::from_millis(100));
        for query in ["a", "b"] {
            fixture.search.set(query, &[1]);
            fixture
                .subscriptions
                .create(query, FirstRun::FromNow, T0)
                .await
                .unwrap();
        }
        fixture.search.calls.store(0, Ordering::SeqCst);
        let subscriptions = fixture.subscriptions.clone();
        let runs = (0..5).map(|_| subscriptions.check_all(|| T0 + HOUR, Duration::ZERO));
        let started = futures::future::join_all(runs).await;
        assert_eq!(started.iter().filter(|started| **started).count(), 1);
        // one search for each subscription, and no second round
        assert_eq!(fixture.search.calls.load(Ordering::SeqCst), 2);

        // once it is over, it can run again
        assert!(
            fixture
                .subscriptions
                .check_all(|| T0 + 2 * HOUR, Duration::ZERO)
                .await
        );
        assert_eq!(fixture.search.calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn following_a_big_search_from_now_on_stores_one_number_and_not_its_matches() {
        let fixture = Fixture::new();
        let matches: Vec<i32> = (1..=10_000).rev().collect();
        fixture.search.set("language:english", &matches);
        let view = fixture
            .subscriptions
            .create("language:english", FirstRun::FromNow, T0)
            .await
            .unwrap();
        let stored = std::fs::read_to_string(fixture.dir.join("subscriptions.json")).unwrap();
        assert!(stored.len() < 1000, "{} bytes", stored.len());
        let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
        let subscription = &stored["subscriptions"][0];
        assert_eq!(subscription["handledUpTo"], 10_000);
        assert_eq!(subscription["handled"], serde_json::json!([]));

        // none of them is new, but the next one is
        let mut with_new = matches.clone();
        with_new.insert(0, 10_001);
        fixture.search.set("language:english", &with_new);
        fixture
            .subscriptions
            .check(&view.id, T0 + HOUR)
            .await
            .unwrap();
        assert_eq!(fixture.queued(), [10_001]);

        // it arrived and was deleted again, it is not queued a second time, after a restart
        // neither: the list holds what the subscription downloaded, and nothing else
        let reopened = fixture.reopen();
        reopened.check(&view.id, T0 + 2 * HOUR).await.unwrap();
        assert_eq!(fixture.queued(), [10_001]);
        let stored = std::fs::read_to_string(fixture.dir.join("subscriptions.json")).unwrap();
        let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(
            stored["subscriptions"][0]["handled"],
            serde_json::json!([10_001])
        );
    }

    #[tokio::test]
    async fn a_file_with_the_old_list_still_counts_its_ids_as_dealt_with() {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.dir.join("subscriptions.json"),
            r#"{"version":1,"subscriptions":[{"id":"old","query":"a","createdAt":1,"handled":[1,2,3]}]}"#,
        )
        .unwrap();
        let subscriptions = fixture.reopen();
        fixture.search.set("a", &[4, 3, 2, 1]);
        subscriptions.check("old", T0).await.unwrap();
        assert_eq!(fixture.queued(), [4]);
    }

    #[tokio::test]
    async fn pausing_and_deleting() {
        let fixture = Fixture::new();
        fixture.search.set("a", &[1]);
        let view = fixture
            .subscriptions
            .create("a", FirstRun::FromNow, T0)
            .await
            .unwrap();
        fixture.subscriptions.set_paused(&view.id, true).unwrap();
        let shown = fixture.view(&view.id);
        assert_eq!((shown.status, shown.next_check_at), (Status::Paused, None));
        fixture.subscriptions.set_paused(&view.id, false).unwrap();
        assert_eq!(fixture.view(&view.id).status, Status::Active);

        fixture.subscriptions.delete(&view.id).unwrap();
        assert!(fixture.subscriptions.list(T0).subscriptions.is_empty());
        assert!(matches!(
            fixture.subscriptions.delete(&view.id),
            Err(SubscriptionError::NotFound)
        ));
        assert!(matches!(
            fixture.subscriptions.set_paused(&view.id, true),
            Err(SubscriptionError::NotFound)
        ));
        assert!(matches!(
            fixture.subscriptions.check(&view.id, T0).await,
            Err(SubscriptionError::NotFound)
        ));
        // it can be followed again, from the start
        assert!(fixture
            .subscriptions
            .create("a", FirstRun::FromNow, T0)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn checks_run_one_at_a_time() {
        let fixture = Fixture::with(None, Duration::from_millis(30));
        let mut ids = Vec::new();
        for query in ["a", "b", "c"] {
            fixture.search.set(query, &[1]);
            ids.push(
                fixture
                    .subscriptions
                    .create(query, FirstRun::FromNow, T0)
                    .await
                    .unwrap()
                    .id,
            );
        }
        // the three searches of the creation ran one after the other as well
        fixture.search.most_running.store(0, Ordering::SeqCst);
        let subscriptions = fixture.subscriptions.clone();
        let (a, b, c) = tokio::join!(
            subscriptions.check(&ids[0], T0),
            subscriptions.check(&ids[1], T0),
            subscriptions.check(&ids[2], T0)
        );
        a.unwrap();
        b.unwrap();
        c.unwrap();
        assert_eq!(fixture.search.most_running.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_subscription_that_is_checked_says_so() {
        let fixture = Fixture::with(None, Duration::from_millis(200));
        fixture.search.set("a", &[1]);
        let view = fixture
            .subscriptions
            .create("a", FirstRun::FromNow, T0)
            .await
            .unwrap();
        let subscriptions = fixture.subscriptions.clone();
        let id = view.id.clone();
        let check = tokio::spawn(async move { subscriptions.check(&id, T0).await });
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(fixture.view(&view.id).status, Status::Checking);
        assert!(fixture.subscriptions.is_checking(&view.id));
        check.await.unwrap().unwrap();
        assert!(!fixture.subscriptions.is_checking(&view.id));
        assert_eq!(fixture.view(&view.id).status, Status::Active);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_delete_or_pause_during_a_check_is_still_there_after_it() {
        let fixture = Fixture::with(None, Duration::from_millis(150));
        fixture.search.set("a", &[1]);
        fixture.search.set("b", &[1]);
        let a = fixture
            .subscriptions
            .create("a", FirstRun::FromNow, T0)
            .await
            .unwrap()
            .id;
        let b = fixture
            .subscriptions
            .create("b", FirstRun::FromNow, T0)
            .await
            .unwrap()
            .id;
        fixture.search.set("a", &[2, 1]);
        fixture.search.set("b", &[3, 1]);

        let subscriptions = fixture.subscriptions.clone();
        let (check_a, check_b) = (a.clone(), b.clone());
        let checks = tokio::spawn(async move {
            let (first, second) = tokio::join!(
                subscriptions.check(&check_a, T0 + HOUR),
                subscriptions.check(&check_b, T0 + HOUR)
            );
            first.unwrap();
            second.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(fixture.subscriptions.is_checking(&a) || fixture.subscriptions.is_checking(&b));
        fixture.subscriptions.delete(&a).unwrap();
        fixture.subscriptions.set_paused(&b, true).unwrap();
        checks.await.unwrap();

        for subscriptions in [fixture.subscriptions.clone(), fixture.reopen()] {
            let list = subscriptions.list(T0 + HOUR).subscriptions;
            assert_eq!(list.len(), 1, "the deleted one came back");
            assert_eq!(list[0].id, b);
            assert_eq!(list[0].status, Status::Paused, "the pause was undone");
        }
    }

    #[tokio::test]
    async fn the_subscriptions_survive_a_restart() {
        let fixture = Fixture::new();
        fixture.search.set("a", &[2, 1]);
        let view = fixture
            .subscriptions
            .create("a", FirstRun::Everything, T0)
            .await
            .unwrap();
        fixture
            .subscriptions
            .check(&view.id, T0 + HOUR)
            .await
            .unwrap();
        fixture.subscriptions.set_paused(&view.id, true).unwrap();

        let reopened = fixture.reopen();
        let list = reopened.list(T0 + 2 * HOUR);
        assert_eq!(list.subscriptions.len(), 1);
        let shown = &list.subscriptions[0];
        assert_eq!(shown.query, "a");
        assert_eq!(shown.status, Status::Paused);
        assert_eq!(
            (shown.last_check_at, shown.last_new, shown.matches),
            (Some(T0 + HOUR), 2, 2)
        );

        // what it dealt with is remembered: nothing is queued again
        reopened.set_paused(&view.id, false).unwrap();
        reopened.check(&view.id, T0 + 8 * HOUR).await.unwrap();
        assert_eq!(fixture.queued(), [1, 2]);
    }

    #[tokio::test]
    async fn a_file_that_can_not_be_read_is_moved_aside_and_starts_empty() {
        let fixture = Fixture::new();
        std::fs::write(fixture.dir.join("subscriptions.json"), b"{ not json").unwrap();
        let reopened = fixture.reopen();
        assert!(reopened.list(T0).subscriptions.is_empty());
        assert!(fixture.dir.join("subscriptions.json.corrupt").exists());
        fixture.search.set("a", &[1]);
        reopened.create("a", FirstRun::FromNow, T0).await.unwrap();
        assert_eq!(fixture.reopen().list(T0).subscriptions.len(), 1);
    }

    #[test]
    fn the_library_ids_come_from_the_metadata_files_wherever_they_are() {
        let dir = std::env::temp_dir().join(format!("hitomi-lib-{}", uuid::Uuid::new_v4()));
        let write = |relative: &str, content: &str| {
            let path = dir.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        };
        write("one/metadata.json", r#"{"id": 11, "title": "x"}"#);
        write("artist/two/metadata.json", r#"{"id": 22}"#);
        // not a comic, or not readable: ignored
        write("broken/metadata.json", "not json");
        write("other/readme.txt", r#"{"id": 33}"#);
        assert_eq!(library_ids(&dir), HashSet::from([11, 22]));
        assert!(library_ids(&dir.join("missing")).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
