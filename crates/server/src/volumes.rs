//! What the server checks about its directories when it starts
//!
//! The directories live in a volume, a bind mount or on a network share, and a lot can be
//! wrong with them: nobody mounted anything, so what is written would be lost with the
//! container, the user the server runs as may not write there, or the file system allows
//! shorter file names than a title needs. Each of these is caught here, at the start and with
//! a message that names the path and the likely cause, not at the first download.
use std::{
    io::{self, ErrorKind},
    path::Path,
};

use anyhow::{anyhow, bail};
use hitomi_core::utils::{DEFAULT_MAX_FILE_NAME_BYTES, MIN_MAX_FILE_NAME_BYTES};

/// The user and group the server runs as, if the system says
pub fn current_user() -> Option<(u32, u32)> {
    // /proc/self/status: "Uid:\treal\teffective\tsaved\tfs", the same for Gid
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let effective = |key: &str| -> Option<u32> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };
    Some((effective("Uid:")?, effective("Gid:")?))
}

/// Owner, group and permissions of `dir`
#[cfg(unix)]
fn owner_of(dir: &Path) -> Option<(u32, u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(dir).ok()?;
    Some((meta.uid(), meta.gid(), meta.mode() & 0o7777))
}

#[cfg(not(unix))]
fn owner_of(_dir: &Path) -> Option<(u32, u32, u32)> {
    None
}

/// What to do about a directory the server may not write to, from who runs the server and
/// who owns the directory
fn permission_advice(user: Option<(u32, u32)>, owner: Option<(u32, u32, u32)>) -> String {
    match (user, owner) {
        (Some((uid, gid)), Some((owner_uid, _, mode))) if owner_uid == uid => format!(
            "The server runs as user {uid}:{gid} and the directory belongs to it too, but its \
             mode {mode:o} does not let it write. Change the permissions of the directory."
        ),
        (user, owner) => {
            let user = user.map_or_else(
                || "an unknown user".to_string(),
                |(uid, gid)| format!("user {uid}:{gid}"),
            );
            let owner = owner.map_or_else(
                || "someone unknown".to_string(),
                |(uid, gid, mode)| format!("{uid}:{gid}, mode {mode:o}"),
            );
            format!(
                "The server runs as {user} and the directory belongs to {owner}. Set PUID and \
                 PGID to the owner of the directory, or change its owner or its permissions. \
                 On an NFS or SMB share the settings of the share decide which user may write, \
                 for example root squash or a user the share maps everyone to."
            )
        }
    }
}

/// Check that the server can create files in `dir`, which holds the `what` it names
///
/// `variable` is the environment variable that sets the directory, for the message.
pub fn check_writable(dir: &Path, what: &str, variable: &str) -> anyhow::Result<()> {
    let probe = dir.join(format!(".hitomi-write-test-{}", std::process::id()));
    let result = std::fs::write(&probe, b"").and_then(|()| std::fs::remove_file(&probe));
    let Err(err) = result else {
        return Ok(());
    };

    let cause = match err.kind() {
        ErrorKind::ReadOnlyFilesystem => {
            "It is mounted read-only: look for `:ro` on the volume or a read-only share."
                .to_string()
        }
        ErrorKind::PermissionDenied => permission_advice(current_user(), owner_of(dir)),
        _ => format!(
            "The server runs as {}, the directory belongs to {}.",
            current_user().map_or_else(
                || "an unknown user".to_string(),
                |(uid, gid)| format!("user {uid}:{gid}")
            ),
            owner_of(dir).map_or_else(
                || "someone unknown".to_string(),
                |(uid, gid, mode)| format!("{uid}:{gid}, mode {mode:o}")
            )
        ),
    };
    Err(anyhow!(
        "The {what} `{}` is not writable: {err}. {cause} The path is set with {variable}.",
        dir.display()
    ))
}

