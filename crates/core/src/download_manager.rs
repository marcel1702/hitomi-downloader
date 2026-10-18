use std::{
    collections::HashMap,
    future::Future,
    ops::ControlFlow,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{anyhow, Context as _};
use bytes::Bytes;
use indexmap::IndexMap;
use parking_lot::RwLock;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use specta::Type;
use tokio::{
    sync::{watch, Notify, Semaphore, SemaphorePermit},
    task::JoinSet,
};

use crate::{
    context::Context,
    events::{DownloadSpeedEvent, DownloadTaskEvent},
    extensions::AnyhowErrorToStringChain,
    hitomi::{image_url_from_image, invalidate_gg, Ext, GalleryFiles},
    hitomi_client::{HitomiClient, UnexpectedStatus},
    pages,
    queue::{self, StoredTask},
    types::{Comic, ComicInfo, DownloadFormat},
    utils::filename_filter,
};

/// Used to manage download tasks
///
/// Cloning `DownloadManager` has minimal overhead, and the performance overhead is almost negligible.
/// You can safely clone and use it in multiple threads.
///
/// Specifically:
/// - `context` is a [`Context`], cloning it only increases a reference count.
/// - Other fields are wrapped in `Arc`, and the cloning operation of these fields only increases the reference count.
#[derive(Clone)]
pub struct DownloadManager {
    context: Context,
    comic_sem: Arc<Semaphore>,
    img_sem: Arc<Semaphore>,
    /// The number of permits `comic_sem` is meant to hold, the permits it currently has
    /// available say nothing about that while downloads hold some of them
    comic_limit: Arc<AtomicU32>,
    img_limit: Arc<AtomicU32>,
    byte_per_sec: Arc<AtomicU64>,
    /// In the order the tasks were created, which is the order the frontend lists them in
    download_tasks: Arc<RwLock<IndexMap<i32, DownloadTask>>>,
    /// Where the queue is kept between runs, see [`crate::context::Options`]
    queue_file: Option<PathBuf>,
    /// Told whenever a task is added or changes its state, which is what the file holds
    queue_changed: Arc<Notify>,
}

/// How long the queue waits for more changes before it is written, so that many tasks that
/// change at once, a batch download above all, are one write
const QUEUE_WRITE_DELAY: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum DownloadTaskState {
    Pending,
    Downloading,
    Paused,
    Cancelled,
    Completed,
    Failed,
}

impl DownloadManager {
    pub fn new(context: &Context) -> Self {
        let (comic_concurrency, img_concurrency) = {
            let config = context.config().read();
            (config.comic_concurrency, config.img_concurrency)
        };
        let manager = DownloadManager {
            context: context.clone(),
            comic_sem: Arc::new(Semaphore::new(comic_concurrency as usize)),
            img_sem: Arc::new(Semaphore::new(img_concurrency as usize)),
            comic_limit: Arc::new(AtomicU32::new(comic_concurrency)),
            img_limit: Arc::new(AtomicU32::new(img_concurrency)),
            byte_per_sec: Arc::new(AtomicU64::new(0)),
            download_tasks: Arc::new(RwLock::new(IndexMap::new())),
            queue_file: context.queue_file().map(Path::to_path_buf),
            queue_changed: Arc::new(Notify::new()),
        };

        context.spawn(manager.clone().emit_download_speed_loop());
        if manager.queue_file.is_some() {
            context.spawn(manager.clone().persist_queue_loop());
        }

        manager
    }

    pub fn create_download_task(&self, comic: Comic) -> anyhow::Result<()> {
        let download_format = self.context.config().read().download_format;
        self.create_task(comic, download_format, DownloadTaskState::Pending)
    }

    /// Create the task of `comic` in `state`, which is `Pending` for a new download
    ///
    /// A task that is restored as `Paused` waits for a resume, one restored as `Failed` is
    /// only listed and waits for a retry.
    fn create_task(
        &self,
        comic: Comic,
        download_format: DownloadFormat,
        state: DownloadTaskState,
    ) -> anyhow::Result<()> {
        use DownloadTaskState::{Downloading, Failed, Paused, Pending};
        let id = comic.id;
        let mut tasks = self.download_tasks.write();
        if let Some(task) = tasks.get(&id) {
            // If the task already exists and the state is `Pending`, `Downloading`, or `Paused`, a new task will not be created
            let state = *task.state_sender.borrow();
            if matches!(state, Pending | Downloading | Paused) {
                return Ok(());
            }
        }
        let task = DownloadTask::new(self.context.clone(), comic, download_format, state)
            .context(format!("Failed to create download task with id `{id}`",))?;
        if state == Failed {
            // nothing runs, the task is a line in the list that can be retried
            task.emit_download_task_create_event();
        } else {
            self.context.spawn(task.clone().process());
        }
        tasks.insert(id, task);
        drop(tasks);
        self.mark_queue_changed();
        Ok(())
    }

    /// Take the downloads of the last run back from the queue file
    ///
    /// Called once the process is set up, not by [`Context::init_with`]: a restored download
    /// works out its directory from the settings, which a server pins to its volumes after
    /// the context exists, and it writes to the log, which starts after that.
    ///
    /// Those that waited or ran start again, and fetch only the pages that are missing: the
    /// ones that were downloaded lie in the temporary directory still. Paused and failed ones
    /// come back as they were.
    pub fn restore_queue(&self) {
        let Some(path) = &self.queue_file else {
            return;
        };
        self.restore(queue::load(path));
    }

    fn restore(&self, stored: Vec<StoredTask>) {
        let total = stored.len();
        let mut restored = 0;
        for StoredTask {
            state,
            download_format,
            comic,
        } in stored
        {
            let id = comic.id;
            let Some(state) = restored_state(state) else {
                continue;
            };
            match self.create_task(comic, download_format, state) {
                Ok(()) => restored += 1,
                Err(err) => {
                    let err_title = format!("Failed to restore the download of `{id}`, dropped");
                    let string_chain = err.to_string_chain();
                    tracing::error!(err_title, message = string_chain);
                }
            }
        }
        if total > 0 {
            tracing::info!(restored, total, "Restored the download queue");
        }
    }

    /// The downloads the queue file holds, in the order they were created
    fn queue_snapshot(&self) -> Vec<StoredTask> {
        self.download_tasks
            .read()
            .values()
            .filter_map(|task| {
                let state = *task.state_sender.borrow();
                if !queue::is_stored(state) {
                    return None;
                }
                // both are worked out again from the settings of the next run
                let mut comic = task.comic.as_ref().clone();
                comic.comic_download_dir = None;
                comic.is_downloaded = None;
                Some(StoredTask {
                    state,
                    download_format: task.download_format,
                    comic,
                })
            })
            .collect()
    }

    fn mark_queue_changed(&self) {
        if self.queue_file.is_some() {
            self.queue_changed.notify_one();
        }
    }

    /// Write the queue file now, which a process that is about to end does
    ///
    /// Also what the end of a download does, so that a comic that is in the library is not in
    /// the file any more when the process ends a moment later.
    pub fn save_queue_now(&self) {
        let Some(path) = &self.queue_file else {
            return;
        };
        if let Err(err) = queue::save(path, &self.queue_snapshot()) {
            let err_title = "Failed to save the download queue";
            let string_chain = err.to_string_chain();
            tracing::error!(err_title, message = string_chain);
        }
    }

    async fn persist_queue_loop(self) {
        loop {
            self.queue_changed.notified().await;
            // what changes while it waits is written with it
            tokio::time::sleep(QUEUE_WRITE_DELAY).await;
            let manager = self.clone();
            // the file can be megabytes, which must not block the async runtime
            if let Err(err) = tokio::task::spawn_blocking(move || manager.save_queue_now()).await {
                tracing::error!("Failed to save the download queue: {err}");
            }
        }
    }

    pub fn pause_download_task(&self, id: i32) -> anyhow::Result<()> {
        let tasks = self.download_tasks.read();
        let Some(task) = tasks.get(&id) else {
            return Err(anyhow!("Can't find download task with id `{id}`"));
        };
        task.set_state(DownloadTaskState::Paused);
        Ok(())
    }

    pub fn resume_download_task(&self, id: i32) -> anyhow::Result<()> {
        use DownloadTaskState::{Cancelled, Completed, Failed, Pending};
        let comic = {
            let tasks = self.download_tasks.read();
            let Some(task) = tasks.get(&id) else {
                return Err(anyhow!("Can't find download task with id `{id}`"));
            };
            let task_state = *task.state_sender.borrow();

            if matches!(task_state, Failed | Cancelled | Completed) {
                // If the task state is `Failed`, `Cancelled`, or `Completed`, get the comic to recreate the download task
                Some(task.comic.as_ref().clone())
            } else {
                task.set_state(Pending);
                None
            }
        };
        // If comic is not None, recreate the download task
        if let Some(comic) = comic {
            self.create_download_task(comic)
                .context(format!("Failed to recreate download task with id `{id}`"))?;
        }
        Ok(())
    }

    pub fn cancel_download_task(&self, id: i32) -> anyhow::Result<()> {
        let tasks = self.download_tasks.read();
        let Some(task) = tasks.get(&id) else {
            return Err(anyhow!("Can't find download task with id `{id}`"));
        };
        task.set_state(DownloadTaskState::Cancelled);
        Ok(())
    }

    /// Whether the comic `id` is being downloaded: waiting, running or paused
    ///
    /// Its pages are about to become its archive then, so it can not be deleted.
    pub fn is_active(&self, id: i32) -> bool {
        self.download_tasks.read().get(&id).is_some_and(|task| {
            matches!(
                *task.state_sender.borrow(),
                DownloadTaskState::Pending
                    | DownloadTaskState::Downloading
                    | DownloadTaskState::Paused
            )
        })
    }

    /// A `Create` event for every task, carrying its current state and progress
    ///
    /// Lets a frontend that connects later, a second browser above all, list the downloads
    /// that started before it. Replaying them is harmless for a frontend that already has
    /// them, a `Create` replaces the entry of its comic.
    pub fn task_snapshots(&self) -> Vec<DownloadTaskEvent> {
        self.download_tasks
            .read()
            .values()
            .map(DownloadTask::create_event)
            .collect()
    }

    /// Change how many comics and images may download at the same time
    ///
    /// Running downloads are never interrupted. A lower limit takes effect as they finish.
    pub fn set_concurrency(&self, comic_concurrency: u32, img_concurrency: u32) {
        let shrinks = [
            resize_semaphore(&self.comic_sem, &self.comic_limit, comic_concurrency),
            resize_semaphore(&self.img_sem, &self.img_limit, img_concurrency),
        ];
        for shrink in shrinks.into_iter().flatten() {
            self.context.spawn(shrink);
        }
        tracing::debug!(
            comic_concurrency,
            img_concurrency,
            "Changed download concurrency"
        );
    }

    #[allow(clippy::cast_precision_loss)]
    async fn emit_download_speed_loop(self) {
        let mut interval = tokio::time::interval(Duration::from_secs(1));

        loop {
            interval.tick().await;
            let byte_per_sec = self.byte_per_sec.swap(0, Ordering::Relaxed);
            let mega_byte_per_sec = byte_per_sec as f64 / 1024.0 / 1024.0;
            let speed = format!("{mega_byte_per_sec:.2} MB/s");

            self.context.emit(DownloadSpeedEvent { speed });
        }
    }
}

/// Make `sem` hold `new_limit` permits, where `limit` tracks how many it is meant to hold
///
/// Growing takes effect at once. Permits held by running downloads can not be taken back,
/// so shrinking returns a future that waits for enough of them to be released and then
/// retires them for good. Until then a semaphore that is fair, as tokio's is, already lets
/// no new download start ahead of that future.
/// The state a stored download comes back in, none for one that should not be stored
///
/// One that was running waits for its turn like the others.
fn restored_state(stored: DownloadTaskState) -> Option<DownloadTaskState> {
    match stored {
        DownloadTaskState::Downloading => Some(DownloadTaskState::Pending),
        state if queue::is_stored(state) => Some(state),
        _ => None,
    }
}

fn resize_semaphore(
    sem: &Arc<Semaphore>,
    limit: &AtomicU32,
    new_limit: u32,
) -> Option<impl Future<Output = ()> + Send + 'static> {
    let old_limit = limit.swap(new_limit, Ordering::Relaxed);
    if new_limit >= old_limit {
        sem.add_permits((new_limit - old_limit) as usize);
        return None;
    }

    let sem = sem.clone();
    let surplus = old_limit - new_limit;
    Some(async move {
        // Only fails once the semaphore is closed, which never happens
        if let Ok(permits) = sem.acquire_many_owned(surplus).await {
            permits.forget();
        }
    })
}

