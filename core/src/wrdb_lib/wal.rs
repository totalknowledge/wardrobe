use crate::wrdb_lib::command::Command;
use crate::wrdb_lib::core::writer::DatabaseWriter;
use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::routing::ExecutionContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Condvar, Mutex, MutexGuard, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const WAL_FILE_NAME: &str = ".wal";
const WAL_MAGIC: [u8; 4] = [0x57, 0x44, 0x57, 0x4c];
const WAL_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DurabilityPolicy {
    Strict,
    Grouped {
        commit_window_ms: u64,
        max_batch_size: usize,
    },
}

impl Default for DurabilityPolicy {
    fn default() -> Self {
        Self::Strict
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalOperation {
    Upsert,
    Delete,
    Maintenance,
    Define,
}

impl WalOperation {
    fn code(self) -> u8 {
        match self {
            Self::Upsert => 1,
            Self::Delete => 2,
            Self::Maintenance => 3,
            Self::Define => 4,
        }
    }

    fn from_code(code: u8) -> Result<Self> {
        match code {
            1 => Ok(Self::Upsert),
            2 => Ok(Self::Delete),
            3 => Ok(Self::Maintenance),
            4 => Ok(Self::Define),
            _ => Err(Error::new(
                ErrorKind::InvalidData,
                format!("Unknown WAL operation code: {code}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalEntry {
    pub sequence: u64,
    pub operation: WalOperation,
    pub scope: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalVerification {
    pub path: String,
    pub entry_count: usize,
    pub last_sequence: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct WalJournal {
    path: PathBuf,
    inner: Arc<WalJournalInner>,
}

#[derive(Debug)]
struct WalJournalInner {
    path: PathBuf,
    state: Mutex<WalJournalState>,
    commit_gate: Condvar,
}

#[derive(Debug)]
struct WalJournalState {
    policy: DurabilityPolicy,
    file: Option<File>,
    next_sequence: Option<u64>,
    pending: Vec<PendingWalEntry>,
    completed: HashMap<u64, Option<String>>,
    sync_count: u64,
}

#[derive(Debug)]
struct PendingWalEntry {
    entry: WalEntry,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TransactionWalOperation {
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
enum TransactionWalRecord {
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

    fn replay_delete_by_filter(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        context: ExecutionContext<'_>,
    ) -> Result<()>;
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
            let commit_entry = append_transaction_record(
                self.database_core,
                &TransactionWalRecord::Commit { tx_id },
            )?;
            record_applied_checkpoint(self.database_core, commit_entry.sequence)?;
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

impl WalJournal {
    pub fn at_database_path(database_path: impl AsRef<Path>) -> Self {
        Self::from_database_path(database_path, None)
    }

    pub fn at_database_path_with_policy(
        database_path: impl AsRef<Path>,
        policy: DurabilityPolicy,
    ) -> Self {
        Self::from_database_path(database_path, Some(policy))
    }

    fn from_database_path(
        database_path: impl AsRef<Path>,
        policy: Option<DurabilityPolicy>,
    ) -> Self {
        let path = database_path.as_ref().join(WAL_FILE_NAME);
        let mut journals = journal_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if let Some(inner) = journals.get(&path).and_then(Weak::upgrade) {
            if let Some(policy) = policy {
                inner.set_policy(policy);
            }
            return Self { path, inner };
        }

        let inner = Arc::new(WalJournalInner {
            path: path.clone(),
            state: Mutex::new(WalJournalState {
                policy: policy.unwrap_or_default(),
                file: None,
                next_sequence: None,
                pending: Vec::new(),
                completed: HashMap::new(),
                sync_count: 0,
            }),
            commit_gate: Condvar::new(),
        });
        journals.insert(path.clone(), Arc::downgrade(&inner));
        Self { path, inner }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, operation: WalOperation, scope: &str, payload: &[u8]) -> Result<WalEntry> {
        let mut state = self.lock_state()?;
        self.initialize_locked(&mut state)?;
        let sequence = state.next_sequence.unwrap_or(1);
        state.next_sequence = Some(sequence + 1);
        let entry = WalEntry {
            sequence,
            operation,
            scope: scope.to_string(),
            payload: payload.to_vec(),
        };
        let bytes = entry.to_bytes()?;

        match state.policy.clone() {
            DurabilityPolicy::Strict => {
                write_entry_locked(&mut state, &bytes)?;
                Ok(entry)
            }
            DurabilityPolicy::Grouped {
                commit_window_ms,
                max_batch_size,
            } => {
                state.pending.push(PendingWalEntry {
                    entry: entry.clone(),
                    bytes,
                });

                let commit_window = Duration::from_millis(commit_window_ms);
                let max_batch_size = max_batch_size.max(1);
                if state.pending.len() >= max_batch_size || commit_window.is_zero() {
                    flush_pending_locked(&self.inner, &mut state)?;
                } else {
                    loop {
                        if let Some(outcome) = state.completed.remove(&entry.sequence) {
                            return wal_outcome(entry, outcome);
                        }

                        let wait_result = self
                            .inner
                            .commit_gate
                            .wait_timeout(state, commit_window)
                            .map_err(|_| Error::other("Wardrobe WAL lock was poisoned"))?;
                        state = wait_result.0;

                        if let Some(outcome) = state.completed.remove(&entry.sequence) {
                            return wal_outcome(entry, outcome);
                        }

                        if wait_result.1.timed_out() {
                            flush_pending_locked(&self.inner, &mut state)?;
                            break;
                        }
                    }
                }

                match state.completed.remove(&entry.sequence) {
                    Some(outcome) => wal_outcome(entry, outcome),
                    None => Ok(entry),
                }
            }
        }
    }

    pub fn read_entries(&self) -> Result<Vec<WalEntry>> {
        read_entries_from_path(&self.path)
    }

    pub fn verify(&self) -> Result<WalVerification> {
        let entries = self.read_entries()?;
        Ok(WalVerification {
            path: self.path.to_string_lossy().into_owned(),
            entry_count: entries.len(),
            last_sequence: entries.last().map(|entry| entry.sequence),
        })
    }

    #[cfg(test)]
    pub fn durable_sync_count(&self) -> u64 {
        self.inner
            .state
            .lock()
            .map(|state| state.sync_count)
            .unwrap_or_default()
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, WalJournalState>> {
        self.inner
            .state
            .lock()
            .map_err(|_| Error::other("Wardrobe WAL lock was poisoned"))
    }

    fn initialize_locked(&self, state: &mut WalJournalState) -> Result<()> {
        if state.next_sequence.is_none() {
            state.next_sequence = Some(
                read_entries_from_path(&self.inner.path)?
                    .last()
                    .map(|entry| entry.sequence + 1)
                    .unwrap_or(1),
            );
        }

        if state.file.is_none() {
            if let Some(parent) = self.inner.path.parent() {
                fs::create_dir_all(parent)?;
            }
            state.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .read(true)
                    .open(&self.inner.path)?,
            );
        }

        Ok(())
    }
}

impl WalJournalInner {
    fn set_policy(&self, policy: DurabilityPolicy) {
        if let Ok(mut state) = self.state.lock() {
            state.policy = policy;
            self.commit_gate.notify_all();
        }
    }
}

impl WalEntry {
    fn to_bytes(&self) -> Result<Vec<u8>> {
        let scope = self.scope.as_bytes();
        if scope.len() > u16::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "WAL scope is too large to encode",
            ));
        }
        if self.payload.len() > u32::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "WAL payload is too large to encode",
            ));
        }

        let mut bytes = Vec::with_capacity(20 + scope.len() + self.payload.len());
        bytes.extend_from_slice(&WAL_MAGIC);
        bytes.push(WAL_VERSION);
        bytes.extend_from_slice(&self.sequence.to_be_bytes());
        bytes.push(self.operation.code());
        bytes.extend_from_slice(&(scope.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(scope);
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }

    fn read_from(reader: &mut impl Read) -> Result<Option<Self>> {
        let mut magic = [0_u8; 4];
        if read_exact_or_none(reader, &mut magic)?.is_none() {
            return Ok(None);
        }

        if magic != WAL_MAGIC {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "WAL magic header is corrupt",
            ));
        }

        let mut version = [0_u8; 1];
        if read_exact_or_none(reader, &mut version)?.is_none() {
            return Ok(None);
        }
        if version[0] != WAL_VERSION {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Unsupported WAL version: {}", version[0]),
            ));
        }

        let mut sequence = [0_u8; 8];
        if read_exact_or_none(reader, &mut sequence)?.is_none() {
            return Ok(None);
        }

        let mut operation = [0_u8; 1];
        if read_exact_or_none(reader, &mut operation)?.is_none() {
            return Ok(None);
        }

        let mut scope_len = [0_u8; 2];
        if read_exact_or_none(reader, &mut scope_len)?.is_none() {
            return Ok(None);
        }
        let scope_len = u16::from_be_bytes(scope_len) as usize;

        let mut payload_len = [0_u8; 4];
        if read_exact_or_none(reader, &mut payload_len)?.is_none() {
            return Ok(None);
        }
        let payload_len = u32::from_be_bytes(payload_len) as usize;

        let mut scope = vec![0_u8; scope_len];
        if read_exact_or_none(reader, &mut scope)?.is_none() {
            return Ok(None);
        }
        let scope = String::from_utf8(scope).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("WAL scope is not valid UTF-8: {error}"),
            )
        })?;

        let mut payload = vec![0_u8; payload_len];
        if read_exact_or_none(reader, &mut payload)?.is_none() {
            return Ok(None);
        }

        Ok(Some(Self {
            sequence: u64::from_be_bytes(sequence),
            operation: WalOperation::from_code(operation[0])?,
            scope,
            payload,
        }))
    }
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
    durability_policy: DurabilityPolicy,
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

    WalJournal::at_database_path_with_policy(database_path, durability_policy)
        .append(operation, &scope, &payload)?;
    Ok(())
}

pub(crate) fn recover_database<E>(database_core: &RwLock<Database>) -> Result<()>
where
    E: WalReplayExecutor,
{
    let journal = journal_for_database(database_core)?;
    if !journal.path().exists() {
        return Ok(());
    }

    let last_checkpoint_sequence = checkpoint_sequence(database_core)?;
    let mut begun_transactions: HashMap<String, (u64, TransactionWalOperation)> = HashMap::new();
    let mut committed_transactions = HashSet::new();
    let mut aborted_transactions = HashSet::new();

    for entry in journal.read_entries()? {
        if entry.sequence <= last_checkpoint_sequence {
            continue;
        }

        let Ok(record) = serde_json::from_slice::<TransactionWalRecord>(&entry.payload) else {
            continue;
        };

        match record {
            TransactionWalRecord::Begin {
                tx_id, operation, ..
            } => {
                begun_transactions.insert(tx_id, (entry.sequence, operation));
            }
            TransactionWalRecord::Commit { tx_id } => {
                committed_transactions.insert(tx_id);
            }
            TransactionWalRecord::Abort { tx_id } => {
                aborted_transactions.insert(tx_id);
            }
        }
    }

    let mut committed_operations = begun_transactions
        .into_iter()
        .filter(|(tx_id, _)| {
            committed_transactions.contains(tx_id) && !aborted_transactions.contains(tx_id)
        })
        .map(|(_, operation)| operation)
        .collect::<Vec<_>>();
    committed_operations.sort_by_key(|(sequence, _)| *sequence);

    if committed_operations.is_empty() {
        return Ok(());
    }

    for (_, operation) in committed_operations {
        replay_wal_operation::<E>(database_core, &operation)?;
    }

    flush_checkpoint(database_core)
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

fn command_operation(command: &Command) -> Option<WalOperation> {
    match command {
        Command::Upsert { .. } => Some(WalOperation::Upsert),
        Command::Delete { .. } => Some(WalOperation::Delete),
        Command::Compact(_) => Some(WalOperation::Maintenance),
        Command::Create(_) | Command::Alter(_) | Command::Drop(_) => Some(WalOperation::Define),
        _ => None,
    }
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

fn replay_wal_operation<E>(
    database_core: &RwLock<Database>,
    operation: &TransactionWalOperation,
) -> Result<()>
where
    E: WalReplayExecutor,
{
    match operation {
        TransactionWalOperation::Upsert {
            drawer_name,
            payload,
            drawer_namespace,
        } => {
            let context = ExecutionContext {
                drawer_namespace: drawer_namespace.as_deref(),
            };
            E::replay_upsert(database_core, drawer_name, payload.clone(), context)?;
        }
        TransactionWalOperation::BulkUpsert {
            drawer_name,
            records,
            drawer_namespace,
        } => {
            let context = ExecutionContext {
                drawer_namespace: drawer_namespace.as_deref(),
            };
            E::replay_bulk_upsert(database_core, drawer_name, records.clone(), context)?;
        }
        TransactionWalOperation::DeleteById {
            pointer,
            drawer_namespace,
        } => {
            let context = ExecutionContext {
                drawer_namespace: drawer_namespace.as_deref(),
            };
            E::replay_delete(database_core, pointer, context)?;
        }
        TransactionWalOperation::DeleteByFilter {
            drawer_name,
            filter,
            drawer_namespace,
        } => {
            let context = ExecutionContext {
                drawer_namespace: drawer_namespace.as_deref(),
            };
            E::replay_delete_by_filter(database_core, drawer_name, filter.clone(), context)?;
        }
    }

    Ok(())
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
    let drawers = read_lock(database_core)?.get_all_drawers();
    for (_name, drawer) in drawers {
        let mut guard = write_lock(&drawer)?;
        guard.commit()?;
    }
    Ok(())
}

fn flush_checkpoint(database_core: &RwLock<Database>) -> Result<()> {
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

fn journal_for_database(database_core: &RwLock<Database>) -> Result<WalJournal> {
    let db = read_lock(database_core)?;
    Ok(WalJournal::at_database_path_with_policy(
        db.storage_directory_path(),
        db.durability_policy(),
    ))
}

fn checkpoint_sequence(database_core: &RwLock<Database>) -> Result<u64> {
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

fn record_applied_checkpoint(database_core: &RwLock<Database>, sequence: u64) -> Result<()> {
    let checkpoint_path = wal_checkpoint_path(database_core)?;
    let checkpoint_body =
        serde_json::json!({"last_checkpoint": now_secs(), "last_checkpoint_sequence": sequence});
    let serialized = serde_json::to_vec(&checkpoint_body)?;
    fs::write(checkpoint_path, serialized)
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

fn read_entries_from_path(path: &Path) -> Result<Vec<WalEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut file = File::open(path)?;
    let mut entries = Vec::new();
    loop {
        match WalEntry::read_from(&mut file) {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => return Ok(entries),
            Err(error) => return Err(error),
        }
    }
}

fn read_exact_or_none(reader: &mut impl Read, buffer: &mut [u8]) -> Result<Option<()>> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(Some(())),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error),
    }
}

fn write_entry_locked(state: &mut WalJournalState, bytes: &[u8]) -> Result<()> {
    let file = state
        .file
        .as_mut()
        .ok_or_else(|| Error::other("Wardrobe WAL file handle is not initialized"))?;
    file.write_all(bytes)?;
    file.flush()?;
    file.flush()?;
    state.sync_count += 1;
    Ok(())
}

fn flush_pending_locked(inner: &WalJournalInner, state: &mut WalJournalState) -> Result<()> {
    if state.pending.is_empty() {
        return Ok(());
    }

    let pending = std::mem::take(&mut state.pending);
    let result = (|| {
        let file = state
            .file
            .as_mut()
            .ok_or_else(|| Error::other("Wardrobe WAL file handle is not initialized"))?;
        for pending_entry in &pending {
            file.write_all(&pending_entry.bytes)?;
        }
        file.flush()?;
        file.flush()?;
        state.sync_count += 1;
        Ok(())
    })();

    let error_message = result.as_ref().err().map(ToString::to_string);
    for pending_entry in pending {
        state
            .completed
            .insert(pending_entry.entry.sequence, error_message.clone());
    }
    inner.commit_gate.notify_all();
    result
}

fn wal_outcome(entry: WalEntry, outcome: Option<String>) -> Result<WalEntry> {
    match outcome {
        Some(error) => Err(Error::other(error)),
        None => Ok(entry),
    }
}

fn journal_registry() -> &'static Mutex<HashMap<PathBuf, Weak<WalJournalInner>>> {
    static JOURNALS: OnceLock<Mutex<HashMap<PathBuf, Weak<WalJournalInner>>>> = OnceLock::new();
    JOURNALS.get_or_init(|| Mutex::new(HashMap::new()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread;

    #[test]
    fn grouped_policy_batches_concurrent_appends_into_one_sync() {
        let directory = temp_wal_directory("grouped_policy_batches");
        let journal = WalJournal::at_database_path_with_policy(
            &directory,
            DurabilityPolicy::Grouped {
                commit_window_ms: 1000,
                max_batch_size: 4,
            },
        );
        let barrier = Arc::new(Barrier::new(4));
        let handles = (0..4)
            .map(|index| {
                let journal = journal.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    journal
                        .append(
                            WalOperation::Maintenance,
                            "test",
                            format!("entry-{index}").as_bytes(),
                        )
                        .expect("grouped append should succeed");
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().expect("append thread should complete");
        }

        let entries = journal.read_entries().expect("wal should read");
        assert_eq!(entries.len(), 4);
        assert_eq!(journal.durable_sync_count(), 1);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn read_entries_ignores_partial_trailing_frame() {
        let directory = temp_wal_directory("partial_trailing_frame");
        let journal = WalJournal::at_database_path(&directory);
        journal
            .append(WalOperation::Define, "test", b"complete")
            .expect("initial append should succeed");

        let partial = WalEntry {
            sequence: 2,
            operation: WalOperation::Define,
            scope: "test".to_string(),
            payload: b"incomplete".to_vec(),
        }
        .to_bytes()
        .expect("entry should encode");
        let mut file = OpenOptions::new()
            .append(true)
            .open(journal.path())
            .expect("wal should open");
        file.write_all(&partial[..7])
            .expect("partial frame should write");
        file.flush().expect("partial frame should flush");

        let entries = WalJournal::at_database_path(&directory)
            .read_entries()
            .expect("wal should ignore partial trailing frame");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].payload, b"complete");
        let _ = fs::remove_dir_all(directory);
    }

    fn temp_wal_directory(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wardrobe_wal_{label}_{}", Uuid::new_v4().simple()))
    }
}
