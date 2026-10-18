//! `GET /api/comics/{id}/download/cbz` and `/download/pdf`: a comic as a file for the browser
//!
//! Where the desktop app copies an export to the export directory, the web version hands the
//! file to the browser, which saves it on the device the user sits at. The CBZ is the archive
//! the download already made, nothing is generated for it. A PDF does not exist yet, it is
//! built into a temporary file, streamed from there and deleted afterwards.
use std::{
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context as TaskContext, Poll},
};

use axum::{
    body::Body,
    extract::{Path as UrlPath, State},
    http::{header, HeaderValue, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use futures::Stream;
use hitomi_core::{export, pages};
use tokio::fs::File;
use tokio_util::io::ReaderStream;

use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new().route("/{id}/download/{format}", get(download))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Cbz,
    Pdf,
}

impl Format {
    fn parse(format: &str) -> Option<Self> {
        match format {
            "cbz" => Some(Self::Cbz),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Cbz => pages::CBZ_EXTENSION,
            Self::Pdf => "pdf",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Self::Cbz => "application/vnd.comicbook+zip",
            Self::Pdf => "application/pdf",
        }
    }
}

/// Where the PDFs are built, inside the config volume and not in a RAM backed `/tmp`
pub fn temp_dir(state: &AppState) -> PathBuf {
    state.context.data_dir().join("tmp")
}

/// Delete what a crash left in the temporary directory
pub fn clear_temp_dir(dir: &Path) {
    if let Err(err) = std::fs::remove_dir_all(dir) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("Failed to clear `{}`: {err}", dir.display());
        }
    }
}

async fn download(
    State(state): State<AppState>,
    UrlPath((id, format)): UrlPath<(i32, String)>,
) -> ApiResult<Response> {
    let Some(format) = Format::parse(&format) else {
        return Err(ApiError::with_status(
            StatusCode::NOT_FOUND,
            "unknown file format",
            anyhow::anyhow!("`{format}` is neither `cbz` nor `pdf`"),
        ));
    };
    let comic = state.comics.get(&state.context, id).await?;
    let Some(dir) = state.library_dir_of(&comic) else {
        return Err(ApiError::with_status(
            StatusCode::NOT_FOUND,
            "Failed to download the comic",
            anyhow::anyhow!("comic `{id}` is not downloaded to the library"),
        ));
    };
    let name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| id.to_string());

    let (file, length, temp) = match format {
        Format::Cbz => {
            let (file, length) = open_cbz(&dir, id).await?;
            (file, length, None)
        }
        Format::Pdf => {
            let temp = build_pdf(&state, &dir, id).await?;
            let (file, length) = open(&temp.0, "Failed to download the comic").await?;
            (file, length, Some(temp))
        }
    };

    let body = Body::from_stream(TempFileStream {
        inner: ReaderStream::new(file),
        temp,
    });
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert(
        header::CONTENT_DISPOSITION,
        content_disposition(&format!("{name}.{}", format.extension())),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// The cbz archive of the comic in `dir`
async fn open_cbz(dir: &Path, id: i32) -> ApiResult<(File, u64)> {
    let path = pages::cbz_path_of(dir).map_err(|err| {
        ApiError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to download the comic",
            err,
        )
    })?;
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Err(ApiError::with_status(
            StatusCode::NOT_FOUND,
            "Failed to download the comic",
            anyhow::anyhow!("comic `{id}` has no cbz archive to download"),
        ));
    }
    open(&path, "Failed to download the comic").await
}

async fn open(path: &Path, err_title: &str) -> ApiResult<(File, u64)> {
    let io_error = |err: std::io::Error| {
        ApiError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            err_title,
            anyhow::Error::from(err).context(format!("Failed to open `{}`", path.display())),
        )
    };
    let file = File::open(path).await.map_err(io_error)?;
    let length = file.metadata().await.map_err(io_error)?.len();
    Ok((file, length))
}

