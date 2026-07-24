# Wardrobe API

This document describes the canonical public Rust API exposed by `wardrobe-core` in workspace release `0.26.725`.

The current operation vocabulary is:

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

The command protocol is still pre-stable. Serialized `Command` and `CommandResult` payloads use the canonical names above, and removed protocol names such as `FindAll`, `FindById`, `FindByFilter`, `DeleteByFilter`, `BulkUpsert`, `Vacuum`, and `Migrate` are not compatibility aliases.

## Entry Points

WardrobeEngine executes directly against an embedded storage root.

WardrobeClient selects embedded, TCP, or Unix socket transport from a connection string while preserving the same high-level operation model.

```rust
impl WardrobeEngine {
pub fn open(directory: &str) -> Result<Self>
pub fn open_with_config(config: WardrobeConfig) -> Result<Self>
pub fn builder() -> WardrobeEngineBuilder
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

Convenience conversions are implemented, but the public model remains payload, filter, and options.

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

Internal parsing is centralized so upsert, read, delete, routing, and hydration use the same pointer semantics.

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

Current public shape:

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

Current public shape:

```rust
pub struct OperationOptions {
pub multi: Option<bool>,
pub atomic: Option<bool>,
pub create_if_missing: Option<bool>,
pub return_shape: Option<ReturnShape>,
pub hydrate: Option<bool>,
pub exclude_hydration: Option<Vec<String>>,
pub projection: Option<Vec<String>>,
pub limit: Option<usize>,
pub offset: Option<usize>,
pub order_by: Option<String>,
pub order_direction: Option<OrderDirection>,
pub cursor: Option<String>,
pub page: Option<usize>,
pub page_size: Option<usize>,
pub include_diagnostics: Option<bool>,
}
```

Serialized JSON equivalent:

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
"cursor": null,
"page": 1,
"page_size": 25,
"include_diagnostics": false
}
```

Rules:

- Missing options use documented defaults.
- Unknown options should produce a clear invalid input error.
- Options must be normalized before execution reaches drawer-level operations.
- Cursor and page pagination require `order_by` so ordering is deterministic; `_id` is the stable tie-breaker.
- `page` is one-based and `page_size` defaults to `100` when either cursor or page pagination is requested.
- Cursor/page pagination cannot be combined with `limit` or `offset`.

## Read

read retrieves records, record pointers, or existence-style results depending on options.

Examples:

```rust
client.read(OperationFilter::drawer("book"), None::<OperationOptions>)?;
client.read(OperationFilter::pointer("@book:123"), None::<OperationOptions>)?;
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

Page-based navigation:

```rust
let page = client.read(
OperationFilter::query_in("book", json!({"author": "Tolkien"})),
OperationOptions::new()
.order_by("title")
.page(1)
.page_size(25),
)?;
```

Current result shape:

```rust
pub enum ReadResult {
Records(Vec<Value>),
Page(PaginatedReadResult),
Record(Option<Value>),
Pointers(Vec<String>),
Exists(bool),
}
```

`PaginatedReadResult` contains `records` and `pagination`. The metadata includes `next_cursor`, `has_more`, the effective page when page navigation was used, and `page_size`. Pass `next_cursor` back with the same `order_by` and `order_direction` to continue a cursor traversal. `count` applies the same query modifiers and returns the number of records in the selected page.

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
client.delete(OperationFilter::pointer("@book:123"), None::<OperationOptions>)?;
client.delete(
OperationFilter::query_in("book", json!({"author": "Tolkien"})),
OperationOptions::new().multi(true),
)?;
```

