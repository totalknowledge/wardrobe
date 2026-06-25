# Wardrobe API

This document describes the current public API of `wardrobe-core`.

There are two main application-facing surfaces:

- Client API: use `WardrobeClient` when code should work across embedded, TCP, and Unix socket targets
- Engine API: use `WardrobeEngine` when code runs directly against the local storage engine

The engine is the local superset. The client mirrors the same command surface wherever transport allows it.

Most operations return `std::io::Result<T>`.

Note on naming: the CLI help and `NOTES.txt` use `wardrobe` and `bay` as user-facing terms. The Rust API names those same layers `database` and `schema`.

---

## Client API

The client API is the preferred boundary for application code that may move between embedded and remote execution.

### `WardrobeClient`

Primary connection-driven facade.

#### Constructors and connection metadata

```rust
pub fn open(connection_string: impl AsRef<str>) -> Result<Self>
pub fn connection_target(&self) -> &ConnectionTarget
pub fn driver_kind(&self) -> DriverKind
pub fn requires_embedded_engine(&self) -> bool
pub fn uses_socket_transport(&self) -> bool
```

Accepted connection shapes:

- Embedded path: `./data`
- Embedded URI: `wardrobe://local/path/to/data`
- File URI: `wardrobe+file://path/to/data`
- TCP URI: `wardrobe://host[:port]`
- Unix socket URI: `wardrobe+unix:///tmp/wardrobe.sock`

#### Record operations

```rust
pub fn upsert(&self, drawer_name: &str, payload: Value) -> Result<String>
pub fn find_all(&self, drawer_name: &str) -> Result<Vec<Value>>
pub fn find_by_filter(
    &self,
    drawer_name: &str,
    filter: Value,
    modifiers: Option<QueryModifiers>,
) -> Result<Vec<Value>>
pub fn count(
    &self,
    drawer_name: &str,
    filter: Option<Value>,
    modifiers: Option<QueryModifiers>,
) -> Result<usize>
pub fn find_by_id(&self, pointer: &str) -> Result<Option<Value>>
pub fn delete_by_id(&self, pointer: &str) -> Result<bool>
pub fn delete_by_filter(&self, drawer_name: &str, filter: Value) -> Result<usize>
pub fn delete<L>(&self, locator: L) -> Result<bool>
where
    L: Into<StorageLocator>
```

`delete` accepts either an inline pointer such as `@user:john` or an explicit locator such as `("user", "john")`.

#### Maintenance, inspection, lifecycle, and recovery

```rust
pub fn vacuum_drawer(&self, drawer_name: &str) -> Result<VacuumReport>
pub fn migrate_drawer(&self, drawer_name: &str) -> Result<VacuumReport>
pub fn inspect_drawer(&self, drawer_name: &str) -> Result<DrawerInspectionMetrics>
pub fn check_path(&self, path: &str) -> Result<CheckReport>
pub fn diagnose_storage(&self) -> Result<StorageDiagnosis>
pub fn list_drawer_names(&self) -> Result<Vec<String>>
pub fn backup_archive(&self, source_path: &str) -> Result<BackupArchive>
pub fn restore_archive(
    &self,
    destination_path: &str,
    archive: BackupArchive,
) -> Result<RestoreReport>
pub fn create_database(&self, database_name: &str) -> Result<StorageInventory>
pub fn create_schema(
    &self,
    database_name: &str,
    schema_name: &str,
) -> Result<StorageInventory>
pub fn create_drawer(
    &self,
    database_name: &str,
    schema_name: &str,
    drawer_name: &str,
) -> Result<StorageInventory>
pub fn register_tenant_route(
    &self,
    tenant_id: &str,
    database_name: &str,
    location: &str,
) -> Result<StorageInventory>
pub fn manage_schema(
    &self,
    drawer_name: &str,
    action: &str,
    kind: &str,
    field_name: &str,
    payload: Value,
) -> Result<Value>
pub fn manage_user(&self, action: &str, payload: Value) -> Result<Value>
```

Notes:

- `manage_schema` is the programmatic surface behind CLI `add` and `remove`
- `manage_user` is the programmatic surface behind CLI `add user`, `grant permission`, and `revoke permission`
- `manage_user` is intentionally remote-only on `WardrobeClient`; embedded targets return `ErrorKind::Unsupported`
- Path-style drawer names such as `inventory/public/tool` are accepted by the structural admin and inspection surfaces

