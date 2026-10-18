use std::path::PathBuf;

use hitomi_core::{api, errors::CommandResult, events::Event, types::Comic, Context};
use indexmap::IndexMap;
use parking_lot::Mutex;
use tokio::sync::{broadcast, watch};

/// What every request handler shares
#[derive(Clone)]
pub struct AppState {
    pub context: Context,
    /// The library and export dir in use, which no client can change
    pub pinned_dirs: PinnedDirs,
    /// Limits the PDFs generated at the same time, each of them is built in memory
    pub pdf_permits: std::sync::Arc<tokio::sync::Semaphore>,
    pub events: broadcast::Sender<Event>,
    pub comics: std::sync::Arc<ComicCache>,
    /// Turns `true` once the server shuts down, which ends the event streams
    pub shutdown: watch::Receiver<bool>,
}

impl AppState {
    /// The download directory of `comic`, if it lies inside the library
    ///
    /// Every comic in the cache came from the server, this makes sure a file is never read
    /// from anywhere else. One that is not in the library is not downloaded.
    pub fn library_dir_of(&self, comic: &Comic) -> Option<PathBuf> {
        let library = self.context.config().read().download_dir.clone();
        crate::paths::rebased_within(comic.comic_download_dir.as_ref()?, &library).ok()
    }
}

/// The library and export directory of the server
///
/// Both are always pinned, whether the environment names them or not: a variable that is not
/// set leaves the directory the server starts with, which is the one in `config.json`, or
/// else the default of the core. Either way no client can move them afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedDirs {
    pub download_dir: PathBuf,
    pub export_dir: PathBuf,
}

impl PinnedDirs {
    /// The directories from the environment, and for those it does not name, from `config`
    pub fn resolve(
        library_dir: Option<&PathBuf>,
        export_dir: Option<&PathBuf>,
        config: &hitomi_core::config::Config,
    ) -> Self {
        Self {
            download_dir: library_dir.unwrap_or(&config.download_dir).clone(),
            export_dir: export_dir.unwrap_or(&config.export_dir).clone(),
        }
    }

    /// Put the pinned directories into `config`, whatever it says
    pub fn apply(&self, config: &mut hitomi_core::config::Config) {
        config.download_dir.clone_from(&self.download_dir);
        config.export_dir.clone_from(&self.export_dir);
    }

    /// Create both directories, and refuse a setup in which an export would land in the library
    ///
    /// The library gets scanned, so a comic exported into it would show up a second time.
    /// This looks at the directories in use, not at the environment variables, so it also
    /// holds when none of them is set.
    pub fn check(&self) -> anyhow::Result<()> {
        let export_existed = self.export_dir.exists();
        for dir in [&self.download_dir, &self.export_dir] {
            std::fs::create_dir_all(dir)
                .map_err(|err| anyhow::anyhow!("Failed to create `{}`: {err}", dir.display()))?;
        }
        let library = self.download_dir.canonicalize()?;
        let export = self.export_dir.canonicalize()?;
        if export == library {
            anyhow::bail!(
                "The export directory is the library directory (`{}`). Exports would show up in the library a second time, give them a directory of their own with HITOMI_EXPORT_DIR",
                library.display()
            );
        }
        if export.starts_with(&library) {
            if !export_existed {
                // do not leave the directories of a refused setup behind in the library
                for dir in export.ancestors().take_while(|dir| *dir != library) {
                    let _ = std::fs::remove_dir(dir);
                }
            }
            anyhow::bail!(
                "The export directory `{}` lies inside the library directory `{}`. Exports would show up in the library a second time, give them a directory of their own with HITOMI_EXPORT_DIR",
                export.display(),
                library.display()
            );
        }
        Ok(())
    }
}

