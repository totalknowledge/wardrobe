use crate::wrdb_lib::drawer::VacuumReport;
use crate::wrdb_lib::query::{OrderDirection, QueryModifiers};
use crate::wrdb_lib::storage::{StorageCoordinate, StorageInventory, StorageLocator, StorageScope};
use crate::wrdb_lib::wal::WalVerification;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Error, ErrorKind, Result};
use std::marker::PhantomData;

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
    pub cursor: Option<String>,
    pub page: Option<usize>,
    pub page_size: Option<usize>,
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

    pub fn cursor(mut self, cursor: impl Into<String>) -> Self {
        self.cursor = Some(cursor.into());
        self
    }

    pub fn page(mut self, page: usize) -> Self {
        self.page = Some(page);
        self
    }

    pub fn page_size(mut self, page_size: usize) -> Self {
        self.page_size = Some(page_size);
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
                "cursor" => options.cursor = Some(expect_string(&key, &value)?),
                "page" => options.page = Some(expect_usize(&key, &value)?),
                "page_size" => options.page_size = Some(expect_usize(&key, &value)?),
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
            && self.cursor.is_none()
            && self.page.is_none()
            && self.page_size.is_none()
        {
            return None;
        }
        Some(QueryModifiers {
            limit: self.limit,
            offset: self.offset,
            order_by: self.order_by.clone(),
            order_direction: self.order_direction,
            cursor: self.cursor.clone(),
            page: self.page,
            page_size: self.page_size,
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
            cursor: modifiers.cursor,
            page: modifiers.page,
            page_size: modifiers.page_size,
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
pub struct PaginationMetadata {
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub page: Option<usize>,
    pub page_size: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaginatedReadResult {
    pub records: Vec<Value>,
    pub pagination: PaginationMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReadResult {
    Records(Vec<Value>),
    Page(PaginatedReadResult),
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

    pub fn relationship(
        drawer_name: impl Into<String>,
        field_name: impl Into<String>,
        target_drawer: impl Into<String>,
    ) -> Self {
        Self::schema_rule(
            drawer_name,
            "add",
            "relationship",
            field_name,
            serde_json::json!({
                "type": "M:1",
                "target_drawer": target_drawer.into()
            }),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DropRequest {
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
    pub fn tenants() -> TypedStatusRequest<Vec<String>> {
        TypedStatusRequest::new(Self::Tenants)
    }

    pub fn databases() -> TypedStatusRequest<Vec<StorageInventory>> {
        TypedStatusRequest::new(Self::Databases)
    }

    pub fn schemas(database_name: impl Into<String>) -> TypedStatusRequest<Vec<String>> {
        TypedStatusRequest::new(Self::Schemas {
            database_name: database_name.into(),
        })
    }

    pub fn drawers(
        database_name: impl Into<String>,
        schema_name: impl Into<String>,
    ) -> TypedStatusRequest<Vec<StorageInventory>> {
        TypedStatusRequest::new(Self::Drawers {
            database_name: database_name.into(),
            schema_name: schema_name.into(),
        })
    }

    pub fn wal(database_name: Option<impl Into<String>>) -> TypedStatusRequest<WalVerification> {
        TypedStatusRequest::new(Self::Wal {
            database_name: database_name.map(Into::into),
        })
    }

    pub fn storage() -> TypedStatusRequest<StorageDiagnosis> {
        TypedStatusRequest::new(Self::Storage)
    }

    pub fn path(path: impl Into<String>) -> TypedStatusRequest<CheckReport> {
        TypedStatusRequest::new(Self::Path { path: path.into() })
    }

    pub fn drawer_names() -> TypedStatusRequest<Vec<String>> {
        TypedStatusRequest::new(Self::DrawerNames)
    }

    pub fn cached_drawer_count() -> TypedStatusRequest<usize> {
        TypedStatusRequest::new(Self::CachedDrawerCount)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypedStatusRequest<T> {
    request: StatusRequest,
    output: PhantomData<fn() -> T>,
}

impl<T> TypedStatusRequest<T> {
    fn new(request: StatusRequest) -> Self {
        Self {
            request,
            output: PhantomData,
        }
    }

    pub fn into_request(self) -> StatusRequest {
        self.request
    }
}

impl<T> From<TypedStatusRequest<T>> for StatusRequest {
    fn from(request: TypedStatusRequest<T>) -> Self {
        request.into_request()
    }
}

pub trait StatusRequestOutput {
    type Output;

    fn into_status_request(self) -> StatusRequest;
    fn decode_status_payload(payload: Value) -> Result<Self::Output>;
}

impl<T> StatusRequestOutput for TypedStatusRequest<T>
where
    T: DeserializeOwned,
{
    type Output = T;

    fn into_status_request(self) -> StatusRequest {
        self.into_request()
    }

    fn decode_status_payload(payload: Value) -> Result<Self::Output> {
        serde_json::from_value(payload).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Wardrobe status returned an invalid payload: {error}"),
            )
        })
    }
}

impl StatusRequestOutput for StatusRequest {
    type Output = Value;

    fn into_status_request(self) -> StatusRequest {
        self
    }

    fn decode_status_payload(payload: Value) -> Result<Self::Output> {
        Ok(payload)
    }
}

pub(crate) fn encode_status_payload<T>(payload: T) -> Result<Value>
where
    T: Serialize,
{
    serde_json::to_value(payload).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("Failed to serialize Wardrobe status payload: {error}"),
        )
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Upsert {
        payload: Value,
        filter: OperationFilter,
        options: OperationOptions,
    },
    Read {
        filter: OperationFilter,
        options: OperationOptions,
    },
    Delete {
        filter: OperationFilter,
        options: OperationOptions,
    },
    Inspect {
        filter: OperationFilter,
        options: OperationOptions,
    },
    Count {
        filter: OperationFilter,
        options: OperationOptions,
    },
    Compact(CompactRequest),
    Create(CreateRequest),
    Alter(AlterRequest),
    Drop(DropRequest),
    Backup {
        source_path: String,
    },
    Restore {
        destination_path: String,
        archive: BackupArchive,
    },
    Grant(PermissionRequest),
    Revoke(PermissionRequest),
    Status(StatusRequest),
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
#[serde(rename_all = "snake_case")]
pub enum CommandResult {
    Upsert(UpsertResult),
    Read(ReadResult),
    Delete(DeleteResult),
    Inspect(InspectResult),
    Count(usize),
    Compact(VacuumReport),
    Create(CreateResult),
    Alter(Value),
    Drop(Value),
    Backup(BackupArchive),
    Restore(RestoreReport),
    Grant(Value),
    Revoke(Value),
    Status(Value),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[test]
    fn canonical_operation_filters_normalize_supported_inputs() {
        assert_eq!(OperationFilter::none(), OperationFilter::None);
        assert_eq!(
            OperationFilter::drawer("@gem"),
            OperationFilter::Drawer("gem".to_string())
        );
        assert_eq!(
            OperationFilter::pointer("@gem"),
            OperationFilter::Drawer("gem".to_string())
        );
        assert_eq!(
            OperationFilter::pointer("@gem:ruby"),
            OperationFilter::Pointer("@gem:ruby".to_string())
        );
        assert_eq!(OperationFilter::query(json!({})), OperationFilter::None);
        assert_eq!(
            OperationFilter::query_in("gem", json!({"color": "red"})),
            OperationFilter::Many(vec![
                OperationFilter::Drawer("gem".to_string()),
                OperationFilter::Query(json!({"color": "red"})),
            ])
        );
        assert_eq!(OperationFilter::many(Vec::new()), OperationFilter::None);
        assert_eq!(OperationFilter::from(()), OperationFilter::None);
        assert_eq!(
            OperationFilter::from(Some(OperationFilter::drawer("gem"))),
            OperationFilter::Drawer("gem".to_string())
        );
        assert_eq!(
            OperationFilter::from("@gem:ruby"),
            OperationFilter::Pointer("@gem:ruby".to_string())
        );
        assert_eq!(
            OperationFilter::from("gem".to_string()),
            OperationFilter::Drawer("gem".to_string())
        );
        assert_eq!(
            OperationFilter::from(&"gem".to_string()),
            OperationFilter::Drawer("gem".to_string())
        );
        assert_eq!(OperationFilter::from(Value::Null), OperationFilter::None);
        assert_eq!(
            OperationFilter::from(json!("@gem:ruby")),
            OperationFilter::Pointer("@gem:ruby".to_string())
        );
        assert_eq!(
            OperationFilter::from(json!(["gem", {"color": "red"}])),
            OperationFilter::Many(vec![
                OperationFilter::Drawer("gem".to_string()),
                OperationFilter::Query(json!({"color": "red"})),
            ])
        );
        assert_eq!(
            OperationFilter::from(json!({"_id": "@gem:ruby"})),
            OperationFilter::Pointer("@gem:ruby".to_string())
        );
        assert_eq!(
            OperationFilter::from(json!({"drawer": "@gem"})),
            OperationFilter::Drawer("gem".to_string())
        );
        assert_eq!(
            OperationFilter::from(StorageLocator::Explicit {
                drawer: "@gem".to_string(),
                id: "lnk_ruby".to_string(),
            }),
            OperationFilter::Pointer("@gem:ruby".to_string())
        );
        assert_eq!(
            OperationFilter::from(("gem", "ruby")),
            OperationFilter::Pointer("@gem:ruby".to_string())
        );
        assert_eq!(
            OperationFilter::from(("gem".to_string(), "ruby".to_string())),
            OperationFilter::Pointer("@gem:ruby".to_string())
        );
    }

    #[test]
    fn operation_options_parse_builders_and_validation_paths() {
        let options = OperationOptions::new()
            .multi(true)
            .atomic(false)
            .create_if_missing(false)
            .return_shape(ReturnShape::Pointers)
            .hydrate(false)
            .limit(10)
            .offset(2)
            .order_by("power")
            .order_direction(OrderDirection::Descending)
            .include_diagnostics(true);

        assert_eq!(options.multi, Some(true));
        assert_eq!(options.atomic_enabled(), false);
        assert_eq!(
            options.query_modifiers().expect("modifiers").limit,
            Some(10)
        );

        let parsed = OperationOptions::from_json(json!({
            "multi": true,
            "atomic": false,
            "create_if_missing": true,
            "return_shape": "record",
            "hydrate": true,
            "limit": 3,
            "offset": 1,
            "order_by": "name",
            "order_direction": "asc",
            "include_diagnostics": false
        }))
        .expect("options parse");
        assert_eq!(parsed.return_shape, Some(ReturnShape::Record));
        assert_eq!(parsed.order_direction, Some(OrderDirection::Ascending));

        let from_modifiers = OperationOptions::from(QueryModifiers {
            limit: Some(1),
            offset: Some(0),
            order_by: Some("name".to_string()),
            order_direction: Some(OrderDirection::Ascending),
            ..QueryModifiers::default()
        });
        assert_eq!(from_modifiers.limit, Some(1));
        assert_eq!(OperationOptions::from(()), OperationOptions::default());
        assert_eq!(
            OperationOptions::from(Some(OperationOptions::new().multi(true))).multi,
            Some(true)
        );

        for invalid in [
            json!(true),
            json!({"multi": "yes"}),
            json!({"return_shape": "bag"}),
            json!({"order_direction": "sideways"}),
            json!({"limit": -1}),
            json!({"unknown": true}),
        ] {
            assert!(OperationOptions::from_json(invalid).is_err());
        }
    }

    #[test]
    fn canonical_request_constructors_cover_all_variants() {
        assert_eq!(
            CompactRequest::drawer("gem"),
            CompactRequest::Drawer {
                drawer_name: "gem".to_string(),
                mode: CompactMode::Vacuum
            }
        );
        assert_eq!(
            CompactRequest::drawer_with_mode("gem", CompactMode::Migrate),
            CompactRequest::Drawer {
                drawer_name: "gem".to_string(),
                mode: CompactMode::Migrate
            }
        );
        assert_eq!(CompactRequest::from("gem"), CompactRequest::drawer("gem"));
        assert_eq!(
            CompactRequest::from("gem".to_string()),
            CompactRequest::drawer("gem")
        );

        assert_eq!(
            CreateRequest::database("wardrobe"),
            CreateRequest::Database {
                database_name: "wardrobe".to_string()
            }
        );
        assert_eq!(
            CreateRequest::schema("wardrobe", "bay"),
            CreateRequest::Schema {
                database_name: "wardrobe".to_string(),
                schema_name: "bay".to_string()
            }
        );
        assert_eq!(
            CreateRequest::drawer("wardrobe", "bay", "drawer"),
            CreateRequest::Drawer {
                database_name: "wardrobe".to_string(),
                schema_name: "bay".to_string(),
                drawer_name: "drawer".to_string()
            }
        );
        assert_eq!(
            CreateRequest::tenant_route("tenant", "wardrobe", "wardrobe/bay"),
            CreateRequest::TenantRoute {
                tenant_id: "tenant".to_string(),
                database_name: "wardrobe".to_string(),
                location: "wardrobe/bay".to_string()
            }
        );
        assert_eq!(
            CreateRequest::user(json!({"username": "alice"})),
            CreateRequest::User {
                payload: json!({"username": "alice"})
            }
        );

        assert_eq!(
            AlterRequest::schema_rule("w/b/d", "add", "index", "field", json!({"x": true})),
            AlterRequest::SchemaRule {
                drawer_name: "w/b/d".to_string(),
                action: "add".to_string(),
                kind: "index".to_string(),
                field_name: "field".to_string(),
                payload: json!({"x": true})
            }
        );
        assert_eq!(
            AlterRequest::relationship("w/b/character", "item_map", "w/b/item"),
            AlterRequest::SchemaRule {
                drawer_name: "w/b/character".to_string(),
                action: "add".to_string(),
                kind: "relationship".to_string(),
                field_name: "item_map".to_string(),
                payload: json!({"type": "M:1", "target_drawer": "w/b/item"})
            }
        );

        assert_eq!(
            DropRequest::database("wardrobe"),
            DropRequest::Database {
                database_name: "wardrobe".to_string()
            }
        );
        assert_eq!(
            DropRequest::schema("wardrobe", "bay"),
            DropRequest::Schema {
                database_name: "wardrobe".to_string(),
                schema_name: "bay".to_string()
            }
        );
        assert_eq!(
            DropRequest::drawer("wardrobe", "bay", "drawer"),
            DropRequest::Drawer {
                database_name: "wardrobe".to_string(),
                schema_name: "bay".to_string(),
                drawer_name: "drawer".to_string()
            }
        );
        assert_eq!(
            DropRequest::schema_rule("w/b/d", "index", "field", json!({})),
            DropRequest::SchemaRule {
                drawer_name: "w/b/d".to_string(),
                kind: "index".to_string(),
                field_name: "field".to_string(),
                payload: json!({})
            }
        );
        assert_eq!(
            DropRequest::user("alice"),
            DropRequest::User {
                username: "alice".to_string()
            }
        );
    }

    #[test]
    fn canonical_results_permissions_and_status_helpers_round_trip() {
        let result = UpsertResult::Pointers(vec!["@gem:ruby".to_string()]);
        assert_eq!(result.pointers(), &["@gem:ruby".to_string()]);
        assert_eq!(*result, ["@gem:ruby".to_string()]);
        assert_eq!(result.clone(), vec!["@gem:ruby".to_string()]);
        assert_eq!(vec!["@gem:ruby".to_string()], result.clone());
        assert_eq!(result.into_pointers(), vec!["@gem:ruby".to_string()]);

        let deleted = DeleteResult { deleted: 2 };
        assert_eq!(deleted.to_string(), "2");
        assert_eq!(usize::from(deleted.clone()), 2);
        assert_eq!(deleted, 2);
        assert_eq!(2, deleted);

        assert_eq!(
            PermissionRequest::new("alice", "wardrobe:read").into_payload(),
            json!({"username": "alice", "permission_scope": "wardrobe:read"})
        );
        assert_eq!(
            PermissionRequest::with_scope("alice", "scoped", "w/b", "rud").into_payload(),
            json!({
                "username": "alice",
                "permission_scope": "scoped",
                "scope": {"path": "w/b", "rights": "rud"}
            })
        );

        assert_eq!(
            StatusRequest::tenants().into_request(),
            StatusRequest::Tenants
        );
        assert_eq!(
            StatusRequest::databases().into_request(),
            StatusRequest::Databases
        );
        assert_eq!(
            StatusRequest::schemas("wardrobe").into_request(),
            StatusRequest::Schemas {
                database_name: "wardrobe".to_string()
            }
        );
        assert_eq!(
            StatusRequest::drawers("wardrobe", "bay").into_request(),
            StatusRequest::Drawers {
                database_name: "wardrobe".to_string(),
                schema_name: "bay".to_string()
            }
        );
        assert_eq!(
            StatusRequest::wal(Some("wardrobe")).into_request(),
            StatusRequest::Wal {
                database_name: Some("wardrobe".to_string())
            }
        );
        assert_eq!(
            StatusRequest::wal(None::<String>).into_request(),
            StatusRequest::Wal {
                database_name: None
            }
        );
        assert_eq!(
            StatusRequest::storage().into_request(),
            StatusRequest::Storage
        );
        assert_eq!(
            StatusRequest::path("wardrobe/bay").into_request(),
            StatusRequest::Path {
                path: "wardrobe/bay".to_string()
            }
        );
        assert_eq!(
            StatusRequest::drawer_names().into_request(),
            StatusRequest::DrawerNames
        );
        assert_eq!(
            StatusRequest::cached_drawer_count().into_request(),
            StatusRequest::CachedDrawerCount
        );

        let databases = json!([{
            "name": "catalog",
            "record_count": 1,
            "disk_size_bytes": 512,
            "register_file_count": 3
        }]);
        let status = CommandResult::Status(databases.clone());
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            json!({"status": databases})
        );
        assert_eq!(
            serde_json::from_value::<CommandResult>(json!({"status": ["public"]})).unwrap(),
            CommandResult::Status(json!(["public"]))
        );
    }
}
