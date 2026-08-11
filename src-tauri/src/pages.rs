use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

use crate::{extensions::PathIsImg, types::ComicInfo};

/// Extension of the archive a comic is packed into
pub const CBZ_EXTENSION: &str = "cbz";
/// Name of the metadata entry inside a cbz archive
const COMIC_INFO_FILE_NAME: &str = "ComicInfo.xml";
/// Name of the cover saved next to the pages of a comic
///
/// The cover is not a page and must never end up in an archive
const COVER_FILE_NAME: &str = "cover.webp";

/// Path of the cbz archive belonging to `download_dir`, whether that archive exists or not
pub fn cbz_path_of(download_dir: &Path) -> anyhow::Result<PathBuf> {
    let download_dir_name = download_dir
        .file_name()
        .context(format!(
            "Failed to get directory name of `{}`",
            download_dir.display()
        ))?
        .to_string_lossy()
        .to_string();

    Ok(download_dir.join(format!("{download_dir_name}.{CBZ_EXTENSION}")))
}

/// Collect the page images lying loose in `dir`, sorted by file name
///
/// `read_dir` yields entries in an arbitrary order, so they have to be sorted
/// to give the comic a deterministic page order.
/// The cover is skipped because it is not a page of the comic.
pub fn collect_dir_pages(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut image_paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .context(format!("Failed to read directory `{}`", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_img() && !is_cover(path))
        .collect();

    image_paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    Ok(image_paths)
}

/// Check if `path` points at the cover of a comic
fn is_cover(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|file_name| file_name.eq_ignore_ascii_case(COVER_FILE_NAME))
}

/// Write `comic_info` and `image_paths` into a cbz archive at `cbz_path`
///
/// The images keep their file name as entry name, so the order they are passed in
/// is the page order of the archive.
pub fn write_cbz(
    cbz_path: &Path,
    comic_info: &ComicInfo,
    image_paths: &[PathBuf],
) -> anyhow::Result<()> {
    // Generate formatted xml
    let cfg = yaserde::ser::Config {
        perform_indent: true,
        ..Default::default()
    };
    let comic_info_xml = yaserde::ser::to_string_with_config(comic_info, &cfg)
        .map_err(|err_msg| anyhow!("Failed to serialize `{COMIC_INFO_FILE_NAME}`: {err_msg}"))?;

    let cbz_file = std::fs::File::create(cbz_path)
        .context(format!("Failed to create file `{}`", cbz_path.display()))?;
    let mut zip_writer = ZipWriter::new(cbz_file);
    // Write ComicInfo.xml into the archive
    zip_writer
        .start_file(COMIC_INFO_FILE_NAME, SimpleFileOptions::default())
        .context(format!(
            "Failed to create `{COMIC_INFO_FILE_NAME}` in `{}`",
            cbz_path.display()
        ))?;
    zip_writer
        .write_all(comic_info_xml.as_bytes())
        .context(format!(
            "Failed to write `{COMIC_INFO_FILE_NAME}` to `{}`",
            cbz_path.display()
        ))?;
    // Write the pages into the archive
    for image_path in image_paths {
        let entry_name = image_path
            .file_name()
            .context(format!(
                "Failed to get file name of `{}`",
                image_path.display()
            ))?
            .to_string_lossy()
            .to_string();

        zip_writer
            .start_file(&entry_name, SimpleFileOptions::default())
            .context(format!(
                "Failed to create `{entry_name}` in `{}`",
                cbz_path.display()
            ))?;

        let mut image_file = std::fs::File::open(image_path)
            .context(format!("Failed to open `{}`", image_path.display()))?;

        std::io::copy(&mut image_file, &mut zip_writer).context(format!(
            "Failed to write `{}` to `{}`",
            image_path.display(),
            cbz_path.display()
        ))?;
    }

    zip_writer
        .finish()
        .context(format!("Failed to close `{}`", cbz_path.display()))?;

    Ok(())
}

