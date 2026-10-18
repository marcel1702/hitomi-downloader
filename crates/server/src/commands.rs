//! `POST /api/<command>`: the commands of the desktop app over HTTP
//!
//! The names and the camelCase arguments are those of the Tauri commands, and so are the
//! answers: the value on success, a `CommandError` on failure. The frontend can call either
//! the same way.
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use hitomi_core::{
    api,
    config::Config,
    hitomi::Suggestion,
    types::{Comic, SearchResult, SearchSort},
};
use serde::Deserialize;

use crate::{
    error::{ApiError, ApiResult, Args},
    paths,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/greet", post(greet))
        .route("/get_config", post(get_config))
        .route("/save_config", post(save_config))
        .route("/search", post(search))
        .route("/get_page", post(get_page))
        .route("/get_comic", post(get_comic))
        .route("/create_download_task", post(create_download_task))
        .route("/pause_download_task", post(pause_download_task))
        .route("/resume_download_task", post(resume_download_task))
        .route("/cancel_download_task", post(cancel_download_task))
        .route("/download_comics_by_ids", post(download_comics_by_ids))
        .route("/get_downloaded_comics", post(get_downloaded_comics))
        .route("/get_synced_comic", post(get_synced_comic))
        .route("/export_cbz", post(export_cbz))
        .route("/export_pdf", post(export_pdf))
        .route("/delete_comic", post(delete_comic))
        .route("/get_search_suggestions", post(get_search_suggestions))
        .route("/get_logs_dir_size", post(get_logs_dir_size))
        .route("/update_tray_menu_labels", post(update_tray_menu_labels))
        .route(
            "/show_path_in_file_manager",
            post(show_path_in_file_manager),
        )
        .route("/get_cover_data", post(replaced_by_image_urls))
        .route("/get_image_data", post(replaced_by_image_urls))
}

/// `comic` with its download directory checked to lie inside the library
///
/// The comic comes from the client, and with it the directory an export reads from and
/// writes next to.
fn checked_comic(state: &AppState, mut comic: Comic) -> ApiResult<Comic> {
    if let Some(dir) = comic.comic_download_dir.take() {
        let library = state.context.config().read().download_dir.clone();
        let dir = paths::rebased_within(&dir, &library).map_err(|err| {
            ApiError::with_status(
                StatusCode::FORBIDDEN,
                "the comic is not in the library",
                err,
            )
        })?;
        comic.comic_download_dir = Some(dir);
    }
    Ok(comic)
}

/// Run blocking core work, a library scan or an export, off the async runtime
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(work).await.map_err(|err| {
        ApiError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the command panicked",
            err,
        )
    })
}

#[derive(Deserialize)]
struct GreetArgs {
    name: String,
}

async fn greet(Args(args): Args<GreetArgs>) -> Json<String> {
    Json(format!(
        "Hello, {}! You've been greeted from Rust!",
        args.name
    ))
}

async fn get_config(State(state): State<AppState>) -> Json<Config> {
    Json(api::get_config(&state.context))
}

#[derive(Deserialize)]
struct SaveConfigArgs {
    config: Config,
}

