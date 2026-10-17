//! What the frontend can ask the core for, one function per command
//!
//! The desktop app exposes each of them as a Tauri command, a server as an endpoint.
//! Arguments and results are the same in both, which keeps the frontend unaware of
//! where it runs.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _};
use indexmap::IndexMap;
use std::fs;
use walkdir::WalkDir;

use crate::{
    config::Config,
    context::Context,
    errors::{CommandError, CommandResult},
    export,
    extensions::AnyhowErrorToStringChain,
    hitomi::Suggestion,
    hitomi::{image_url_from_image, Ext, GalleryFiles},
    logger,
    pages::PageSource,
    types::{Comic, SearchResult, SearchSort},
};

pub fn get_config(context: &Context) -> Config {
    let config = context.config().read().clone();
    tracing::debug!("get config success");
    config
}

pub fn save_config(context: &Context, mut config: Config) -> CommandResult<()> {
    // An invalid proxy keeps the one in effect, the other settings are still saved
    let config_state = context.config();
    let (proxy, proxy_err) = config.proxy.valid_or(&config_state.read().proxy);
    config.proxy = proxy;
    config
        .validate_concurrency()
        .map_err(|err| CommandError::from("invalid concurrency config", err))?;

    let proxy_changed = config_state.read().proxy != config.proxy;
    let concurrency = (config.comic_concurrency, config.img_concurrency);
    let concurrency_changed = {
        let config_state = config_state.read();
        (config_state.comic_concurrency, config_state.img_concurrency) != concurrency
    };

    let enable_file_logger = config.enable_file_logger;
    let enable_file_logger_changed = config_state
        .read()
        .enable_file_logger
        .ne(&enable_file_logger);

    {
        // Wrapped in braces to automatically release the write lock
        let mut config_state = config_state.write();
        *config_state = config;
        config_state
            .save(context.data_dir())
            .map_err(|err| CommandError::from("save config failed", err))?;
        tracing::debug!("save config success");
    }

    if proxy_changed {
        context.hitomi_client().reload_client();
    }

    if concurrency_changed {
        let (comic_concurrency, img_concurrency) = concurrency;
        context
            .download_manager()
            .set_concurrency(comic_concurrency, img_concurrency);
    }

    if enable_file_logger_changed {
        if enable_file_logger {
            logger::reload_file_logger()
                .map_err(|err| CommandError::from("reload file logger failed", err))?;
        } else {
            logger::disable_file_logger()
                .map_err(|err| CommandError::from("disable file logger failed", err))?;
        }
    }

    if let Some(err) = proxy_err {
        return Err(CommandError::from(
            "invalid proxy config, the other settings were saved and the previous proxy stays in use",
            err,
        ));
    }

    Ok(())
}

pub async fn search(
    context: &Context,
    query: &str,
    page_num: usize,
    sort: SearchSort,
) -> CommandResult<SearchResult> {
    let search_result = context
        .hitomi_client()
        .search(query, page_num, sort)
        .await
        .map_err(|err| CommandError::from("search failed", err))?;
    tracing::debug!("search success");
    Ok(search_result)
}

pub async fn get_page(
    context: &Context,
    ids: Vec<i32>,
    page_num: usize,
) -> CommandResult<SearchResult> {
    let search_result = context
        .hitomi_client()
        .get_page(ids, page_num)
        .await
        .map_err(|err| CommandError::from("get page failed", err))?;
    tracing::debug!("get page success");
    Ok(search_result)
}

pub async fn get_comic(context: &Context, id: i32) -> CommandResult<Comic> {
    let comic = context
        .hitomi_client()
        .get_comic(id)
        .await
        .map_err(|err| CommandError::from("get comic failed", err))?;
    tracing::debug!("get comic success");
    Ok(comic)
}

pub fn create_download_task(context: &Context, comic: Comic) -> CommandResult<()> {
    let id = comic.id;
    context
        .download_manager()
        .create_download_task(comic)
        .map_err(|err| {
            let err_msg = format!("Failed to create download task with ID `{id}`");
            CommandError::from(&err_msg, err)
        })?;
    tracing::debug!("Created download task with ID `{id}` successfully");
    Ok(())
}

pub fn export_cbz(context: &Context, comic: Comic) -> CommandResult<()> {
    let title = &comic.title;
    export::cbz(context, &comic).map_err(|err| {
        CommandError::from(&format!("Failed to export cbz for comic `{title}`"), err)
    })?;
    tracing::debug!("Exported cbz for comic `{title}` successfully");
    Ok(())
}