/// Build the PDF of the comic in `dir` into a temporary file
async fn build_pdf(state: &AppState, dir: &Path, id: i32) -> ApiResult<TempFile> {
    // Building a PDF holds all its pages in memory, so only a few are built at a time
    let _permit = state.pdf_permits.acquire().await.map_err(|err| {
        ApiError::with_status(
            StatusCode::SERVICE_UNAVAILABLE,
            "Failed to create the PDF",
            err,
        )
    })?;

    let temp_dir = temp_dir(state);
    std::fs::create_dir_all(&temp_dir).map_err(|err| {
        ApiError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to create the PDF",
            anyhow::Error::from(err).context(format!("Failed to create `{}`", temp_dir.display())),
        )
    })?;
    // removed again whatever happens from here on
    let temp = TempFile(temp_dir.join(format!("{:016x}.pdf", rand::random::<u64>())));

    let (source, target) = (dir.to_path_buf(), temp.0.clone());
    tokio::task::spawn_blocking(move || export::write_pdf(&source, &target))
        .await
        .map_err(|err| {
            ApiError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to create the PDF",
                err,
            )
        })?
        .map_err(|err| {
            ApiError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to create the PDF",
                err.context(format!("Failed to create the PDF of comic `{id}`")),
            )
        })?;
    Ok(temp)
}

/// A temporary file that is deleted when this goes out of scope
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        // not there if the PDF never got written, nothing to report then
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Streams a file and deletes the temporary file it came from once it is dropped, which is
/// when the transfer finished and also when the browser broke it off
struct TempFileStream {
    inner: ReaderStream<File>,
    #[allow(dead_code)]
    temp: Option<TempFile>,
}

impl Stream for TempFileStream {
    type Item = std::io::Result<axum::body::Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

/// The `Content-Disposition` header that makes a browser save a file under `filename`
///
/// A plain `filename=` can only carry ASCII, a title with an accent or in Japanese would
/// arrive mangled. So the header holds an ASCII fallback for old clients and the real name
/// as `filename*`, percent encoded as UTF-8 (RFC 6266 and 5987).
fn content_disposition(filename: &str) -> HeaderValue {
    let fallback: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() && !matches!(c, '"' | '\\' | '%') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded = filename.bytes().fold(String::new(), |mut encoded, byte| {
        // the characters RFC 5987 allows as they are
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
        encoded
    });
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}"
    ))
    .expect("the fallback is printable ASCII and the rest is percent encoded")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a browser reads out of `filename*`
    fn decode_filename_star(header: &str) -> String {
        let encoded = header.split("filename*=UTF-8''").nth(1).unwrap();
        let mut bytes = Vec::new();
        let mut rest = encoded.bytes();
        while let Some(byte) = rest.next() {
            if byte == b'%' {
                let hex: Vec<u8> = rest.by_ref().take(2).collect();
                bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).unwrap(), 16).unwrap());
            } else {
                bytes.push(byte);
            }
        }
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn the_real_name_arrives_whatever_it_holds() {
        for name in [
            "La habitación donde solo puedes salir - 3651691.cbz",
            "日本語のタイトル (完全版) - 42.pdf",
            "quote \" backslash \\ percent % star * apostrophe ' semicolon ; - 1.cbz",
            "emoji 🎌 and ünïcödé - 7.cbz",
            "plain - 1.pdf",
        ] {
            let header = content_disposition(name);
            let header = header.to_str().unwrap();
            assert_eq!(decode_filename_star(header), name, "{header}");
            assert!(header.starts_with("attachment; filename=\""), "{header}");
            assert!(header.is_ascii(), "{header}");
        }
    }

    #[test]
    fn the_ascii_fallback_can_not_break_out_of_its_quotes() {
        let header = content_disposition("a\"b\\c%d\ne\r\n.cbz");
        let header = header.to_str().unwrap();
        let fallback = header
            .strip_prefix("attachment; filename=\"")
            .and_then(|rest| rest.split("\"; filename*").next())
            .unwrap();
        assert_eq!(fallback, "a_b_c_d_e__.cbz");
    }

    #[test]
    fn formats_are_cbz_and_pdf_only() {
        assert_eq!(Format::parse("cbz"), Some(Format::Cbz));
        assert_eq!(Format::parse("pdf"), Some(Format::Pdf));
        assert_eq!(Format::parse("zip"), None);
        assert_eq!(Format::parse("../etc/passwd"), None);
        assert_eq!(Format::Cbz.extension(), "cbz");
    }
}
