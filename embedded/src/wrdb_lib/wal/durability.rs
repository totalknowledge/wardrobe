use super::*;

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

use crate::engine::WardrobeEngine;

pub(crate) fn append_command(
    engine: &WardrobeEngine,
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

    let journal = engine.logical_wal_journal(database_path)?;
    journal.append(operation, &scope, &payload)?;
    Ok(())
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