Current result shape:

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
client.inspect(OperationFilter::drawer("book"), None::<OperationOptions>)?;
client.inspect(OperationFilter::pointer("@book:123"), None::<OperationOptions>)?;
client.inspect(
OperationFilter::query_in("book", json!({"author": "Tolkien"})),
None::<OperationOptions>,
)?;
```

Current result shape:

```rust
pub enum InspectResult {
Drawer(DrawerInspectionMetrics),
Record(Option<Value>),
Query(Vec<Value>),
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
client.count(OperationFilter::drawer("book"), None::<OperationOptions>)?;
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

## Execution Normalization

Before executing a RUDIC operation, Wardrobe normalizes filters and options into an explicit internal selection.

Rules:

- Public methods should normalize early.
- Drawer-level storage methods should receive explicit drawer/id/filter values.
- Pointer parsing and drawer inference should not be duplicated across command handlers.
- CLI, client, engine, and server protocol should all converge on the same normalized operation model.

## Structural Operations

Structural operations use CAD:

```rust
pub fn create<C: Into<CreateRequest>>(&self, request: C) -> Result<CreateResult>
pub fn alter<A: Into<AlterRequest>>(&self, request: A) -> Result<Value>
pub fn drop<D: Into<DropRequest>>(&self, request: D) -> Result<Value>
```

create creates structures and administrative resources.

alter modifies structures, schema rules, metadata, and administrative state.

drop removes structures and administrative resources.

delete is reserved for records.

## Create Requests

Common creation requests:

```rust
CreateRequest::database("catalog")
CreateRequest::schema("catalog", "public")
CreateRequest::drawer("catalog", "public", "book")
CreateRequest::tenant_route("tenant_a", "catalog", "tenant_a/catalog/public")
CreateRequest::user(json!({"username": "admin"}))
```

Terminology note: Rust request types use database/schema/drawer. The CLI presents the same structure as wardrobe/bay/drawer.

Current shape:

```rust
pub enum CreateRequest {
Database { database_name: String },
Schema { database_name: String, schema_name: String },
Drawer { database_name: String, schema_name: String, drawer_name: String },
TenantRoute { tenant_id: String, database_name: String, location: String },
User { payload: Value },
}
```

Creation returns `CreateResult::StorageInventory` for structural resources and `CreateResult::Admin` for administrative resources.

## Alter Requests

Schema rule changes use alter.

Examples:

```rust
AlterRequest::schema_rule(
"book",
"alter",
"index",
"author_id",
json!({"kind": "index"})
)
```

```rust
AlterRequest::schema_rule(
"book",
"alter",
"constraint",
"isbn",
json!({"kind": "unique"})
)
```

Current shape:

```rust
pub enum AlterRequest {
SchemaRule {
drawer_name: String,
action: String,
kind: String,
field_name: String,
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
DropRequest::database("catalog")
DropRequest::schema("catalog", "public")
DropRequest::drawer("catalog", "public", "book")
DropRequest::schema_rule("book", "index", "author_id", json!({}))
DropRequest::user("admin")
```

Current shape:

```rust
pub enum DropRequest {
Database { database_name: String },
Schema { database_name: String, schema_name: String },
Drawer { database_name: String, schema_name: String, drawer_name: String },
SchemaRule {
drawer_name: String,
kind: String,
field_name: String,
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
pub fn compact<C: Into<CompactRequest>>(&self, request: C) -> Result<VacuumReport>
pub fn backup(&self, source_path: &str) -> Result<BackupArchive>
pub fn restore(&self, destination_path: &str, archive: BackupArchive) -> Result<RestoreReport>
```

`compact` is the Rust maintenance operation. The CLI accepts a wardrobe, bay, or drawer path and fans broader scopes out to drawer compaction calls.

Compaction examples:

```rust
CompactRequest::drawer("book")
CompactRequest::drawer_with_mode("book", CompactMode::Migrate)
```

Current shape:

```rust
pub enum CompactRequest {
Drawer { drawer_name: String, mode: CompactMode },
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
let archive = client.backup("catalog/public")?;
client.restore("catalog/public-copy", archive)?;
```

## Administration

```rust
pub fn grant(&self, request: PermissionRequest) -> Result<Value>
pub fn revoke(&self, request: PermissionRequest) -> Result<Value>
```

Example:

```rust
PermissionRequest::new("admin", "catalog/public:rud")
```

Current shape:

```rust
pub struct PermissionRequest {
pub username: String,
pub permission_scope: String,
pub scope: Option<PermissionScopeDescriptor>,
}

pub struct PermissionScopeDescriptor {
pub path: String,
pub rights: String,
}
```

Rules:

- User creation uses create(CreateRequest::user(...)).
- User removal uses drop(DropRequest::user(...)).
- Permission grant uses grant.
- Permission revoke uses revoke.
- Embedded `WardrobeClient` calls reject server access-control administration.

## Status

```rust
pub fn status<S: StatusRequestOutput>(&self, request: S) -> Result<S::Output>
```

Common status requests:

```rust
StatusRequest::tenants()
StatusRequest::databases()
StatusRequest::schemas("catalog")
StatusRequest::drawers("catalog", "public")
StatusRequest::wal(None::<String>)
StatusRequest::storage()
StatusRequest::path("catalog/public/book")
StatusRequest::drawer_names()
StatusRequest::cached_drawer_count()
```

Current request variants:

```rust
pub enum StatusRequest {
Tenants,
Databases,
Schemas { database_name: String },
Drawers { database_name: String, schema_name: String },
Wal { database_name: Option<String> },
Storage,
Path { path: String },
DrawerNames,
CachedDrawerCount,
}
```

Typed status requests return their payloads directly:

```text
tenants             -> Vec<String>
databases           -> Vec<StorageInventory>
schemas             -> Vec<String>
drawers             -> Vec<StorageInventory>
wal                 -> WalVerification
storage             -> StorageDiagnosis
path                -> CheckReport
drawer_names        -> Vec<String>
cached_drawer_count -> usize
```

The serialized command result uses a raw status payload, such as
`{"status":[...]}`, without `Databases`, `Schemas`, or `Drawers` variant tags.
Passing a raw `StatusRequest` rather than a typed constructor returns `serde_json::Value` for low-level protocol integration.

## Language Binding Status Contract

JavaScript and TypeScript use `@wardrobe/client` for server connections and `@wardrobe/embedded` for local storage. Both packages expose `WardrobeClient` and accept the same serialized status requests:

```javascript
const databases = await wardrobe.status('Databases');
const schemas = await wardrobe.status({ Schemas: { database_name: 'catalog' } });
const drawers = await wardrobe.status({
  Drawers: { database_name: 'catalog', schema_name: 'public' }
});
```

TypeScript declares these as `StorageInventory[]`, `string[]`, and `StorageInventory[]` respectively. No result-side `Databases`, `Schemas`, or `Drawers` property is present.

Python uses `wardrobe-client` for server connections and `wardrobe-embedded` for local storage:

```python
databases = wardrobe.status("Databases")
schemas = wardrobe.status({"Schemas": {"database_name": "catalog"}})
drawers = wardrobe.status(
    {"Drawers": {"database_name": "catalog", "schema_name": "public"}}
)
```

Each value is returned as a Python list directly. The tagged objects above select the requested operation; they do not wrap the response payload.

## Storage Format Compatibility

`wardrobe-core` publicly exports `StorageFormat`, `BsonBinaryFormat`, and `NativeBinaryIndexFormat` for lower-level integrations and tests.

- Current record writes use version-2 WRDB frames with a field-count header, presence bitmap, and typed positional values resolved through the drawer field-name map.
- Version-1 BSON-backed WRDB records remain readable.
- Current index writes use compact WIDX native binary frames; older BSON-backed index entries remain readable.
- Compaction rewrites live records and index entries into the current formats.
- Materialized secondary indexes provide ordered equality and numeric/string range candidates while preserving duplicate-key offsets.

## Configuration

Wardrobe exposes a shared configuration model used by server, CLI, and embedded engine.

The server loads TOML configuration files through `WardrobeConfig::from_toml_file` or `WardrobeConfig::from_toml_str`.

The embedded library accepts config structs and builders directly and does not automatically read global config files.

Current entry points:

```rust
pub fn open_with_config(config: WardrobeConfig) -> Result<Self>
pub fn builder() -> WardrobeEngineBuilder
```

Builder example:

```rust
WardrobeEngine::builder()
.directory("./data")
.max_cached_drawers(128)
.durability(DurabilityPolicy::Strict)
.open()?;
```

Current config surface:

```rust
pub struct WardrobeConfig {
pub data: DataConfig,
pub network: NetworkConfig,
pub cache: CacheConfig,
pub wal: WalConfig,
pub transactions: TransactionConfig,
pub security: SecurityConfig,
pub logging: ApplicationLoggingConfig,
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

Application logging is implemented as an explicit Wardrobe application logger plus `tracing` events. The server and CLI may initialize Wardrobe logging from command-line/config values. Embedded engine construction does not install or replace a global logger by default.

Current public logging hooks:

```rust
ApplicationLoggingConfig::from_parts(level, format, destination, file)?
init_application_logging(config)?
shutdown_application_logging()
application_logging_is_configured()
emit_application_log(ApplicationLogEvent::new(...))
```

Supported initial values:

```text
level: trace, debug, info, warn, error, off
format: pretty, json
destination: stderr, stdout, file
```

Logging configuration controls:

- level
- format
- destination
- file path

Application logs must not be used for recovery.

WAL and transaction logs must not be treated as operator-facing application logs.

Sensitive payload values, credentials, tokens, permissions, user records, and secrets should not be logged by default. Debug and trace logging remain redacted unless a future explicit unsafe diagnostic option is added.

The `wardrobe` CLI keeps command output and logs separated: command output remains stdout, while application logs default to stderr when enabled. `wardrobe-server` emits startup, shutdown, config, listener, connection, command execution, failure, recovery, backup/restore, and compaction events through the application logging stream.