/// Check that something is mounted at `dir`, and that it is not a part of the file system the
/// container started with
///
/// What is written there would be lost with the container. The typical cause is a bind mount
/// whose source did not exist: Docker makes the missing directory on the host when the short
/// syntax is used, or here, inside the container, when nothing is mounted at all.
#[cfg(unix)]
pub fn check_mounted(dir: &Path, root: &Path, what: &str, variable: &str) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if std::fs::metadata(dir)?.dev() == std::fs::metadata(root)?.dev() {
        bail!(
            "Nothing is mounted at the {what} `{}`: it is on the same file system as `{}`, so \
             what the server writes there is lost with the container. Mount a volume or a \
             directory there. For a bind mount use the long syntax with `create_host_path: false`, \
             so that a missing source stops the container instead of being made on the host. \
             Set HITOMI_REQUIRE_MOUNT=false to run without a mount on purpose. The path is set \
             with {variable}.",
            dir.display(),
            root.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn check_mounted(
    _dir: &Path,
    _root: &Path,
    _what: &str,
    _variable: &str,
) -> anyhow::Result<()> {
    Ok(())
}

/// The longest file name, in bytes, `dir` takes, found by trying
///
/// A file system can allow less than the usual 255: an encrypted shared folder of a Synology
/// NAS stops at about 143, because its names are encrypted. The names of the comics are
/// shortened to what is found here.
pub fn probe_max_name_bytes(dir: &Path) -> anyhow::Result<usize> {
    probe_with(DEFAULT_MAX_FILE_NAME_BYTES, |bytes| {
        let name = format!("{:x<bytes$}", ".hitomi-name-test-");
        let path = dir.join(&name);
        match std::fs::File::create(&path) {
            Ok(_) => {
                std::fs::remove_file(&path)?;
                Ok(true)
            }
            // ENAMETOOLONG
            Err(err) if err.kind() == ErrorKind::InvalidFilename => Ok(false),
            Err(err) => Err(err),
        }
    })
    .map_err(|err| {
        anyhow!(
            "Failed to find out how long a file name in `{}` may be: {err}",
            dir.display()
        )
    })
}

/// The largest `n` up to `max` for which `accepts(n)` holds, assuming that it holds up to a
/// limit and not beyond
fn probe_with(max: usize, mut accepts: impl FnMut(usize) -> io::Result<bool>) -> io::Result<usize> {
    if accepts(max)? {
        return Ok(max);
    }
    // `low` is accepted, `high` is not
    let (mut low, mut high) = (0, max);
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        if accepts(middle)? {
            low = middle;
        } else {
            high = middle;
        }
    }
    Ok(low)
}

/// The limit the names of the comics are shortened to, from what `dirs` allow and what the
/// owner asked for with `HITOMI_MAX_NAME_BYTES`
///
/// The owner's value can only lower the limit, it can not raise it above what a directory takes.
pub fn max_name_bytes(dirs: &[(&Path, &str)], requested: Option<usize>) -> anyhow::Result<usize> {
    let mut limit = requested.unwrap_or(DEFAULT_MAX_FILE_NAME_BYTES);
    for (dir, what) in dirs {
        let found = probe_max_name_bytes(dir)?;
        if found < MIN_MAX_FILE_NAME_BYTES {
            bail!(
                "File names in the {what} `{}` may only be {found} bytes long, which is too short for titles. The shortest limit supported is {MIN_MAX_FILE_NAME_BYTES} bytes",
                dir.display()
            );
        }
        limit = limit.min(found);
    }
    Ok(limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hitomi-volumes-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn is_root() -> bool {
        current_user().is_some_and(|(uid, _)| uid == 0)
    }

    #[test]
    fn the_user_is_read_from_the_system() {
        let (uid, gid) = current_user().expect("Linux has /proc/self/status");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // a file made now belongs to the user the server runs as
            let dir = temp_dir();
            let meta = std::fs::metadata(&dir).unwrap();
            assert_eq!((meta.uid(), meta.gid()), (uid, gid));
            std::fs::remove_dir_all(dir).unwrap();
        }
        let _ = (uid, gid);
    }

    #[test]
    fn a_writable_directory_passes_and_stays_clean() {
        let dir = temp_dir();
        check_writable(&dir, "library", "HITOMI_LIBRARY_DIR").unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_the_mode_forbids_is_explained() {
        use std::os::unix::fs::PermissionsExt;
        if is_root() {
            return; // root writes everywhere
        }
        let dir = temp_dir();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let err = check_writable(&dir, "library", "HITOMI_LIBRARY_DIR")
            .unwrap_err()
            .to_string();
        let (uid, gid) = current_user().unwrap();
        assert!(
            err.contains(&dir.display().to_string()),
            "names the path: {err}"
        );
        assert!(
            err.contains(&format!("user {uid}:{gid}")),
            "names the user: {err}"
        );
        assert!(
            err.contains("mode 555") && err.contains("does not let it write"),
            "{err}"
        );
        assert!(
            err.contains("HITOMI_LIBRARY_DIR"),
            "names the variable: {err}"
        );
        // its owner is the user, so asking for another PUID would not help
        assert!(!err.contains("PUID"), "{err}");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_directory_of_another_user_points_at_puid_and_the_share() {
        let advice = permission_advice(Some((1000, 1000)), Some((0, 0, 0o755)));
        assert!(
            advice.contains("user 1000:1000") && advice.contains("0:0, mode 755"),
            "{advice}"
        );
        assert!(
            advice.contains("PUID") && advice.contains("PGID") && advice.contains("root squash"),
            "{advice}"
        );
        // without knowing either it still says what to try
        assert!(permission_advice(None, None).contains("PUID"));
        // the same owner is a matter of the mode
        let own = permission_advice(Some((1000, 1000)), Some((1000, 100, 0o555)));
        assert!(
            own.contains("does not let it write") && !own.contains("PUID"),
            "{own}"
        );
    }

    #[test]
    fn a_missing_directory_is_not_writable_either() {
        let dir = temp_dir().join("missing");
        assert!(check_writable(&dir, "export directory", "HITOMI_EXPORT_DIR").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_on_the_file_system_of_the_root_is_not_a_mount() {
        let dir = temp_dir();
        // the same file system as itself, which is how an unmounted directory looks
        let err = check_mounted(&dir, &dir, "library", "HITOMI_LIBRARY_DIR")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Nothing is mounted") && err.contains("create_host_path: false"),
            "{err}"
        );
        assert!(
            err.contains("HITOMI_REQUIRE_MOUNT=false") && err.contains("HITOMI_LIBRARY_DIR"),
            "{err}"
        );
        // another file system, which /proc always is
        check_mounted(&dir, Path::new("/proc"), "library", "HITOMI_LIBRARY_DIR").unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_longest_name_is_found_by_halving() {
        for limit in [255, 254, 200, 143, 100, 64, 1] {
            let mut tries = 0;
            let found = probe_with(255, |bytes| {
                tries += 1;
                Ok(bytes <= limit)
            })
            .unwrap();
            assert_eq!(found, limit);
            assert!(tries <= 9, "limit {limit} took {tries} tries");
        }
        // the usual case is one try
        let mut tries = 0;
        probe_with(255, |_| {
            tries += 1;
            Ok(true)
        })
        .unwrap();
        assert_eq!(tries, 1);
    }

    #[test]
    fn another_error_than_a_name_too_long_is_not_taken_for_a_limit() {
        let err =
            probe_with(255, |_| Err(io::Error::from(ErrorKind::PermissionDenied))).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_real_directory_takes_the_usual_255_bytes() {
        let dir = temp_dir();
        assert_eq!(probe_max_name_bytes(&dir).unwrap(), 255);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "the probe leaves nothing behind"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_owner_can_lower_the_limit_but_not_raise_it() {
        let dir = temp_dir();
        let dirs = [(dir.as_path(), "library")];
        assert_eq!(max_name_bytes(&dirs, None).unwrap(), 255);
        assert_eq!(max_name_bytes(&dirs, Some(143)).unwrap(), 143);
        assert_eq!(max_name_bytes(&dirs, Some(4000)).unwrap(), 255);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
