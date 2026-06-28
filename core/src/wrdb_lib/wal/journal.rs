use super::format::read_entries_from_path;
use super::*;

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
