use super::drawer::VacuumReport;
use super::query::{OrderDirection, QueryModifiers};
use super::storage::{StorageCoordinate, StorageInventory, StorageLocator, StorageScope};
use super::wal::WalVerification;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Error, ErrorKind, Result};

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
pub enum OperationFilter {
    None,
    Drawer(String),
    Pointer(String),
    Query(Value),
    Many(Vec<OperationFilter>),
}

impl OperationFilter {
    pub fn none() -> Self {
        Self::None
    }

    pub fn drawer(drawer_name: impl Into<String>) -> Self {
        let drawer_name = drawer_name.into();
        Self::Drawer(drawer_name.trim_start_matches('@').to_string())
    }

    pub fn pointer(pointer: impl Into<String>) -> Self {
        let pointer = pointer.into();
        if pointer.starts_with('@') && !pointer.contains(':') {
            return Self::drawer(pointer);
        }
        Self::Pointer(pointer)
    }

    pub fn query(query: Value) -> Self {
        match query {
            Value::Object(ref fields) if fields.is_empty() => Self::None,
            query => Self::Query(query),
        }
    }

    pub fn many(filters: Vec<OperationFilter>) -> Self {
        if filters.is_empty() {
            Self::None
        } else {
            Self::Many(filters)
        }
    }

    pub fn query_in(drawer_name: impl Into<String>, query: Value) -> Self {
        Self::Many(vec![Self::drawer(drawer_name), Self::query(query)])
    }
}

impl From<()> for OperationFilter {
    fn from(_: ()) -> Self {
        Self::None
    }
}

impl From<Option<OperationFilter>> for OperationFilter {
    fn from(value: Option<OperationFilter>) -> Self {
        value.unwrap_or(Self::None)
    }
}

impl From<&str> for OperationFilter {
    fn from(value: &str) -> Self {
        if value.starts_with('@') {
            Self::pointer(value)
        } else {
            Self::drawer(value)
        }
    }
}

impl From<String> for OperationFilter {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

impl From<&String> for OperationFilter {
    fn from(value: &String) -> Self {
        Self::from(value.as_str())
    }
}

impl From<Value> for OperationFilter {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Self::None,
            Value::String(value) => Self::from(value),
            Value::Array(filters) => {
                Self::many(filters.into_iter().map(OperationFilter::from).collect())
            }
            Value::Object(fields) if fields.is_empty() => Self::None,
            Value::Object(fields) => {
                if let Some(Value::String(id)) = fields.get("_id") {
                    return Self::pointer(id.clone());
                }
                if let Some(Value::String(drawer_name)) = fields.get("drawer") {
                    return Self::drawer(drawer_name.clone());
                }
                Self::Query(Value::Object(fields))
            }
            value => Self::Query(value),
        }
    }
}

impl From<StorageLocator> for OperationFilter {
    fn from(locator: StorageLocator) -> Self {
        match locator {
            StorageLocator::Inline(pointer) => Self::pointer(pointer),
            StorageLocator::Explicit { drawer, id } => {
                let drawer = drawer.trim_start_matches('@');
                let id = id.strip_prefix("lnk_").unwrap_or(&id);
                Self::pointer(format!("@{drawer}:{id}"))
            }
        }
    }
}

impl From<(&str, &str)> for OperationFilter {
    fn from((drawer, id): (&str, &str)) -> Self {
        Self::from(StorageLocator::Explicit {
            drawer: drawer.to_string(),
            id: id.to_string(),
        })
    }
}