#### Discovery and verification

```rust
pub fn show_tenants(&self) -> Result<Vec<String>>
pub fn list_tenants(&self) -> Result<Vec<String>>
pub fn show_databases(&self) -> Result<Vec<StorageInventory>>
pub fn list_databases(&self) -> Result<Vec<StorageInventory>>
pub fn verify_wal(&self, database_name: Option<&str>) -> Result<WalVerification>
pub fn show_schemas(&self, database_name: &str) -> Result<Vec<String>>
pub fn list_schemas(&self, database_name: &str) -> Result<Vec<String>>
pub fn show_drawers(
    &self,
    database_name: &str,
    schema_name: &str,
) -> Result<Vec<StorageInventory>>
pub fn list_drawers(
    &self,
    database_name: &str,
    schema_name: &str,
) -> Result<Vec<StorageInventory>>
```

`list_*` methods are aliases for the corresponding `show_*` methods.

---

## Engine API

The engine API is the local storage engine surface. It includes the same record, maintenance, inspection, lifecycle, and discovery operations as the client, plus local engine construction, cache inspection, direct command execution, and tenant-scoped execution helpers.

### `WardrobeEngine`

Local filesystem-backed engine facade.

#### Constructors

```rust
pub fn open(directory: &str) -> Result<Self>
pub fn open_with_drawer_cache_limit(
    directory: &str,
    max_cached_drawers: usize,
) -> Result<Self>
pub fn open_with_wal_checkpoint_thresholds(
    directory: &str,
    wal_size_threshold_bytes: u64,
    wal_ops_threshold_count: u64,
) -> Result<Self>
pub fn open_with_drawer_cache_limit_and_wal_checkpoint_thresholds(
    directory: &str,
    max_cached_drawers: usize,
    wal_size_threshold_bytes: u64,
    wal_ops_threshold_count: u64,
) -> Result<Self>
#[deprecated(note = "Use WardrobeEngine::open for filesystem-backed initialization")]
pub fn new(directory: &str) -> Result<Self>
```

#### Record operations

```rust
pub fn upsert(&self, drawer_name: &str, payload: Value) -> Result<String>
pub fn find_all(&self, drawer_name: &str) -> Result<Vec<Value>>
pub fn find_by_filter(
    &self,
    drawer_name: &str,
    filter: Value,
    modifiers: Option<QueryModifiers>,
) -> Result<Vec<Value>>
pub fn count(
    &self,
    drawer_name: &str,
    filter: Option<Value>,
    modifiers: Option<QueryModifiers>,
) -> Result<usize>
pub fn find_by_id(&self, pointer: &str) -> Result<Option<Value>>
pub fn delete<L>(&self, locator: L) -> Result<bool>
where
    L: Into<StorageLocator>
pub fn delete_by_id<L>(&self, locator: L) -> Result<bool>
where
    L: Into<StorageLocator>
pub fn delete_by_filter(&self, drawer_name: &str, filter: Value) -> Result<usize>
```

#### Maintenance, inspection, lifecycle, and recovery

```rust
pub fn vacuum_drawer(&self, drawer_name: &str) -> Result<VacuumReport>
pub fn migrate_drawer(&self, drawer_name: &str) -> Result<VacuumReport>
pub fn manage_schema(
    &self,
    drawer_name: &str,
    action: &str,
    kind: &str,
    field_name: &str,
    payload: Value,
) -> Result<Value>
pub fn inspect_drawer(&self, drawer_name: &str) -> Result<DrawerInspectionMetrics>
pub fn check_path(&self, raw_path: &str) -> Result<CheckReport>
pub fn diagnose_storage(&self) -> Result<StorageDiagnosis>
pub fn list_drawer_names(&self) -> Result<Vec<String>>
pub fn backup_archive(&self, source_path: &str) -> Result<BackupArchive>
pub fn restore_archive(
    &self,
    destination_path: &str,
    archive: BackupArchive,
) -> Result<RestoreReport>
pub fn manage_user(&self, action: &str, payload: Value) -> Result<Value>
pub fn cached_drawer_count(&self) -> Result<usize>
pub fn create_database(&self, database_name: &str) -> Result<StorageInventory>
pub fn create_schema(
    &self,
    database_name: &str,
    schema_name: &str,
) -> Result<StorageInventory>
pub fn create_drawer(
    &self,
    database_name: &str,
    schema_name: &str,
    drawer_name: &str,
) -> Result<StorageInventory>
pub fn register_tenant_route(
    &self,
    tenant_id: &str,
    database_name: &str,
    location: &str,
) -> Result<StorageInventory>
```

