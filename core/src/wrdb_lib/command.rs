use super::drawer::VacuumReport;
use super::query::QueryModifiers;
use super::storage::{StorageCoordinate, StorageInventory, StorageScope};
use super::wal::WalVerification;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrawerInspectionMetrics {
    pub path: String,
    pub data_bytes: u64,
    pub index_bytes: u64,
    pub meta_bytes: u64,
    pub total_bytes: u64,
    pub record_count: usize,
    pub register_file_count: usize,
    pub tombstone_fragmentation_percent: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckReport {
    pub path: String,
    pub kind: String,
    pub entries: Vec<CheckEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckEntry {
    pub label: String,
    pub path: String,
    pub exists: bool,
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageDiagnosis {
    pub storage_directory: String,
    #[serde(default)]
    pub storage_bytes: u64,
    #[serde(default)]
    pub data_bytes: u64,
    #[serde(default)]
    pub index_bytes: u64,
    #[serde(default)]
    pub metadata_bytes: u64,
    #[serde(default)]
    pub logical_wal_bytes: u64,
    #[serde(default)]
    pub transaction_wal_bytes: u64,
    #[serde(default)]
    pub other_bytes: u64,
    pub drawer_count: usize,
    pub status: String,
    pub drawers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupArchive {
    pub format: String,
    pub source_path: String,
    pub scope: String,
    pub files: Vec<BackupArchiveFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupArchiveFile {
    pub path: String,
    pub bytes_hex: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreReport {
    pub destination_path: String,
    pub scope: String,
    pub file_count: usize,
    pub byte_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    ShowTenants,
    ShowDatabases,
    VerifyWal {
        database_name: Option<String>,
    },
    ShowSchemas {
        database_name: String,
    },
    ShowDrawers {
        database_name: String,
        schema_name: String,
    },
    Upsert {
        drawer_name: String,
        payload: Value,
    },
    BulkUpsert {
        drawer_name: String,
        records: Vec<Value>,
        atomic: bool,
    },
    FindAll {
        drawer_name: String,
    },
    FindById {
        pointer: String,
    },
    FindByFilter {
        drawer_name: String,
        filter: Value,
        modifiers: Option<QueryModifiers>,
    },
    Count {
        drawer_name: String,
        filter: Option<Value>,
        modifiers: Option<QueryModifiers>,
    },
    Delete {
        pointer: String,
    },
    DeleteByFilter {
        drawer_name: String,
        filter: Value,
    },
    Vacuum {
        drawer_name: String,
    },
    Migrate {
        drawer_name: String,
    },
    Inspect {
        drawer_name: String,
    },
    Check {
        path: String,
    },
    Diagnose,
    ListDrawers,
    Backup {
        source_path: String,
    },
    Restore {
        destination_path: String,
        archive: BackupArchive,
    },
    DefineDatabase {
        database_name: String,
    },
    DefineSchema {
        database_name: String,
        schema_name: String,
    },
    DefineDrawer {
        database_name: String,
        schema_name: String,
        drawer_name: String,
    },
    DefineTenantRoute {
        tenant_id: String,
        database_name: String,
        location: String,
    },
    ManageSchema {
        action: String,
        kind: String,
        drawer_name: String,
        field_name: String,
        payload: Value,
    },
    ManageUser {
        action: String,
        payload: Value,
    },
    ExecuteForTenant {
        tenant_id: String,
        database_name: String,
        schema_name: String,
        command: Box<Command>,
    },
    Execute {
        coordinate: StorageCoordinate,
        command: Box<Command>,
    },
    ExecuteInScope {
        scope: StorageScope,
        command: Box<Command>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CommandResult {
    StorageInventory(StorageInventory),
    Tenants(Vec<String>),
    Databases(Vec<StorageInventory>),
    WalVerification(WalVerification),
    Schemas(Vec<String>),
    Drawers(Vec<StorageInventory>),
    Pointer(String),
    Pointers(Vec<String>),
    Records(Vec<Value>),
    Record(Option<Value>),
    Count(usize),
    Deleted(bool),
    Vacuumed(VacuumReport),
    Migrated(VacuumReport),
    Inspection(DrawerInspectionMetrics),
    Check(CheckReport),
    Diagnosis(StorageDiagnosis),
    DrawerNames(Vec<String>),
    Backup(BackupArchive),
    Restored(RestoreReport),
    Admin(Value),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_diagnosis_defaults_missing_storage_bytes_for_older_payloads() {
        let payload = r#"{
            "storage_directory": "/srv/wardrobe",
            "drawer_count": 0,
            "status": "empty",
            "drawers": []
        }"#;

        let diagnosis: StorageDiagnosis =
            serde_json::from_str(payload).expect("legacy diagnosis should deserialize");

        assert_eq!(diagnosis.storage_directory, "/srv/wardrobe");
        assert_eq!(diagnosis.storage_bytes, 0);
        assert_eq!(diagnosis.data_bytes, 0);
        assert_eq!(diagnosis.index_bytes, 0);
        assert_eq!(diagnosis.metadata_bytes, 0);
        assert_eq!(diagnosis.logical_wal_bytes, 0);
        assert_eq!(diagnosis.transaction_wal_bytes, 0);
        assert_eq!(diagnosis.other_bytes, 0);
        assert_eq!(diagnosis.drawer_count, 0);
        assert_eq!(diagnosis.status, "empty");
        assert!(diagnosis.drawers.is_empty());
    }
}
