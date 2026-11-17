//! Writing a file so that it is whole or untouched, never half written.
use std::io::Write;
use std::path::Path;

/// Writes `text` to `path` through a temporary file beside it, then renames that over it: a full
/// disk or a crash leaves the old file, not a truncated one. The file keeps its permissions, and
/// a symlink is followed, so the link stays a link.
pub(crate) fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path)?;
    let name = target
        .file_name()
        .ok_or_else(|| std::io::Error::other("not a file"))?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(".po-save");
    let temp = target.with_file_name(temp_name);
    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.set_permissions(std::fs::metadata(&target)?.permissions())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &target)
    })();
    if written.is_err() {
        drop(std::fs::remove_file(&temp));
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own, removed when dropped.
    struct Dir(std::path::PathBuf);

    impl Dir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                drop(std::fs::set_permissions(
                    &self.0,
                    std::fs::Permissions::from_mode(0o755),
                ));
            }
            drop(std::fs::remove_dir_all(&self.0));
        }
    }

    fn dir() -> Dir {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "po-write-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Dir(path)
    }

    #[test]
    fn it_replaces_the_text_and_leaves_no_temporary_file() {
        let dir = dir();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "old text that is longer").unwrap();
        write_atomic(&file, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = dir();
        let file = dir.path().join("run.sh");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o751)).unwrap();
        write_atomic(&file, "#!/bin/sh\necho hi\n").unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o751);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_stays_a_link_and_its_target_changes() {
        let dir = dir();
        let real = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&real, "one").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_atomic(&link, "two").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "two");
    }

    #[test]
    fn a_missing_file_is_an_error_and_makes_nothing() {
        let dir = dir();
        assert!(write_atomic(&dir.path().join("gone.txt"), "x").is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_write_leaves_the_old_file_and_no_temporary_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = dir();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "keep me").unwrap();
        // A read-only directory refuses the temporary file.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = write_atomic(&file, "lost");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root ignores directory permissions, so only a refusal is held to leave things as they were.
        if result.is_err() {
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep me");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }
}
