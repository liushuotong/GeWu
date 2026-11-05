//! Filesystem Utilities
//!
//! This module provides high-level async wrappers around `tokio::fs` operations,
//! simplifying common tasks like reading/writing text, JSON handling, and
//! recursive file searching.

#![allow(dead_code)]
#![allow(clippy::collapsible_if)]
use anyhow::{Context, Result};
use mime_guess;
use serde::{Serialize, de::DeserializeOwned};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use tokio::fs;

pub fn into_tool_error(e: impl Into<anyhow::Error>) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.into()))
}

pub fn parse_empty_args(tool_name: &str, required_params: &[(&str, &str)]) -> String {
    let params: Vec<String> = required_params
        .iter()
        .map(|(name, type_)| format!("  - {}: {}", name, type_))
        .collect();
    format!(
        "Arguments are required for {}.\n\nRequired parameters:\n{}",
        tool_name,
        params.join("\n")
    )
}

/// Normalizes a path by resolving `.` and `..` components lexically.
/// This does not require the path to exist.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut components = path.components().peekable();
    let mut ret = if let Some(c @ Component::Prefix(..)) = components.peek().cloned() {
        components.next();
        PathBuf::from(c.as_os_str())
    } else {
        PathBuf::new()
    };

    for component in components {
        match component {
            Component::Prefix(..) => unreachable!(),
            Component::RootDir => {
                ret.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                ret.pop();
            }
            Component::Normal(c) => {
                ret.push(c);
            }
        }
    }
    ret
}

/// Resolves a user-provided path against a root directory, ensuring it stays within the root.
/// Returns the absolute path if safe, or an error if it attempts to traverse outside.
pub fn resolve_safe_path(root: impl AsRef<Path>, user_path: impl AsRef<Path>) -> Result<PathBuf> {
    let root = root.as_ref();
    let user_path = user_path.as_ref();

    // If user_path is absolute, it must still be within root.
    // If it is relative, we join it to root.
    let joined: PathBuf = if user_path.is_absolute() {
        // SECURITY: Absolute paths must start with root to prevent escaping
        // via paths like /etc/passwd or /home/user/../root
        if !user_path.starts_with(root) {
            return Err(anyhow::anyhow!(
                "Absolute path {:?} is outside of working directory {:?}",
                user_path,
                root
            ));
        }
        user_path.to_path_buf()
    } else {
        root.join(user_path)
    };

    let normalized = normalize_path(&joined);

    // Check if the normalized path starts with the root
    if normalized.starts_with(root) {
        Ok(normalized)
    } else {
        Err(anyhow::anyhow!(
            "Path traversal detected: {:?} is outside of root {:?}",
            normalized,
            root
        ))
    }
}

/// Canonicalizes a filesystem root used to constrain mutating operations.
pub async fn canonicalize_root(root: impl AsRef<Path>) -> Result<PathBuf> {
    let root = fs::canonicalize(root.as_ref())
        .await
        .with_context(|| format!("Failed to resolve working directory: {:?}", root.as_ref()))?;
    let metadata = fs::metadata(&root).await?;
    anyhow::ensure!(
        metadata.is_dir(),
        "Working directory is not a directory: {:?}",
        root
    );
    Ok(root)
}

async fn reject_symlink_components(root: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("Path {:?} is outside working directory {:?}", path, root))?;
    let mut current = root.to_path_buf();

    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current).await {
            Ok(metadata) => anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "Symbolic links are not allowed in patch paths: {:?}",
                current
            ),
            Err(error) if error.kind() == ErrorKind::NotFound => break,
            Err(error) => {
                return Err(error).with_context(|| format!("Failed to inspect {:?}", current));
            }
        }
    }

    Ok(())
}

/// Resolves an existing path and rejects traversal or symbolic-link escapes.
pub async fn resolve_safe_existing_path(
    root: impl AsRef<Path>,
    user_path: impl AsRef<Path>,
) -> Result<PathBuf> {
    let root = canonicalize_root(root).await?;
    let path = resolve_safe_path(&root, user_path)?;
    reject_symlink_components(&root, &path).await?;
    let canonical = fs::canonicalize(&path)
        .await
        .with_context(|| format!("Path does not exist: {:?}", path))?;
    anyhow::ensure!(
        canonical.starts_with(&root),
        "Path {:?} is outside working directory {:?}",
        canonical,
        root
    );
    Ok(canonical)
}

/// Resolves a potential destination and verifies its existing parent chain.
pub async fn resolve_safe_write_path(
    root: impl AsRef<Path>,
    user_path: impl AsRef<Path>,
) -> Result<PathBuf> {
    let root = canonicalize_root(root).await?;
    let path = resolve_safe_path(&root, user_path)?;
    reject_symlink_components(&root, &path).await?;

    let mut parent = path.parent();
    while let Some(candidate) = parent {
        match fs::canonicalize(candidate).await {
            Ok(canonical) => {
                anyhow::ensure!(
                    canonical.starts_with(&root),
                    "Path {:?} is outside working directory {:?}",
                    path,
                    root
                );
                return Ok(path);
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                parent = candidate.parent();
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to resolve parent of {:?}", path));
            }
        }
    }

    anyhow::bail!("Unable to resolve a safe parent for {:?}", path)
}