/// Create the directory the pages of running downloads are kept in, and check that it is usable
///
/// It has to be writable, and it must not lie inside `library`: the point of keeping the
/// pages elsewhere is that a scan of the library never finds them.
pub fn check_temp_dir(temp_dir: &std::path::Path, library: &std::path::Path) -> anyhow::Result<()> {
    let existed = temp_dir.exists();
    std::fs::create_dir_all(temp_dir).map_err(|err| {
        anyhow::anyhow!(
            "Failed to create the directory for the pages of running downloads, `{}`: {err}. Set HITOMI_TEMP_DIR to a directory the server may write to",
            temp_dir.display()
        )
    })?;
    // creating a directory does not show that files can be written to it
    let probe = temp_dir.join(format!(".write-test-{}", std::process::id()));
    std::fs::write(&probe, b"").and_then(|()| std::fs::remove_file(&probe)).map_err(|err| {
        anyhow::anyhow!(
            "The directory for the pages of running downloads, `{}`, is not writable: {err}. Set HITOMI_TEMP_DIR to a directory the server may write to",
            temp_dir.display()
        )
    })?;
    let (temp_resolved, library_resolved) = (temp_dir.canonicalize()?, library.canonicalize()?);
    if temp_resolved.starts_with(&library_resolved) {
        if !existed {
            // do not leave the directories of a refused setup behind in the library
            for dir in temp_resolved
                .ancestors()
                .take_while(|dir| *dir != library_resolved)
            {
                let _ = std::fs::remove_dir(dir);
            }
        }
        anyhow::bail!(
            "The directory for the pages of running downloads, `{}`, lies inside the library `{}`, where a scan of the library would find unfinished downloads. Set HITOMI_TEMP_DIR to a directory outside of it",
            temp_dir.display(),
            library.display()
        );
    }
    Ok(())
}

/// How many comics the cache keeps, about 40 search pages
const COMIC_CACHE_CAPACITY: usize = 1000;

/// The comics the frontend saw last, so an image request knows the files and the download
/// directory of its comic without asking Hitomi for the gallery again
#[derive(Default)]
pub struct ComicCache {
    comics: Mutex<IndexMap<i32, Comic>>,
}

impl ComicCache {
    pub fn insert(&self, comic: &Comic) {
        let mut comics = self.comics.lock();
        // moved to the end, so the comics seen last are the ones that stay
        comics.shift_remove(&comic.id);
        comics.insert(comic.id, comic.clone());
        while comics.len() > COMIC_CACHE_CAPACITY {
            comics.shift_remove_index(0);
        }
    }

