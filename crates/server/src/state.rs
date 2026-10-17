use std::path::PathBuf;

use hitomi_core::{api, errors::CommandResult, events::Event, types::Comic, Context};
use indexmap::IndexMap;
use parking_lot::Mutex;
use tokio::sync::{broadcast, watch};

/// What every request handler shares
#[derive(Clone)]
pub struct AppState {
    pub context: Context,
    /// Download and export dir set by the environment, which the config can not change
    pub pinned_dirs: PinnedDirs,
    pub events: broadcast::Sender<Event>,
    pub comics: std::sync::Arc<ComicCache>,
    /// Turns `true` once the server shuts down, which ends the event streams
    pub shutdown: watch::Receiver<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinnedDirs {
    pub download_dir: Option<PathBuf>,
    pub export_dir: Option<PathBuf>,
}

impl PinnedDirs {
    /// Put the pinned directories into `config`, whatever it says
    pub fn apply(&self, config: &mut hitomi_core::config::Config) {
        if let Some(download_dir) = &self.download_dir {
            config.download_dir.clone_from(download_dir);
        }
        if let Some(export_dir) = &self.export_dir {
            config.export_dir.clone_from(export_dir);
        }
    }
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

    #[test]
    fn pinned_dirs_win_over_the_config() {
        let mut config: hitomi_core::config::Config = serde_json::from_value(serde_json::json!({
            "downloadDir": "/somewhere", "exportDir": "/elsewhere", "enableFileLogger": true,
            "downloadFormat": "Webp", "dirFmt": "{id}",
            "proxy": {"enabled": false, "host": "127.0.0.1", "port": 1080,
                      "username": "", "password": "", "remoteDns": true},
            "comicConcurrency": 2, "imgConcurrency": 4, "language": "en-US"
        }))
        .unwrap();

        PinnedDirs {
            download_dir: Some(PathBuf::from("/data/library")),
            export_dir: None,
        }
        .apply(&mut config);
        assert_eq!(config.download_dir, PathBuf::from("/data/library"));
        assert_eq!(config.export_dir, PathBuf::from("/elsewhere"));
    }
}