/// Checks if a path exists.
pub async fn exists(path: impl AsRef<Path>) -> bool {
    fs::metadata(path).await.is_ok()
}

/// Checks if a path exists and is a directory.
pub async fn is_dir(path: impl AsRef<Path>) -> bool {
    fs::metadata(path)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
}

/// Reads a file into a String.
pub async fn read_text(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read text file: {:?}", path))
}

/// Reads a file into a byte vector.
pub async fn read_bytes(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let path = path.as_ref();
    fs::read(path)
        .await
        .with_context(|| format!("Failed to read file: {:?}", path))
}

/// Reads a JSON file and deserializes it into type T.
pub async fn read_json<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T> {
    let path = path.as_ref();
    let content = read_text(path).await?;
    serde_json::from_str(&content).with_context(|| format!("Failed to parse JSON from: {:?}", path))
}

/// Writes byte content to a file, creating parent directories if needed.
pub async fn write(path: impl AsRef<Path>, content: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !exists(parent).await {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directory: {:?}", parent))?;
        }
    }
    fs::write(path, content)
        .await
        .with_context(|| format!("Failed to write file: {:?}", path))?;
    Ok(())
}

/// Serializes data to JSON and writes it to a file.
pub async fn write_json<T: Serialize>(path: impl AsRef<Path>, data: &T) -> Result<()> {
    let content = serde_json::to_string_pretty(data)?;
    write(path, content.as_bytes()).await
}

/// Guesses the MIME type of a file based on its extension.
pub fn mime_type(path: impl AsRef<Path>) -> String {
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string()
}

/// Returns true if the file appears to be binary based on extension and content sampling.
/// Extension check first, then sample content if needed.
pub async fn is_binary_file(path: impl AsRef<Path>, file_size: u64) -> Result<bool> {
    let path = path.as_ref();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    // Binary extensions list (same as legacy)
    match ext.as_deref() {
        Some(
            ".zip" | ".tar" | ".gz" | ".exe" | ".dll" | ".so" | ".class" | ".jar" | ".war" | ".7z"
            | ".doc" | ".docx" | ".xls" | ".xlsx" | ".ppt" | ".pptx" | ".odt" | ".ods" | ".odp"
            | ".bin" | ".dat" | ".obj" | ".o" | ".a" | ".lib" | ".wasm" | ".pyc" | ".pyo",
        ) => return Ok(true),
        _ => {}
    }

    if file_size == 0 {
        return Ok(false);
    }

    let fh = tokio::fs::File::open(path).await?;
    let mut reader = tokio::io::BufReader::new(fh);
    let sample_size = std::cmp::min(4096, file_size as usize);
    let mut bytes = vec![0u8; sample_size];
    let result = tokio::io::AsyncReadExt::read(&mut reader, &mut bytes).await?;
    if result == 0 {
        return Ok(false);
    }

    let mut non_printable_count = 0;
    for i in 0..result {
        if bytes[i] == 0 {
            return Ok(true);
        }
        if bytes[i] < 9 || (bytes[i] > 13 && bytes[i] < 32) {
            non_printable_count += 1;
        }
    }

    // If >30% non-printable characters, consider it binary
    Ok(non_printable_count as f64 / result as f64 > 0.3)
}