Unlike `WardrobeClient`, the engine can execute `manage_user` locally because the server uses the same engine-backed administrative primitive.

#### Discovery, verification, and command execution

```rust
pub fn show_tenants(&self) -> Result<Vec<String>>
pub fn list_tenants(&self) -> Result<Vec<String>>
pub fn show_databases(&self) -> Result<Vec<StorageInventory>>
pub fn list_databases(&self) -> Result<Vec<StorageInventory>>
pub fn verify_wal(&self, database_name: Option<&str>) -> Result<WalVerification>
pub fn show_schemas(&self, database_name: &str) -> Result<Vec<String>>
pub fn list_schemas(&self, database_name: &str) -> Result<Vec<String>>
pub fn show_drawers(
    &self,
    database_name: &str,
    schema_name: &str,
) -> Result<Vec<StorageInventory>>
pub fn list_drawers(
    &self,
    database_name: &str,
    schema_name: &str,
) -> Result<Vec<StorageInventory>>
pub fn execute(
    &self,
    coordinate: StorageCoordinate,
    command: Command,
) -> Result<CommandResult>
pub fn execute_in_scope(&self, scope: StorageScope, command: Command) -> Result<CommandResult>
pub fn execute_for_tenant(
    &self,
    tenant_id: &str,
    database_name: &str,
    schema_name: &str,
    command: Command,
) -> Result<CommandResult>
pub fn execute_command(&self, command: Command) -> Result<CommandResult>
```

`execute`, `execute_in_scope`, `execute_for_tenant`, and `execute_command` all use the shared `Command` / `CommandResult` protocol types that also drive remote server execution.

---

## Shared API Types

These types are used by both the client and engine APIs.

### `QueryModifiers`

Optional query modifiers for filtered reads.

Fields:

- `order_by: Option<String>`
- `order_direction: Option<OrderDirection>`
- `limit: Option<usize>`
- `offset: Option<usize>`

### `OrderDirection`

Sort direction for `QueryModifiers`.

Variants:

- `Ascending`
- `Descending`

### `StorageInventory`

Inventory summary returned by discovery and lifecycle methods.

Fields:

- `name: String`
- `record_count: usize`
- `disk_size_bytes: u64`
- `register_file_count: usize`

### `StorageLocator`

Flexible record locator used by delete APIs.

Variants:

- `Explicit { drawer: String, id: String }`
- `Inline(String)`

Helpers:

```rust
pub fn explicit(drawer: &str, id: &str) -> Self
pub fn inline(locator: &str) -> Self
```

Conversions:

- `&str` -> `StorageLocator::Inline`
- `String` -> `StorageLocator::Inline`
- `&String` -> `StorageLocator::Inline`
- `(&str, &str)` -> `StorageLocator::Explicit`

### `VacuumReport`

Maintenance result returned by `vacuum_drawer` and `migrate_drawer`.

Fields:

- `records_rewritten: usize`
- `data_bytes_before: u64`
- `data_bytes_after: u64`
- `index_bytes_before: u64`
- `index_bytes_after: u64`
- `bytes_reclaimed: u64`

### `WalVerification`

WAL verification result returned by `verify_wal`.

Fields:

- `path: String`
- `entry_count: usize`
- `last_sequence: Option<u64>`

### `DrawerInspectionMetrics`

Inspection summary returned by `inspect_drawer`.

Fields:

- `path: String`
- `data_bytes: u64`
- `index_bytes: u64`
- `meta_bytes: u64`
- `total_bytes: u64`
- `record_count: usize`
- `register_file_count: usize`
- `tombstone_fragmentation_percent: Option<f64>`

### `CheckReport`

Structural presence and sanity report returned by `check_path`.

Fields:

- `path: String`
- `kind: String`
- `entries: Vec<CheckEntry>`

### `CheckEntry`

One physical check result inside a `CheckReport`.

Fields:

- `label: String`
- `path: String`
- `exists: bool`
- `bytes: Option<u64>`

### `StorageDiagnosis`

High-level storage diagnosis returned by `diagnose_storage`.

Fields:

- `storage_directory: String`
- `storage_bytes: u64`
- `drawer_count: usize`
- `status: String`
- `drawers: Vec<String>`

### `BackupArchive`

