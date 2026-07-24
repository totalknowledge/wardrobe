use super::{BackupArchive, CommandResult, RestoreReport};
use crate::wrdb_lib::drawer::VacuumReport;
use serde_json::Value;
use std::io::{Error, ErrorKind, Result};

pub(crate) fn upsert(result: CommandResult) -> Result<super::UpsertResult> {
    match result {
        CommandResult::Upsert(result) => Ok(result),
        other => unexpected_result("upsert result", other),
    }
}

pub(crate) fn read(result: CommandResult) -> Result<super::ReadResult> {
    match result {
        CommandResult::Read(result) => Ok(result),
        other => unexpected_result("read result", other),
    }
}

pub(crate) fn delete(result: CommandResult) -> Result<super::DeleteResult> {
    match result {
        CommandResult::Delete(result) => Ok(result),
        other => unexpected_result("delete result", other),
    }
}

pub(crate) fn inspect(result: CommandResult) -> Result<super::InspectResult> {
    match result {
        CommandResult::Inspect(result) => Ok(result),
        other => unexpected_result("inspect result", other),
    }
}

pub(crate) fn compact(result: CommandResult) -> Result<VacuumReport> {
    match result {
        CommandResult::Compact(result) => Ok(result),
        other => unexpected_result("compact result", other),
    }
}

pub(crate) fn create(result: CommandResult) -> Result<super::CreateResult> {
    match result {
        CommandResult::Create(result) => Ok(result),
        other => unexpected_result("create result", other),
    }
}

pub(crate) fn status(result: CommandResult) -> Result<Value> {
    match result {
        CommandResult::Status(result) => Ok(result),
        other => unexpected_result("status result", other),
    }
}

pub(crate) fn count(result: CommandResult) -> Result<usize> {
    match result {
        CommandResult::Count(count) => Ok(count),
        other => unexpected_result("count", other),
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
        CommandResult::Restore(report) => Ok(report),
        other => unexpected_result("restore report", other),
    }
}

pub(crate) fn admin(result: CommandResult) -> Result<Value> {
    match result {
        CommandResult::Create(super::CreateResult::Admin(payload))
        | CommandResult::Alter(payload)
        | CommandResult::Drop(payload)
        | CommandResult::Grant(payload)
        | CommandResult::Revoke(payload) => Ok(payload),
        other => unexpected_result("admin response", other),
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
    use crate::wrdb_lib::command::{CreateResult, DeleteResult, ReadResult, UpsertResult};
    use crate::wrdb_lib::drawer::VacuumReport;
    use crate::wrdb_lib::storage::StorageInventory;
    use serde_json::json;

    fn archive() -> BackupArchive {
        BackupArchive {
            format: "wardrobe-archive/v1".to_string(),
            source_path: "catalog/public".to_string(),
            scope: "schema".to_string(),
            files: Vec::new(),
        }
    }

    fn restore_report() -> RestoreReport {
        RestoreReport {
            destination_path: "catalog/copy".to_string(),
            scope: "schema".to_string(),
            file_count: 0,
            byte_count: 0,
        }
    }

    fn inventory() -> StorageInventory {
        StorageInventory {
            name: "gem".to_string(),
            record_count: 0,
            disk_size_bytes: 0,
            register_file_count: 0,
        }
    }

    fn vacuum_report() -> VacuumReport {
        VacuumReport {
            records_rewritten: 1,
            data_bytes_before: 10,
            data_bytes_after: 7,
            index_bytes_before: 3,
            index_bytes_after: 2,
            bytes_reclaimed: 4,
        }
    }

    fn assert_invalid_data<T: std::fmt::Debug>(result: Result<T>) {
        let error = result.expect_err("wrong command result should be rejected");
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("Wardrobe protocol returned an unexpected result")
        );
    }

    #[test]
    fn unexpected_result_returns_invaliddata() {
        let result: Result<String> = unexpected_result("pointer", CommandResult::Count(5));
        assert!(result.is_err());
        assert_eq!(result.err().unwrap().kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn canonical_result_extractors_accept_matching_variants() {
        assert_eq!(
            upsert(CommandResult::Upsert(UpsertResult::Pointers(vec![
                "@gem:fire".to_string()
            ])))
            .unwrap(),
            UpsertResult::Pointers(vec!["@gem:fire".to_string()])
        );
        assert_eq!(
            read(CommandResult::Read(ReadResult::Records(vec![json!({
                "_id": "fire"
            })])))
            .unwrap(),
            ReadResult::Records(vec![json!({ "_id": "fire" })])
        );
        assert_eq!(
            delete(CommandResult::Delete(DeleteResult { deleted: 2 })).unwrap(),
            DeleteResult { deleted: 2 }
        );
        assert_eq!(
            compact(CommandResult::Compact(vacuum_report())).unwrap(),
            vacuum_report()
        );
        assert_eq!(
            create(CommandResult::Create(CreateResult::StorageInventory(
                inventory()
            )))
            .unwrap(),
            CreateResult::StorageInventory(inventory())
        );
        assert_eq!(status(CommandResult::Status(json!(3))).unwrap(), json!(3));
        assert_eq!(count(CommandResult::Count(4)).unwrap(), 4);
        assert_eq!(backup(CommandResult::Backup(archive())).unwrap(), archive());
        assert_eq!(
            restored(CommandResult::Restore(restore_report())).unwrap(),
            restore_report()
        );
        assert_eq!(
            admin(CommandResult::Create(CreateResult::Admin(json!({
                "created": true
            }))))
            .unwrap(),
            json!({ "created": true })
        );
        assert_eq!(
            admin(CommandResult::Alter(json!({ "altered": true }))).unwrap(),
            json!({ "altered": true })
        );
        assert_eq!(
            admin(CommandResult::Drop(json!({ "dropped": true }))).unwrap(),
            json!({ "dropped": true })
        );
        assert_eq!(
            admin(CommandResult::Grant(json!({ "granted": true }))).unwrap(),
            json!({ "granted": true })
        );
        assert_eq!(
            admin(CommandResult::Revoke(json!({ "revoked": true }))).unwrap(),
            json!({ "revoked": true })
        );
    }

    #[test]
    fn canonical_result_extractors_reject_wrong_variants() {
        assert_invalid_data(upsert(CommandResult::Count(0)));
        assert_invalid_data(read(CommandResult::Count(0)));
        assert_invalid_data(delete(CommandResult::Count(0)));
        assert_invalid_data(inspect(CommandResult::Count(0)));
        assert_invalid_data(compact(CommandResult::Count(0)));
        assert_invalid_data(create(CommandResult::Count(0)));
        assert_invalid_data(status(CommandResult::Count(0)));
        assert_invalid_data(count(CommandResult::Read(ReadResult::Exists(true))));
        assert_invalid_data(backup(CommandResult::Count(0)));
        assert_invalid_data(restored(CommandResult::Count(0)));
        assert_invalid_data(admin(CommandResult::Count(0)));
    }
}