/// Searches for a file or directory by walking up from `start_dir`.
pub async fn find_up(name: impl AsRef<Path>, start_dir: impl AsRef<Path>) -> Option<PathBuf> {
    let mut current = start_dir.as_ref().to_path_buf();
    loop {
        let target = current.join(&name);
        if exists(&target).await {
            return Some(target);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Finds files matching a glob pattern.
pub fn glob(pattern: &str) -> Result<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = glob::glob(pattern)?
        .filter_map(|entry| entry.ok())
        .collect();
    Ok(paths)
}

/// Finds files matching a glob pattern, respecting .gitignore files.
pub fn glob_with_gitignore(root: impl AsRef<Path>, pattern: &str) -> Result<Vec<PathBuf>> {
    let root = root.as_ref();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false) // Allow hidden files unless in .gitignore
        .git_ignore(true)
        .build();

    let glob_pattern = glob::Pattern::new(pattern)
        .with_context(|| format!("Invalid glob pattern: {}", pattern))?;

    let mut matches = Vec::new();

    for result in walker {
        match result {
            Ok(entry) => {
                let path = entry.path();
                if path.is_file() {
                    // Compute path relative to root to match against the pattern
                    if let Ok(relative_path) = path.strip_prefix(root) {
                        // glob::Pattern matches against the string representation
                        //We need to handle the case where the pattern assumes the root is the current directory
                        if glob_pattern.matches_path(relative_path) {
                            matches.push(path.to_path_buf());
                        }
                    }
                }
            }
            Err(_err) => {
                // We might want to log this but for now just ignore/continue
                // eprintln!("Error walking directory: {}", err);
            }
        }
    }

    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_read_write_json() -> Result<()> {
        let dir = tempdir()?;
        let file_path = dir.path().join("test.json");

        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct TestData {
            name: String,
            value: i32,
        }

        let data = TestData {
            name: "test".to_string(),
            value: 42,
        };

        write_json(&file_path, &data).await?;
        let read_data: TestData = read_json(&file_path).await?;

        assert_eq!(data, read_data);
        Ok(())
    }

    #[tokio::test]
    async fn safe_write_path_rejects_parent_traversal() -> Result<()> {
        let dir = tempdir()?;
        let result = resolve_safe_write_path(dir.path(), "../outside.txt").await;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn safe_existing_path_resolves_inside_root() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("inside.txt");
        fs::write(&path, "inside").await?;
        let resolved = resolve_safe_existing_path(dir.path(), "inside.txt").await?;
        assert_eq!(resolved, fs::canonicalize(path).await?);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn safe_write_path_rejects_symlink_components() -> Result<()> {
        use std::os::unix::fs::symlink;

        let root = tempdir()?;
        let outside = tempdir()?;
        symlink(outside.path(), root.path().join("escape"))?;
        let result = resolve_safe_write_path(root.path(), "escape/file.txt").await;
        assert!(result.is_err());
        Ok(())
    }

    // ============================================
    // Tests for resolve_safe_path security fix
    // ============================================

    #[test]
    fn resolve_safe_path_rejects_absolute_outside_root() {
        // Absolute paths outside root should be rejected
        let root = Path::new("/home/user/project");
        let result = resolve_safe_path(root, "/etc/passwd");
        assert!(
            result.is_err(),
            "Absolute path /etc/passwd should be rejected"
        );

        let result = resolve_safe_path(root, "/var/log");
        assert!(result.is_err(), "Absolute path /var/log should be rejected");

        let result = resolve_safe_path(root, "/home/user/../root");
        assert!(
            result.is_err(),
            "Path with parent traversal should be rejected"
        );
    }

    #[test]
    fn resolve_safe_path_accepts_absolute_inside_root() {
        // Absolute paths inside root should be accepted
        let root = Path::new("/home/user/project");
        let result = resolve_safe_path(root, "/home/user/project/src/main.rs");
        assert!(
            result.is_ok(),
            "Absolute path inside root should be accepted"
        );

        let result = resolve_safe_path(root, "/home/user/project/./src");
        assert!(
            result.is_ok(),
            "Absolute path with current dir should be accepted"
        );
    }

    #[test]
    fn resolve_safe_path_accepts_relative_paths() {
        // Relative paths joined to root should work
        let root = Path::new("/home/user/project");
        let result = resolve_safe_path(root, "src/main.rs");
        assert!(result.is_ok());

        let result = resolve_safe_path(root, "./src/../src/lib.rs");
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_safe_path_rejects_parent_traversal_relative() {
        // Parent traversal from relative paths should be rejected
        let root = Path::new("/home/user/project");
        let result = resolve_safe_path(root, "../outside.txt");
        assert!(result.is_err(), "Parent traversal should be rejected");

        let result = resolve_safe_path(root, "foo/../../bar");
        assert!(result.is_err(), "Deep parent traversal should be rejected");
    }

    #[test]
    fn resolve_safe_path_normalizes_dots() {
        // Verify that the function properly normalizes paths
        let root = Path::new("/home/user/project");

        // ./foo becomes /home/user/project/foo
        let result = resolve_safe_path(root, "./foo").unwrap();
        assert_eq!(result, Path::new("/home/user/project/foo"));

        // foo/./bar becomes /home/user/project/foo/bar
        let result = resolve_safe_path(root, "foo/./bar").unwrap();
        assert_eq!(result, Path::new("/home/user/project/foo/bar"));
    }

    #[test]
    fn resolve_safe_path_edge_case_root() {
        // Edge case: root is root itself
        let root = Path::new("/");

        // Absolute path outside / should be rejected
        let result = resolve_safe_path(root, "/etc");
        // Since / is root, /etc is not inside /
        // Actually, all absolute paths start with / so they're "inside" /
        // The check is: does /etc start with /? Yes it does.
        // But after normalization, we should still be inside /
        assert!(result.is_ok(), "/etc should be valid relative to /");

        // Relative path should work
        let result = resolve_safe_path(root, "home");
        assert!(result.is_ok());
    }
}
