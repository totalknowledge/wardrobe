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

mod durability;
mod format;
mod journal;
mod replay;
mod transaction;
mod verification;

pub use durability::DurabilityPolicy;
pub use format::{WAL_FILE_NAME, WalEntry, WalOperation};
pub use journal::WalJournal;
pub use verification::WalVerification;

pub(crate) use durability::append_command;
pub(crate) use replay::{WalReplayExecutor, recover_database};
pub(crate) use transaction::{
    TransactionCoordinator, run_bulk_upsert_transaction, run_delete_by_filter_transaction,
    run_delete_transaction, run_upsert_transaction,
};
pub(crate) use verification::verify;

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
