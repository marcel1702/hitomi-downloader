//! `GET /api/comics/{id}/cover` and `GET /api/comics/{id}/pages/{index}`
//!
//! The images as real HTTP responses a browser can cache, in place of the JSON number arrays
//! the desktop app passes over its IPC. The server looks up where a comic lives itself, so a
//! client never names a path on the server.
use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use hitomi_core::api;

use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{id}/cover", get(cover))
        .route("/{id}/pages/{index}", get(page))
}

/// A gallery on Hitomi never changes its images, a downloaded one only when it is
/// downloaded again, so a day is a safe time for a browser to keep them
const CACHE_CONTROL: HeaderValue = HeaderValue::from_static("private, max-age=86400");

async fn cover(State(state): State<AppState>, Path(id): Path<i32>) -> ApiResult<Response> {
    let comic = state.comics.get(&state.context, id).await?;
    let download_dir = comic
        .comic_download_dir
        .as_ref()
        .map(|dir| dir.to_string_lossy().into_owned());
    let data = api::get_cover_data(&state.context, comic.cover_url.clone(), download_dir).await?;
    Ok(image_response(data))
}

async fn page(
    State(state): State<AppState>,
    Path((id, index)): Path<(i32, usize)>,
) -> ApiResult<Response> {
    let comic = state.comics.get(&state.context, id).await?;
    let Some(file) = comic.files.get(index).cloned() else {
        return Err(ApiError::with_status(
            StatusCode::NOT_FOUND,
            "Failed to get image data",
            anyhow::anyhow!(
                "comic `{id}` has {} pages, there is no page {index}",
                comic.files.len()
            ),
        ));
    };
    let download_dir = comic
        .comic_download_dir
        .as_ref()
        .map(|dir| dir.to_string_lossy().into_owned());
    let data = api::get_image_data(
        &state.context,
        id,
        index,
        comic.is_downloaded,
        download_dir,
        file,
    )
    .await?;
    Ok(image_response(data))
}

fn image_response(data: Vec<u8>) -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(&data)),
            ),
            (header::CACHE_CONTROL, CACHE_CONTROL),
        ],
        data,
    )
        .into_response()
}

/// The media type of an image, told by its first bytes
///
/// A page can be webp or avif depending on what Hitomi has, and the pages of old folder
/// style downloads keep whatever format they came in.
fn content_type(data: &[u8]) -> &'static str {
    match data {
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        [_, _, _, _, b'f', b't', b'y', b'p', b'a', b'v', b'i', b'f' | b's', ..] => "image/avif",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [0xFF, 0x0A, ..] | [0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', ..] => "image/jxl",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_formats_are_told_by_their_first_bytes() {
        let cases: [(&[u8], &str); 8] = [
            (b"RIFF\x24\x00\x00\x00WEBPVP8 ", "image/webp"),
            (b"\x00\x00\x00\x1cftypavif\x00\x00", "image/avif"),
            (b"\x00\x00\x00\x1cftypavis\x00\x00", "image/avif"),
            (b"\xFF\xD8\xFF\xE0\x00\x10JFIF", "image/jpeg"),
            (b"\x89PNG\r\n\x1a\n", "image/png"),
            (b"GIF89a", "image/gif"),
            (b"\x00\x00\x00\x0cJXL \r\n", "image/jxl"),
            (b"<html>", "application/octet-stream"),
        ];
        for (data, expected) in cases {
            assert_eq!(content_type(data), expected, "{data:?}");
        }
        assert_eq!(content_type(b""), "application/octet-stream");
    }
}
