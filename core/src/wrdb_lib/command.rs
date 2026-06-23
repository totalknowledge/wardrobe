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
