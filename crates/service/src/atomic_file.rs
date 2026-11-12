//! Crash-safe replacement of small durable files such as JSON manifests.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};

/// Replaces `path` with `bytes` so that a crash or power loss leaves either the
/// previous complete file or the new complete file, never a truncated one.
///
/// The bytes are written to a uniquely named temporary file in the same
/// directory, flushed with `sync_all`, renamed over the destination, and the
/// parent directory is synced where the platform supports it so the rename
/// itself is durable. The temporary file is removed if any step before the
/// rename fails.
///
/// Once the rename succeeds the new contents are the visible file, so an error
/// is never returned after that point: a caller that kept its previous
/// in-memory state on error would otherwise disagree with what a restart loads.
/// The directory sync is therefore best effort.
pub fn write_file_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = parent_directory(path);
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no file name", path.display()),
        )
    })?;
    let mut temporary_name = file_name.to_os_string();
    temporary_name.push(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    let temporary = parent.join(temporary_name);

    let result = write_and_sync(&temporary, bytes).and_then(|()| fs::rename(&temporary, path));
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    let _ = sync_directory(parent);
    Ok(())
}

fn parent_directory(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    fs::File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> io::Result<()> {
    // Windows cannot open a directory handle through std; NTFS journals the
    // rename metadata, and the file contents were already flushed above.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn entries(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn creates_and_replaces_the_destination_without_leaving_temporary_files() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("jobs.json");

        write_file_atomic(&path, b"{\"first\":true}").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{\"first\":true}");

        write_file_atomic(&path, b"{}").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{}");
        assert_eq!(entries(temp.path()), vec!["jobs.json".to_string()]);
    }

    #[test]
    fn failed_replacement_keeps_the_previous_file_and_removes_the_temporary_file() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("devices.json");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"previous").unwrap();

        write_file_atomic(&path, b"{}").expect_err("renaming over a directory must fail");

        assert!(path.is_dir());
        assert_eq!(fs::read(path.join("keep")).unwrap(), b"previous");
        assert_eq!(entries(temp.path()), vec!["devices.json".to_string()]);
    }

    #[test]
    fn rejects_a_path_without_a_file_name() {
        let error = write_file_atomic(Path::new("/"), b"{}").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
