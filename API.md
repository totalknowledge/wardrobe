# Wardrobe API

This document describes the canonical public Rust API exposed by wardrobe-core.

The stable operation vocabulary is:

- read
- upsert
- delete
- inspect
- count
- compact
- create
- alter
- drop
- backup
- restore
- grant
- revoke
- status

The public API should use the same operation names as the CLI and server command protocol. Internal helper functions may use more specific names, but public-facing Rust methods, CLI commands, and command variants should follow this vocabulary.

## Entry Points

WardrobeEngine executes directly against an embedded storage root.

WardrobeClient selects embedded, TCP, or Unix socket transport from a connection string while preserving the same high-level operation model.

```rust
impl WardrobeEngine {
pub fn open(directory: impl AsRef<str>) -> Result<Self>
}

impl WardrobeClient {
pub fn open(connection_string: impl AsRef<str>) -> Result<Self>
}
```

WardrobeClient also exposes connection metadata:

```rust
pub fn connection_target(&self) -> &ConnectionTarget
pub fn driver_kind(&self) -> DriverKind
pub fn requires_embedded_engine(&self) -> bool
pub fn uses_socket_transport(&self) -> bool
```

## Shared Operation Shape

The canonical data-operation shape is:

```rust
read(filter, options)
upsert(payload, filter, options)
delete(filter, options)
inspect(filter, options)
count(filter, options)
```

Where:

- payload is the data being written.
- filter identifies a drawer, record, candidate key, query criteria, or structural target.
- options modifies execution behavior, return shape, safety rules, traversal, hydration, sorting, limits, or diagnostic depth.

upsert is the only RUDIC operation whose first argument is payload.

read, delete, inspect, and count use filter as their first argument.

options is always optional.

## Record Operations

```rust
pub fn upsert<P, F, O>(&self, payload: P, filter: F, options: O) -> Result<UpsertResult>
where
P: Into<Value>,
F: Into<OperationFilter>,
O: Into<OperationOptions>

pub fn read<F, O>(&self, filter: F, options: O) -> Result<ReadResult>
where
F: Into<OperationFilter>,
O: Into<OperationOptions>

pub fn delete<F, O>(&self, filter: F, options: O) -> Result<DeleteResult>
where
F: Into<OperationFilter>,
O: Into<OperationOptions>

pub fn inspect<F, O>(&self, filter: F, options: O) -> Result<InspectResult>
where
F: Into<OperationFilter>,
O: Into<OperationOptions>

pub fn count<F, O>(&self, filter: F, options: O) -> Result<usize>
where
F: Into<OperationFilter>,
O: Into<OperationOptions>
```

Convenience constructors may be provided, but the public model should remain payload, filter, and options.

## Upsert

upsert accepts either a single JSON object or a JSON array of objects.

```rust
client.upsert(
json!({"_id": "@book:123", "title": "The Hobbit"}),
OperationFilter::none(),
None::<OperationOptions>,
)?;
```

```rust
client.upsert(
json!({"title": "The Hobbit"}),
OperationFilter::drawer("book"),
None::<OperationOptions>,
)?;
```

```rust
client.upsert(
json!([
{"_id": "@book:123", "title": "The Hobbit"},
{"_id": "@book:456", "title": "Dune"}
]),
OperationFilter::none(),
None::<OperationOptions>,
)?;
```

upsert returns stored pointers in input order.

```rust
pub enum UpsertResult {
Pointers(Vec<String>),
}
```

A single-object upsert still returns a one-item pointer list.

Wardrobe should not expose a separate public bulk_upsert method. Array payloads are bulk upserts.

## Pointer And ID Resolution

Wardrobe pointer parsing must distinguish:

```text
@book drawer-only pointer
@book:123 fully qualified record pointer
123 local record id
```

Internal parsing should use a focused parser, not inline ad hoc string handling inside upsert.

Suggested internal shape:

```rust
pub enum ParsedPointer {
DrawerOnly { drawer: String },
FullPointer { drawer: String, id: String },
LocalId { id: String },
}
```

Resolution rules:

- _id: "@book:123" means update or create record 123 in drawer book.
- _id: "@book" means create a new record in drawer book.
- _id: "123" requires a drawer from the filter and means update or create record 123 in that drawer.
- Missing _id requires a drawer from the filter and means create a new record.
- Missing _id with no drawer context is invalid.
- Conflicting drawer information between payload and filter is invalid unless an explicit future option defines override behavior.

## OperationFilter

OperationFilter is the canonical target/filter input for read, upsert, delete, inspect, and count.

It may represent:

- no filter
- drawer target
- record pointer target
- candidate-key filter
- query filter
- multiple filters

Suggested public shape:

```rust
pub enum OperationFilter {
None,
Drawer(String),
Pointer(String),
Query(Value),
Many(Vec<OperationFilter>),
}
```

