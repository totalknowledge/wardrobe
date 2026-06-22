# Wardrobe API

This document lists the public API intended for application programmers using Wardrobe through the client or engine entry points.

There are two main API surfaces:

- Client API: use `WardrobeClient` when application code should work across embedded, TCP, and Unix socket targets.
- Engine API: use `WardrobeEngine` when application code is running directly against the local storage engine. The engine API is a superset of the client API.

Most operations return `std::io::Result<T>`.

---

## Client API

The client API is the preferred application boundary for code that may later move between local embedded storage and remote server-backed storage.

### `WardrobeClient`

Primary connection-driven client façade.

#### Constructors and connection metadata

```rust
pub fn open(connection_string: impl AsRef<str>) -> Result<Self>
```

Opens a Wardrobe connection. The connection string selects the driver.

Usage:

```rust
let client = WardrobeClient::open("./data")?;
let remote = WardrobeClient::open("wardrobe://localhost:24842")?;
```

```rust
pub fn connection_target(&self) -> &ConnectionTarget
```

Returns the parsed connection target.

```rust
pub fn driver_kind(&self) -> DriverKind
```

Returns the selected driver kind: embedded, network, or Unix socket.

```rust
pub fn requires_embedded_engine(&self) -> bool
```

Returns `true` when the target uses the local embedded engine.

```rust
pub fn uses_socket_transport(&self) -> bool
```

Returns `true` when the target communicates over TCP or Unix sockets.

#### Record operations

```rust
pub fn upsert(&self, drawer_name: &str, payload: Value) -> Result<String>
```

Creates or updates a record in a drawer and returns its Wardrobe pointer.

Usage:

```rust
let pointer = client.upsert("user", serde_json::json!({
    "_id": "john",
    "name": "John"
}))?;
```

```rust
pub fn find_all(&self, drawer_name: &str) -> Result<Vec<Value>>
```

Returns all live records in a drawer.

```rust
pub fn find_by_filter(
    &self,
    drawer_name: &str,
    filter: Value,
    modifiers: Option<QueryModifiers>,
) -> Result<Vec<Value>>
```

Returns records matching a JSON filter, optionally applying sorting, offset, and limit.

```rust
pub fn count(
    &self,
    drawer_name: &str,
    filter: Option<Value>,
    modifiers: Option<QueryModifiers>,
) -> Result<usize>
```

Counts matching records. Pagination modifiers are accepted for API consistency, but count semantics are based on matching records.

```rust
pub fn find_by_id(&self, pointer: &str) -> Result<Option<Value>>
```

Finds one record by Wardrobe pointer, such as `@user:john`.

```rust
pub fn delete_by_id(&self, pointer: &str) -> Result<bool>
```

Deletes one record by Wardrobe pointer. Returns `true` when a record was deleted.

```rust
pub fn delete<L>(&self, locator: L) -> Result<bool>
where
    L: Into<StorageLocator>
```

Deletes by pointer string or explicit drawer/id locator.

Usage:

```rust
client.delete("@user:john")?;
client.delete(("user", "john"))?;
```

#### Maintenance operations

```rust
pub fn vacuum_drawer(&self, drawer_name: &str) -> Result<VacuumReport>
```

Compacts a drawer and rebuilds its live storage representation.

```rust
pub fn migrate_drawer(&self, drawer_name: &str) -> Result<VacuumReport>
```

Migrates drawer records to the current storage layout.

```rust
pub fn verify_wal(&self, database_name: Option<&str>) -> Result<WalVerification>
```

Verifies the write-ahead log for the root database or a named database.

#### Discovery operations

```rust
pub fn show_tenants(&self) -> Result<Vec<String>>
pub fn list_tenants(&self) -> Result<Vec<String>>
```

Lists known tenant identifiers. `list_tenants` is an alias.

```rust
pub fn show_databases(&self) -> Result<Vec<StorageInventory>>
pub fn list_databases(&self) -> Result<Vec<StorageInventory>>
```

Lists known databases with inventory metadata. `list_databases` is an alias.

```rust
pub fn show_schemas(&self, database_name: &str) -> Result<Vec<String>>
pub fn list_schemas(&self, database_name: &str) -> Result<Vec<String>>
```

Lists schemas for a database. `list_schemas` is an alias.

```rust
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

Lists drawers for a database and schema. `list_drawers` is an alias.

---

## Engine API

The engine API is the local storage-engine API. It includes the same record, maintenance, and discovery operations as the client, plus local engine construction, cache inspection, scoped execution, lifecycle operations, tenant routing, and direct command dispatch.

### `WardrobeEngine`

Local engine façade for filesystem-backed storage.

#### Constructors

```rust
pub fn open(directory: &str) -> Result<Self>
```

Opens or initializes a local Wardrobe storage root.

```rust
let engine = WardrobeEngine::open("./data")?;
```

```rust
pub fn open_with_drawer_cache_limit(
    directory: &str,
    max_cached_drawers: usize,
) -> Result<Self>
```

Opens a local engine with a maximum active drawer cache size.

```rust
pub fn open_with_wal_checkpoint_thresholds(
    directory: &str,
    wal_size_threshold_bytes: u64,
    wal_ops_threshold_count: u64,
) -> Result<Self>
```

Opens a local engine with custom automatic WAL checkpoint thresholds. A checkpoint is triggered when either threshold is reached.

```rust
pub fn open_with_drawer_cache_limit_and_wal_checkpoint_thresholds(
    directory: &str,
    max_cached_drawers: usize,
    wal_size_threshold_bytes: u64,
    wal_ops_threshold_count: u64,
) -> Result<Self>
```

Opens a local engine with both drawer cache and WAL checkpoint thresholds configured.

```rust
#[deprecated(note = "Use WardrobeEngine::open for filesystem-backed initialization")]
pub fn new(directory: &str) -> Result<Self>
```

Deprecated alias for `open`.

#### Client-equivalent record operations

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
```

