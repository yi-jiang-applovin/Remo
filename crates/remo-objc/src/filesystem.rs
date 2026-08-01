//! Sandbox filesystem browsing/reading/deleting — generic to any app, not
//! something an app has to register a capability for itself.
//!
//! Listing/reading/deleting a path is pure `std::fs` — the process is
//! already confined to its own sandbox by the OS itself (the same reason
//! `__view_tree`/`__screenshot` never needed extra scoping logic either),
//! so there is nothing this layer needs to enforce on top of that; it just
//! needs to be fully portable so it can be exercised directly in tests on
//! any OS, not only against a real iOS sandbox. The one genuinely
//! Apple-specific piece is resolving the sandbox's home directory
//! (`NSHomeDirectory()`), used to make a relative path resolve against the
//! app's own sandbox rather than whatever directory the host process
//! happens to have as its OS-level working directory — gated on
//! `target_vendor = "apple"` alone (Foundation, not UIKit), matching
//! `user_defaults.rs`'s reasoning for why that's real and testable on a bare
//! macOS dev machine too.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FileEntry {
    pub name: String,
    pub is_directory: bool,
    /// Byte size for files; `0` for directories (matching `std::fs`'s own
    /// metadata, which doesn't report a directory's recursive size either).
    pub size: u64,
    pub modified_unix_secs: Option<i64>,
}

#[cfg(target_vendor = "apple")]
fn apple_home_directory() -> String {
    objc2_foundation::NSHomeDirectory().to_string()
}

#[cfg(not(target_vendor = "apple"))]
fn apple_home_directory() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

/// The sandbox's home directory — what a relative path in the capabilities
/// below resolves against.
pub fn home_directory() -> String {
    apple_home_directory()
}

/// Resolves `path` against the sandbox home directory if it's relative;
/// returns it unchanged if already absolute.
pub fn resolve(path: &str) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        Path::new(&home_directory()).join(candidate)
    }
}

fn modified_unix_secs(metadata: &std::fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// Lists the immediate contents of `path` (resolved via [`resolve`]),
/// sorted by name. Not recursive — matches `ls`, not `find`.
pub fn list_directory(path: &str) -> Result<Vec<FileEntry>, String> {
    let resolved = resolve(path);
    let read_dir = std::fs::read_dir(&resolved)
        .map_err(|e| format!("failed to read directory {}: {e}", resolved.display()))?;

    let mut entries = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|e| format!("failed to read directory entry: {e}"))?;
        let metadata = entry
            .metadata()
            .map_err(|e| format!("failed to stat {}: {e}", entry.path().display()))?;
        entries.push(FileEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_directory: metadata.is_dir(),
            size: if metadata.is_dir() { 0 } else { metadata.len() },
            modified_unix_secs: modified_unix_secs(&metadata),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// Reads the entire contents of the file at `path` (resolved via
/// [`resolve`]).
pub fn read_file(path: &str) -> Result<Vec<u8>, String> {
    let resolved = resolve(path);
    std::fs::read(&resolved).map_err(|e| format!("failed to read {}: {e}", resolved.display()))
}

/// Deletes the file or directory at `path` (resolved via [`resolve`]).
/// Directories are removed recursively — there is no "are you sure" here;
/// the caller (a human at a Console prompt, or an agent) already decided.
pub fn delete_path(path: &str) -> Result<(), String> {
    let resolved = resolve(path);
    let metadata = std::fs::symlink_metadata(&resolved)
        .map_err(|e| format!("failed to stat {}: {e}", resolved.display()))?;
    if metadata.is_dir() {
        std::fs::remove_dir_all(&resolved)
            .map_err(|e| format!("failed to remove directory {}: {e}", resolved.display()))
    } else {
        std::fs::remove_file(&resolved)
            .map_err(|e| format!("failed to remove file {}: {e}", resolved.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_read_and_delete_round_trip() {
        let dir = std::env::temp_dir().join(format!("remo-objc-fs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("hello.txt");
        std::fs::write(&file_path, b"hello world").unwrap();

        let entries = list_directory(dir.to_str().unwrap()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "hello.txt");
        assert!(!entries[0].is_directory);
        assert_eq!(entries[0].size, 11);
        assert!(entries[0].modified_unix_secs.is_some());

        let contents = read_file(file_path.to_str().unwrap()).unwrap();
        assert_eq!(contents, b"hello world");

        delete_path(file_path.to_str().unwrap()).unwrap();
        assert!(!file_path.exists());

        delete_path(dir.to_str().unwrap()).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn list_directory_on_a_missing_path_is_a_clear_error_not_a_panic() {
        let missing = std::env::temp_dir().join("remo-objc-fs-test-definitely-missing");
        let result = list_directory(missing.to_str().unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn delete_recursively_removes_a_nonempty_directory() {
        let dir = std::env::temp_dir().join(format!(
            "remo-objc-fs-test-recursive-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested").join("file.txt"), b"x").unwrap();

        delete_path(dir.to_str().unwrap()).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn relative_path_resolves_against_home_directory() {
        let resolved = resolve("Documents/foo.txt");
        assert!(resolved.starts_with(home_directory()));
    }

    #[test]
    fn absolute_path_is_unchanged() {
        assert_eq!(resolve("/tmp/foo.txt"), Path::new("/tmp/foo.txt"));
    }
}
