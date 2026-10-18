use std::sync::atomic::{AtomicUsize, Ordering};

/// Maximum length of a single path component on the common filesystems
///
/// ext4, APFS and NTFS all stop at 255, and they count bytes rather than characters,
/// so a title outside ASCII runs into the limit far earlier than its character count
/// suggests: 250 Japanese characters are 750 bytes.
pub const DEFAULT_MAX_FILE_NAME_BYTES: usize = 255;

/// The shortest limit [`set_max_file_name_bytes`] accepts
///
/// Below that the budget for a title shrinks to a few characters, and a file system that
/// strict is not one a library can sensibly live on.
pub const MIN_MAX_FILE_NAME_BYTES: usize = 100;

/// The limit the names of the comic folders and archives are shortened to
///
/// A few file systems allow less than 255 bytes: an encrypted shared folder of a Synology
/// NAS, which is built on eCryptfs, stops at about 143. Without the real limit, a long title
/// is downloaded in full and then fails to be written, when the archive is packed.
static MAX_FILE_NAME_BYTES: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_FILE_NAME_BYTES);

/// Shorten the names of comic folders and archives to `bytes` bytes from now on
///
/// # Errors
/// If `bytes` is below [`MIN_MAX_FILE_NAME_BYTES`] or above [`DEFAULT_MAX_FILE_NAME_BYTES`].
pub fn set_max_file_name_bytes(bytes: usize) -> anyhow::Result<()> {
    if !(MIN_MAX_FILE_NAME_BYTES..=DEFAULT_MAX_FILE_NAME_BYTES).contains(&bytes) {
        anyhow::bail!(
            "a file name limit of {bytes} bytes is outside of {MIN_MAX_FILE_NAME_BYTES} to {DEFAULT_MAX_FILE_NAME_BYTES}"
        );
    }
    MAX_FILE_NAME_BYTES.store(bytes, Ordering::Relaxed);
    Ok(())
}

pub fn max_file_name_bytes() -> usize {
    MAX_FILE_NAME_BYTES.load(Ordering::Relaxed)
}

/// Room kept free for the affixes the app puts on a comic directory name
///
/// The temporary download directory is the name prefixed with `.downloading-`
/// (13 bytes) and the archive inside the download directory is the name suffixed
/// with `.cbz.tmp` (8 bytes) while it is being written. The longer of the two
/// decides how much has to stay free.
const RESERVED_AFFIX_BYTES: usize = ".downloading-".len();

/// Room kept free after the title for the rest of what a folder format puts in a name
///
/// The id and the language of the comic (` (1234567) - english` is 20 bytes), the rest is room
/// for a type. 62 bytes leave a title of 180 bytes in the 255 byte limit.
const RESERVED_AFTER_TITLE_BYTES: usize = 62;

/// Byte budget for a directory name built from `dir_fmt`, with names limited to `limit` bytes
fn dir_name_budget(limit: usize) -> usize {
    limit - RESERVED_AFFIX_BYTES
}

/// Byte budget for the title inside a directory name
///
/// Kept well below [`dir_name_budget`] so that whatever the format string puts
/// after the title, above all the id of the comic, still fits. Without that the
/// truncation would cut off exactly the part that tells two comics apart.
fn title_budget(limit: usize) -> usize {
    dir_name_budget(limit) - RESERVED_AFTER_TITLE_BYTES
}

/// Truncate `s` to at most `max_bytes` bytes, never splitting a character
pub fn truncate_to_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk back to the last byte index that starts a character
    let boundary = (0..=max_bytes)
        .rev()
        .find(|&index| s.is_char_boundary(index))
        .unwrap_or(0);

    &s[..boundary]
}

/// Shorten `title` so a directory name built from it leaves room for the rest of the format
pub fn truncate_title(title: &str) -> String {
    truncate_title_to(title, max_file_name_bytes())
}

/// [`truncate_title`] for names limited to `limit` bytes
pub fn truncate_title_to(title: &str, limit: usize) -> String {
    let truncated = truncate_to_bytes(title, title_budget(limit));
    if truncated.len() == title.len() {
        return title.to_string();
    }
    format!("{}...", truncated.trim_end())
}

pub fn filename_filter(s: &str) -> String {
    filename_filter_to(s, max_file_name_bytes())
}

/// [`filename_filter`] for names limited to `limit` bytes
pub fn filename_filter_to(s: &str, limit: usize) -> String {
    let filtered = s
        .chars()
        .map(|c| match c {
            '\\' | '/' | '\n' => ' ',
            ':' => '：',
            '*' => '⭐',
            '?' => '？',
            '"' => '\'',
            '<' => '《',
            '>' => '》',
            '|' => '丨',
            _ => c,
        })
        .collect::<String>();

    truncate_to_bytes(filtered.trim(), dir_name_budget(limit))
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
        .trim_end_matches('.')
        .trim()
        .to_string()
}

