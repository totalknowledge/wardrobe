use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum TransactionWalOperation {
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
    DeleteByFilter {
        drawer_name: String,
        filter: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drawer_namespace: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum TransactionWalRecord {
    Begin {
        tx_id: String,
        operation: TransactionWalOperation,
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

pub(crate) struct TransactionCoordinator<'a> {
    database_core: &'a RwLock<Database>,
}

impl<'a> TransactionCoordinator<'a> {
    pub(crate) fn new(database_core: &'a RwLock<Database>) -> Self {
        Self { database_core }
    }

    fn commit<T, F>(&self, operation: TransactionWalOperation, apply: F) -> Result<T>
    where
        F: FnOnce() -> Result<T>,
    {
        let tx_id = Uuid::new_v4().simple().to_string();
        append_transaction_record(
            self.database_core,
            &TransactionWalRecord::Begin {
                tx_id: tx_id.clone(),
                operation,
                ts: now_secs(),
            },
        )?;

        let result = apply().and_then(|value| {
            self.harden_mutations()?;
            Ok(value)
        });

        if result.is_ok() {
            let _commit_entry = append_transaction_record(
                self.database_core,
                &TransactionWalRecord::Commit { tx_id },
            )?;
        } else {
            let _ = append_transaction_record(
                self.database_core,
                &TransactionWalRecord::Abort { tx_id },
            );
        }

        result
    }

    fn harden_mutations(&self) -> Result<()> {
        harden_database(self.database_core)
    }

    pub(crate) fn harden_writer(writer: &mut DatabaseWriter) -> Result<()> {
        let file = writer.file_handle_mut();
        file.flush()?;
        file.flush()
    }
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
    let operation = TransactionWalOperation::Upsert {
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
    let operation = TransactionWalOperation::BulkUpsert {
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
    let operation = TransactionWalOperation::DeleteById {
        pointer: pointer.to_string(),
        drawer_namespace: context.drawer_namespace.map(str::to_string),
    };
    run_transaction(database_core, operation, apply)
}

pub(crate) fn run_delete_by_filter_transaction<T, F>(
    database_core: &RwLock<Database>,
    drawer_name: &str,
    filter: &Value,
    context: ExecutionContext<'_>,
    apply: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let operation = TransactionWalOperation::DeleteByFilter {
        drawer_name: drawer_name.to_string(),
        filter: filter.clone(),
        drawer_namespace: context.drawer_namespace.map(str::to_string),
    };
    run_transaction(database_core, operation, apply)
}

fn run_transaction<T, F>(
    database_core: &RwLock<Database>,
    operation: TransactionWalOperation,
    apply: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    TransactionCoordinator::new(database_core).commit(operation, apply)
}

fn append_transaction_record(
    database_core: &RwLock<Database>,
    record: &TransactionWalRecord,
) -> Result<WalEntry> {
    let journal = journal_for_database(database_core)?;
    let serialized = serde_json::to_vec(record)?;
    let entry = journal.append(transaction_record_kind(record), "transaction", &serialized)?;
    let bytes_written = entry.to_bytes()?.len() as u64;
    {
        let db = read_lock(database_core)?;
        db.record_wal_activity(bytes_written, 1);
    }
    check_wal_thresholds(database_core)?;
    Ok(entry)
}

fn transaction_record_kind(record: &TransactionWalRecord) -> WalOperation {
    match record {
        TransactionWalRecord::Begin { operation, .. } => match operation {
            TransactionWalOperation::Upsert { .. } | TransactionWalOperation::BulkUpsert { .. } => {
                WalOperation::Upsert
            }
            TransactionWalOperation::DeleteById { .. }
            | TransactionWalOperation::DeleteByFilter { .. } => WalOperation::Delete,
        },
        TransactionWalRecord::Commit { .. } | TransactionWalRecord::Abort { .. } => {
            WalOperation::Maintenance
        }
    }
}

fn check_wal_thresholds(database_core: &RwLock<Database>) -> Result<()> {
    let (bytes, ops) = read_lock(database_core)?.get_wal_counters();
    let (threshold_bytes, threshold_ops) = read_lock(database_core)?.wal_thresholds();
    if bytes >= threshold_bytes || ops >= threshold_ops {
        flush_checkpoint(database_core)?;
    }
    Ok(())
}

fn harden_database(database_core: &RwLock<Database>) -> Result<()> {
    let mutated_drawers = {
        let db = read_lock(database_core)?;
        db.take_mutated_drawers()
    };
    for name in mutated_drawers {
        if let Some(drawer) = read_lock(database_core)?.get_drawer(&name) {
            let mut guard = write_lock(&drawer)?;
            guard.commit()?;
        }
    }

    Ok(())
}

pub(super) fn flush_checkpoint(database_core: &RwLock<Database>) -> Result<()> {
    let wal_path = wal_path(database_core)?;
    if let Some(parent) = wal_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let wal_handle = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&wal_path)?;
    wal_handle.sync_all()?;

    let last_sequence = WalJournal::at_database_path(database_path(database_core)?)
        .verify()?
        .last_sequence
        .unwrap_or(0);

    let drawers = read_lock(database_core)?.get_all_drawers();
    for (_name, drawer) in drawers {
        let mut guard = write_lock(&drawer)?;
        guard.checkpoint()?;
    }

    let checkpoint_path = wal_checkpoint_path(database_core)?;
    let checkpoint_body = serde_json::json!({"last_checkpoint": now_secs(), "last_checkpoint_sequence": last_sequence});
    let serialized = serde_json::to_vec(&checkpoint_body)?;
    fs::write(&checkpoint_path, &serialized)?;
    let meta_f = OpenOptions::new().write(true).open(&checkpoint_path)?;
    meta_f.sync_all()?;

    wal_handle.set_len(0)?;
    wal_handle.sync_all()?;

    read_lock(database_core)?.reset_wal_counters();

    Ok(())
}

pub(super) fn journal_for_database(database_core: &RwLock<Database>) -> Result<WalJournal> {
    let db = read_lock(database_core)?;
    Ok(db.wal_journal.clone())
}

pub(super) fn checkpoint_sequence(database_core: &RwLock<Database>) -> Result<u64> {
    let checkpoint_path = wal_checkpoint_path(database_core)?;
    if !checkpoint_path.exists() {
        return Ok(0);
    }

    let contents = fs::read_to_string(checkpoint_path)?;
    let value = serde_json::from_str::<Value>(&contents).unwrap_or(Value::Null);
    Ok(value
        .get("last_checkpoint_sequence")
        .and_then(Value::as_u64)
        .unwrap_or(0))
}

fn wal_path(database_core: &RwLock<Database>) -> Result<PathBuf> {
    Ok(database_path(database_core)?.join(WAL_FILE_NAME))
}

fn wal_checkpoint_path(database_core: &RwLock<Database>) -> Result<PathBuf> {
    Ok(database_path(database_core)?.join(".wal.meta"))
}

fn database_path(database_core: &RwLock<Database>) -> Result<PathBuf> {
    Ok(read_lock(database_core)?.storage_directory_path())
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
