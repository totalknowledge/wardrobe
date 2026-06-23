use super::drawer::VacuumReport;
use super::query::QueryModifiers;
use super::storage::{StorageCoordinate, StorageInventory, StorageScope};
use super::wal::WalVerification;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    Admin(Value),
}