/// Write `bytes` to `path`, all of it or nothing
///
/// The file is written next to it, made durable and then renamed over it, so that a crash
/// leaves the old file or the new one and never half of one.
pub fn write_atomically(path: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::io::Write as _;

    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let mut out = std::fs::File::create(&tmp)
        .with_context(|| format!("Failed to create `{}`", tmp.display()))?;
    out.write_all(bytes)
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
        let _ = crate::pages::sync_dir(dir);
    }
    Ok(())
}

/// Move a file that can not be used to `<name>.corrupt`, so that a new one can be written
/// and what was in it can still be looked at
pub fn set_aside(path: &std::path::Path, what: &str, reason: &str) {
    let mut aside = path.as_os_str().to_owned();
    aside.push(".corrupt");
    let aside = std::path::PathBuf::from(aside);
    match std::fs::rename(path, &aside) {
        Ok(()) => tracing::error!(
            "{what} `{}` can not be used ({reason}), it was moved to `{}` and starts empty",
            path.display(),
            aside.display()
        ),
        Err(err) => tracing::error!(
            "{what} `{}` can not be used ({reason}) and could not be moved away: {err}",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The limits a file system may have: ext4, an encrypted Synology folder, the shortest accepted
    const LIMITS: [usize; 3] = [DEFAULT_MAX_FILE_NAME_BYTES, 143, MIN_MAX_FILE_NAME_BYTES];

    /// Every name the app derives from a directory name has to fit into one path component
    fn assert_fits(dir_name: &str, limit: usize) {
        let derived = [
            dir_name.to_string(),
            format!(".downloading-{dir_name}"),
            format!("{dir_name}.cbz"),
            format!("{dir_name}.cbz.tmp"),
        ];
        for name in derived {
            assert!(
                name.len() <= limit,
                "`{}` is {} bytes, over the {limit} byte limit",
                &name[..40.min(name.len())],
                name.len()
            );
        }
    }

    fn dir_name_of(title: &str, limit: usize) -> String {
        filename_filter_to(
            &format!("{} - {}", truncate_title_to(title, limit), 1_234_567),
            limit,
        )
    }

    #[test]
    fn long_names_fit_in_a_path_component() {
        for limit in LIMITS {
            for title in [
                "a".repeat(300),
                "日".repeat(300),
                "é".repeat(300),
                "🎌".repeat(300),
            ] {
                assert_fits(&dir_name_of(&title, limit), limit);
            }
        }
    }

    #[test]
    fn truncation_keeps_the_id() {
        for limit in LIMITS {
            let dir_name = dir_name_of(&"日".repeat(300), limit);
            assert!(
                dir_name.ends_with(" - 1234567"),
                "{limit}: got `{dir_name}`"
            );
        }
    }

    #[test]
    fn a_shorter_limit_leaves_a_shorter_title() {
        let title = "a".repeat(300);
        let lengths: Vec<usize> = LIMITS
            .iter()
            .map(|limit| dir_name_of(&title, *limit).len())
            .collect();
        assert!(
            lengths[0] > lengths[1] && lengths[1] > lengths[2],
            "{lengths:?}"
        );
        // the default budget is what it always was: 180 bytes of title
        assert_eq!(
            truncate_title_to(&title, DEFAULT_MAX_FILE_NAME_BYTES).len(),
            183
        );
    }

    #[test]
    fn the_limit_is_set_for_the_process_and_checked() {
        // the only test that touches the global, and it puts the value back
        let before = max_file_name_bytes();
        assert!(set_max_file_name_bytes(MIN_MAX_FILE_NAME_BYTES - 1).is_err());
        assert!(set_max_file_name_bytes(DEFAULT_MAX_FILE_NAME_BYTES + 1).is_err());
        assert_eq!(max_file_name_bytes(), before);
        set_max_file_name_bytes(before).unwrap();
        assert_eq!(max_file_name_bytes(), before);
    }

    #[test]
    fn truncation_never_splits_a_character() {
        // 4 byte characters, so no multiple of 4 lines up with the budget
        let flags = "🎌".repeat(100);
        let truncated = truncate_to_bytes(&flags, 10);
        assert_eq!(truncated, "🎌🎌");
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn short_names_are_left_alone() {
        assert_eq!(
            filename_filter("Normaler Titel - 42"),
            "Normaler Titel - 42"
        );
        assert_eq!(truncate_title("Kurzer Titel"), "Kurzer Titel");
    }

    #[test]
    fn special_characters_are_replaced() {
        assert_eq!(
            filename_filter(r#"a/b\c:d*e?f"g<h>i|j"#),
            "a b c：d⭐e？f'g《h》i丨j"
        );
        assert_eq!(filename_filter("trailing dots..."), "trailing dots");
        assert_eq!(filename_filter("  padded  "), "padded");
    }
}