impl From<(String, String)> for OperationFilter {
    fn from((drawer, id): (String, String)) -> Self {
        Self::from(StorageLocator::Explicit { drawer, id })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReturnShape {
    Default,
    Records,
    Record,
    Pointers,
    Exists,
    Diagnostics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OperationOptions {
    pub multi: Option<bool>,
    pub atomic: Option<bool>,
    pub create_if_missing: Option<bool>,
    pub return_shape: Option<ReturnShape>,
    pub hydrate: Option<bool>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub order_by: Option<String>,
    pub order_direction: Option<OrderDirection>,
    pub include_diagnostics: Option<bool>,
}

impl OperationOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn multi(mut self, multi: bool) -> Self {
        self.multi = Some(multi);
        self
    }

    pub fn atomic(mut self, atomic: bool) -> Self {
        self.atomic = Some(atomic);
        self
    }

    pub fn create_if_missing(mut self, create_if_missing: bool) -> Self {
        self.create_if_missing = Some(create_if_missing);
        self
    }

    pub fn return_shape(mut self, return_shape: ReturnShape) -> Self {
        self.return_shape = Some(return_shape);
        self
    }

    pub fn hydrate(mut self, hydrate: bool) -> Self {
        self.hydrate = Some(hydrate);
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn order_by(mut self, order_by: impl Into<String>) -> Self {
        self.order_by = Some(order_by.into());
        self
    }

    pub fn order_direction(mut self, order_direction: OrderDirection) -> Self {
        self.order_direction = Some(order_direction);
        self
    }

    pub fn include_diagnostics(mut self, include_diagnostics: bool) -> Self {
        self.include_diagnostics = Some(include_diagnostics);
        self
    }

    pub fn from_json(value: Value) -> Result<Self> {
        let Value::Object(fields) = value else {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "operation options must be a JSON object",
            ));
        };
        let mut options = Self::default();
        for (key, value) in fields {
            match key.as_str() {
                "multi" => options.multi = Some(expect_bool(&key, &value)?),
                "atomic" => options.atomic = Some(expect_bool(&key, &value)?),
                "create_if_missing" => options.create_if_missing = Some(expect_bool(&key, &value)?),
                "return_shape" => {
                    options.return_shape = Some(match expect_string(&key, &value)?.as_str() {
                        "default" => ReturnShape::Default,
                        "records" => ReturnShape::Records,
                        "record" => ReturnShape::Record,
                        "pointers" => ReturnShape::Pointers,
                        "exists" => ReturnShape::Exists,
                        "diagnostics" => ReturnShape::Diagnostics,
                        other => {
                            return Err(Error::new(
                                ErrorKind::InvalidInput,
                                format!("unsupported return_shape option '{other}'"),
                            ));
                        }
                    })
                }
                "hydrate" => options.hydrate = Some(expect_bool(&key, &value)?),
                "limit" => options.limit = Some(expect_usize(&key, &value)?),
                "offset" => options.offset = Some(expect_usize(&key, &value)?),
                "order_by" => options.order_by = Some(expect_string(&key, &value)?),
                "order_direction" => {
                    options.order_direction = Some(match expect_string(&key, &value)?.as_str() {
                        "asc" => OrderDirection::Ascending,
                        "desc" => OrderDirection::Descending,
                        other => {
                            return Err(Error::new(
                                ErrorKind::InvalidInput,
                                format!("unsupported order_direction option '{other}'"),
                            ));
                        }
                    })
                }
                "include_diagnostics" => {
                    options.include_diagnostics = Some(expect_bool(&key, &value)?)
                }
                other => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("unknown operation option '{other}'"),
                    ));
                }
            }
        }
        Ok(options)
    }

    pub fn query_modifiers(&self) -> Option<QueryModifiers> {
        if self.limit.is_none()
            && self.offset.is_none()
            && self.order_by.is_none()
            && self.order_direction.is_none()
        {
            return None;
        }
        Some(QueryModifiers {
            limit: self.limit,
            offset: self.offset,
            order_by: self.order_by.clone(),
            order_direction: self.order_direction,
        })
    }

    pub fn atomic_enabled(&self) -> bool {
        self.atomic.unwrap_or(true)
    }
}

impl From<QueryModifiers> for OperationOptions {
    fn from(modifiers: QueryModifiers) -> Self {
        Self {
            limit: modifiers.limit,
            offset: modifiers.offset,
            order_by: modifiers.order_by,
            order_direction: modifiers.order_direction,
            ..Self::default()
        }
    }
}

impl From<()> for OperationOptions {
    fn from(_: ()) -> Self {
        Self::default()
    }
}