pub async fn download_comics_by_ids(context: &Context, ids: Vec<i32>) -> CommandResult<()> {
    let total = ids.len();
    tracing::info!(total, "Start batch downloading comics by IDs");

    for (i, id) in ids.iter().enumerate() {
        let comic = match context.hitomi_client().get_comic(*id).await {
            Ok(comic) => comic,
            Err(err) => {
                let err_title = format!(
                    "Failed to get comic info for ID `{id}`, skipping ({}/{})",
                    i + 1,
                    total
                );
                let string_chain = err.to_string_chain();
                tracing::error!(err_title, message = string_chain);
                continue;
            }
        };
        if let Err(err) = context.download_manager().create_download_task(comic) {
            let err_title = format!(
                "Failed to create download task for ID `{id}`, skipping ({}/{})",
                i + 1,
                total
            );
            let string_chain = err.to_string_chain();
            tracing::error!(err_title, message = string_chain);
        }
    }

    tracing::info!(total, "Batch download by IDs completed");
    Ok(())
}

pub fn pause_download_task(context: &Context, id: i32) -> CommandResult<()> {
    context
        .download_manager()
        .pause_download_task(id)
        .map_err(|err| {
            let err_msg = format!("Failed to pause download task with ID `{id}`");
            CommandError::from(&err_msg, err)
        })?;
    tracing::debug!("Paused download task with ID `{id}` successfully");
    Ok(())
}

pub fn resume_download_task(context: &Context, id: i32) -> CommandResult<()> {
    context
        .download_manager()
        .resume_download_task(id)
        .map_err(|err| {
            let err_msg = format!("Failed to resume download task with ID `{id}`");
            CommandError::from(&err_msg, err)
        })?;
    tracing::debug!("Resumed download task with ID `{id}` successfully");
    Ok(())
}

pub fn cancel_download_task(context: &Context, id: i32) -> CommandResult<()> {
    context
        .download_manager()
        .cancel_download_task(id)
        .map_err(|err| {
            let err_msg = format!("Failed to cancel download task with ID `{id}`");
            CommandError::from(&err_msg, err)
        })?;
    tracing::debug!("Canceled download task with ID `{id}` successfully");
    Ok(())
}

pub fn get_downloaded_comics(context: &Context) -> Vec<Comic> {
    let download_dir = context.config().read().download_dir.clone();
    // Traverse the download directory to get the path and modification time of all metadata files
    let mut metadata_path_with_modify_time = Vec::new();
    for entry in WalkDir::new(&download_dir)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        if entry.file_name() != "metadata.json" {
            continue;
        }
        // now the entry is the metadata.json file
        let metadata = match path
            .metadata()
            .map_err(anyhow::Error::from)
            .context(format!(
                "Failed to get file metadata of `{}`",
                path.display()
            )) {
            Ok(metadata) => metadata,
            Err(err) => {
                let err_title = "An error occurred while getting downloaded comics, skipped";
                let string_chain = err.to_string_chain();
                tracing::error!(err_title, message = string_chain);
                continue;
            }
        };

        let modify_time = match metadata
            .modified()
            .map_err(anyhow::Error::from)
            .context(format!(
                "Failed to get file modification time of `{}`",
                path.display()
            )) {
            Ok(modify_time) => modify_time,
            Err(err) => {
                let err_title = "An error occurred while getting downloaded comics, skipped";
                let string_chain = err.to_string_chain();
                tracing::error!(err_title, message = string_chain);
                continue;
            }
        };

        metadata_path_with_modify_time.push((path.to_path_buf(), modify_time));
    }
    // Sort by file modification time, with the newest at the front
    metadata_path_with_modify_time.sort_by(|(_, a), (_, b)| b.cmp(a));
    // Create Comic from metadata file
    let mut downloaded_comics = Vec::new();
    for (metadata_path, _) in metadata_path_with_modify_time {
        match Comic::from_metadata(&metadata_path).context(format!(
            "Failed to create Comic from metadata `{}`",
            metadata_path.display()
        )) {
            Ok(comic) => downloaded_comics.push(comic),
            Err(err) => {
                let err_title = "An error occurred while getting downloaded comics, skipped";
                let string_chain = err.to_string_chain();
                tracing::error!(err_title, message = string_chain);
            }
        }
    }

    tracing::debug!("get downloaded comics success");

    // Group comics by their ID to facilitate deduplication
    let mut comics_by_id: IndexMap<i32, Vec<Comic>> = IndexMap::new();
    for comic in downloaded_comics {
        comics_by_id.entry(comic.id).or_default().push(comic);
    }

    let mut unique_comics = Vec::new();
    for (_comic_id, mut comics) in comics_by_id {
        // The download directories for all comics with the same ID, which may have multiple versions, so we need to deduplicate
        let comic_download_dirs: Vec<&PathBuf> = comics
            .iter()
            .filter_map(|comic| comic.comic_download_dir.as_ref())
            .collect();

        if comic_download_dirs.is_empty() {
            // This situation should not actually happen, because the comic metadata file should always have a download directory
            continue;
        }

        // Choose the first one as the retained comic
        let chosen_download_dir = comic_download_dirs[0];

        if comics.len() > 1 {
            let dir_paths_string = comic_download_dirs
                .iter()
                .map(|path| format!("`{}`", path.display()))
                .collect::<Vec<String>>()
                .join(", ");
            // If there are duplicate comics, report an error
            let comic_title = &comics[0].title;
            let err_title = "An error occurred while getting downloaded comics";
            let string_chain = anyhow!("All version paths: [{dir_paths_string}]")
                .context(format!(
                    "To proceed, temporarily selected only the version '{}' from the multiple versions found",
                    chosen_download_dir.display()
                ))
                .context(format!(
                    "Comic `{comic_title}` has multiple versions in the download directory. Please handle this manually, keeping only one",
                ))
                .to_string_chain();
            tracing::error!(err_title, message = string_chain);
        }
        // Choose the first one as the retained comic
        let chosen_comic = comics.remove(0);
        unique_comics.push(chosen_comic);
    }

    unique_comics
}

