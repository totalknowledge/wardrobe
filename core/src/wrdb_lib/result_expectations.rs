use super::command::{
    BackupArchive, CheckReport, CommandResult, DrawerInspectionMetrics, RestoreReport,
    StorageDiagnosis,
};
use super::drawer::VacuumReport;
use super::storage::StorageInventory;
use super::wal::WalVerification;
use serde_json::Value;
use std::io::{Error, ErrorKind, Result};

pub(crate) fn upsert_pointers(result: CommandResult) -> Result<Vec<String>> {
    match result {
        CommandResult::Pointer(pointer) => Ok(vec![pointer]),
        CommandResult::Pointers(pointers) => Ok(pointers),
        other => unexpected_result("upsert pointer list", other),
    }
}

pub(crate) fn records(result: CommandResult) -> Result<Vec<Value>> {
    match result {
        CommandResult::Records(records) => Ok(records),
        other => unexpected_result("records", other),
    }
}

pub(crate) fn record(result: CommandResult) -> Result<Option<Value>> {
    match result {
        CommandResult::Record(record) => Ok(record),
        other => unexpected_result("record", other),
    }
}

pub(crate) fn count(result: CommandResult) -> Result<usize> {
    match result {
        CommandResult::Count(count) => Ok(count),
        other => unexpected_result("count", other),
    }
}

pub(crate) fn deleted(result: CommandResult) -> Result<bool> {
    match result {
        CommandResult::Deleted(deleted) => Ok(deleted),
        other => unexpected_result("deleted flag", other),
    }
}

pub(crate) fn vacuumed(result: CommandResult) -> Result<VacuumReport> {
    match result {
        CommandResult::Vacuumed(report) => Ok(report),
        other => unexpected_result("vacuum report", other),
    }
}

pub(crate) fn migrated(result: CommandResult) -> Result<VacuumReport> {
    match result {
        CommandResult::Migrated(report) => Ok(report),
        other => unexpected_result("migration report", other),
    }
}

pub(crate) fn inspection(result: CommandResult) -> Result<DrawerInspectionMetrics> {
    match result {
        CommandResult::Inspection(metrics) => Ok(metrics),
        other => unexpected_result("inspection metrics", other),
    }
}

pub(crate) fn check(result: CommandResult) -> Result<CheckReport> {
    match result {
        CommandResult::Check(report) => Ok(report),
        other => unexpected_result("check report", other),
    }
}

pub(crate) fn diagnosis(result: CommandResult) -> Result<StorageDiagnosis> {
    match result {
        CommandResult::Diagnosis(report) => Ok(report),
        other => unexpected_result("storage diagnosis", other),
    }
}

pub(crate) fn drawer_names(result: CommandResult) -> Result<Vec<String>> {
    match result {
        CommandResult::DrawerNames(drawers) => Ok(drawers),
        other => unexpected_result("drawer names", other),
    }
}

pub(crate) fn backup(result: CommandResult) -> Result<BackupArchive> {
    match result {
        CommandResult::Backup(archive) => Ok(archive),
        other => unexpected_result("backup archive", other),
    }
}

pub(crate) fn restored(result: CommandResult) -> Result<RestoreReport> {
    match result {
        CommandResult::Restored(report) => Ok(report),
        other => unexpected_result("restore report", other),
    }
}

pub(crate) fn storage_inventory(result: CommandResult) -> Result<StorageInventory> {
    match result {
        CommandResult::StorageInventory(inventory) => Ok(inventory),
        other => unexpected_result("storage inventory", other),
    }
}

pub(crate) fn admin(result: CommandResult) -> Result<Value> {
    match result {
        CommandResult::Admin(payload) => Ok(payload),
        other => unexpected_result("admin response", other),
    }
}

pub(crate) fn tenants(result: CommandResult) -> Result<Vec<String>> {
    match result {
        CommandResult::Tenants(tenants) => Ok(tenants),
        other => unexpected_result("tenants", other),
    }
}

pub(crate) fn databases(result: CommandResult) -> Result<Vec<StorageInventory>> {
    match result {
        CommandResult::Databases(databases) => Ok(databases),
        other => unexpected_result("databases", other),
    }
}

pub(crate) fn schemas(result: CommandResult) -> Result<Vec<String>> {
    match result {
        CommandResult::Schemas(schemas) => Ok(schemas),
        other => unexpected_result("schemas", other),
    }
}

pub(crate) fn drawers(result: CommandResult) -> Result<Vec<StorageInventory>> {
    match result {
        CommandResult::Drawers(drawers) => Ok(drawers),
        other => unexpected_result("drawers", other),
    }
}

pub(crate) fn wal_verification(result: CommandResult) -> Result<WalVerification> {
    match result {
        CommandResult::WalVerification(verification) => Ok(verification),
        other => unexpected_result("wal verification", other),
    }
}

fn unexpected_result<T>(expected: &str, actual: CommandResult) -> Result<T> {
    Err(Error::new(
        ErrorKind::InvalidData,
        format!(
            "Wardrobe protocol returned an unexpected result; expected {expected}, got {actual:?}",
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unexpected_result_returns_invaliddata() {
        let result: Result<String> = unexpected_result("pointer", CommandResult::Count(5));
        assert!(result.is_err());
        assert_eq!(result.err().unwrap().kind(), ErrorKind::InvalidData);
    }
}