Archive payload returned by `backup_archive` and consumed by `restore_archive`.

Fields:

- `format: String`
- `source_path: String`
- `scope: String`
- `files: Vec<BackupArchiveFile>`

### `BackupArchiveFile`

One archived file inside a `BackupArchive`.

Fields:

- `path: String`
- `bytes_hex: String`

### `RestoreReport`

Restore summary returned by `restore_archive`.

Fields:

- `destination_path: String`
- `scope: String`
- `file_count: usize`
- `byte_count: usize`

### `ConnectionTarget`

Parsed client connection target.

Variants:

- `EmbeddedPath(PathBuf)`
- `Network { host: String, port: u16 }`
- `UnixSocket { path: PathBuf }`

Helpers:

```rust
pub fn parse(connection_string: &str) -> Result<Self>
pub fn driver_kind(&self) -> DriverKind
pub fn requires_embedded_engine(&self) -> bool
pub fn uses_socket_transport(&self) -> bool
```

### `DriverKind`

Driver category selected from a connection target.

Variants:

- `Embedded`
- `Network`
- `UnixSocket`

Helpers:

```rust
pub fn requires_embedded_engine(self) -> bool
pub fn uses_socket_transport(self) -> bool
```

### `Command`

Shared command enum used by local execution and remote protocol execution.

Variants:

- `ShowTenants`
- `ShowDatabases`
- `VerifyWal { database_name }`
- `ShowSchemas { database_name }`
- `ShowDrawers { database_name, schema_name }`
- `Upsert { drawer_name, payload }`
- `FindAll { drawer_name }`
- `FindById { pointer }`
- `FindByFilter { drawer_name, filter, modifiers }`
- `Count { drawer_name, filter, modifiers }`
- `Delete { pointer }`
- `DeleteByFilter { drawer_name, filter }`
- `Vacuum { drawer_name }`
- `Migrate { drawer_name }`
- `Inspect { drawer_name }`
- `Check { path }`
- `Diagnose`
- `ListDrawers`
- `Backup { source_path }`
- `Restore { destination_path, archive }`
- `DefineDatabase { database_name }`
- `DefineSchema { database_name, schema_name }`
- `DefineDrawer { database_name, schema_name, drawer_name }`
- `DefineTenantRoute { tenant_id, database_name, location }`
- `ManageSchema { action, kind, drawer_name, field_name, payload }`
- `ManageUser { action, payload }`
- `ExecuteForTenant { tenant_id, database_name, schema_name, command }`
- `Execute { coordinate, command }`
- `ExecuteInScope { scope, command }`

### `CommandResult`

Result enum returned by local and remote command execution.

Variants:

- `StorageInventory(StorageInventory)`
- `Tenants(Vec<String>)`
- `Databases(Vec<StorageInventory>)`
- `WalVerification(WalVerification)`
- `Schemas(Vec<String>)`
- `Drawers(Vec<StorageInventory>)`
- `Pointer(String)`
- `Records(Vec<Value>)`
- `Record(Option<Value>)`
- `Count(usize)`
- `Deleted(bool)`
- `Vacuumed(VacuumReport)`
- `Migrated(VacuumReport)`
- `Inspection(DrawerInspectionMetrics)`
- `Check(CheckReport)`
- `Diagnosis(StorageDiagnosis)`
- `DrawerNames(Vec<String>)`
- `Backup(BackupArchive)`
- `Restored(RestoreReport)`
- `Admin(Value)`

---

## Engine-Oriented Supporting Types

These types are primarily useful when working with routed engine execution.

### `StorageCoordinate`

Tenant/database/schema coordinate used by `WardrobeEngine::execute`.

Helpers:

```rust
pub fn new(tenant: &str, database: &str, schema: &str) -> Self
pub fn tenant(&self) -> &str
pub fn database(&self) -> &str
pub fn schema(&self) -> &str
```

### `StorageScope`

Scope selector used by `WardrobeEngine::execute_in_scope`.

Variants:

- `Tenant { tenant_id, database, schema }`
- `Database { database }`
- `Schema { database, schema }`
- `Drawer { namespace }`

Helpers:

```rust
pub fn tenant(
    tenant_id: impl Into<String>,
    database: impl Into<String>,
    schema: impl Into<String>,
) -> Self
pub fn database(database: &str) -> Self
pub fn schema(database: &str, schema: &str) -> Self
pub fn drawer(namespace: &str) -> Self
```
