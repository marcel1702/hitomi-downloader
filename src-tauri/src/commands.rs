//! The Tauri commands of the desktop app
//!
//! Each command hands its work to the function of the same name in [`hitomi_core::api`],
//! only the tray and the file manager are desktop features of their own.
use anyhow::Context as _;
use hitomi_core::{
    api,
    config::Config,
    errors::{CommandError, CommandResult},
    hitomi::{GalleryFiles, Suggestion},
    types::{Comic, SearchResult, SearchSort},
    Context,
};
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

use crate::TrayMenuItems;

#[tauri::command]
#[specta::specta]
pub fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command(async)]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn get_config(context: State<Context>) -> Config {
    api::get_config(&context)
}

#[tauri::command(async)]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn save_config(context: State<Context>, config: Config) -> CommandResult<()> {
    api::save_config(&context, config)
}

#[tauri::command(async)]
#[specta::specta]
pub async fn search(
    context: State<'_, Context>,
    query: &str,
    page_num: usize,
    sort: SearchSort,
) -> CommandResult<SearchResult> {
    api::search(&context, query, page_num, sort).await
}

#[tauri::command(async)]
#[specta::specta]
pub async fn get_page(
    context: State<'_, Context>,
    ids: Vec<i32>,
    page_num: usize,
) -> CommandResult<SearchResult> {
    api::get_page(&context, ids, page_num).await
}

#[tauri::command(async)]
#[specta::specta]
pub async fn get_comic(context: State<'_, Context>, id: i32) -> CommandResult<Comic> {
    api::get_comic(&context, id).await
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command(async)]
#[specta::specta]
pub fn create_download_task(context: State<Context>, comic: Comic) -> CommandResult<()> {
    api::create_download_task(&context, comic)
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command(async)]
#[specta::specta]
pub fn export_cbz(context: State<Context>, comic: Comic) -> CommandResult<()> {
    api::export_cbz(&context, comic)
}

#[tauri::command(async)]
#[specta::specta]
pub async fn download_comics_by_ids(
    context: State<'_, Context>,
    ids: Vec<i32>,
) -> CommandResult<()> {
    api::download_comics_by_ids(&context, ids).await
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command(async)]
#[specta::specta]
pub fn pause_download_task(context: State<Context>, id: i32) -> CommandResult<()> {
    api::pause_download_task(&context, id)
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command(async)]
#[specta::specta]
pub fn resume_download_task(context: State<Context>, id: i32) -> CommandResult<()> {
    api::resume_download_task(&context, id)
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command(async)]
#[specta::specta]
pub fn cancel_download_task(context: State<Context>, id: i32) -> CommandResult<()> {
    api::cancel_download_task(&context, id)
}

#[tauri::command(async)]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn get_downloaded_comics(context: State<Context>) -> Vec<Comic> {
    api::get_downloaded_comics(&context)
}

#[tauri::command(async)]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn export_pdf(context: State<Context>, comic: Comic) -> CommandResult<()> {
    api::export_pdf(&context, comic)
}

#[tauri::command(async)]
#[specta::specta]
pub async fn get_search_suggestions(
    context: State<'_, Context>,
    query: String,
) -> CommandResult<Vec<Suggestion>> {
    api::get_search_suggestions(&context, query).await
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command(async)]
#[specta::specta]
pub fn get_logs_dir_size(context: State<Context>) -> CommandResult<u64> {
    api::get_logs_dir_size(&context)
}

/// Translate the labels of the system tray menu
///
/// The tray is rendered by the operating system and cannot read the frontend locale
/// files, so the frontend passes the already translated labels in. It calls this once
/// on startup and again on every language change.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command(async)]
#[specta::specta]
pub fn update_tray_menu_labels(
    tray_menu_items: State<TrayMenuItems>,
    show_window: String,
    quit: String,
) -> CommandResult<()> {
    tray_menu_items
        .show
        .set_text(&show_window)
        .context("Failed to set the text of the show window item")
        .map_err(|err| CommandError::from("Failed to update the tray menu labels", err))?;

    tray_menu_items
        .quit
        .set_text(&quit)
        .context("Failed to set the text of the quit item")
        .map_err(|err| CommandError::from("Failed to update the tray menu labels", err))?;

    tracing::debug!("Updated the tray menu labels successfully");
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command(async)]
#[specta::specta]
pub fn show_path_in_file_manager(app: AppHandle, path: &str) -> CommandResult<()> {
    app.opener()
        .reveal_item_in_dir(path)
        .context(format!("Failed to open `{path}` in file manager"))
        .map_err(|err| CommandError::from("Failed to open in file manager", err))?;
    tracing::debug!("Opened in file manager successfully");
    Ok(())
}

#[tauri::command(async)]
#[specta::specta]
pub async fn get_cover_data(
    context: State<'_, Context>,
    cover_url: String,
    comic_download_dir: Option<String>,
) -> CommandResult<Vec<u8>> {
    api::get_cover_data(&context, cover_url, comic_download_dir).await
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command(async)]
#[specta::specta]
pub fn get_synced_comic(context: State<Context>, comic: Comic) -> CommandResult<Comic> {
    api::get_synced_comic(&context, comic)
}

#[tauri::command(async)]
#[specta::specta]
pub async fn get_image_data(
    context: State<'_, Context>,
    comic_id: i32,
    index: usize,
    is_downloaded: Option<bool>,
    comic_download_dir: Option<String>,
    file: GalleryFiles,
) -> CommandResult<Vec<u8>> {
    api::get_image_data(
        &context,
        comic_id,
        index,
        is_downloaded,
        comic_download_dir,
        file,
    )
    .await
}
