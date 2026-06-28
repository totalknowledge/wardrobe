use std::fs::{self, File, OpenOptions};
use std::io::{Error, ErrorKind, Result, Write};
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::{Duration, SystemTime};

pub(crate) const STORAGE_ROOT_LOCK_FILE_NAME: &str = ".wardrobe-storage.lock";
const SERVER_OWNER: &str = "wardrobe-server";
const LOCAL_ADMIN_OWNER: &str = "wardrobe-local-admin";

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
    acquire_lock(root_directory, SERVER_OWNER)
}

pub(crate) fn acquire_local_admin_lock(root_directory: &Path) -> Result<StorageRootLockGuard> {
    acquire_lock(root_directory, LOCAL_ADMIN_OWNER)
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
                return Err(storage_locked_error(&path, owner));
            }
            Err(error) => return Err(error),
        }
    }
}

fn write_lock_owner(file: &mut File, owner: &str) -> Result<()> {
    writeln!(file, "{owner}")?;
    writeln!(file, "pid={}", std::process::id())?;
    #[cfg(target_os = "linux")]
    {
        if let Some(boot_id) = current_boot_id() {
            writeln!(file, "boot_id={boot_id}")?;
        }
        if let Some(start_time_ticks) = process_start_time_ticks(std::process::id()) {
            writeln!(file, "start_time_ticks={start_time_ticks}")?;
        }
    }
    file.flush()
}

#[cfg(target_os = "linux")]
fn should_reclaim_existing_lock(path: &Path, requester: &str) -> Result<bool> {
    let contents = fs::read_to_string(path).unwrap_or_default();
    let existing_owner = parse_lock_owner(&contents);
    if let Some(pid) = parse_lock_pid(&contents) {
        if !Path::new("/proc").join(pid.to_string()).exists() {
            return Ok(true);
        }

        if let Some(stored_boot_id) = parse_lock_value(&contents, "boot_id") {
            if current_boot_id().is_some_and(|current_boot_id| current_boot_id != stored_boot_id) {
                return Ok(true);
            }
        }

        if let Some(stored_start_time_ticks) = parse_lock_value(&contents, "start_time_ticks")
            .and_then(|value| value.parse::<u64>().ok())
        {
            return Ok(
                process_start_time_ticks(pid).is_some_and(|actual_start_time_ticks| {
                    actual_start_time_ticks != stored_start_time_ticks
                }),
            );
        }

        return Ok(legacy_server_marker_can_be_reclaimed(
            requester,
            existing_owner.as_deref(),
            pid,
        ));
    }

    Ok(requester == SERVER_OWNER
        && existing_owner
            .as_deref()
            .is_none_or(|owner| owner == SERVER_OWNER)
        && lock_marker_age(path)?.is_some_and(|age| age >= Duration::from_secs(2)))
}

#[cfg(not(target_os = "linux"))]
fn should_reclaim_existing_lock(_path: &Path, _requester: &str) -> Result<bool> {
    Ok(false)
}

#[cfg(target_os = "linux")]
fn parse_lock_pid(contents: &str) -> Option<u32> {
    parse_lock_value(contents, "pid").and_then(|pid| pid.parse::<u32>().ok().filter(|pid| *pid > 0))
}

#[cfg(target_os = "linux")]
fn parse_lock_owner(contents: &str) -> Option<String> {
    contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.contains('='))
        .map(ToOwned::to_owned)
}

#[cfg(target_os = "linux")]
fn parse_lock_value(contents: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    contents.lines().find_map(|line| {
        line.trim()
            .strip_prefix(&prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

#[cfg(target_os = "linux")]
fn lock_marker_age(path: &Path) -> Result<Option<Duration>> {
    let modified = fs::metadata(path)?.modified()?;
    Ok(SystemTime::now().duration_since(modified).ok())
}

#[cfg(target_os = "linux")]
fn legacy_server_marker_can_be_reclaimed(
    requester: &str,
    existing_owner: Option<&str>,
    pid: u32,
) -> bool {
    if requester != SERVER_OWNER || existing_owner != Some(SERVER_OWNER) {
        return false;
    }

    // A pid-only marker cannot distinguish a stale container PID from the current
    // process after a restart. If this process has the same PID, the marker
    // predates this acquisition attempt and is therefore stale.
    pid == std::process::id() || !process_cmdline_matches_owner(pid, SERVER_OWNER)
}

#[cfg(target_os = "linux")]
fn process_cmdline_matches_owner(pid: u32, owner: &str) -> bool {
    let Ok(cmdline) = fs::read(Path::new("/proc").join(pid.to_string()).join("cmdline")) else {
        return false;
    };
    let owner = owner.to_ascii_lowercase();
    cmdline
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .filter_map(|part| std::str::from_utf8(part).ok())
        .map(|part| part.replace('\\', "/").to_ascii_lowercase())
        .any(|part| {
            part.rsplit('/')
                .next()
                .is_some_and(|name| name.contains(&owner))
        })
}

#[cfg(target_os = "linux")]
fn current_boot_id() -> Option<String> {
    fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(target_os = "linux")]
fn process_start_time_ticks(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(Path::new("/proc").join(pid.to_string()).join("stat")).ok()?;
    let (_, after_command) = stat.rsplit_once(") ")?;
    after_command
        .split_whitespace()
        .nth(19)
        .and_then(|field| field.parse::<u64>().ok())
}

fn storage_locked_error(path: &Path, requester: &str) -> Error {
    let message = if requester == SERVER_OWNER {
        format!(
            "Wardrobe storage root is locked at {}; another Wardrobe process appears to own this storage root. Stop the existing process or remove the stale lock file after verifying no daemon is running",
            path.display()
        )
    } else {
        format!(
            "Wardrobe storage root is locked at {}; use the running Wardrobe server socket for user management or stop the daemon before writing the local authorization ledger",
            path.display()
        )
    };
    Error::new(ErrorKind::WouldBlock, message)
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

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "linux")]
    #[test]
    fn storage_root_lock_reclaims_legacy_server_marker_for_reused_current_pid() {
        let root = temp_root("legacy_reused_pid");
        fs::create_dir_all(&root).expect("root should create");
        fs::write(
            root.join(STORAGE_ROOT_LOCK_FILE_NAME),
            format!("{SERVER_OWNER}\npid={}\n", std::process::id()),
        )
        .expect("legacy marker should write");

        let _server_lock =
            acquire_server_lock(&root).expect("server should reclaim legacy reused-pid marker");

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn storage_root_lock_keeps_live_marker_with_matching_process_identity() {
        if process_start_time_ticks(std::process::id()).is_none() {
            return;
        }

        let root = temp_root("live_identity");
        fs::create_dir_all(&root).expect("root should create");
        let lock_path = root.join(STORAGE_ROOT_LOCK_FILE_NAME);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .expect("lock marker should create");
        write_lock_owner(&mut file, SERVER_OWNER).expect("lock marker identity should write");
        drop(file);

        let blocked = acquire_server_lock(&root).expect_err("matching live marker should block");
        assert_eq!(blocked.kind(), ErrorKind::WouldBlock);
        assert!(blocked.to_string().contains("another Wardrobe process"));

        let _ = fs::remove_dir_all(root);
    }
}