#[derive(Clone)]
struct DownloadTask {
    context: Context,
    download_manager: DownloadManager,
    comic: Arc<Comic>,
    state_sender: watch::Sender<DownloadTaskState>,
    downloaded_img_count: Arc<AtomicU32>,
    total_img_count: Arc<AtomicU32>,
    download_format: DownloadFormat,
}

impl DownloadTask {
    pub fn new(
        context: Context,
        mut comic: Comic,
        download_format: DownloadFormat,
        state: DownloadTaskState,
    ) -> anyhow::Result<Self> {
        comic
            .update_dir_name_fields_by_fmt(&context)
            .context(format!(
                "Failed to update directory name fields by fmt of `{}`",
                comic.title
            ))?;

        let download_manager = context.download_manager().clone();
        let (state_sender, _) = watch::channel(state);
        // a task that waits for a resume or a retry shows the size of its comic already
        let total_img_count = if state == DownloadTaskState::Pending {
            0
        } else {
            u32::try_from(comic.files.len()).unwrap_or(u32::MAX)
        };

        let task = Self {
            context,
            download_manager,
            comic: Arc::new(comic),
            state_sender,
            downloaded_img_count: Arc::new(AtomicU32::new(0)),
            total_img_count: Arc::new(AtomicU32::new(total_img_count)),
            download_format,
        };

        Ok(task)
    }

