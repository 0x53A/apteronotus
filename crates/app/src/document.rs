//! Native source files. Loading and saving never evaluate a document.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Debug)]
pub struct SavedDocument {
    pub path: PathBuf,
    source: String,
}

impl SavedDocument {
    pub fn open(path: &Path) -> io::Result<Self> {
        // Resolve once: changing the process directory or a symlink afterwards
        // must not redirect Save to an unrelated file.
        let path = fs::canonicalize(path)?;
        let source = read_source(&path)?;
        Ok(Self { path, source })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn is_modified(&self, source: &str) -> bool {
        self.source != source
    }

    /// Publish a new source file without replacing any existing destination.
    pub fn save_as(path: &Path, source: &str) -> io::Result<Self> {
        let path = absolute_destination(path)?;
        publish(&path, source, false)?;
        Ok(Self {
            path,
            source: source.into(),
        })
    }

    /// Replace the last explicitly opened/saved path. A changed on-disk copy
    /// is a conflict, not permission to silently overwrite somebody's edit.
    pub fn save(&mut self, source: &str) -> io::Result<()> {
        if read_source(&self.path)? != self.source {
            return Err(io::Error::other(
                "the file changed on disk; reopen it or save under a new name",
            ));
        }
        if source == self.source {
            return Ok(());
        }
        publish(&self.path, source, true)?;
        self.source.clear();
        self.source.push_str(source);
        Ok(())
    }
}

fn read_source(path: &Path) -> io::Result<String> {
    if !fs::metadata(path)?.is_file() {
        return Err(io::Error::other("choose a regular source file"));
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("choose a regular source file"));
    }
    let limit = apteronotus_lua::Limits::default().source_bytes;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other(format!(
            "source exceeds the {limit}-byte document limit"
        )));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn absolute_destination(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("choose a filename"))?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(fs::canonicalize(parent)?.join(name))
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn publish(path: &Path, source: &str, replace: bool) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    // Complete the write in the destination directory before publication.
    // Errors and short writes leave the previous file untouched.
    let (temporary, mut file) = loop {
        let candidate = path.with_file_name(format!(
            ".apteronotus-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => break (Temporary(candidate), file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    if replace {
        // Preserve ordinary permissions. Never follow a replacement symlink
        // after the conflict check and accidentally change another file.
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(io::Error::other(
                "the destination is no longer a regular file",
            ));
        }
        file.set_permissions(metadata.permissions())?;
    }
    file.write_all(source.as_bytes())?;
    file.sync_all()?;
    drop(file);
    if replace {
        fs::rename(&temporary.0, path)?;
    } else {
        // A hard-link publication is atomic and refuses an existing name.
        // Both paths are on the same filesystem by construction.
        fs::hard_link(&temporary.0, path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "apteronotus-doc-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn open_edit_and_save_preserve_unicode_and_exact_newline_bytes() {
        let dir = Directory::new();
        let path = dir.0.join("水.eod");
        let original = "-- 水\r\ntempo(96)\n";
        let saved = SavedDocument::save_as(&path, original).unwrap();
        assert!(!saved.is_modified(original));
        let mut opened = SavedDocument::open(&path).unwrap();
        assert_eq!(opened.source(), original);
        let edit = "-- 波\r\ntempo(108)\n";
        assert!(opened.is_modified(edit));
        opened.save(edit).unwrap();
        assert!(!opened.is_modified(edit));
        assert_eq!(fs::read_to_string(path).unwrap(), edit);
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn save_as_and_external_conflicts_do_not_destroy_material() {
        let dir = Directory::new();
        let path = dir.0.join("song.eod");
        let mut saved = SavedDocument::save_as(&path, "original").unwrap();
        assert!(SavedDocument::save_as(&path, "replacement").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
        fs::write(&path, "external edit").unwrap();
        assert!(saved.save("my edit").is_err());
        assert_eq!(saved.source(), "original");
        assert_eq!(fs::read_to_string(&path).unwrap(), "external edit");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn missing_invalid_and_oversized_files_are_refused() {
        let dir = Directory::new();
        assert!(SavedDocument::open(&dir.0.join("missing.eod")).is_err());
        assert!(SavedDocument::open(&dir.0).is_err());
        let path = dir.0.join("bad.eod");
        fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(SavedDocument::open(&path).is_err());
        File::create(&path)
            .unwrap()
            .set_len(apteronotus_lua::Limits::default().source_bytes as u64 + 1)
            .unwrap();
        assert!(SavedDocument::open(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn save_keeps_permissions_and_refuses_a_substituted_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = Directory::new();
        let path = dir.0.join("song.eod");
        let other = dir.0.join("other.eod");
        let mut saved = SavedDocument::save_as(&path, "original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        saved.save("edit").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::write(&other, "edit").unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&other, &path).unwrap();
        assert!(saved.save("another edit").is_err());
        assert_eq!(fs::read_to_string(other).unwrap(), "edit");
    }
}
