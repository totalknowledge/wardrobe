use super::transaction::{
    TransactionWalOperation, TransactionWalRecord, checkpoint_sequence, flush_checkpoint,
    journal_for_database,
};
use super::*;

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

pub(super) fn replay_wal_operation<E>(
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