    async fn process(self) {
        self.emit_download_task_create_event();

        let download_comic_task = self.download_comic();
        tokio::pin!(download_comic_task);

        let mut state_receiver = self.state_sender.subscribe();
        state_receiver.mark_changed();
        let mut permit = None;
        loop {
            let state_is_downloading = *state_receiver.borrow() == DownloadTaskState::Downloading;
            let state_is_pending = *state_receiver.borrow() == DownloadTaskState::Pending;
            tokio::select! {
                () = &mut download_comic_task, if state_is_downloading && permit.is_some() => break,
                control_flow = self.acquire_comic_permit(&mut permit), if state_is_pending => {
                    match control_flow {
                        ControlFlow::Continue(()) => continue,
                        ControlFlow::Break(()) => break,
                    }
                },
                _ = state_receiver.changed() => {
                    match self.handle_state_change(&mut permit, &mut state_receiver) {
                        ControlFlow::Continue(()) => continue,
                        ControlFlow::Break(()) => break,
                    }
                }
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    async fn download_comic(&self) {
        let id = self.comic.id;
        let comic_title = &self.comic.title;
        // the total number of images that need to be downloaded
        self.total_img_count
            .store(self.comic.files.len() as u32, Ordering::Relaxed);
        // create temporary download directory
        let Some(temp_download_dir) = self.create_temp_download_dir() else {
            return;
        };
        // The format of each image and where it is saved. The format is the one the task
        // was created with, not the current config, so that a format change while the task
        // waits can not mix up file names and downloaded data
        let img_formats: Vec<DownloadFormat> = self
            .comic
            .files
            .iter()
            .map(|file| img_format_of(file, self.download_format))
            .collect();
        let save_paths: Vec<PathBuf> = img_formats
            .iter()
            .enumerate()
            .map(|(i, format)| {
                let extension = format.to_extension();
                temp_download_dir.join(format!("{:04}.{extension}", i + 1))
            })
            .collect();
        // delete files in the temporary download directory that are not one of the images above
        if let Err(err) = self.clean_temp_download_dir(&temp_download_dir, &save_paths) {
            let err_title =
                format!("Failed to clean temporary download directory of `{comic_title}`");
            let string_chain = err.to_string_chain();
            tracing::error!(err_title, message = string_chain);

            self.set_state(DownloadTaskState::Failed);
            self.emit_download_task_update_event();

            return;
        }

        let mut join_set = JoinSet::new();
        // create download tasks one by one
        let imgs = self.comic.files.iter().zip(img_formats).zip(save_paths);
        for ((file, format), save_path) in imgs {
            let download_img_task = DownloadImgTask::new(self, file.clone(), format, save_path);
            // create download task
            join_set.spawn(download_img_task.process());
        }
        // wait for all download tasks to complete
        join_set.join_all().await;
        tracing::trace!(id, comic_title, "All images downloaded");
        // check if all images of this comic are downloaded successfully
        let downloaded_img_count = self.downloaded_img_count.load(Ordering::Relaxed);
        let total_img_count = self.total_img_count.load(Ordering::Relaxed);
        if downloaded_img_count != total_img_count {
            // not all images of this comic are downloaded successfully
            let err_title = format!("`{comic_title}` download incomplete");
            let err_msg = format!(
                "There are `{total_img_count}` images in total, but only `{downloaded_img_count}` images are downloaded"
            );
            tracing::error!(err_title, message = err_msg);

            self.set_state(DownloadTaskState::Failed);
            self.emit_download_task_update_event();

            return;
        }
        // all images of this comic are downloaded successfully
        // Packing reads and writes the whole comic, so it must not block the async runtime
        let pack_result = {
            let task = self.clone();
            let temp_download_dir = temp_download_dir.clone();
            tokio::task::spawn_blocking(move || task.pack_temp_download_dir(&temp_download_dir))
                .await
                .map_err(anyhow::Error::from)
                .and_then(|pack_result| pack_result)
        };
        let download_dir = match pack_result {
            Ok(download_dir) => download_dir,
            Err(err) => {
                let err_title =
                    format!("Failed to pack the downloaded images of `{comic_title}` into a cbz");
                let string_chain = err.to_string_chain();
                tracing::error!(err_title, message = string_chain);

                self.set_state(DownloadTaskState::Failed);
                self.emit_download_task_update_event();

                return;
            }
        };
        // finally, save the metadata and cover of this comic
        if let Err(err) = self.save_metadata(&download_dir) {
            // The library scan finds comics by their metadata file, so without it the
            // comic stays invisible no matter that its archive is on disk
            let err_title = format!("Failed to save metadata of `{comic_title}`");
            let string_chain = err.to_string_chain();
            tracing::error!(err_title, message = string_chain);

            self.set_state(DownloadTaskState::Failed);
            self.emit_download_task_update_event();

            return;
        }
        if let Err(err) = self.save_cover(&download_dir).await {
            let err_title = format!("Failed to save cover of `{comic_title}`");
            let string_chain = err.to_string_chain();
            tracing::error!(err_title, message = string_chain);
            // cover saving failure is not fatal, continue
        }
        tracing::info!(id, comic_title, "Comic download successfully");

        self.set_state(DownloadTaskState::Completed);
        self.emit_download_task_update_event();
        // not after the delay of the writer: a restart in between would download it again
        let manager = self.download_manager.clone();
        if let Err(err) = tokio::task::spawn_blocking(move || manager.save_queue_now()).await {
            tracing::error!("Failed to save the download queue: {err}");
        }
    }

    fn create_temp_download_dir(&self) -> Option<PathBuf> {
        let id = self.comic.id;
        let comic_title = &self.comic.title;

        let temp_download_dir = match self.comic.get_temp_download_dir(&self.context) {
            Ok(temp_download_dir) => temp_download_dir,
            Err(err) => {
                let err_title = format!("Failed to get temp download directory of `{comic_title}`");
                let string_chain = err.to_string_chain();
                tracing::error!(err_title, message = string_chain);

                self.set_state(DownloadTaskState::Failed);
                self.emit_download_task_update_event();

                return None;
            }
        };

        if let Err(err) = std::fs::create_dir_all(&temp_download_dir).map_err(anyhow::Error::from) {
            let err_title = format!(
                "Failed to create directory of `{comic_title}`: `{}`",
                temp_download_dir.display()
            );
            let string_chain = err.to_string_chain();
            tracing::error!(err_title, message = string_chain);

            self.set_state(DownloadTaskState::Failed);
            self.emit_download_task_update_event();

            return None;
        }

        tracing::trace!(
            id,
            comic_title,
            "Create temporary download directory `{}` successfully",
            temp_download_dir.display()
        );

        Some(temp_download_dir)
    }

    /// Delete files in the temporary download directory that do not match `config.download_format`
    fn clean_temp_download_dir(
        &self,
        temp_download_dir: &Path,
        save_paths: &[PathBuf],
    ) -> anyhow::Result<()> {
        let id = self.comic.id;
        let comic_title = &self.comic.title;

        let entries = std::fs::read_dir(temp_download_dir).context(format!(
            "Failed to read temporary download directory `{}`",
            temp_download_dir.display()
        ))?;

        for path in entries.filter_map(Result::ok).map(|entry| entry.path()) {
            if !save_paths.contains(&path) {
                std::fs::remove_file(&path)
                    .context(format!("Failed to delete file `{}`", path.display()))?;
            }
        }

        tracing::trace!(
            id,
            comic_title,
            "Clean temporary download directory `{}` successfully",
            temp_download_dir.display()
        );

        Ok(())
    }

    async fn acquire_comic_permit<'a>(
        &'a self,
        permit: &mut Option<SemaphorePermit<'a>>,
    ) -> ControlFlow<()> {
        let id = self.comic.id;
        let comic_title = &self.comic.title;

        tracing::debug!(id, comic_title, "Comic is pending");

        self.emit_download_task_update_event();

        *permit = match permit.take() {
            // If there is a permit, use it directly
            Some(permit) => Some(permit),
            // If there is no permit, get the permit
            None => match self
                .download_manager
                .comic_sem
                .acquire()
                .await
                .map_err(anyhow::Error::from)
            {
                Ok(permit) => Some(permit),
                Err(err) => {
                    let err_title =
                        format!("Failed to get the permit to download the comic `{comic_title}`");
                    let string_chain = err.to_string_chain();
                    tracing::error!(err_title, message = string_chain);

                    self.set_state(DownloadTaskState::Failed);
                    self.emit_download_task_update_event();

                    return ControlFlow::Break(());
                }
            },
        };
        // If the current task state is not `Pending`, the task state will not be set to `Downloading`
        if *self.state_sender.borrow() != DownloadTaskState::Pending {
            return ControlFlow::Continue(());
        }
        // Set the task state to `Downloading`
        if let Err(err) = self
            .state_sender
            .send(DownloadTaskState::Downloading)
            .map_err(anyhow::Error::from)
        {
            let err_title = format!("Failed to send state `Downloading` to `{comic_title}`");
            let string_chain = err.to_string_chain();
            tracing::error!(err_title, message = string_chain);
            return ControlFlow::Break(());
        }
        self.download_manager.mark_queue_changed();
        ControlFlow::Continue(())
    }

    fn handle_state_change<'a>(
        &'a self,
        permit: &mut Option<SemaphorePermit<'a>>,
        state_receiver: &mut watch::Receiver<DownloadTaskState>,
    ) -> ControlFlow<()> {
        let id = self.comic.id;
        let comic_title = &self.comic.title;

        self.emit_download_task_update_event();
        let state = *state_receiver.borrow();
        match state {
            DownloadTaskState::Paused => {
                tracing::debug!(id, comic_title, "Comic is paused");
                if let Some(permit) = permit.take() {
                    drop(permit);
                }
                ControlFlow::Continue(())
            }
            DownloadTaskState::Cancelled => {
                tracing::debug!(id, comic_title, "Comic is cancelled");
                ControlFlow::Break(())
            }
            _ => ControlFlow::Continue(()),
        }
    }

    fn set_state(&self, state: DownloadTaskState) {
        // `send` leaves the state as it is while nobody listens, which is the case until the
        // task has started: a pause or a cancel right after the task was created would be lost
        self.state_sender.send_replace(state);
        self.download_manager.mark_queue_changed();
    }

    fn emit_download_task_update_event(&self) {
        self.context.emit(DownloadTaskEvent::Update {
            comic_id: self.comic.id,
            state: *self.state_sender.borrow(),
            downloaded_img_count: self.downloaded_img_count.load(Ordering::Relaxed),
            total_img_count: self.total_img_count.load(Ordering::Relaxed),
        });
    }

    fn emit_download_task_create_event(&self) {
        self.context.emit(self.create_event());
    }

    fn create_event(&self) -> DownloadTaskEvent {
        DownloadTaskEvent::Create {
            state: *self.state_sender.borrow(),
            comic: Box::new(self.comic.as_ref().clone()),
            downloaded_img_count: self.downloaded_img_count.load(Ordering::Relaxed),
            total_img_count: self.total_img_count.load(Ordering::Relaxed),
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn save_metadata(&self, download_dir: &Path) -> anyhow::Result<()> {
        let mut comic = self.comic.as_ref().clone();
        // Set the `is_downloaded` and `comic_download_dir` field to `None`
        // so that the `is_downloaded` and `comic_download_dir` field is ignored during serialization
        comic.is_downloaded = None;
        comic.comic_download_dir = None;

        let comic_title = &comic.title;
        let comic_json = serde_json::to_string_pretty(&comic).context(format!(
            "Failed to save metadata of `{comic_title}`, Failed to serialize Comic to json"
        ))?;

        let metadata_path = download_dir.join("metadata.json");

        std::fs::write(&metadata_path, comic_json).context(format!(
            "Failed to save metadata of `{comic_title}`, Failed to write json to `{}`",
            metadata_path.display()
        ))?;

        Ok(())
    }

    async fn save_cover(&self, download_dir: &Path) -> anyhow::Result<()> {
        let comic_title = &self.comic.title;
        let cover_url = &self.comic.cover_url;
        let hitomi_client = self.context.hitomi_client().clone();

        let cover_data = hitomi_client
            .get_cover_data(cover_url)
            .await
            .context(format!("Failed to download cover for `{comic_title}`"))?;

        let cover_path = download_dir.join("cover.webp");
        std::fs::write(&cover_path, &cover_data).context(format!(
            "Failed to save cover for `{comic_title}` to `{}`",
            cover_path.display()
        ))?;

        tracing::trace!(comic_title, "Cover saved to `{}`", cover_path.display());
        Ok(())
    }

    /// Pack the images in the temporary download directory into the comic's cbz archive,
    /// return the download directory
    ///
    /// The archive is written next to `metadata.json` and `cover.webp`, which the caller
    /// writes afterwards. The download directory itself is never deleted, so the metadata
    /// and cover of an earlier download of the same comic survive until they are overwritten.
    fn pack_temp_download_dir(&self, temp_download_dir: &Path) -> anyhow::Result<PathBuf> {
        let id = self.comic.id;
        let comic_title = &self.comic.title;

        let download_dir = self
            .comic
            .comic_download_dir
            .clone()
            .context("`comic_download_dir` is None")?;

        std::fs::create_dir_all(&download_dir).context(format!(
            "Failed to create directory `{}`",
            download_dir.display()
        ))?;

        let cbz_path = pages::cbz_path_of(&download_dir).context(format!(
            "Failed to get the cbz path of `{}`",
            download_dir.display()
        ))?;
        // Write the archive to a temporary path first, so an interrupted run can never
        // leave a half written archive behind that later looks like a finished download
        let cbz_tmp_path = pages::cbz_tmp_path_of(&cbz_path);

        let comic_info = ComicInfo::from_comic(&self.comic);
        let image_paths = pages::collect_dir_pages(temp_download_dir).context(format!(
            "Failed to collect the downloaded images in `{}`",
            temp_download_dir.display()
        ))?;

        if let Err(err) = pages::write_cbz(&cbz_tmp_path, &comic_info, &image_paths) {
            // Leave the temporary download directory in place so the next run can resume from it
            let _ = std::fs::remove_file(&cbz_tmp_path);
            return Err(err).context(format!("Failed to write `{}`", cbz_tmp_path.display()));
        }

        std::fs::rename(&cbz_tmp_path, &cbz_path).context(format!(
            "Failed to rename `{}` to `{}`",
            cbz_tmp_path.display(),
            cbz_path.display()
        ))?;
        // The pages are deleted below, so the rename has to be on the disk before that. A
        // directory that can not be synced does not fail the download, the archive is on the
        // disk already, and a rename lost to a crash leaves the complete `.cbz.tmp` behind.
        if let Err(err) = pages::sync_dir(&download_dir) {
            let err_title = format!("Failed to sync the directory of `{comic_title}` to the disk");
            let string_chain = anyhow::Error::from(err).to_string_chain();
            tracing::warn!(err_title, message = string_chain);
        }
        tracing::trace!(
            id,
            comic_title,
            "Packed the downloaded images into `{}` successfully",
            cbz_path.display()
        );
        // The comic is on disk from here on, so a problem while tidying up is logged
        // instead of failing a download that already succeeded
        if let Err(err) = clean_up_after_packing(temp_download_dir, &download_dir) {
            let err_title = format!("Failed to clean up after packing `{comic_title}`");
            let string_chain = err.to_string_chain();
            tracing::warn!(err_title, message = string_chain);
        }

        Ok(download_dir)
    }
}

/// Delete what the finished archive replaces
///
/// That is the images just downloaded, and the loose pages of an earlier folder style
/// download of the same comic, which would otherwise sit next to the archive as a stale
/// duplicate. `collect_dir_pages` skips the cover and `metadata.json` is not an image,
/// so neither of them is touched here.
fn clean_up_after_packing(temp_download_dir: &Path, download_dir: &Path) -> anyhow::Result<()> {
    std::fs::remove_dir_all(temp_download_dir).context(format!(
        "Failed to delete directory `{}`",
        temp_download_dir.display()
    ))?;

    let stale_image_paths = pages::collect_dir_pages(download_dir).context(format!(
        "Failed to collect the stale images in `{}`",
        download_dir.display()
    ))?;

    for stale_image_path in stale_image_paths {
        std::fs::remove_file(&stale_image_path).context(format!(
            "Failed to delete file `{}`",
            stale_image_path.display()
        ))?;
    }

    Ok(())
}

/// The format `file` is downloaded in when the comic is downloaded as `download_format`
///
/// Not every image is available as avif, those fall back to webp instead of failing the comic.
fn img_format_of(file: &GalleryFiles, download_format: DownloadFormat) -> DownloadFormat {
    match download_format {
        DownloadFormat::Avif if file.hasavif == 0 => DownloadFormat::Webp,
        download_format => download_format,
    }
}

/// Check if `err` says the image is not at the url, which is what a stale `gg.js` causes
fn is_stale_url_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<UnexpectedStatus>()
        .is_some_and(|err| matches!(err.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND))
}

/// How often an image whose transfer broke off is downloaded again before it counts as failed
const TRANSFER_RETRIES: u32 = 3;

/// Check if `err` says the transfer of the image body broke off, as opposed to the server
/// refusing it
///
/// Failures before the response headers are left out, the retry middleware already tried
/// those again.
fn is_interrupted_transfer_error(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .any(|err| err.is_body() || err.is_decode())
}

/// Path an image is written to before it is renamed to `save_path`
///
/// Only a completely written image ever carries its final name, so an image cut off by a
/// crash or by quitting the app is downloaded again instead of being packed into the archive.
/// The leftover is removed by `clean_temp_download_dir`, as it is not one of the save paths.
fn part_path_of(save_path: &Path) -> PathBuf {
    let mut part_path = save_path.as_os_str().to_os_string();
    part_path.push(".part");
    PathBuf::from(part_path)
}

/// Write `data` to `save_path` so that the file is either complete or not there at all
async fn write_img(save_path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let part_path = part_path_of(save_path);
    tokio::fs::write(&part_path, data)
        .await
        .context(format!("Failed to write `{}`", part_path.display()))?;
    tokio::fs::rename(&part_path, save_path)
        .await
        .context(format!(
            "Failed to rename `{}` to `{}`",
            part_path.display(),
            save_path.display()
        ))?;
    Ok(())
}

#[derive(Clone)]
struct DownloadImgTask {
    context: Context,
    download_manager: DownloadManager,
    download_task: DownloadTask,
    file: GalleryFiles,
    format: DownloadFormat,
    save_path: PathBuf,
}

impl DownloadImgTask {
    pub fn new(
        download_task: &DownloadTask,
        file: GalleryFiles,
        format: DownloadFormat,
        save_path: PathBuf,
    ) -> Self {
        Self {
            context: download_task.context.clone(),
            download_manager: download_task.download_manager.clone(),
            download_task: download_task.clone(),
            file,
            format,
            save_path,
        }
    }

    async fn process(self) {
        let download_img_task = self.download_img();
        tokio::pin!(download_img_task);

        let mut state_receiver = self.download_task.state_sender.subscribe();
        state_receiver.mark_changed();
        let mut permit = None;

        loop {
            let state_is_downloading = *state_receiver.borrow() == DownloadTaskState::Downloading;
            tokio::select! {
                () = &mut download_img_task, if state_is_downloading && permit.is_some() => break,
                control_flow = self.acquire_img_permit(&mut permit), if state_is_downloading && permit.is_none() => {
                    match control_flow {
                        ControlFlow::Continue(()) => continue,
                        ControlFlow::Break(()) => break,
                    }
                },
                _ = state_receiver.changed() => {
                    match self.handle_state_change(&mut permit, &mut state_receiver) {
                        ControlFlow::Continue(()) => continue,
                        ControlFlow::Break(()) => break,
                    }
                }
            }
        }
    }

    async fn download_img(&self) {
        let id = self.download_task.comic.id;
        let comic_title = &self.download_task.comic.title;
        let save_path = &self.save_path;
        let hash = &self.file.hash;

        tracing::trace!(id, comic_title, hash, "Start downloading images");

        if save_path.exists() {
            // If the image already exists, skip it
            self.download_task
                .downloaded_img_count
                .fetch_add(1, Ordering::Relaxed);

            self.download_task.emit_download_task_update_event();

            tracing::trace!(id, comic_title, hash, "Image already exists, skip download");
            return;
        }
        // download image
        let img_data = match self.fetch_img_data().await {
            Ok(img_data) => img_data,
            Err(err) => {
                let err_title = format!("Failed to download image `{hash}` of `{comic_title}`");
                let string_chain = err.to_string_chain();
                tracing::error!(err_title, message = string_chain);
                return;
            }
        };

        tracing::trace!(id, comic_title, hash, "Image downloaded to memory");
        // save image
        if let Err(err) = write_img(save_path, &img_data).await {
            let err_title = format!("Failed to save image `{}`", save_path.display());
            let string_chain = err.to_string_chain();
            tracing::error!(err_title, message = string_chain);
            return;
        }

        tracing::trace!(
            id,
            hash,
            comic_title,
            "Image successfully saved to `{}`",
            save_path.display()
        );
        // Record the number of bytes downloaded
        self.download_manager
            .byte_per_sec
            .fetch_add(img_data.len() as u64, Ordering::Relaxed);

        self.download_task
            .downloaded_img_count
            .fetch_add(1, Ordering::Relaxed);

        self.download_task.emit_download_task_update_event();
    }

    /// Download the image, starting over when the transfer breaks off midway
    ///
    /// The retry middleware of the client only covers the request up to the response headers.
    /// A body that stalls past the read timeout, common through a proxy, would otherwise fail
    /// the image at once, and with it the whole comic.
    async fn fetch_img_data(&self) -> anyhow::Result<Bytes> {
        let mut attempt = 0;
        loop {
            match self.fetch_img_data_once().await {
                Ok(img_data) => return Ok(img_data),
                Err(err) if attempt < TRANSFER_RETRIES && is_interrupted_transfer_error(&err) => {
                    attempt += 1;
                    let delay = Duration::from_secs(1 << (attempt - 1));
                    tracing::debug!(
                        hash = self.file.hash,
                        attempt,
                        "Image transfer broke off, trying again in {delay:?}: {}",
                        err.to_string_chain()
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Download the image, building its url right before the request
    ///
    /// The url depends on `gg.js`, which Hitomi rotates. A url built when the comic was
    /// queued can be stale by the time the image is downloaded, above all after a pause.
    /// If the server says the image is not there, the url is built once more from a fresh
    /// `gg.js` before giving up.
    async fn fetch_img_data_once(&self) -> anyhow::Result<Bytes> {
        let hitomi_client = self.hitomi_client();
        let url = self.img_url().await?;
        let err = match hitomi_client.get_img_data(&url).await {
            Ok(img_data) => return Ok(img_data),
            Err(err) => err,
        };

        if !is_stale_url_error(&err) {
            return Err(err).context(format!("Failed to download `{url}`"));
        }

        tracing::debug!(
            url,
            "Image url looks stale, building it again from a fresh gg.js"
        );
        invalidate_gg().await;
        let fresh_url = self.img_url().await?;
        hitomi_client
            .get_img_data(&fresh_url)
            .await
            .context(format!("Failed to download `{fresh_url}`"))
    }

    async fn img_url(&self) -> anyhow::Result<String> {
        let ext = match self.format {
            DownloadFormat::Webp => Ext::Webp,
            DownloadFormat::Avif => Ext::Avif,
        };
        image_url_from_image(self.download_task.comic.id, &self.file, ext)
            .await
            .context(format!(
                "Failed to get the url of image `{}`",
                self.file.hash
            ))
    }

    async fn acquire_img_permit<'a>(
        &'a self,
        permit: &mut Option<SemaphorePermit<'a>>,
    ) -> ControlFlow<()> {
        let hash = &self.file.hash;
        let id = self.download_task.comic.id;
        let comic_title = &self.download_task.comic.title;

        tracing::trace!(id, comic_title, hash, "Image is pending");

        *permit = match permit.take() {
            // If there is a permit, use it directly
            Some(permit) => Some(permit),
            // If there is no permit, get the permit
            None => match self
                .download_manager
                .img_sem
                .acquire()
                .await
                .map_err(anyhow::Error::from)
            {
                Ok(permit) => Some(permit),
                Err(err) => {
                    let err_title =
                        format!("Failed to get the permit to download the image `{comic_title}`");
                    let string_chain = err.to_string_chain();
                    tracing::error!(err_title, message = string_chain);
                    return ControlFlow::Break(());
                }
            },
        };
        ControlFlow::Continue(())
    }

    fn handle_state_change<'a>(
        &'a self,
        permit: &mut Option<SemaphorePermit<'a>>,
        state_receiver: &mut watch::Receiver<DownloadTaskState>,
    ) -> ControlFlow<()> {
        let hash = &self.file.hash;
        let id = self.download_task.comic.id;
        let comic_title = &self.download_task.comic.title;

        let state = *state_receiver.borrow();
        match state {
            DownloadTaskState::Paused => {
                tracing::trace!(id, comic_title, hash, "Image is paused");
                if let Some(permit) = permit.take() {
                    drop(permit);
                }
                ControlFlow::Continue(())
            }
            DownloadTaskState::Cancelled => {
                tracing::trace!(id, comic_title, hash, "Image is cancelled");
                ControlFlow::Break(())
            }
            _ => ControlFlow::Continue(()),
        }
    }

    fn hitomi_client(&self) -> HitomiClient {
        self.context.hitomi_client().clone()
    }
}

#[derive(Default, Debug, PartialEq, Clone, Serialize, Deserialize, Type)]
pub struct DirFmtParams {
    id: i32,
    title: String,
    language: String,
    language_localname: String,
    #[serde(rename = "type")]
    type_field: String,
    artists: String,
    tags: String,
}

impl Comic {
    /// Update the `comic_download_dir` fields based on the fmt
    fn update_dir_name_fields_by_fmt(&mut self, context: &Context) -> anyhow::Result<()> {
        let comic_title = &self.title;

        let fmt_params = DirFmtParams {
            id: self.id,
            // Shortened here rather than only when the whole directory name is clamped,
            // so that what follows the title in the format string survives. With the
            // default format that is the id and the language.
            title: crate::utils::truncate_title(&self.title),
            language: self.language.clone(),
            language_localname: self.language_localname.clone(),
            type_field: self.type_field.clone(),
            artists: self.artists.join(", "),
            tags: {
                let mut joined_tags = self
                    .tags
                    .iter()
                    .map(|tag| tag.tag.clone())
                    .collect::<Vec<String>>()
                    .join(", ");
                if joined_tags.chars().count() > 100 {
                    joined_tags =
                        format!("{}...", joined_tags.chars().take(97).collect::<String>());
                }
                joined_tags
            },
        };
        let comic_download_dir = Comic::get_comic_download_dir_by_fmt(context, &fmt_params)
            .context(format!(
                "Failed to get download directory by fmt of `{comic_title}`"
            ))?;
        self.comic_download_dir = Some(comic_download_dir);

        Ok(())
    }

    fn get_comic_download_dir_by_fmt(
        context: &Context,
        fmt_params: &DirFmtParams,
    ) -> anyhow::Result<PathBuf> {
        use strfmt::strfmt;

        let json_value = serde_json::to_value(fmt_params)
            .context("Failed to convert DirFmtParams to serde_json::Value")?;

        let json_map = json_value
            .as_object()
            .context("DirFmtParams is not a JSON object")?;

        let vars: HashMap<String, String> = json_map
            .into_iter()
            .map(|(k, v)| {
                let key = k.clone();
                let value = match v {
                    serde_json::Value::String(s) => s.clone(),
                    _ => v.to_string(),
                };
                (key, value)
            })
            .collect();

        let (download_dir, dir_fmt) = {
            let config = context.config().read();
            (config.download_dir.clone(), config.dir_fmt.clone())
        };

        let dir_fmt_parts: Vec<&str> = dir_fmt.split('/').collect();

        let mut dir_names = Vec::new();
        for fmt in dir_fmt_parts {
            let dir_name = strfmt(fmt, &vars).context("Failed to format directory name")?;
            let dir_name = filename_filter(&dir_name);
            if !dir_name.is_empty() {
                dir_names.push(dir_name);
            }
        }

        // Join the formatted directory names to create the comic download directory
        let mut comic_download_dir = download_dir;
        for dir_name in dir_names {
            comic_download_dir = comic_download_dir.join(dir_name);
        }

        Ok(comic_download_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growing_adds_permits_at_once() {
        let sem = Arc::new(Semaphore::new(2));
        let limit = AtomicU32::new(2);

        assert!(resize_semaphore(&sem, &limit, 5).is_none());

        assert_eq!(sem.available_permits(), 5);
        assert_eq!(limit.load(Ordering::Relaxed), 5);
    }

    #[tokio::test]
    async fn shrinking_waits_for_running_downloads() {
        let sem = Arc::new(Semaphore::new(5));
        let limit = AtomicU32::new(5);
        // three downloads are running
        let running = sem.clone().acquire_many_owned(3).await.unwrap();

        let shrink = resize_semaphore(&sem, &limit, 2).expect("shrinking needs to wait");
        let shrink = tokio::spawn(shrink);
        tokio::task::yield_now().await;
        // the two idle permits went to the shrink at once, no new download may start
        assert_eq!(sem.available_permits(), 0);
        assert!(!shrink.is_finished());

        drop(running);
        shrink.await.unwrap();
        assert_eq!(sem.available_permits(), 2);
        assert_eq!(limit.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn growing_while_a_shrink_waits_ends_at_the_last_limit() {
        let sem = Arc::new(Semaphore::new(5));
        let limit = AtomicU32::new(5);
        let running = sem.clone().acquire_many_owned(5).await.unwrap();

        let shrink = tokio::spawn(resize_semaphore(&sem, &limit, 2).unwrap());
        assert!(resize_semaphore(&sem, &limit, 4).is_none());

        drop(running);
        shrink.await.unwrap();
        assert_eq!(sem.available_permits(), 4);
    }

    fn gallery_file(hasavif: i32) -> GalleryFiles {
        GalleryFiles {
            width: 1200,
            hash: "0".repeat(64),
            haswebp: 1,
            hasavif,
            hasjxl: 0,
            name: "01.jpg".to_string(),
            height: 1700,
        }
    }

    #[test]
    fn avif_falls_back_to_webp_when_unavailable() {
        assert_eq!(
            img_format_of(&gallery_file(0), DownloadFormat::Avif),
            DownloadFormat::Webp
        );
        assert_eq!(
            img_format_of(&gallery_file(1), DownloadFormat::Avif),
            DownloadFormat::Avif
        );
        assert_eq!(
            img_format_of(&gallery_file(1), DownloadFormat::Webp),
            DownloadFormat::Webp
        );
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

    #[tokio::test]
    async fn stalled_body_counts_as_interrupted_transfer() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // sends the headers and part of the body, then stops sending without closing
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\npartial")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let client = reqwest::Client::builder()
            .no_proxy()
            .read_timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let resp = client.get(format!("http://{addr}/")).send().await.unwrap();
        let err =
            anyhow::Error::from(resp.bytes().await.unwrap_err()).context("Failed to download");

        assert!(is_interrupted_transfer_error(&err));
        server.abort();
    }

    #[tokio::test]
    async fn failures_before_the_body_are_no_interrupted_transfer() {
        // nothing listens on port 1, the connection is refused
        let err = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .unwrap_err();

        assert!(!is_interrupted_transfer_error(&err.into()));
        assert!(!is_interrupted_transfer_error(&status_error(
            StatusCode::NOT_FOUND
        )));
        assert!(!is_interrupted_transfer_error(&anyhow!("connection reset")));
    }

    #[test]
    fn mixed_formats_keep_the_page_order() {
        let dir = std::env::temp_dir().join(format!("hitomi-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // an avif download where page 2 was only available as webp
        for name in ["0003.avif", "0002.webp", "0001.avif"] {
            std::fs::write(dir.join(name), b"page").unwrap();
        }

        assert_eq!(
            pages::collect_dir_pages(&dir).unwrap(),
            vec![
                dir.join("0001.avif"),
                dir.join("0002.webp"),
                dir.join("0003.avif")
            ]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn part_file_is_not_a_page() {
        use crate::extensions::PathIsImg;

        let part_path = part_path_of(Path::new("/tmp/comic/0001.webp"));
        assert_eq!(part_path, PathBuf::from("/tmp/comic/0001.webp.part"));
        assert!(!part_path.is_img());
    }

    #[tokio::test]
    async fn written_image_leaves_no_part_file() {
        let dir = std::env::temp_dir().join(format!("hitomi-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let save_path = dir.join("0001.webp");

        write_img(&save_path, b"image data").await.unwrap();

        assert_eq!(std::fs::read(&save_path).unwrap(), b"image data");
        assert!(!part_path_of(&save_path).exists());
        assert_eq!(pages::collect_dir_pages(&dir).unwrap(), vec![save_path]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn leftover_part_file_is_not_packed() {
        let dir = std::env::temp_dir().join(format!("hitomi-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("0001.webp"), b"complete").unwrap();
        // what an interrupted write leaves behind
        std::fs::write(dir.join("0002.webp.part"), b"cut o").unwrap();

        assert_eq!(
            pages::collect_dir_pages(&dir).unwrap(),
            vec![dir.join("0001.webp")]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // The tests that need a context share one, there is only one per process

    struct NoEvents;

    impl crate::events::EventSink for NoEvents {
        fn emit(&self, _event: crate::events::Event) {}
    }

    /// The context of the tests and the root of what they write
    fn test_context() -> &'static (Context, PathBuf) {
        static CONTEXT: std::sync::OnceLock<(Context, PathBuf)> = std::sync::OnceLock::new();
        static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        CONTEXT.get_or_init(|| {
            let runtime = RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap());
            let root =
                std::env::temp_dir().join(format!("hitomi-core-test-{}", uuid::Uuid::new_v4()));
            let context = Context::init_with(
                root.join("config"),
                runtime.handle().clone(),
                NoEvents,
                crate::context::Options {
                    temp_dir: Some(root.join("config").join("downloading")),
                    queue_file: Some(root.join("config").join("queue.json")),
                },
            )
            .unwrap();
            (context, root)
        })
    }

    /// The directory the default folder format gives for a comic with `artists`
    fn default_dir_of(artists: &[&str]) -> PathBuf {
        let (context, _) = test_context();
        let mut comic = Comic {
            id: 7,
            title: "A Title".to_string(),
            language: "english".to_string(),
            artists: artists.iter().map(ToString::to_string).collect(),
            ..Comic::default()
        };
        comic.update_dir_name_fields_by_fmt(context).unwrap();
        let library = context.config().read().download_dir.clone();
        comic
            .comic_download_dir
            .unwrap()
            .strip_prefix(library)
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn the_default_folder_format_puts_a_comic_in_the_folder_of_its_artists() {
        assert_eq!(
            default_dir_of(&["one", "two"]),
            PathBuf::from("one, two").join("A Title (7) - english")
        );
        assert_eq!(
            default_dir_of(&["one"]),
            PathBuf::from("one").join("A Title (7) - english")
        );
    }

    #[test]
    fn a_comic_without_artists_goes_into_the_library_itself() {
        // the empty folder name is left out, there is no `[]` or empty level
        assert_eq!(default_dir_of(&[]), PathBuf::from("A Title (7) - english"));
    }

    fn stored(id: i32, state: DownloadTaskState, format: DownloadFormat) -> StoredTask {
        StoredTask {
            state,
            download_format: format,
            comic: Comic {
                id,
                title: format!("Queued {id}"),
                files: vec![gallery_file(0); 3],
                ..Comic::default()
            },
        }
    }

    fn state_of(manager: &DownloadManager, id: i32) -> Option<DownloadTaskState> {
        manager
            .download_tasks
            .read()
            .get(&id)
            .map(|task| *task.state_sender.borrow())
    }

    #[test]
    fn a_stored_download_comes_back_as_it_was_but_a_running_one_waits() {
        use DownloadTaskState::{Cancelled, Completed, Downloading, Failed, Paused, Pending};
        assert_eq!(restored_state(Pending), Some(Pending));
        assert_eq!(restored_state(Downloading), Some(Pending));
        assert_eq!(restored_state(Paused), Some(Paused));
        assert_eq!(restored_state(Failed), Some(Failed));
        assert_eq!(restored_state(Completed), None);
        assert_eq!(restored_state(Cancelled), None);
    }

    #[test]
    fn paused_and_failed_downloads_are_restored_listed_and_queued_again() {
        let (context, _) = test_context();
        let manager = context.download_manager();
        manager.restore(vec![
            stored(710_003, DownloadTaskState::Paused, DownloadFormat::Avif),
            stored(710_001, DownloadTaskState::Failed, DownloadFormat::Webp),
            // an end state is not restored, whatever put it in the file
            stored(710_002, DownloadTaskState::Completed, DownloadFormat::Webp),
        ]);

        assert_eq!(state_of(manager, 710_003), Some(DownloadTaskState::Paused));
        assert_eq!(state_of(manager, 710_001), Some(DownloadTaskState::Failed));
        assert_eq!(state_of(manager, 710_002), None);

        // they are listed for a frontend that connects now, with the size of their comic
        let listed: Vec<_> = manager
            .task_snapshots()
            .into_iter()
            .filter_map(|event| match event {
                DownloadTaskEvent::Create {
                    comic,
                    state,
                    total_img_count,
                    ..
                } if (710_000..711_000).contains(&comic.id) => {
                    Some((comic.id, state, total_img_count))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            listed,
            [
                (710_003, DownloadTaskState::Paused, 3),
                (710_001, DownloadTaskState::Failed, 3)
            ],
            "in the order they were stored"
        );

        // the format they were queued with is kept, not the one of the settings now
        let formats: Vec<_> = manager
            .queue_snapshot()
            .into_iter()
            .filter(|task| (710_000..711_000).contains(&task.comic.id))
            .map(|task| (task.comic.id, task.state, task.download_format))
            .collect();
        assert_eq!(
            formats,
            [
                (710_003, DownloadTaskState::Paused, DownloadFormat::Avif),
                (710_001, DownloadTaskState::Failed, DownloadFormat::Webp)
            ]
        );
        // the directory is worked out again at the next start, it is not stored
        assert!(manager
            .queue_snapshot()
            .iter()
            .all(|task| task.comic.comic_download_dir.is_none()));
    }

    #[test]
    fn a_cancelled_download_leaves_the_queue() {
        let (context, _) = test_context();
        let manager = context.download_manager();
        manager.restore(vec![stored(
            720_001,
            DownloadTaskState::Paused,
            DownloadFormat::Webp,
        )]);
        let in_queue = |id| {
            manager
                .queue_snapshot()
                .iter()
                .any(|task| task.comic.id == id)
        };
        assert!(in_queue(720_001));
        manager.cancel_download_task(720_001).unwrap();
        assert!(!in_queue(720_001));
    }

    #[test]
    fn the_queue_file_follows_the_queue() {
        let (context, root) = test_context();
        let manager = context.download_manager();
        let file = root.join("config").join("queue.json");
        let ids_in_file = || -> Vec<i32> {
            queue::load(&file)
                .into_iter()
                .map(|task| task.comic.id)
                .filter(|id| (730_000..731_000).contains(id))
                .collect()
        };
        let wait_for = |expected: Vec<i32>| {
            // written a moment after the change, many changes are one write
            for _ in 0..60 {
                if ids_in_file() == expected {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            false
        };

        manager.restore(vec![
            stored(730_001, DownloadTaskState::Paused, DownloadFormat::Webp),
            stored(730_002, DownloadTaskState::Failed, DownloadFormat::Webp),
        ]);
        assert!(wait_for(vec![730_001, 730_002]), "{:?}", ids_in_file());

        manager.cancel_download_task(730_001).unwrap();
        assert!(wait_for(vec![730_002]), "{:?}", ids_in_file());
    }

    /// A task for the comic `id` whose archive goes to `library/<name>`
    fn test_task(id: i32, library: &Path, name: &str) -> DownloadTask {
        let (context, _) = test_context();
        let comic = Comic {
            id,
            title: name.to_string(),
            comic_download_dir: Some(library.join(name)),
            ..Comic::default()
        };
        DownloadTask {
            context: context.clone(),
            download_manager: context.download_manager().clone(),
            comic: Arc::new(comic),
            state_sender: watch::channel(DownloadTaskState::Pending).0,
            downloaded_img_count: Arc::new(AtomicU32::new(0)),
            total_img_count: Arc::new(AtomicU32::new(0)),
            download_format: DownloadFormat::Webp,
        }
    }

    /// Bytes that do not compress away, so writing an archive takes a moment
    fn noise(len: usize, seed: u32) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect()
    }

    const IMAGE_EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "webp", "gif", "avif"];
    /// What a scanner takes for an archive, matched against the last extension only
    const ARCHIVE_EXTENSIONS: [&str; 8] = ["cbz", "zip", "rar", "cbr", "7z", "cb7", "cbt", "gz"];

    fn extension_of(name: &str) -> String {
        Path::new(name)
            .extension()
            .map(|extension| extension.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    }

    /// Every file under `dir`, as a path relative to it
    fn files_under(dir: &Path) -> Vec<String> {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| {
                entry
                    .path()
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn the_pages_of_a_download_never_touch_the_library() {
        let (_, root) = test_context();
        let library = root.join("library-pages");
        std::fs::create_dir_all(&library).unwrap();
        let task = test_task(41, &library, "Pages Comic - 41");

        let temp_dir = task.create_temp_download_dir().unwrap();
        assert_eq!(temp_dir, root.join("config").join("downloading").join("41"));
        for (index, name) in ["0001.webp", "0002.webp", "0002.webp.part"]
            .iter()
            .enumerate()
        {
            std::fs::write(temp_dir.join(name), noise(1024, index as u32)).unwrap();
        }

        // pages, complete and partly written ones, are all outside, the library has nothing
        assert_eq!(std::fs::read_dir(&library).unwrap().count(), 0);
        std::fs::remove_dir_all(root.join("config").join("downloading").join("41")).unwrap();
    }

    #[test]
    fn the_library_only_ever_holds_the_archive_and_never_a_cut_off_one() {
        let (_, root) = test_context();
        let library = root.join("library-pack");
        std::fs::create_dir_all(&library).unwrap();
        let name = "Pack Comic - 42";
        let task = test_task(42, &library, name);
        let temp_dir = task.create_temp_download_dir().unwrap();
        // 60 pages of 200 KB, so that writing the archive takes long enough to be watched
        for page in 1..=60u32 {
            std::fs::write(
                temp_dir.join(format!("{page:04}.webp")),
                noise(200 * 1024, page),
            )
            .unwrap();
        }

        // what a scan of the library could see: every file of it, polled while the archive is written
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher = {
            let (library, stop) = (library.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut seen = std::collections::BTreeSet::new();
                let mut archives_checked = 0;
                let mut broken = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    for file in files_under(&library) {
                        if extension_of(&file) == "cbz" {
                            // a scan opens whatever carries an archive extension, it must be whole
                            let opened = std::fs::File::open(library.join(&file))
                                .map_err(|err| err.to_string())
                                .and_then(|f| {
                                    zip::ZipArchive::new(f).map_err(|err| err.to_string())
                                });
                            match opened {
                                Ok(archive) if archive.len() == 61 => archives_checked += 1,
                                Ok(archive) => {
                                    broken.push(format!("{file}: {} entries", archive.len()))
                                }
                                Err(err) => broken.push(format!("{file}: {err}")),
                            }
                        }
                        seen.insert(file);
                    }
                }
                (seen, archives_checked, broken)
            })
        };

        task.pack_temp_download_dir(&temp_dir).unwrap();
        stop.store(true, Ordering::Relaxed);
        let (seen, archives_checked, broken) = watcher.join().unwrap();

        // the watcher has to have seen the archive in the making and whole ones, else it proves nothing
        assert!(
            seen.iter().any(|file| file.ends_with(".cbz.tmp")),
            "the watcher never saw the archive being written: {seen:?}"
        );
        assert!(
            archives_checked > 0,
            "the watcher never opened the finished archive: {seen:?}"
        );
        assert!(
            broken.is_empty(),
            "a scan could have opened a cut off archive: {broken:?}"
        );
        for file in &seen {
            let file_name = Path::new(file).file_name().unwrap().to_string_lossy();
            assert!(
                file_name == format!("{name}.cbz.tmp") || file_name == format!("{name}.cbz"),
                "unexpected file in the library: {file}"
            );
            // by the last extension, as a scanner matches it, the archive in the making is `.tmp`
            if file_name.ends_with(".tmp") {
                assert!(
                    !ARCHIVE_EXTENSIONS.contains(&extension_of(file).as_str()),
                    "{file}"
                );
            }
            assert!(
                !IMAGE_EXTENSIONS.contains(&extension_of(file).as_str()),
                "{file}"
            );
        }
        // finished: the archive, and nothing else of the download is left
        assert_eq!(files_under(&library), vec![format!("{name}/{name}.cbz")]);
        assert!(
            !temp_dir.exists(),
            "the pages are deleted once the archive is on the disk"
        );
    }

    #[test]
    fn a_failed_packing_keeps_the_pages_for_the_next_run() {
        let (_, root) = test_context();
        // a file where the library should be makes creating the comic folder fail
        let library = root.join("library-is-a-file");
        std::fs::write(&library, b"not a directory").unwrap();
        let task = test_task(43, &library, "Failing Comic - 43");
        let temp_dir = task.create_temp_download_dir().unwrap();
        std::fs::write(temp_dir.join("0001.webp"), noise(1024, 1)).unwrap();

        assert!(task.pack_temp_download_dir(&temp_dir).is_err());
        assert_eq!(files_under(&temp_dir), vec!["0001.webp".to_string()]);
        // and a retry, with the library back, packs them without downloading anything
        std::fs::remove_file(&library).unwrap();
        std::fs::create_dir_all(&library).unwrap();
        task.pack_temp_download_dir(&temp_dir).unwrap();
        assert_eq!(
            files_under(&library),
            vec!["Failing Comic - 43/Failing Comic - 43.cbz".to_string()]
        );
    }

    #[test]
    fn the_archive_is_complete_when_the_function_returns() {
        let (_, root) = test_context();
        let library = root.join("library-whole");
        std::fs::create_dir_all(&library).unwrap();
        let task = test_task(44, &library, "Whole Comic - 44");
        let temp_dir = task.create_temp_download_dir().unwrap();
        std::fs::write(temp_dir.join("0001.webp"), noise(4096, 7)).unwrap();

        task.pack_temp_download_dir(&temp_dir).unwrap();
        let cbz = library
            .join("Whole Comic - 44")
            .join("Whole Comic - 44.cbz");
        let mut archive = zip::ZipArchive::new(std::fs::File::open(cbz).unwrap()).unwrap();
        assert_eq!(archive.len(), 2);
        assert!(archive.by_name("ComicInfo.xml").is_ok());
        assert_eq!(archive.by_name("0001.webp").unwrap().size(), 4096);
    }
}
