//! Deleting a downloaded comic from the library
//!
//! The comic comes from the frontend, and with it the directory that is removed. Nothing is
//! taken on trust: the directory has to lie inside the library, has to be a comic and has to
//! be the comic that was asked for. Exports are never touched.

use std::path::Path;

use anyhow::{anyhow, bail, Context as _};
use walkdir::WalkDir;

use crate::{context::Context, types::Comic};

/// Delete the directory of `comic` from the library, and the pages it left behind
///
/// A comic that is being downloaded can not be deleted.
pub fn comic(context: &Context, comic: &Comic) -> anyhow::Result<()> {
    if context.download_manager().is_active(comic.id) {
        bail!("the comic is being downloaded, cancel the download first");
    }
    let library = context.config().read().download_dir.clone();
    delete_in(&library, comic, context.temp_dir())
}

/// [`comic`] for the library `library` and the pages in `temp_root`, the temporary directory
/// of the web version
fn delete_in(library: &Path, comic: &Comic, temp_root: Option<&Path>) -> anyhow::Result<()> {
    let dir = comic
        .comic_download_dir
        .as_ref()
        .context("the comic has no download directory")?;
    let library = library
        .canonicalize()
        .with_context(|| format!("the library `{}` does not exist", library.display()))?;
    // `..` and symlinks are resolved, a link that leads out of the library is not followed
    let dir = dir
        .canonicalize()
        .with_context(|| format!("`{}` does not exist", dir.display()))?;
    if dir == library || !dir.starts_with(&library) {
        bail!(
            "`{}` is not a directory inside the library `{}`",
            dir.display(),
            library.display()
        );
    }

    check_is_the_comic(&dir, comic)?;

    std::fs::remove_dir_all(&dir)
        .with_context(|| format!("Failed to delete `{}`", dir.display()))?;

    // the pages of a download that was cancelled or interrupted, outside of the library
    if let Some(temp_root) = temp_root {
        let pages = temp_root.join(comic.id.to_string());
        if pages.is_dir() {
            if let Err(err) = std::fs::remove_dir_all(&pages) {
                tracing::warn!("Failed to delete the pages `{}`: {err}", pages.display());
            }
        }
    }

    remove_empty_parents(&dir, &library);
    Ok(())
}

/// `dir` holds the metadata of `comic` and of no other comic
fn check_is_the_comic(dir: &Path, comic: &Comic) -> anyhow::Result<()> {
    let metadata_path = dir.join("metadata.json");
    if !metadata_path.is_file() {
        bail!(
            "`{}` is not a downloaded comic, it has no metadata.json",
            dir.display()
        );
    }
    let on_disk = Comic::from_metadata(&metadata_path)?;
    if on_disk.id != comic.id {
        bail!(
            "`{}` holds the comic {}, not the comic {}",
            dir.display(),
            on_disk.id,
            comic.id
        );
    }
    // a folder format can put comics inside each other, deleting this one would take the
    // other with it
    let other = WalkDir::new(dir)
        .min_depth(2)
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| entry.file_name() == "metadata.json");
    if let Some(other) = other {
        return Err(anyhow!(
            "`{}` holds another comic in `{}`, delete that one first",
            dir.display(),
            other.path().display()
        ));
    }
    Ok(())
}