These methods mirror the client record API but execute directly against the local engine.

#### Client-equivalent maintenance and discovery

```rust
pub fn vacuum_drawer(&self, drawer_name: &str) -> Result<VacuumReport>
pub fn migrate_drawer(&self, drawer_name: &str) -> Result<VacuumReport>
pub fn verify_wal(&self, database_name: Option<&str>) -> Result<WalVerification>
pub fn show_tenants(&self) -> Result<Vec<String>>
pub fn list_tenants(&self) -> Result<Vec<String>>
pub fn show_databases(&self) -> Result<Vec<StorageInventory>>
pub fn list_databases(&self) -> Result<Vec<StorageInventory>>
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

These methods mirror the client maintenance and discovery API.

```rust
pub fn cached_drawer_count(&self) -> Result<usize>
```

Returns the number of drawers currently held in the engine drawer cache.

#### Scoped and routed execution

```rust
pub fn execute(
    &self,
    coordinate: StorageCoordinate,
    command: Command,
) -> Result<CommandResult>
```

Executes a command inside a tenant/database/schema coordinate.

```rust
let coordinate = StorageCoordinate::new("tenant_a", "production", "public");
let result = engine.execute(coordinate, Command::FindAll {
    drawer_name: "user".to_string(),
})?;
```

```rust
pub fn execute_in_scope(&self, scope: StorageScope, command: Command) -> Result<CommandResult>
```

Executes a command in a database, schema, drawer namespace, or tenant scope.

```rust
pub fn execute_for_tenant(
    &self,
    tenant_id: &str,
    database_name: &str,
    schema_name: &str,
    command: Command,
) -> Result<CommandResult>
```

Executes a command using a tenant route registered in the catalog.

```rust
pub fn execute_command(&self, command: Command) -> Result<CommandResult>
```

Executes a top-level command directly against the engine boundary.

#### Lifecycle operations

```rust
pub fn create_database(&self, database_name: &str) -> Result<StorageInventory>
```

Creates a database and records it in the catalog.

```rust
pub fn create_schema(
    &self,
    database_name: &str,
    schema_name: &str,
) -> Result<StorageInventory>
```

Creates a schema inside a database and records it in the catalog.

```rust
pub fn create_drawer(
    &self,
    database_name: &str,
    schema_name: &str,
    drawer_name: &str,
) -> Result<StorageInventory>
```

Creates a drawer inside a database/schema and records it in the catalog.

```rust
pub fn register_tenant_route(
    &self,
    tenant_id: &str,
    database_name: &str,
    location: &str,
) -> Result<StorageInventory>
```

Registers a tenant route to a database storage location.

---

## Shared API Types

These types are used by both the client and engine APIs.

### `QueryModifiers`

Optional query modifiers for filtered reads.

Fields:

- `order_by: Option<String>`: field name to sort by
- `order_direction: Option<OrderDirection>`: ascending or descending
- `limit: Option<usize>`: maximum records returned
- `offset: Option<usize>`: records to skip

Usage:

```rust
let modifiers = QueryModifiers {
    order_by: Some("name".to_string()),
    order_direction: Some(OrderDirection::Ascending),
    limit: Some(10),
    offset: Some(0),
};
```

### `OrderDirection`

Sort direction for `QueryModifiers`.

Variants:

- `Ascending`
- `Descending`

### `StorageInventory`

Inventory summary returned by database and drawer discovery methods.

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

Functions:

```rust
pub fn explicit(drawer: &str, id: &str) -> Self
pub fn inline(locator: &str) -> Self
```

Conversions:

- `&str` becomes `StorageLocator::Inline`
- `String` becomes `StorageLocator::Inline`
- `&String` becomes `StorageLocator::Inline`
- `(&str, &str)` becomes `StorageLocator::Explicit`

### `VacuumReport`

Maintenance result returned by `vacuum_drawer` and `migrate_drawer`.

Fields:

- `drawer_name: String`
- `records_rewritten: usize`
- `data_bytes_before: u64`
- `data_bytes_after: u64`
- `index_bytes_before: u64`
- `index_bytes_after: u64`
- `bytes_reclaimed: u64`

### `WalVerification`

WAL verification result returned by `verify_wal`.

Fields:

- `entry_count: usize`
- `last_sequence: Option<u64>`
- `path: String`

### `ConnectionTarget`

Parsed client connection target.

Variants:

- `EmbeddedPath(PathBuf)`
- `Network { host: String, port: u16 }`
- `UnixSocket { path: PathBuf }`

Functions:

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

Functions:

```rust
pub fn requires_embedded_engine(self) -> bool
pub fn uses_socket_transport(self) -> bool
```

### `Command`

Engine command enum used by `execute`, `execute_in_scope`, `execute_for_tenant`, and `execute_command`.

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
- `Vacuum { drawer_name }`
- `Migrate { drawer_name }`
- `DefineDatabase { database_name }`
- `DefineSchema { database_name, schema_name }`
- `DefineDrawer { database_name, schema_name, drawer_name }`
- `DefineTenantRoute { tenant_id, database_name, location }`
- `ExecuteForTenant { tenant_id, database_name, schema_name, command }`
- `Execute { coordinate, command }`
- `ExecuteInScope { scope, command }`

### `CommandResult`

Result enum returned by command execution.

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

---

## Engine-Only Supporting Types

These types are primarily useful with scoped engine execution.

### `StorageCoordinate`

Tenant/database/schema coordinate used by `WardrobeEngine::execute`.

Functions:

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

Functions:

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