Common constructors:

```rust
OperationFilter::none()
OperationFilter::drawer("book")
OperationFilter::pointer("@book:123")
OperationFilter::query(json!({"isbn": "9780000000000"}))
OperationFilter::many(vec![
OperationFilter::pointer("@book:123"),
OperationFilter::pointer("@book:456"),
])
```

Supported JSON-equivalent filter shapes:

```json
"@book:123"
```

```json
"@book"
```

```json
"book"
```

```json
{ "_id": "@book:123" }
```

```json
{ "drawer": "book" }
```

```json
{ "isbn": "9780000000000" }
```

```json
[
"@book:123",
"@book:456"
]
```

```json
{}
```

```json
[]
```

Rules:

- String full pointer means exact record target.
- String drawer pointer means drawer target.
- Object with _id means exact or partial pointer target.
- Object with drawer means drawer target.
- Object without _id or drawer means query/candidate filter.
- Array means multiple filters.
- Empty object means no filter.
- Empty array means no filter.
- Empty filter is valid only when the operation can infer enough context elsewhere.

## OperationOptions

OperationOptions modifies execution without changing the operation verb.

Suggested initial shape:

```rust
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
```

Suggested JSON equivalent:

```json
{
"multi": false,
"atomic": true,
"create_if_missing": true,
"return_shape": "default",
"hydrate": true,
"limit": 100,
"offset": 0,
"order_by": "name",
"order_direction": "asc",
"include_diagnostics": false
}
```

Rules:

- Missing options use documented defaults.
- Unknown options should produce a clear invalid input error.
- Options must be normalized before execution reaches drawer-level operations.

## Read

read retrieves records, record pointers, or existence-style results depending on options.

Examples:

```rust
client.read(OperationFilter::drawer("book"), None)?;
client.read(OperationFilter::pointer("@book:123"), None)?;
client.read(
OperationFilter::query_in("book", json!({"author": "Tolkien"})),
None::<OperationOptions>,
)?;
```

With options:

```rust
client.read(
OperationFilter::query_in("book", json!({"author": "Tolkien"})),
OperationOptions::new()
.limit(25)
.offset(0)
.order_by("title")
.order_direction(OrderDirection::Ascending),
)?;
```

Suggested result shape:

```rust
pub enum ReadResult {
Records(Vec<Value>),
Record(Option<Value>),
Pointers(Vec<String>),
Exists(bool),
}
```

Rules:

- Full record pointer returns ReadResult::Record.
- Drawer target returns ReadResult::Records.
- Query filter returns ReadResult::Records.
- Options may request pointers instead of hydrated records.
- Hydration should default to the current public behavior unless explicitly changed by options.

## Upsert Rules

Single object:

- Object with _id: "@book:123" can resolve drawer and record without filter.
- Object with _id: "@book" can resolve drawer and create a new record.
- Object with _id: "123" requires a drawer filter.
- Object with no _id requires a drawer filter.

Array:

- Array with drawer filter applies that drawer to unqualified items.
- Array without drawer filter requires every item to resolve its own drawer.
- Mixed-drawer arrays are allowed when every item can resolve a drawer.
- Mixed-drawer arrays should preserve one logical operation boundary.
- If any item cannot resolve a drawer, the whole operation is invalid.

Candidate-key filter:

- If matching records exist, update matching records according to multi.
- If no records match, create only when create_if_missing permits it.
- If multiple records match and multi is false, return an error.

## Delete

delete removes records only. It does not remove wardrobes, bays, drawers, indexes, rules, users, permissions, or other structures.

Examples:

```rust
client.delete(OperationFilter::pointer("@book:123"), None)?;
client.delete(
OperationFilter::query_in("book", json!({"author": "Tolkien"})),
OperationOptions::new().multi(true),
)?;
```

Suggested result shape:

```rust
pub struct DeleteResult {
pub deleted: usize,
}
```

Rules:

- Full record pointer deletes one record.
- Query filter deletes matching records according to safety options.
- Drawer-only target without a query is rejected unless a future explicit destructive option is added.
- Delete rules, cascades, restrict, and set-null behavior must remain equivalent to deleting the same records individually.
- delete returns the exact number of records removed.

## Inspect

inspect returns diagnostic information about records, filters, drawers, storage, indexes, metadata, or execution plans.

Examples:

```rust
client.inspect(OperationFilter::drawer("book"), None)?;
client.inspect(OperationFilter::pointer("@book:123"), None::<OperationOptions>)?;
client.inspect(
OperationFilter::query_in("book", json!({"author": "Tolkien"})),
None::<OperationOptions>,
)?;
```

Suggested result shape:

```rust
pub enum InspectResult {
Drawer(DrawerInspectionMetrics),
Record(Value),
Query(QueryInspection),
Storage(StorageDiagnosis),
Path(CheckReport),
}
```