pub fn export_pdf(context: &Context, comic: Comic) -> CommandResult<()> {
    let title = &comic.title;
    export::pdf(context, &comic).map_err(|err| {
        CommandError::from(&format!("Failed to export pdf for comic `{title}`"), err)
    })?;
    tracing::debug!("Exported pdf for comic `{title}` successfully");
    Ok(())
}

pub async fn get_search_suggestions(
    context: &Context,
    query: String,
) -> CommandResult<Vec<Suggestion>> {
    let suggestions = context
        .hitomi_client()
        .get_search_suggestions(&query)
        .await
        .map_err(|err| CommandError::from("Failed to get search suggestions", err))?;
    tracing::debug!("get search suggestions success");
    Ok(suggestions)
}

pub fn get_logs_dir_size(context: &Context) -> CommandResult<u64> {
    let logs_dir = context.logs_dir();
    let logs_dir_size = std::fs::read_dir(&logs_dir)
        .context(format!(
            "Failed to read logs directory `{}`",
            logs_dir.display()
        ))
        .map_err(|err| CommandError::from("Failed to get logs directory size", err))?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum::<u64>();
    tracing::debug!("get logs directory size success");
    Ok(logs_dir_size)
}

pub async fn get_cover_data(
    context: &Context,
    cover_url: String,
    comic_download_dir: Option<String>,
) -> CommandResult<Vec<u8>> {
    // Try local cover first if download directory is provided
    if let Some(ref dir) = comic_download_dir {
        let cover_path = PathBuf::from(dir).join("cover.webp");
        if cover_path.exists() {
            if let Ok(data) = fs::read(&cover_path) {
                return Ok(data);
            }
        }
    }

    let cover_data = context
        .hitomi_client()
        .get_cover_data(&cover_url)
        .await
        .map_err(|err| CommandError::from("Failed to get cover", err))?;
    Ok(cover_data.to_vec())
}

pub fn get_synced_comic(context: &Context, mut comic: Comic) -> CommandResult<Comic> {
    comic.update_fields(context).map_err(|err| {
        let err_title = format!("Failed to update fields for comic `{}`", comic.title);
        CommandError::from(&err_title, err)
    })?;

    Ok(comic)
}

/// Read page `index` of the comic downloaded to `download_dir`
fn read_local_page(download_dir: &Path, index: usize) -> anyhow::Result<Vec<u8>> {
    let page_source = PageSource::open(download_dir).context(format!(
        "Failed to open the pages of `{}`",
        download_dir.display()
    ))?;

    let page_data = page_source.read_page(index).context(format!(
        "Failed to read page `{index}` of `{}`",
        download_dir.display()
    ))?;

    Ok(page_data)
}

pub async fn get_image_data(
    context: &Context,
    comic_id: i32,
    index: usize,
    is_downloaded: Option<bool>,
    comic_download_dir: Option<String>,
    file: GalleryFiles,
) -> CommandResult<Vec<u8>> {
    // First try local if downloaded
    if let Some(true) = is_downloaded {
        if let Some(ref download_dir) = comic_download_dir {
            match read_local_page(Path::new(download_dir), index) {
                Ok(page_data) => return Ok(page_data),
                Err(err) => {
                    // Reading locally is only an optimization, so fall back to the network
                    let err_title =
                        format!("Failed to read page `{index}` of `{download_dir}` locally, falling back to the network");
                    let string_chain = err.to_string_chain();
                    tracing::warn!(err_title, message = string_chain);
                }
            }
        }
    }

    // Fallback to network, preferring the smaller avif if the image is available as one
    let mut result = Err(anyhow!("Image is not available as avif"));
    if file.hasavif != 0 {
        let url = image_url_from_image(comic_id, &file, Ext::Avif)
            .await
            .map_err(|err| CommandError::from("Failed to get avif url", err))?;
        result = context.hitomi_client().get_img_data(&url).await;
    }
    if result.is_err() {
        let url = image_url_from_image(comic_id, &file, Ext::Webp)
            .await
            .map_err(|err| CommandError::from("Failed to get webp url", err))?;
        result = context.hitomi_client().get_img_data(&url).await;
    }

    let data = result.map_err(|err| CommandError::from("Failed to get image data", err))?;

    Ok(data.to_vec())
}