    pub fn insert_all<'a>(&self, comics: impl IntoIterator<Item = &'a Comic>) {
        for comic in comics {
            self.insert(comic);
        }
    }

    /// Forget `id`, its download directory has changed
    pub fn remove(&self, id: i32) {
        self.comics.lock().shift_remove(&id);
    }

    /// The comic `id`, from the cache or else from Hitomi
    pub async fn get(&self, context: &Context, id: i32) -> CommandResult<Comic> {
        if let Some(comic) = self.comics.lock().get(&id).cloned() {
            return Ok(comic);
        }
        let comic = api::get_comic(context, id).await?;
        self.insert(&comic);
        Ok(comic)
    }

    #[cfg(test)]
    pub fn ids(&self) -> Vec<i32> {
        self.comics.lock().keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comic(id: i32) -> Comic {
        Comic {
            id,
            ..Comic::default()
        }
    }

    #[test]
    fn the_comics_seen_last_stay() {
        let cache = ComicCache::default();
        for id in 0..i32::try_from(COMIC_CACHE_CAPACITY).unwrap() + 2 {
            cache.insert(&comic(id));
        }
        // seen again, so it moves to the end and survives the next eviction
        cache.insert(&comic(2));
        cache.insert(&comic(-1));

        let ids = cache.ids();
        assert_eq!(ids.len(), COMIC_CACHE_CAPACITY);
        assert!(!ids.contains(&0) && !ids.contains(&1) && !ids.contains(&3));
        assert_eq!(&ids[ids.len() - 2..], &[2, -1]);
    }

    fn config_with(download_dir: &str, export_dir: &str) -> hitomi_core::config::Config {
        serde_json::from_value(serde_json::json!({
            "downloadDir": download_dir, "exportDir": export_dir, "enableFileLogger": true,
            "downloadFormat": "Webp", "dirFmt": "{id}",
            "proxy": {"enabled": false, "host": "127.0.0.1", "port": 1080,
                      "username": "", "password": "", "remoteDns": true},
            "comicConcurrency": 2, "imgConcurrency": 4, "language": "en-US"
        }))
        .unwrap()
    }

    #[test]
    fn the_environment_names_the_directories_and_the_config_fills_the_rest() {
        let config = config_with("/config/download", "/config/export");
        let library = PathBuf::from("/data/library");
        let export = PathBuf::from("/data/export");

        let both = PinnedDirs::resolve(Some(&library), Some(&export), &config);
        assert_eq!((&both.download_dir, &both.export_dir), (&library, &export));

        // a variable that is not set leaves the directory the server starts with
        let only_library = PinnedDirs::resolve(Some(&library), None, &config);
        assert_eq!(only_library.export_dir, PathBuf::from("/config/export"));
        let none = PinnedDirs::resolve(None, None, &config);
        assert_eq!(none.download_dir, PathBuf::from("/config/download"));
    }

    #[test]
    fn pinned_dirs_win_over_whatever_the_config_says() {
        let mut config = config_with("/somewhere", "/elsewhere");
        let pinned = PinnedDirs {
            download_dir: PathBuf::from("/data/library"),
            export_dir: PathBuf::from("/data/export"),
        };
        pinned.apply(&mut config);
        assert_eq!(config.download_dir, PathBuf::from("/data/library"));
        assert_eq!(config.export_dir, PathBuf::from("/data/export"));
    }

    fn temp_dirs(library: &str, export: &str) -> (PathBuf, PinnedDirs) {
        let root = std::env::temp_dir().join(format!("hitomi-pinned-{}", rand::random::<u64>()));
        let pinned = PinnedDirs {
            download_dir: root.join(library),
            export_dir: root.join(export),
        };
        (root, pinned)
    }

    #[test]
    fn separate_directories_are_created_and_accepted() {
        let (root, pinned) = temp_dirs("library", "export");
        pinned.check().unwrap();
        assert!(pinned.download_dir.is_dir() && pinned.export_dir.is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_export_dir_in_or_as_the_library_is_refused() {
        for (library, export) in [
            ("library", "library"),
            ("library", "library/export"),
            ("library", "library/a/b"),
        ] {
            let (root, pinned) = temp_dirs(library, export);
            let err = pinned.check().unwrap_err().to_string();
            assert!(
                err.contains("HITOMI_EXPORT_DIR"),
                "{library} / {export}: {err}"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_into_the_library_does_not_hide_the_overlap() {
        let (root, pinned) = temp_dirs("library", "export");
        std::fs::create_dir_all(&pinned.download_dir).unwrap();
        std::os::unix::fs::symlink(&pinned.download_dir, &pinned.export_dir).unwrap();
        assert!(pinned.check().is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_pages_of_downloads_get_a_usable_directory_outside_the_library() {
        let (root, pinned) = temp_dirs("library", "export");
        pinned.check().unwrap();
        let temp = root.join("config").join("downloading");
        check_temp_dir(&temp, &pinned.download_dir).unwrap();
        assert!(temp.is_dir());
        // the write test leaves nothing behind
        assert_eq!(std::fs::read_dir(&temp).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_temp_dir_in_the_library_is_refused() {
        let (root, pinned) = temp_dirs("library", "export");
        pinned.check().unwrap();
        for inside in [
            pinned.download_dir.clone(),
            pinned.download_dir.join("pages"),
        ] {
            let err = check_temp_dir(&inside, &pinned.download_dir)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("HITOMI_TEMP_DIR") && err.contains("inside the library"),
                "{err}"
            );
        }
        // what the refused setup created is gone again, the library is as it was
        assert_eq!(std::fs::read_dir(&pinned.download_dir).unwrap().count(), 0);
        // also when the directory was new and several levels deep
        let deep = pinned.download_dir.join("a").join("b");
        assert!(check_temp_dir(&deep, &pinned.download_dir).is_err());
        assert_eq!(std::fs::read_dir(&pinned.download_dir).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_temp_dir_that_can_not_be_created_is_explained() {
        let (root, pinned) = temp_dirs("library", "export");
        pinned.check().unwrap();
        // a file where a directory is needed
        let blocker = root.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let err = check_temp_dir(&blocker.join("downloading"), &pinned.download_dir)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("blocker") && err.contains("HITOMI_TEMP_DIR"),
            "{err}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_refused_export_dir_leaves_nothing_in_the_library() {
        for export in ["library/export", "library/a/b"] {
            let (root, pinned) = temp_dirs("library", export);
            std::fs::create_dir_all(&pinned.download_dir).unwrap();
            assert!(pinned.check().is_err());
            assert_eq!(
                std::fs::read_dir(&pinned.download_dir).unwrap().count(),
                0,
                "{export}"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
        // one that was there before is not the business of the check, it is left alone
        let (root, pinned) = temp_dirs("library", "library/mine");
        std::fs::create_dir_all(&pinned.export_dir).unwrap();
        assert!(pinned.check().is_err());
        assert!(pinned.export_dir.is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_library_inside_the_export_dir_is_fine() {
        // a scan of the library never sees the exports there, they land next to the library
        let (root, pinned) = temp_dirs("exports/library", "exports");
        pinned.check().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
