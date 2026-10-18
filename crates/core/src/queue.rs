//! The download queue on disk, so that a restart does not lose it
//!
//! The file holds every download that has not ended: the ones that wait, run or are paused,
//! and the ones that failed, which stay in the list for a retry. A comic is stored whole, so
//! that restoring the queue needs no request to Hitomi, and with the format it was queued
//! with, which a later change of the setting must not alter.
use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::{
    download_manager::DownloadTaskState,
    pages,
    types::{Comic, DownloadFormat},
};

/// Bumped when the file changes in a way an older version could not read
const FILE_VERSION: u32 = 1;

/// A download as it is stored
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTask {
    pub state: DownloadTaskState,
    pub download_format: DownloadFormat,
    pub comic: Comic,
}

#[derive(Serialize, Deserialize)]
struct QueueFile {
    version: u32,
    tasks: Vec<StoredTask>,
}

/// Whether a download in `state` belongs in the file
///
/// A completed download is in the library, a cancelled one was dropped by the user.
pub fn is_stored(state: DownloadTaskState) -> bool {
    use DownloadTaskState::{Downloading, Failed, Paused, Pending};
    matches!(state, Pending | Downloading | Paused | Failed)
}

/// The downloads in the file at `path`, none if there is no file
///
/// A file that can not be read is moved aside, as `<name>.corrupt`, and the queue starts
/// empty: an error at the start would keep the server from running at all.
pub fn load(path: &Path) -> Vec<StoredTask> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::error!(
                "Failed to read the download queue `{}`: {err}",
                path.display()
            );
            return Vec::new();
        }
    };
    match serde_json::from_str::<QueueFile>(&text) {
        Ok(file) if file.version == FILE_VERSION => file.tasks,
        Ok(file) => {
            set_aside(
                path,
                &format!(
                    "its version is {}, this one reads {FILE_VERSION}",
                    file.version
                ),
            );
            Vec::new()
        }
        Err(err) => {
            set_aside(path, &err.to_string());
            Vec::new()
        }
    }
}

fn set_aside(path: &Path, reason: &str) {
    let mut aside = path.as_os_str().to_owned();
    aside.push(".corrupt");
    let aside = PathBuf::from(aside);
    match std::fs::rename(path, &aside) {
        Ok(()) => tracing::error!(
            "The download queue `{}` can not be used ({reason}), it was moved to `{}` and the queue starts empty",
            path.display(),
            aside.display()
        ),
        Err(err) => tracing::error!(
            "The download queue `{}` can not be used ({reason}) and could not be moved away: {err}",
            path.display()
        ),
    }
}

/// Write `tasks` to `path`, all of it or nothing
///
/// The file is written next to it, made durable and then renamed over it, so that a crash
/// leaves the old queue or the new one and never half of one.
pub fn save(path: &Path, tasks: &[StoredTask]) -> anyhow::Result<()> {
    let file = QueueFile {
        version: FILE_VERSION,
        tasks: tasks.to_vec(),
    };
    let json = serde_json::to_vec(&file).context("Failed to serialize the download queue")?;

    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut out = std::fs::File::create(&tmp)
        .with_context(|| format!("Failed to create `{}`", tmp.display()))?;
    out.write_all(&json)
        .and_then(|()| out.sync_all())
        .with_context(|| format!("Failed to write `{}`", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| {
        format!(
            "Failed to rename `{}` to `{}`",
            tmp.display(),
            path.display()
        )
    })?;
    if let Some(dir) = path.parent() {
        // not every file system can sync a directory, the file itself is on the disk
        let _ = pages::sync_dir(dir);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("hitomi-queue-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("queue.json"), dir)
    }

    fn task(id: i32, state: DownloadTaskState) -> StoredTask {
        StoredTask {
            state,
            download_format: DownloadFormat::Avif,
            comic: Comic {
                id,
                title: format!("Comic {id}"),
                ..Comic::default()
            },
        }
    }

    #[test]
    fn what_was_saved_is_loaded_in_the_same_order() {
        let (path, dir) = temp_file();
        let tasks = vec![
            task(3, DownloadTaskState::Downloading),
            task(1, DownloadTaskState::Pending),
            task(2, DownloadTaskState::Failed),
        ];
        save(&path, &tasks).unwrap();
        let loaded = load(&path);
        assert_eq!(
            loaded.iter().map(|t| t.comic.id).collect::<Vec<_>>(),
            [3, 1, 2]
        );
        assert_eq!(loaded[0].state, DownloadTaskState::Downloading);
        assert_eq!(loaded[0].download_format, DownloadFormat::Avif);
        assert_eq!(loaded[1].comic.title, "Comic 1");
        // nothing is left next to it
        assert!(!dir.join("queue.json.tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn no_file_is_an_empty_queue_and_leaves_nothing_behind() {
        let (path, dir) = temp_file();
        assert!(load(&path).is_empty());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_that_can_not_be_read_is_moved_aside_and_the_queue_starts_empty() {
        let (path, dir) = temp_file();
        std::fs::write(&path, b"{ this is not json").unwrap();
        assert!(load(&path).is_empty());
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(dir.join("queue.json.corrupt")).unwrap(),
            b"{ this is not json"
        );
        // and it can be written again
        save(&path, &[task(1, DownloadTaskState::Pending)]).unwrap();
        assert_eq!(load(&path).len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_of_another_version_is_moved_aside_too() {
        let (path, dir) = temp_file();
        std::fs::write(&path, br#"{"version": 99, "tasks": []}"#).unwrap();
        assert!(load(&path).is_empty());
        assert!(dir.join("queue.json.corrupt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saving_over_an_older_queue_replaces_it() {
        let (path, dir) = temp_file();
        save(
            &path,
            &[
                task(1, DownloadTaskState::Pending),
                task(2, DownloadTaskState::Pending),
            ],
        )
        .unwrap();
        save(&path, &[task(2, DownloadTaskState::Paused)]).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].state, DownloadTaskState::Paused);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_downloads_that_are_not_over_are_stored() {
        use DownloadTaskState::{Cancelled, Completed, Downloading, Failed, Paused, Pending};
        for state in [Pending, Downloading, Paused, Failed] {
            assert!(is_stored(state), "{state:?}");
        }
        for state in [Completed, Cancelled] {
            assert!(!is_stored(state), "{state:?}");
        }
    }
}