/// Remove the folders `dir` was in, up to the library, as far as they are empty
///
/// A folder format such as `{artist}/{title}` leaves the folder of the artist behind.
fn remove_empty_parents(dir: &Path, library: &Path) {
    let mut parent = dir.parent();
    while let Some(folder) = parent {
        if folder == library || !folder.starts_with(library) {
            break;
        }
        // fails if the folder is not empty, which is the end of it
        if std::fs::remove_dir(folder).is_err() {
            break;
        }
        parent = folder.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Library {
        root: PathBuf,
    }

    impl Library {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("hitomi-delete-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(root.join("library")).unwrap();
            std::fs::create_dir_all(root.join("elsewhere")).unwrap();
            Self { root }
        }

        fn library(&self) -> PathBuf {
            self.root.join("library")
        }

        /// A downloaded comic `id` in `library/<relative>`, with an archive next to its metadata
        fn comic(&self, id: i32, relative: &str) -> Comic {
            let dir = self.library().join(relative);
            std::fs::create_dir_all(&dir).unwrap();
            let comic = Comic {
                id,
                title: relative.to_string(),
                ..Comic::default()
            };
            std::fs::write(
                dir.join("metadata.json"),
                serde_json::to_string(&comic).unwrap(),
            )
            .unwrap();
            std::fs::write(dir.join("comic.cbz"), b"archive").unwrap();
            std::fs::write(dir.join("cover.webp"), b"cover").unwrap();
            Comic {
                comic_download_dir: Some(dir),
                ..comic
            }
        }
    }

    impl Drop for Library {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn deletes_the_directory_of_the_comic_and_leaves_the_others() {
        let fixture = Library::new();
        let one = fixture.comic(1, "one");
        let two = fixture.comic(2, "two");
        delete_in(&fixture.library(), &one, None).unwrap();
        assert!(!one.comic_download_dir.as_ref().unwrap().exists());
        assert!(two
            .comic_download_dir
            .as_ref()
            .unwrap()
            .join("comic.cbz")
            .exists());
        assert!(fixture.library().exists());
    }

    #[test]
    fn removes_the_empty_folders_it_was_in_but_not_the_ones_with_other_comics() {
        let fixture = Library::new();
        let alone = fixture.comic(1, "artist a/alone");
        let first = fixture.comic(2, "artist b/first");
        let second = fixture.comic(3, "artist b/second");
        delete_in(&fixture.library(), &alone, None).unwrap();
        assert!(!fixture.library().join("artist a").exists());
        delete_in(&fixture.library(), &first, None).unwrap();
        assert!(fixture.library().join("artist b").exists());
        delete_in(&fixture.library(), &second, None).unwrap();
        assert!(!fixture.library().join("artist b").exists());
        assert!(fixture.library().exists());
    }

    #[test]
    fn exports_are_not_touched() {
        let fixture = Library::new();
        let export = fixture.root.join("export");
        std::fs::create_dir_all(&export).unwrap();
        std::fs::write(export.join("one.cbz"), b"export").unwrap();
        let one = fixture.comic(1, "one");
        delete_in(&fixture.library(), &one, None).unwrap();
        assert!(export.join("one.cbz").exists());
    }

    #[test]
    fn the_pages_of_an_interrupted_download_go_too() {
        let fixture = Library::new();
        let temp = fixture.root.join("temp");
        std::fs::create_dir_all(temp.join("1")).unwrap();
        std::fs::write(temp.join("1/0.webp"), b"page").unwrap();
        std::fs::create_dir_all(temp.join("2")).unwrap();
        let one = fixture.comic(1, "one");
        delete_in(&fixture.library(), &one, Some(&temp)).unwrap();
        assert!(!temp.join("1").exists());
        assert!(temp.join("2").exists(), "the pages of another comic stay");
    }

    #[test]
    fn a_directory_outside_the_library_is_refused() {
        let fixture = Library::new();
        let outside = fixture.root.join("elsewhere");
        std::fs::write(outside.join("metadata.json"), "{}").unwrap();
        let comic = Comic {
            id: 1,
            comic_download_dir: Some(outside.clone()),
            ..Comic::default()
        };
        assert!(delete_in(&fixture.library(), &comic, None).is_err());
        assert!(outside.join("metadata.json").exists());
    }

    #[test]
    fn dot_dot_out_of_the_library_is_refused() {
        let fixture = Library::new();
        let other = fixture.comic(1, "one");
        let outside = fixture.root.join("elsewhere");
        std::fs::write(
            outside.join("metadata.json"),
            serde_json::to_string(&other).unwrap(),
        )
        .unwrap();
        let comic = Comic {
            id: 1,
            comic_download_dir: Some(fixture.library().join("one/../../elsewhere")),
            ..Comic::default()
        };
        assert!(delete_in(&fixture.library(), &comic, None).is_err());
        assert!(outside.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_library_is_refused_and_its_target_stays() {
        let fixture = Library::new();
        let target = fixture.root.join("elsewhere");
        let comic = Comic {
            id: 1,
            ..Comic::default()
        };
        std::fs::write(
            target.join("metadata.json"),
            serde_json::to_string(&comic).unwrap(),
        )
        .unwrap();
        std::os::unix::fs::symlink(&target, fixture.library().join("link")).unwrap();
        let comic = Comic {
            comic_download_dir: Some(fixture.library().join("link")),
            ..comic
        };
        assert!(delete_in(&fixture.library(), &comic, None).is_err());
        assert!(target.join("metadata.json").exists());
    }

    #[test]
    fn the_library_itself_is_refused() {
        let fixture = Library::new();
        let one = fixture.comic(1, "one");
        let comic = Comic {
            comic_download_dir: Some(fixture.library()),
            ..one
        };
        assert!(delete_in(&fixture.library(), &comic, None).is_err());
        assert!(fixture.library().join("one/comic.cbz").exists());
    }

    #[test]
    fn a_directory_that_is_not_a_comic_is_refused() {
        let fixture = Library::new();
        let folder = fixture.library().join("photos");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("holiday.jpg"), b"photo").unwrap();
        let comic = Comic {
            id: 1,
            comic_download_dir: Some(folder.clone()),
            ..Comic::default()
        };
        assert!(delete_in(&fixture.library(), &comic, None).is_err());
        assert!(folder.join("holiday.jpg").exists());
    }

    #[test]
    fn a_directory_of_another_comic_is_refused() {
        let fixture = Library::new();
        let one = fixture.comic(1, "one");
        let asked_for = Comic {
            id: 2,
            ..one.clone()
        };
        assert!(delete_in(&fixture.library(), &asked_for, None).is_err());
        assert!(one.comic_download_dir.unwrap().join("comic.cbz").exists());
    }

    #[test]
    fn a_comic_that_holds_another_comic_is_refused() {
        let fixture = Library::new();
        let outer = fixture.comic(1, "outer");
        let _inner = fixture.comic(2, "outer/inner");
        assert!(delete_in(&fixture.library(), &outer, None).is_err());
        assert!(fixture.library().join("outer/inner/comic.cbz").exists());
        // the inner one can go, then the outer one
        let inner = Comic {
            id: 2,
            comic_download_dir: Some(fixture.library().join("outer/inner")),
            ..Comic::default()
        };
        delete_in(&fixture.library(), &inner, None).unwrap();
        delete_in(&fixture.library(), &outer, None).unwrap();
    }

    #[test]
    fn a_missing_directory_and_a_comic_without_one_are_errors() {
        let fixture = Library::new();
        let gone = Comic {
            id: 1,
            comic_download_dir: Some(fixture.library().join("gone")),
            ..Comic::default()
        };
        assert!(delete_in(&fixture.library(), &gone, None).is_err());
        assert!(delete_in(&fixture.library(), &Comic::default(), None).is_err());
    }
}