impl From<Option<OperationOptions>> for OperationOptions {
    fn from(value: Option<OperationOptions>) -> Self {
        value.unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UpsertResult {
    Pointers(Vec<String>),
}

impl UpsertResult {
    pub fn pointers(&self) -> &[String] {
        match self {
            Self::Pointers(pointers) => pointers,
        }
    }

    pub fn into_pointers(self) -> Vec<String> {
        match self {
            Self::Pointers(pointers) => pointers,
        }
    }
}

impl std::ops::Deref for UpsertResult {
    type Target = [String];

    fn deref(&self) -> &Self::Target {
        self.pointers()
    }
}

impl PartialEq<Vec<String>> for UpsertResult {
    fn eq(&self, other: &Vec<String>) -> bool {
        self.pointers() == other.as_slice()
    }
}

impl PartialEq<UpsertResult> for Vec<String> {
    fn eq(&self, other: &UpsertResult) -> bool {
        self.as_slice() == other.pointers()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteResult {
    pub deleted: usize,
}

impl std::fmt::Display for DeleteResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.deleted)
    }
}

impl From<DeleteResult> for usize {
    fn from(result: DeleteResult) -> Self {
        result.deleted
    }
}

impl PartialEq<usize> for DeleteResult {
    fn eq(&self, other: &usize) -> bool {
        self.deleted == *other
    }
}

impl PartialEq<DeleteResult> for usize {
    fn eq(&self, other: &DeleteResult) -> bool {
        *self == other.deleted
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InspectResult {
    Drawer(DrawerInspectionMetrics),
    Record(Option<Value>),
    Query(Vec<Value>),
    Storage(StorageDiagnosis),
    Path(CheckReport),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReadResult {
    Records(Vec<Value>),
    Record(Option<Value>),
    Pointers(Vec<String>),
    Exists(bool),
}

fn expect_bool(key: &str, value: &Value) -> Result<bool> {
    value.as_bool().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("operation option '{key}' must be a boolean"),
        )
    })
}

fn expect_string(key: &str, value: &Value) -> Result<String> {
    value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("operation option '{key}' must be a string"),
        )
    })
}