Rules:

- Drawer target returns drawer/storage/index/metadata diagnostics.
- Record pointer may return record-level diagnostics where supported.
- Query filter may return query path, index usage, candidate count, or storage impact.
- Inspect must not mutate records.

## Count

count returns the number of records matching a drawer, pointer, or query filter.

Examples:

```rust
client.count(OperationFilter::drawer("book"), None)?;
client.count(
OperationFilter::query_in("book", json!({"author": "Tolkien"})),
None::<OperationOptions>,
)?;
```

Rules:

- Drawer target counts records in that drawer.
- Query filter counts matching records.
- Pointer target returns 1 if present and 0 if missing.
- Count should use indexed candidate paths where possible.

## Internal Normalization

Before executing any RUDIC operation, Wardrobe should normalize payload, filter, and options into an explicit operation plan.

Suggested internal shape:

```rust
pub struct NormalizedOperation {
pub verb: OperationVerb,
pub payload: Option<Value>,
pub targets: Vec<ResolvedTarget>,
pub filter: Option<Value>,
pub options: OperationOptions,
}
```

```rust
pub enum OperationVerb {
Read,
Upsert,
Delete,
Inspect,
Count,
}
```

```rust
pub enum ResolvedTarget {
Drawer { drawer: String },
Record { drawer: String, id: String },
Query { drawer: Option<String>, filter: Value },
}
```

Rules:

- Public methods should normalize early.
- Drawer-level storage methods should receive explicit drawer/id/filter values.
- Pointer parsing and drawer inference should not be duplicated across command handlers.
- CLI, client, engine, and server protocol should all converge on the same normalized operation model.

## Structural Operations

Structural operations use CAD:

```rust
pub fn create<C: Into<CreateRequest>>(&self, request: C) -> Result<CreateResult>
pub fn alter<A: Into<AlterRequest>>(&self, request: A) -> Result<AlterResult>
pub fn drop<D: Into<DropRequest>>(&self, request: D) -> Result<DropResult>
```

create creates structures and administrative resources.

alter modifies structures, schema rules, metadata, and administrative state.

drop removes structures and administrative resources.

delete is reserved for records.

## Create Requests

Common creation requests:

```rust
CreateRequest::wardrobe("catalog")
CreateRequest::bay("catalog", "public")
CreateRequest::drawer("catalog", "public", "book")
CreateRequest::tenant_route("tenant_a", "catalog", "tenant_a/catalog/public")
CreateRequest::user(json!({"username": "admin"}))
```

Compatibility note:

The Rust internals may still use database/schema terminology, but public user-facing API docs should prefer wardrobe/bay/drawer unless a lower-level internal type requires otherwise.

Suggested shape:

```rust
pub enum CreateRequest {
Wardrobe { name: String },
Bay { wardrobe: String, bay: String },
Drawer { wardrobe: String, bay: String, drawer: String },
TenantRoute { tenant_id: String, wardrobe: String, location: String },
User { payload: Value },
}
```

## Alter Requests

Schema rule changes use alter.

Examples:

```rust
AlterRequest::schema_rule(
"book",
"index",
"author_id",
json!({"kind": "index"})
)
```

```rust
AlterRequest::schema_rule(
"book",
"constraint",
"isbn",
json!({"kind": "unique"})
)
```

Suggested shape:

```rust
pub enum AlterRequest {
SchemaRule {
drawer: String,
kind: String,
field: String,
payload: Value,
},
User {
username: String,
payload: Value,
},
}
```

Rules:

- alter replaces public manage_schema.
- alter should cover indexes, keys, constraints, triggers, relationships, and cascade-delete rules.
- alter should not directly mutate records.

## Drop Requests

Examples:

```rust
DropRequest::wardrobe("catalog")
DropRequest::bay("catalog", "public")
DropRequest::drawer("catalog", "public", "book")
DropRequest::schema_rule("book", "index", "author_id", json!({}))
DropRequest::user("admin")
```

Suggested shape:

```rust
pub enum DropRequest {
Wardrobe { name: String },
Bay { wardrobe: String, bay: String },
Drawer { wardrobe: String, bay: String, drawer: String },
SchemaRule {
drawer: String,
kind: String,
field: String,
payload: Value,
},
User { username: String },
}
```

Rules:

- drop removes structures or admin resources.
- drop must not be used for record deletion.
- delete must not be used for structural deletion.

## Maintenance And Recovery

```rust
pub fn compact<C: Into<CompactRequest>>(&self, request: C) -> Result<CompactResult>
pub fn backup<B: Into<BackupRequest>>(&self, request: B) -> Result<BackupArchive>
pub fn restore<R: Into<RestoreRequest>>(&self, request: R) -> Result<RestoreReport>
```

compact replaces public vacuum_drawer, migrate_drawer, and CLI clean.

Compaction examples:

