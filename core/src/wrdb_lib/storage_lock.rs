use std::fs::{self, File, OpenOptions};
use std::io::{Error, ErrorKind, Result, Write};
use std::path::{Path, PathBuf};

pub(crate) const STORAGE_ROOT_LOCK_FILE_NAME: &str = ".wardrobe-storage.lock";

#[derive(Debug)]
pub(crate) struct StorageRootLockGuard {
    path: PathBuf,
    _file: File,
}

impl Drop for StorageRootLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn acquire_server_lock(root_directory: &Path) -> Result<StorageRootLockGuard> {
    acquire_lock(root_directory, "wardrobe-server")
}

pub(crate) fn acquire_local_admin_lock(root_directory: &Path) -> Result<StorageRootLockGuard> {
    acquire_lock(root_directory, "wardrobe-local-admin")
}

fn acquire_lock(root_directory: &Path, owner: &str) -> Result<StorageRootLockGuard> {
    fs::create_dir_all(root_directory)?;
    let path = root_directory.join(STORAGE_ROOT_LOCK_FILE_NAME);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            if error.kind() == ErrorKind::AlreadyExists {
                Error::new(
                    ErrorKind::WouldBlock,
                    format!(
                        "Wardrobe storage root is locked at {}; use the running Wardrobe server socket for user management or stop the daemon before writing the local authorization ledger",
                        path.display()
                    ),
                )
            } else {
                error
            }
        })?;

    if let Err(error) = writeln!(file, "{owner}") {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    if let Err(error) = file.flush() {
        let _ = fs::remove_file(&path);
        return Err(error);
    }

    Ok(StorageRootLockGuard { path, _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("wardrobe_storage_lock_{name}_{nanos}"))
    }

    #[test]
    fn storage_root_lock_blocks_competing_local_admin_access() {
        let root = temp_root("contention");
        let server_lock = acquire_server_lock(&root).expect("server should lock storage root");

        let blocked =
            acquire_local_admin_lock(&root).expect_err("local admin should be blocked by server");
        assert_eq!(blocked.kind(), ErrorKind::WouldBlock);
        assert!(
            blocked
                .to_string()
                .contains("Wardrobe storage root is locked")
        );

        drop(server_lock);
        acquire_local_admin_lock(&root).expect("local admin should lock after server releases");

        let _ = fs::remove_dir_all(root);
    }
}
