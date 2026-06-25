use crate::wrdb_lib::command::Command;
use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::routing::ExecutionContext;
use crate::wrdb_lib::wal::{WalJournal, WalOperation as DurableWalOperation, WalVerification};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Error, ErrorKind, Result, Write};
use std::path::{Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WalOperation {
    Upsert {
        drawer_name: String,
        payload: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drawer_namespace: Option<String>,
    },
    BulkUpsert {
        drawer_name: String,
        records: Vec<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drawer_namespace: Option<String>,
    },
    DeleteById {
        pointer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drawer_namespace: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum WalRecord {
    Begin {
        tx_id: String,
        operation: WalOperation,
        #[serde(default)]
        ts: u64,
    },
    Commit {
        tx_id: String,
    },
    Abort {
        tx_id: String,
    },
}

pub(crate) trait WalReplayExecutor {
    fn replay_upsert(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<()>;

    fn replay_bulk_upsert(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        records: Vec<Value>,
        context: ExecutionContext<'_>,
    ) -> Result<()>;

    fn replay_delete(
        database_core: &RwLock<Database>,
        pointer: &str,
        context: ExecutionContext<'_>,
    ) -> Result<()>;
}

pub(crate) fn verify(
    root_directory: &Path,
    database_name: Option<&str>,
) -> Result<WalVerification> {
    let database_path = match database_name {
        Some(database_name) => crate::wrdb_lib::catalog_validation::database_path_from_name(
            root_directory,
            database_name,
        )?,
        None => root_directory.to_path_buf(),
    };
    WalJournal::at_database_path(database_path).verify()
}

pub(crate) fn append_command(
    database_path: &Path,
    schema_name: Option<&str>,
    command: &Command,
) -> Result<()> {
    let Some(operation) = command_operation(command) else {
        return Ok(());
    };

    let scope = schema_name
        .map(|schema_name| format!("schema:{schema_name}"))
        .unwrap_or_else(|| "database".to_string());
    let payload = serde_json::to_vec(command).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("Failed to serialize WAL command payload: {error}"),
        )
    })?;

    WalJournal::at_database_path(database_path).append(operation, &scope, &payload)?;
    Ok(())
}

pub(crate) fn recover_database<E>(database_core: &RwLock<Database>) -> Result<()>
where
    E: WalReplayExecutor,
{
    let wal_path = wal_path(database_core)?;
    if !wal_path.exists() {
        return Ok(());
    }

    let checkpoint_path = wal_path.with_extension("wal.meta");
    let mut last_checkpoint: u64 = 0;
    let mut checkpoint_found = false;
    if checkpoint_path.exists() {
        if let Ok(contents) = fs::read_to_string(&checkpoint_path) {
            if let Ok(value) = serde_json::from_str::<Value>(&contents) {
                last_checkpoint = value
                    .get("last_checkpoint")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                checkpoint_found = true;
            }
        }
    }

    let contents = fs::read_to_string(wal_path)?;
    let mut begun_transactions = Vec::new();
    let mut closed_transactions = HashSet::new();

    for line in contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let record: WalRecord = serde_json::from_str(line).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to parse WAL record during recovery: {error}"),
            )
        })?;

        match record {
            WalRecord::Begin {
                tx_id,
                operation,
                ts,
            } => {
                if checkpoint_found && ts <= last_checkpoint {
                    continue;
                }
                begun_transactions.push((tx_id, operation));
            }
            WalRecord::Commit { tx_id } | WalRecord::Abort { tx_id } => {
                closed_transactions.insert(tx_id);
            }
        }
    }

    for (tx_id, operation) in begun_transactions {
        if closed_transactions.contains(&tx_id) {
            continue;
        }

        replay_wal_operation::<E>(database_core, &operation)?;
        append_wal_record(database_core, &WalRecord::Commit { tx_id })?;
    }

    Ok(())
}

pub(crate) fn run_upsert_transaction<T, F>(
    database_core: &RwLock<Database>,
    drawer_name: &str,
    payload: &Value,
    context: ExecutionContext<'_>,
    apply: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let operation = WalOperation::Upsert {
        drawer_name: drawer_name.to_string(),
        payload: payload.clone(),
        drawer_namespace: context.drawer_namespace.map(str::to_string),
    };
    run_transaction(database_core, operation, apply)
}

pub(crate) fn run_bulk_upsert_transaction<T, F>(
    database_core: &RwLock<Database>,
    drawer_name: &str,
    records: &[Value],
    context: ExecutionContext<'_>,
    apply: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let operation = WalOperation::BulkUpsert {
        drawer_name: drawer_name.to_string(),
        records: records.to_vec(),
        drawer_namespace: context.drawer_namespace.map(str::to_string),
    };
    run_transaction(database_core, operation, apply)
}

pub(crate) fn run_delete_transaction<T, F>(
    database_core: &RwLock<Database>,
    pointer: &str,
    context: ExecutionContext<'_>,
    apply: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let operation = WalOperation::DeleteById {
        pointer: pointer.to_string(),
        drawer_namespace: context.drawer_namespace.map(str::to_string),
    };
    run_transaction(database_core, operation, apply)
}

