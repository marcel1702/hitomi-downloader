//! Keeps the paths a client sends inside the volumes of the server
//!
//! The desktop app takes the download directory of a comic as the frontend sends it. On a
//! server that would let any client name any directory: `/data/library/../../etc` passes a
//! plain prefix check, and an export would then read from there and write next to it.
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _};

/// `path` with every `..` and symlink resolved, if it lies inside `root`
///
/// Both have to exist. A symlink inside `root` that points outside of it is rejected, as
/// its target is what would actually be read.
pub fn ensure_within(path: &Path, root: &Path) -> anyhow::Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("`{}` does not exist", root.display()))?;
    let resolved = path
        .canonicalize()
        .with_context(|| format!("`{}` does not exist", path.display()))?;
    if resolved.starts_with(&root) {
        Ok(resolved)
    } else {
        Err(anyhow!(
            "`{}` lies outside of `{}`",
            path.display(),
            root.display()
        ))
    }
}

/// `path` checked to lie inside `root`, and spelled from `root` as the config names it
///
/// The core finds where a comic goes in the export directory by stripping the configured
/// library directory off the comic directory, so the result has to start with that very
/// spelling, not with the resolved one, and must not hold any `..`.
pub fn rebased_within(path: &Path, root: &Path) -> anyhow::Result<PathBuf> {
    let resolved = ensure_within(path, root)?;
    let resolved_root = root.canonicalize()?;
    let relative = resolved
        .strip_prefix(&resolved_root)
        .context("checked to lie inside the root")?;
    Ok(root.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("hitomi-paths-{}", rand::random::<u64>()));
        std::fs::create_dir_all(root.join("library/comic")).unwrap();
        std::fs::create_dir_all(root.join("elsewhere")).unwrap();
        root
    }

    #[test]
    fn a_dir_inside_the_root_passes() {
        let root = temp_root();
        let library = root.join("library");
        assert_eq!(
            rebased_within(&library.join("comic"), &library).unwrap(),
            library.join("comic")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dot_dot_out_of_the_root_is_rejected() {
        let root = temp_root();
        let library = root.join("library");
        let escape = library.join("comic/../../elsewhere");
        // the plain prefix check the core does would let this through
        assert!(escape.starts_with(&library));
        assert!(ensure_within(&escape, &library).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dot_dot_that_stays_inside_is_spelled_without_it() {
        let root = temp_root();
        let library = root.join("library");
        let detour = library.join("comic/../comic");
        assert_eq!(
            rebased_within(&detour, &library).unwrap(),
            library.join("comic")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_pointing_out_of_the_root_is_rejected() {
        let root = temp_root();
        let library = root.join("library");
        std::os::unix::fs::symlink(root.join("elsewhere"), library.join("link")).unwrap();
        assert!(ensure_within(&library.join("link"), &library).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn other_absolute_paths_and_missing_dirs_are_rejected() {
        let root = temp_root();
        let library = root.join("library");
        assert!(ensure_within(&root.join("elsewhere"), &library).is_err());
        assert!(ensure_within(Path::new("/"), &library).is_err());
        assert!(ensure_within(&library.join("missing"), &library).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
