use std::{
    collections::HashMap,
    ops::ControlFlow,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{anyhow, Context};
use bytes::Bytes;
use parking_lot::RwLock;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use tokio::{
    sync::{watch, Semaphore, SemaphorePermit},
    task::JoinSet,
};

use crate::{
    config::Config,
    events::{DownloadSpeedEvent, DownloadTaskEvent},
    extensions::AnyhowErrorToStringChain,
    hitomi::{image_url_from_image, invalidate_gg, Ext, GalleryFiles},
    hitomi_client::{HitomiClient, UnexpectedStatus},
    pages,
    types::{Comic, ComicInfo, DownloadFormat},
    utils::filename_filter,
};

/// Used to manage download tasks
///
/// Cloning `DownloadManager` has minimal overhead, and the performance overhead is almost negligible.
/// You can safely clone and use it in multiple threads.
///
/// Specifically:
/// - `app` is of type `AppHandle`, according to the `Tauri` documentation, the overhead of cloning it is minimal.
/// - Other fields are wrapped in `Arc`, and the cloning operation of these fields only increases the reference count.
#[derive(Clone)]
pub struct DownloadManager {
    app: AppHandle,
    comic_sem: Arc<Semaphore>,
    img_sem: Arc<Semaphore>,
    byte_per_sec: Arc<AtomicU64>,
    download_tasks: Arc<RwLock<HashMap<i32, DownloadTask>>>,
}

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
    pub fn new(app: &AppHandle) -> Self {
        let manager = DownloadManager {
            app: app.clone(),
            comic_sem: Arc::new(Semaphore::new(2)),
            img_sem: Arc::new(Semaphore::new(4)),
            byte_per_sec: Arc::new(AtomicU64::new(0)),
            download_tasks: Arc::new(RwLock::new(HashMap::new())),
        };

        tauri::async_runtime::spawn(manager.clone().emit_download_speed_loop());

        manager
    }

    pub fn create_download_task(&self, comic: Comic) -> anyhow::Result<()> {
        use DownloadTaskState::{Downloading, Paused, Pending};
        let id = comic.id;
        let mut tasks = self.download_tasks.write();
        if let Some(task) = tasks.get(&id) {
            // If the task already exists and the state is `Pending`, `Downloading`, or `Paused`, a new task will not be created
            let state = *task.state_sender.borrow();
            if matches!(state, Pending | Downloading | Paused) {
                return Ok(());
            }
        }
        let task = DownloadTask::new(self.app.clone(), comic)
            .context(format!("Failed to create download task with id `{id}`",))?;
        tauri::async_runtime::spawn(task.clone().process());
        tasks.insert(id, task);
        Ok(())
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

    #[allow(clippy::cast_precision_loss)]
    async fn emit_download_speed_loop(self) {
        let mut interval = tokio::time::interval(Duration::from_secs(1));

        loop {
            interval.tick().await;
            let byte_per_sec = self.byte_per_sec.swap(0, Ordering::Relaxed);
            let mega_byte_per_sec = byte_per_sec as f64 / 1024.0 / 1024.0;
            let speed = format!("{mega_byte_per_sec:.2} MB/s");

            let _ = DownloadSpeedEvent { speed }.emit(&self.app);
        }
    }
}

#[derive(Clone)]
struct DownloadTask {
    app: AppHandle,
    download_manager: DownloadManager,
    comic: Arc<Comic>,
    state_sender: watch::Sender<DownloadTaskState>,
    downloaded_img_count: Arc<AtomicU32>,
    total_img_count: Arc<AtomicU32>,
    download_format: DownloadFormat,
}

impl DownloadTask {
    pub fn new(app: AppHandle, mut comic: Comic) -> anyhow::Result<Self> {
        comic.update_dir_name_fields_by_fmt(&app).context(format!(
            "Failed to update directory name fields by fmt of `{}`",
            comic.title
        ))?;

        let download_manager = app.state::<DownloadManager>().inner().clone();
        let (state_sender, _) = watch::channel(DownloadTaskState::Pending);
        let download_format = app.state::<RwLock<Config>>().read().download_format;

        let task = Self {
            app,
            download_manager,
            comic: Arc::new(comic),
            state_sender,
            downloaded_img_count: Arc::new(AtomicU32::new(0)),
            total_img_count: Arc::new(AtomicU32::new(0)),
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
    }

    fn create_temp_download_dir(&self) -> Option<PathBuf> {
        let id = self.comic.id;
        let comic_title = &self.comic.title;

        let temp_download_dir = match self.comic.get_temp_download_dir() {
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
        let comic_title = &self.comic.title;
        if let Err(err) = self.state_sender.send(state).map_err(anyhow::Error::from) {
            let err_title = format!("Failed to send state `{state:?}` to `{comic_title}`");
            let string_chain = err.to_string_chain();
            tracing::error!(err_title, message = string_chain);
        }
    }

    fn emit_download_task_update_event(&self) {
        let _ = DownloadTaskEvent::Update {
            comic_id: self.comic.id,
            state: *self.state_sender.borrow(),
            downloaded_img_count: self.downloaded_img_count.load(Ordering::Relaxed),
            total_img_count: self.total_img_count.load(Ordering::Relaxed),
        }
        .emit(&self.app);
    }

    fn emit_download_task_create_event(&self) {
        let _ = DownloadTaskEvent::Create {
            state: *self.state_sender.borrow(),
            comic: Box::new(self.comic.as_ref().clone()),
            downloaded_img_count: self.downloaded_img_count.load(Ordering::Relaxed),
            total_img_count: self.total_img_count.load(Ordering::Relaxed),
        }
        .emit(&self.app);
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
        let hitomi_client = self.app.state::<HitomiClient>().inner().clone();

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
    app: AppHandle,
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
            app: download_task.app.clone(),
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

    /// Download the image, building its url right before the request
    ///
    /// The url depends on `gg.js`, which Hitomi rotates. A url built when the comic was
    /// queued can be stale by the time the image is downloaded, above all after a pause.
    /// If the server says the image is not there, the url is built once more from a fresh
    /// `gg.js` before giving up.
    async fn fetch_img_data(&self) -> anyhow::Result<Bytes> {
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
        self.app.state::<HitomiClient>().inner().clone()
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
    fn update_dir_name_fields_by_fmt(&mut self, app: &AppHandle) -> anyhow::Result<()> {
        let comic_title = &self.title;

        let fmt_params = DirFmtParams {
            id: self.id,
            // Shortened here rather than only when the whole directory name is clamped,
            // so that what follows the title in the format string survives. With the
            // default format that is the id, which is what tells two comics apart.
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
        let comic_download_dir = Comic::get_comic_download_dir_by_fmt(app, &fmt_params).context(
            format!("Failed to get download directory by fmt of `{comic_title}`"),
        )?;
        self.comic_download_dir = Some(comic_download_dir);

        Ok(())
    }

    fn get_comic_download_dir_by_fmt(
        app: &AppHandle,
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
            let config = app.state::<RwLock<Config>>();
            let config = config.read();
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
}