async fn save_config(
    State(state): State<AppState>,
    Args(SaveConfigArgs { mut config }): Args<SaveConfigArgs>,
) -> ApiResult<Json<()>> {
    // The environment decides where downloads and exports go, not the frontend
    state.pinned_dirs.apply(&mut config);
    blocking(move || api::save_config(&state.context, config)).await??;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchArgs {
    query: String,
    page_num: usize,
    sort: SearchSort,
}

async fn search(
    State(state): State<AppState>,
    Args(args): Args<SearchArgs>,
) -> ApiResult<Json<SearchResult>> {
    let result = api::search(&state.context, &args.query, args.page_num, args.sort).await?;
    state.comics.insert_all(result.comics());
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetPageArgs {
    ids: Vec<i32>,
    page_num: usize,
}

async fn get_page(
    State(state): State<AppState>,
    Args(args): Args<GetPageArgs>,
) -> ApiResult<Json<SearchResult>> {
    let result = api::get_page(&state.context, args.ids, args.page_num).await?;
    state.comics.insert_all(result.comics());
    Ok(Json(result))
}

#[derive(Deserialize)]
struct IdArgs {
    id: i32,
}

async fn get_comic(
    State(state): State<AppState>,
    Args(IdArgs { id }): Args<IdArgs>,
) -> ApiResult<Json<Comic>> {
    let comic = api::get_comic(&state.context, id).await?;
    state.comics.insert(&comic);
    Ok(Json(comic))
}

#[derive(Deserialize)]
struct ComicArgs {
    comic: Comic,
}

async fn create_download_task(
    State(state): State<AppState>,
    Args(ComicArgs { mut comic }): Args<ComicArgs>,
) -> ApiResult<Json<()>> {
    // where the comic goes follows from the folder format, never from the client
    comic.comic_download_dir = None;
    blocking(move || api::create_download_task(&state.context, comic)).await??;
    Ok(Json(()))
}

async fn pause_download_task(
    State(state): State<AppState>,
    Args(IdArgs { id }): Args<IdArgs>,
) -> ApiResult<Json<()>> {
    api::pause_download_task(&state.context, id)?;
    Ok(Json(()))
}

async fn resume_download_task(
    State(state): State<AppState>,
    Args(IdArgs { id }): Args<IdArgs>,
) -> ApiResult<Json<()>> {
    blocking(move || api::resume_download_task(&state.context, id)).await??;
    Ok(Json(()))
}

async fn cancel_download_task(
    State(state): State<AppState>,
    Args(IdArgs { id }): Args<IdArgs>,
) -> ApiResult<Json<()>> {
    api::cancel_download_task(&state.context, id)?;
    Ok(Json(()))
}

#[derive(Deserialize)]
struct IdsArgs {
    ids: Vec<i32>,
}

async fn download_comics_by_ids(
    State(state): State<AppState>,
    Args(IdsArgs { ids }): Args<IdsArgs>,
) -> ApiResult<Json<()>> {
    api::download_comics_by_ids(&state.context, ids).await?;
    Ok(Json(()))
}

async fn get_downloaded_comics(State(state): State<AppState>) -> ApiResult<Json<Vec<Comic>>> {
    let context = state.context.clone();
    let comics = blocking(move || api::get_downloaded_comics(&context)).await?;
    state.comics.insert_all(&comics);
    Ok(Json(comics))
}

async fn get_synced_comic(
    State(state): State<AppState>,
    Args(ComicArgs { mut comic }): Args<ComicArgs>,
) -> ApiResult<Json<Comic>> {
    // looked up in the library afresh, the core would keep what the client sent otherwise
    comic.comic_download_dir = None;
    comic.is_downloaded = None;
    let context = state.context.clone();
    let comic = blocking(move || api::get_synced_comic(&context, comic)).await??;
    state.comics.insert(&comic);
    Ok(Json(comic))
}

async fn export_cbz(
    State(state): State<AppState>,
    Args(ComicArgs { comic }): Args<ComicArgs>,
) -> ApiResult<Json<()>> {
    let comic = checked_comic(&state, comic)?;
    blocking(move || api::export_cbz(&state.context, comic)).await??;
    Ok(Json(()))
}

async fn export_pdf(
    State(state): State<AppState>,
    Args(ComicArgs { comic }): Args<ComicArgs>,
) -> ApiResult<Json<()>> {
    let comic = checked_comic(&state, comic)?;
    blocking(move || api::export_pdf(&state.context, comic)).await??;
    Ok(Json(()))
}

async fn delete_comic(
    State(state): State<AppState>,
    Args(ComicArgs { comic }): Args<ComicArgs>,
) -> ApiResult<Json<()>> {
    let comic = checked_comic(&state, comic)?;
    let id = comic.id;
    let context = state.context.clone();
    blocking(move || api::delete_comic(&context, comic)).await??;
    // the cached comic still names the directory that is gone
    state.comics.remove(id);
    Ok(Json(()))
}

#[derive(Deserialize)]
struct QueryArgs {
    query: String,
}

async fn get_search_suggestions(
    State(state): State<AppState>,
    Args(QueryArgs { query }): Args<QueryArgs>,
) -> ApiResult<Json<Vec<Suggestion>>> {
    Ok(Json(
        api::get_search_suggestions(&state.context, query).await?,
    ))
}

async fn get_logs_dir_size(State(state): State<AppState>) -> ApiResult<Json<u64>> {
    let size = blocking(move || api::get_logs_dir_size(&state.context)).await??;
    Ok(Json(size))
}

/// The web version has no tray, the frontend still sends its labels on startup
async fn update_tray_menu_labels() -> Json<()> {
    Json(())
}

async fn show_path_in_file_manager() -> ApiError {
    ApiError::with_status(
        StatusCode::NOT_IMPLEMENTED,
        "Failed to open in file manager",
        anyhow::anyhow!("the web version has no file manager, the files are on the server"),
    )
}

/// Took the path of a comic from the frontend and answered with the image as JSON numbers
///
/// The image endpoints under `/api/comics/{id}` find the comic themselves and answer with
/// the image, which a browser can cache.
async fn replaced_by_image_urls() -> ApiError {
    ApiError::with_status(
        StatusCode::GONE,
        "the command is not available in the web version",
        anyhow::anyhow!(
            "use GET /api/comics/{{id}}/cover and GET /api/comics/{{id}}/pages/{{index}}"
        ),
    )
}