fn expect_usize(key: &str, value: &Value) -> Result<usize> {
    let value = value.as_u64().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("operation option '{key}' must be an unsigned integer"),
        )
    })?;
    usize::try_from(value).map_err(|_| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("operation option '{key}' is too large for this platform"),
        )
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompactMode {
    Vacuum,
    Migrate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompactRequest {
    Drawer {
        drawer_name: String,
        mode: CompactMode,
    },
}

impl CompactRequest {
    pub fn drawer(drawer_name: impl Into<String>) -> Self {
        Self::Drawer {
            drawer_name: drawer_name.into(),
            mode: CompactMode::Vacuum,
        }
    }

    pub fn drawer_with_mode(drawer_name: impl Into<String>, mode: CompactMode) -> Self {
        Self::Drawer {
            drawer_name: drawer_name.into(),
            mode,
        }
    }
}

impl From<&str> for CompactRequest {
    fn from(drawer_name: &str) -> Self {
        Self::drawer(drawer_name)
    }
}

impl From<String> for CompactRequest {
    fn from(drawer_name: String) -> Self {
        Self::drawer(drawer_name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CreateRequest {
    Database {
        database_name: String,
    },
    Schema {
        database_name: String,
        schema_name: String,
    },
    Drawer {
        database_name: String,
        schema_name: String,
        drawer_name: String,
    },
    TenantRoute {
        tenant_id: String,
        database_name: String,
        location: String,
    },
    User {
        payload: Value,
    },
}

impl CreateRequest {
    pub fn database(database_name: impl Into<String>) -> Self {
        Self::Database {
            database_name: database_name.into(),
        }
    }

    pub fn schema(database_name: impl Into<String>, schema_name: impl Into<String>) -> Self {
        Self::Schema {
            database_name: database_name.into(),
            schema_name: schema_name.into(),
        }
    }

    pub fn drawer(
        database_name: impl Into<String>,
        schema_name: impl Into<String>,
        drawer_name: impl Into<String>,
    ) -> Self {
        Self::Drawer {
            database_name: database_name.into(),
            schema_name: schema_name.into(),
            drawer_name: drawer_name.into(),
        }
    }

    pub fn tenant_route(
        tenant_id: impl Into<String>,
        database_name: impl Into<String>,
        location: impl Into<String>,
    ) -> Self {
        Self::TenantRoute {
            tenant_id: tenant_id.into(),
            database_name: database_name.into(),
            location: location.into(),
        }
    }

    pub fn user(payload: Value) -> Self {
        Self::User { payload }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CreateResult {
    StorageInventory(StorageInventory),
    Admin(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AlterRequest {
    SchemaRule {
        drawer_name: String,
        action: String,
        kind: String,
        field_name: String,
        payload: Value,
    },
}

impl AlterRequest {
    pub fn schema_rule(
        drawer_name: impl Into<String>,
        action: impl Into<String>,
        kind: impl Into<String>,
        field_name: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self::SchemaRule {
            drawer_name: drawer_name.into(),
            action: action.into(),
            kind: kind.into(),
            field_name: field_name.into(),
            payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DropRequest {
    SchemaRule {
        drawer_name: String,
        kind: String,
        field_name: String,
        payload: Value,
    },
    User {
        username: String,
    },
}

impl DropRequest {
    pub fn schema_rule(
        drawer_name: impl Into<String>,
        kind: impl Into<String>,
        field_name: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self::SchemaRule {
            drawer_name: drawer_name.into(),
            kind: kind.into(),
            field_name: field_name.into(),
            payload,
        }
    }

    pub fn user(username: impl Into<String>) -> Self {
        Self::User {
            username: username.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub username: String,
    pub permission_scope: String,
    pub scope: Option<PermissionScopeDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionScopeDescriptor {
    pub path: String,
    pub rights: String,
}

impl PermissionRequest {
    pub fn new(username: impl Into<String>, permission_scope: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            permission_scope: permission_scope.into(),
            scope: None,
        }
    }

    pub fn with_scope(
        username: impl Into<String>,
        permission_scope: impl Into<String>,
        path: impl Into<String>,
        rights: impl Into<String>,
    ) -> Self {
        Self {
            username: username.into(),
            permission_scope: permission_scope.into(),
            scope: Some(PermissionScopeDescriptor {
                path: path.into(),
                rights: rights.into(),
            }),
        }
    }

    pub fn into_payload(self) -> Value {
        let mut payload = serde_json::json!({
            "username": self.username,
            "permission_scope": self.permission_scope,
        });
        if let Some(scope) = self.scope {
            payload["scope"] = serde_json::json!({
                "path": scope.path,
                "rights": scope.rights,
            });
        }
        payload
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StatusRequest {
    Tenants,
    Databases,
    Schemas {
        database_name: String,
    },
    Drawers {
        database_name: String,
        schema_name: String,
    },
    Wal {
        database_name: Option<String>,
    },
    Storage,
    Path {
        path: String,
    },
    DrawerNames,
    CachedDrawerCount,
}

impl StatusRequest {
    pub fn tenants() -> Self {
        Self::Tenants
    }

    pub fn databases() -> Self {
        Self::Databases
    }

    pub fn schemas(database_name: impl Into<String>) -> Self {
        Self::Schemas {
            database_name: database_name.into(),
        }
    }

    pub fn drawers(database_name: impl Into<String>, schema_name: impl Into<String>) -> Self {
        Self::Drawers {
            database_name: database_name.into(),
            schema_name: schema_name.into(),
        }
    }

    pub fn wal(database_name: Option<impl Into<String>>) -> Self {
        Self::Wal {
            database_name: database_name.map(Into::into),
        }
    }

    pub fn storage() -> Self {
        Self::Storage
    }

    pub fn path(path: impl Into<String>) -> Self {
        Self::Path { path: path.into() }
    }

    pub fn drawer_names() -> Self {
        Self::DrawerNames
    }

    pub fn cached_drawer_count() -> Self {
        Self::CachedDrawerCount
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StatusResult {
    Tenants(Vec<String>),
    Databases(Vec<StorageInventory>),
    Schemas(Vec<String>),
    Drawers(Vec<StorageInventory>),
    Wal(WalVerification),
    Storage(StorageDiagnosis),
    Check(CheckReport),
    DrawerNames(Vec<String>),
    CachedDrawerCount(usize),
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