/// The pages of a downloaded comic
pub enum PageSource {
    /// The pages come from the cbz archive written by the downloader
    Cbz {
        cbz_path: PathBuf,
        /// Entry names of the pages, in page order
        entry_names: Vec<String>,
    },
    /// The pages come from image files lying loose in the comic download directory
    Dir {
        /// Paths of the pages, in page order
        image_paths: Vec<PathBuf>,
    },
}

impl PageSource {
    /// Open the pages of the comic downloaded to `download_dir`
    ///
    /// The cbz archive is preferred, downloads made before the cbz format was
    /// introduced fall back to the image files lying loose in `download_dir`.
    pub fn open(download_dir: &Path) -> anyhow::Result<Self> {
        let cbz_path = cbz_path_of(download_dir).context(format!(
            "Failed to get the cbz path of `{}`",
            download_dir.display()
        ))?;

        if !cbz_path.exists() {
            let image_paths = collect_dir_pages(download_dir).context(format!(
                "Failed to collect the pages of `{}`",
                download_dir.display()
            ))?;
            return Ok(Self::Dir { image_paths });
        }

        let entry_names = read_cbz_entry_names(&cbz_path).context(format!(
            "Failed to read the entries of `{}`",
            cbz_path.display()
        ))?;

        Ok(Self::Cbz {
            cbz_path,
            entry_names,
        })
    }

    /// The number of pages of the comic
    pub fn page_count(&self) -> usize {
        match self {
            Self::Cbz { entry_names, .. } => entry_names.len(),
            Self::Dir { image_paths } => image_paths.len(),
        }
    }

    /// Read the page at `index`
    ///
    /// Only the page itself is read, the rest of the archive is left untouched.
    pub fn read_page(&self, index: usize) -> anyhow::Result<Vec<u8>> {
        match self {
            Self::Cbz {
                cbz_path,
                entry_names,
            } => {
                let entry_name = entry_names.get(index).context(format!(
                    "Page `{index}` is out of range, `{}` has `{}` pages",
                    cbz_path.display(),
                    entry_names.len()
                ))?;

                let cbz_file = std::fs::File::open(cbz_path)
                    .context(format!("Failed to open `{}`", cbz_path.display()))?;

                let mut archive = ZipArchive::new(std::io::BufReader::new(cbz_file))
                    .context(format!("Failed to open archive `{}`", cbz_path.display()))?;

                let mut entry = archive.by_name(entry_name).context(format!(
                    "Failed to find `{entry_name}` in `{}`",
                    cbz_path.display()
                ))?;

                let mut page_data = Vec::new();
                entry.read_to_end(&mut page_data).context(format!(
                    "Failed to read `{entry_name}` from `{}`",
                    cbz_path.display()
                ))?;

                Ok(page_data)
            }
            Self::Dir { image_paths } => {
                let image_path = image_paths.get(index).context(format!(
                    "Page `{index}` is out of range, there are `{}` pages",
                    image_paths.len()
                ))?;

                let page_data = std::fs::read(image_path)
                    .context(format!("Failed to read `{}`", image_path.display()))?;

                Ok(page_data)
            }
        }
    }
}

/// Read the entry names of the pages in the cbz archive at `cbz_path`, sorted by name
///
/// The order the archive reports its entries in is not guaranteed, so the names are
/// sorted to give the archive a deterministic page order.
/// Non-image entries such as `ComicInfo.xml` and a cover added by another tool are skipped.
fn read_cbz_entry_names(cbz_path: &Path) -> anyhow::Result<Vec<String>> {
    let cbz_file = std::fs::File::open(cbz_path)
        .context(format!("Failed to open `{}`", cbz_path.display()))?;

    let archive = ZipArchive::new(std::io::BufReader::new(cbz_file))
        .context(format!("Failed to open archive `{}`", cbz_path.display()))?;

    let mut entry_names: Vec<String> = archive
        .file_names()
        .filter(|entry_name| {
            let entry_path = Path::new(entry_name);
            entry_path.is_img() && !is_cover(entry_path)
        })
        .map(ToString::to_string)
        .collect();

    entry_names.sort();

    Ok(entry_names)
}