fn command_operation(command: &Command) -> Option<DurableWalOperation> {
    match command {
        Command::Upsert { .. } | Command::BulkUpsert { .. } => Some(DurableWalOperation::Upsert),
        Command::Delete { .. } | Command::DeleteByFilter { .. } => {
            Some(DurableWalOperation::Delete)
        }
        Command::Vacuum { .. } | Command::Migrate { .. } => Some(DurableWalOperation::Maintenance),
        Command::DefineDatabase { .. }
        | Command::DefineSchema { .. }
        | Command::DefineDrawer { .. }
        | Command::DefineTenantRoute { .. }
        | Command::ManageSchema { .. } => Some(DurableWalOperation::Define),
        _ => None,
    }
}

fn run_transaction<T, F>(
    database_core: &RwLock<Database>,
    operation: WalOperation,
    apply: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let tx_id = Uuid::new_v4().simple().to_string();
    append_wal_record(
        database_core,
        &WalRecord::Begin {
            tx_id: tx_id.clone(),
            operation,
            ts: now_secs(),
        },
    )?;

    let result = apply().and_then(|value| {
        flush_dirty_metadata(database_core)?;
        Ok(value)
    });
    if result.is_ok() {
        append_wal_record(database_core, &WalRecord::Commit { tx_id })?;
    } else {
        let _ = append_wal_record(database_core, &WalRecord::Abort { tx_id });
    }

    result
}

fn replay_wal_operation<E>(database_core: &RwLock<Database>, operation: &WalOperation) -> Result<()>
where
    E: WalReplayExecutor,
{
    match operation {
        WalOperation::Upsert {
            drawer_name,
            payload,
            drawer_namespace,
        } => {
            let context = ExecutionContext {
                drawer_namespace: drawer_namespace.as_deref(),
            };
            E::replay_upsert(database_core, drawer_name, payload.clone(), context)?;
        }
        WalOperation::BulkUpsert {
            drawer_name,
            records,
            drawer_namespace,
        } => {
            let context = ExecutionContext {
                drawer_namespace: drawer_namespace.as_deref(),
            };
            E::replay_bulk_upsert(database_core, drawer_name, records.clone(), context)?;
        }
        WalOperation::DeleteById {
            pointer,
            drawer_namespace,
        } => {
            let context = ExecutionContext {
                drawer_namespace: drawer_namespace.as_deref(),
            };
            E::replay_delete(database_core, pointer, context)?;
        }
    }

    Ok(())
}

fn append_wal_record(database_core: &RwLock<Database>, record: &WalRecord) -> Result<()> {
    let wal_path = wal_path(database_core)?;
    let wal_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(wal_path)?;
    let serialized = serde_json::to_vec(record)?;
    let mut writer = BufWriter::new(&wal_file);
    writer.write_all(&serialized)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    wal_file.sync_all()?;
    let bytes_written = serialized.len() as u64 + 1;
    {
        let db = read_lock(database_core)?;
        db.record_wal_activity(bytes_written, 1);
    }
    check_wal_thresholds(database_core)?;
    Ok(())
}

fn check_wal_thresholds(database_core: &RwLock<Database>) -> Result<()> {
    let (bytes, ops) = read_lock(database_core)?.get_wal_counters();
    let (threshold_bytes, threshold_ops) = read_lock(database_core)?.wal_thresholds();
    if bytes >= threshold_bytes || ops >= threshold_ops {
        flush_checkpoint(database_core)?;
    }
    Ok(())
}

fn flush_dirty_metadata(database_core: &RwLock<Database>) -> Result<()> {
    let drawers = read_lock(database_core)?.get_all_drawers();
    for (_name, drawer) in drawers {
        let mut guard = write_lock(&drawer)?;
        guard.flush_metadata_if_dirty()?;
    }
    Ok(())
}

fn flush_checkpoint(database_core: &RwLock<Database>) -> Result<()> {
    let wal_path = wal_path(database_core)?;
    let wal_handle = OpenOptions::new().read(true).write(true).open(&wal_path)?;
    wal_handle.sync_all()?;

    let drawers = read_lock(database_core)?.get_all_drawers();
    for (_name, drawer) in drawers {
        let mut guard = write_lock(&drawer)?;
        guard.checkpoint()?;
    }

    let checkpoint_path = wal_path.with_extension("wal.meta");
    let checkpoint_body = serde_json::json!({"last_checkpoint": now_secs()});
    let serialized = serde_json::to_vec(&checkpoint_body)?;
    fs::write(&checkpoint_path, &serialized)?;
    let meta_f = OpenOptions::new().write(true).open(&checkpoint_path)?;
    meta_f.sync_all()?;

    wal_handle.set_len(0)?;
    wal_handle.sync_all()?;

    read_lock(database_core)?.reset_wal_counters();

    Ok(())
}

fn wal_path(database_core: &RwLock<Database>) -> Result<PathBuf> {
    Ok(read_lock(database_core)?
        .storage_directory_path()
        .join("wardrobe.wal"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_lock<T>(lock: &RwLock<T>) -> Result<RwLockReadGuard<'_, T>> {
    lock.read()
        .map_err(|_| Error::other("Wardrobe WAL lock was poisoned during read"))
}

fn write_lock<T>(lock: &RwLock<T>) -> Result<RwLockWriteGuard<'_, T>> {
    lock.write()
        .map_err(|_| Error::other("Wardrobe WAL lock was poisoned during write"))
}
