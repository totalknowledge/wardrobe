use std::fs::{self, File, OpenOptions};
use std::io::{Error, ErrorKind, Result, Write};
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::{Duration, SystemTime};

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

    loop {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(error) = write_lock_owner(&mut file, owner) {
                    let _ = fs::remove_file(&path);
                    return Err(error);
                }
                return Ok(StorageRootLockGuard { path, _file: file });
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if should_reclaim_existing_lock(&path, owner)? {
                    match fs::remove_file(&path) {
                        Ok(()) => continue,
                        Err(remove_error) if remove_error.kind() == ErrorKind::NotFound => {
                            continue;
                        }
                        Err(remove_error) => return Err(remove_error),
                    }
                }
                return Err(storage_locked_error(&path));
            }
            Err(error) => return Err(error),
        }
    }
}

fn write_lock_owner(file: &mut File, owner: &str) -> Result<()> {
    writeln!(file, "{owner}")?;
    writeln!(file, "pid={}", std::process::id())?;
    file.flush()
}

#[cfg(target_os = "linux")]
fn should_reclaim_existing_lock(path: &Path, requester: &str) -> Result<bool> {
    let contents = fs::read_to_string(path).unwrap_or_default();
    if let Some(pid) = parse_lock_pid(&contents) {
        return Ok(!Path::new("/proc").join(pid.to_string()).exists());
    }

    Ok(requester == "wardrobe-server"
        && lock_marker_age(path)?.is_some_and(|age| age >= Duration::from_secs(2)))
}

#[cfg(not(target_os = "linux"))]
fn should_reclaim_existing_lock(_path: &Path, _requester: &str) -> Result<bool> {
    Ok(false)
}

#[cfg(target_os = "linux")]
fn parse_lock_pid(contents: &str) -> Option<u32> {
    contents.lines().find_map(|line| {
        line.strip_prefix("pid=")
            .and_then(|pid| pid.trim().parse::<u32>().ok())
            .filter(|pid| *pid > 0)
    })
}

#[cfg(target_os = "linux")]
fn lock_marker_age(path: &Path) -> Result<Option<Duration>> {
    let modified = fs::metadata(path)?.modified()?;
    Ok(SystemTime::now().duration_since(modified).ok())
}

fn storage_locked_error(path: &Path) -> Error {
    Error::new(
        ErrorKind::WouldBlock,
        format!(
            "Wardrobe storage root is locked at {}; use the running Wardrobe server socket for user management or stop the daemon before writing the local authorization ledger",
            path.display()
        ),
    )
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

    #[cfg(unix)]
    #[test]
    fn storage_root_lock_reclaims_stale_unheld_marker_file() {
        let root = temp_root("stale_marker");
        fs::create_dir_all(&root).expect("root should create");
        fs::write(
            root.join(STORAGE_ROOT_LOCK_FILE_NAME),
            "wardrobe-server\npid=4294967295\n",
        )
        .expect("stale marker should write");

        let _server_lock =
            acquire_server_lock(&root).expect("server should reclaim stale unheld marker");

        let _ = fs::remove_dir_all(root);
    }
}
