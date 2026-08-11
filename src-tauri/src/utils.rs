use std::sync::OnceLock;
use tauri::AppHandle;

/// Maximum length of a single path component on the common filesystems
///
/// ext4, APFS and NTFS all stop at 255, and they count bytes rather than characters,
/// so a title outside ASCII runs into the limit far earlier than its character count
/// suggests: 250 Japanese characters are 750 bytes.
const MAX_FILE_NAME_BYTES: usize = 255;

/// Room kept free for the affixes the app puts on a comic directory name
///
/// The temporary download directory is the name prefixed with `.downloading-`
/// (13 bytes) and the archive inside the download directory is the name suffixed
/// with `.cbz.tmp` (8 bytes) while it is being written. The longer of the two
/// decides how much has to stay free.
const RESERVED_AFFIX_BYTES: usize = ".downloading-".len();

/// Byte budget for a directory name built from `dir_fmt`
const MAX_DIR_NAME_BYTES: usize = MAX_FILE_NAME_BYTES - RESERVED_AFFIX_BYTES;

/// Byte budget for the title inside a directory name
///
/// Kept well below [`MAX_DIR_NAME_BYTES`] so that whatever the format string puts
/// after the title, above all the id of the comic, still fits. Without that the
/// truncation would cut off exactly the part that tells two comics apart.
const MAX_TITLE_BYTES: usize = 180;

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
    let truncated = truncate_to_bytes(title, MAX_TITLE_BYTES);
    if truncated.len() == title.len() {
        return title.to_string();
    }
    format!("{}...", truncated.trim_end())
}

pub fn filename_filter(s: &str) -> String {
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

    truncate_to_bytes(filtered.trim(), MAX_DIR_NAME_BYTES)
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
        .trim_end_matches('.')
        .trim()
        .to_string()
}

pub static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

pub fn get_app_handle() -> AppHandle {
    APP_HANDLE
        .get()
        .expect("APP_HANDLE not initialized")
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name the app derives from a directory name has to fit into one path component
    fn assert_fits(dir_name: &str) {
        let derived = [
            dir_name.to_string(),
            format!(".downloading-{dir_name}"),
            format!("{dir_name}.cbz"),
            format!("{dir_name}.cbz.tmp"),
        ];
        for name in derived {
            assert!(
                name.len() <= MAX_FILE_NAME_BYTES,
                "`{}` is {} bytes, over the {MAX_FILE_NAME_BYTES} byte limit",
                &name[..40.min(name.len())],
                name.len()
            );
        }
    }

    #[test]
    fn long_names_fit_in_a_path_component() {
        for title in [
            "a".repeat(300),
            "日".repeat(300),
            "é".repeat(300),
            "🎌".repeat(300),
        ] {
            let dir_name = filename_filter(&format!("{} - {}", truncate_title(&title), 1_234_567));
            assert_fits(&dir_name);
        }
    }

    #[test]
    fn truncation_keeps_the_id() {
        let title = "日".repeat(300);
        let dir_name = filename_filter(&format!("{} - {}", truncate_title(&title), 1_234_567));
        assert!(dir_name.ends_with(" - 1234567"), "got `{dir_name}`");
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