```rust
CompactRequest::drawer("book")
CompactRequest::drawer_with_mode("book", CompactMode::Migrate)
CompactRequest::bay("catalog", "public")
CompactRequest::wardrobe("catalog")
```

Suggested shape:

```rust
pub enum CompactRequest {
Drawer { drawer: String, mode: CompactMode },
Bay { wardrobe: String, bay: String, mode: CompactMode },
Wardrobe { wardrobe: String, mode: CompactMode },
}
```

```rust
pub enum CompactMode {
Vacuum,
Migrate,
}
```

Backup and restore examples:

```rust
client.backup(BackupRequest::path("catalog/public"))?;
client.restore(RestoreRequest::path("catalog/public", archive))?;
```

## Administration

```rust
pub fn grant(&self, request: PermissionRequest) -> Result<PermissionResult>
pub fn revoke(&self, request: PermissionRequest) -> Result<PermissionResult>
```

Example:

```rust
PermissionRequest::new("admin", "catalog/public:rud")
```

Suggested shape:

```rust
pub struct PermissionRequest {
pub username: String,
pub scope: String,
}
```

Rules:

- User creation uses create(CreateRequest::user(...)).
- User removal uses drop(DropRequest::user(...)).
- Permission grant uses grant.
- Permission revoke uses revoke.
- Public manage_user should be removed or made internal.

## Status

```rust
pub fn status<S: Into<StatusRequest>>(&self, request: S) -> Result<StatusResult>
```

Common status requests:

```rust
StatusRequest::tenants()
StatusRequest::wardrobes()
StatusRequest::bays("catalog")
StatusRequest::drawers("catalog", "public")
StatusRequest::wal(None::<String>)
StatusRequest::storage()
StatusRequest::path("catalog/public/book")
StatusRequest::drawer_names()
StatusRequest::cached_drawer_count()
StatusRequest::server()
StatusRequest::config()
```

Suggested shape:

```rust
pub enum StatusRequest {
Tenants,
Wardrobes,
Bays { wardrobe: String },
Drawers { wardrobe: String, bay: String },
Wal { wardrobe: Option<String> },
Storage,
Path { path: String },
DrawerNames,
CachedDrawerCount,
Server,
Config,
}
```

StatusResult variants include tenants, wardrobes, bays, drawers, WAL verification, storage diagnosis, path checks, drawer names, cached drawer count, server status, and resolved config.

Suggested shape:

```rust
pub enum StatusResult {
Tenants(Vec<String>),
Wardrobes(Vec<StorageInventory>),
Bays(Vec<String>),
Drawers(Vec<StorageInventory>),
Wal(WalVerification),
Storage(StorageDiagnosis),
Path(CheckReport),
DrawerNames(Vec<String>),
CachedDrawerCount(usize),
Server(Value),
Config(Value),
}
```

## Configuration

Wardrobe should expose a shared configuration model used by server, CLI, and embedded engine.

The server may load TOML configuration files.

The embedded library should accept config structs/builders directly and should not automatically read global config files.

Suggested entry points:

```rust
pub fn open_with_config(config: WardrobeConfig) -> Result<Self>
pub fn builder() -> WardrobeEngineBuilder
```

Suggested builder style:

```rust
WardrobeEngine::builder()
.directory("./data")
.max_cached_drawers(128)
.durability(DurabilityPolicy::Strict)
.open()?;
```

Suggested config surface:

```rust
pub struct WardrobeConfig {
pub data: DataConfig,
pub network: NetworkConfig,
pub cache: CacheConfig,
pub wal: WalConfig,
pub transactions: TransactionConfig,
pub security: SecurityConfig,
pub logging: LoggingConfig,
}
```

TOML example:

```toml
[data]
directory = "./data"

[network]
tcp_enabled = true
tcp_bind = "127.0.0.1:24842"
unix_socket_enabled = false
unix_socket = "/tmp/wardrobe.sock"

[cache]
max_cached_drawers = 128

[wal]
durability = "strict"
checkpoint_size_bytes = 1048576
checkpoint_ops = 1000

[transactions]
enabled = true
log_directory = "./data/.transactions"
recovery = "automatic"

[security]
access_control_file = "_wardrobe_access_control.json"
auth_required = false

[logging]
level = "info"
format = "pretty"
destination = "stderr"
file = "./logs/wardrobe.log"
```

## Logging

Wardrobe distinguishes three log-like systems:

- WAL: durable command/write recovery log.
- Transaction log: atomicity and transaction recovery artifact.
- Application log: operator-facing observability stream.

Application logging should use a structured logging stack such as tracing.

Logging configuration should control:

- level
- format
- destination
- file path
- module filters

Application logs must not be used for recovery.

WAL and transaction logs must not be treated as operator-facing application logs.

Sensitive payload values, credentials, tokens, and secrets should not be logged by default.
