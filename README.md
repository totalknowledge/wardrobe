# Wardrobe

[![coverage](https://github.com/totalknowledge/wardrobe/actions/workflows/coverage.yml/badge.svg)](https://github.com/totalknowledge/wardrobe/actions/workflows/coverage.yml)

Current workspace release: `0.26.722`.

Wardrobe is a hierarchical document database with native relationship support, designed to bridge the gap between traditional document stores, relational databases, and graph databases. Complex object graphs are stored naturally-automatically separating embedded documents from related entities while preserving relationships, referential integrity, and intuitive traversal.

It combines the flexibility of JSON documents with built-in referential integrity, relationship traversal, automatic hydration, cascading operations, and schema validation without requiring separate graph storage or complex object-relational mapping.

Written in Rust, Wardrobe can run directly inside your application as an embedded database or as a standalone server while exposing the same API in both deployment models. Applications can move from embedded development to client/server deployment without rewriting their data access layer.

Unlike many document databases that treat references as ordinary strings, Wardrobe understands relationships between documents. References can participate in integrity validation, automatic object hydration, virtual relationships, cascading updates and deletes, and efficient traversal while remaining simple fields inside your documents.

Documents are organized hierarchically into Wardrobes, Bays, Drawers, and Documents, providing an intuitive logical structure that maps directly onto the on-disk storage layout. This transparent organization makes applications easier to understand, navigate, back up, and administer than systems built around opaque storage engines.

Under the hood, Wardrobe stores documents in versioned binary record files backed by native indexes, write-ahead logging, crash recovery, archive-based backup and restore, online compaction, and bounded in-memory caching. The result is a lightweight storage engine that requires no external services while providing capabilities typically associated with much larger database systems.

Whether you're building desktop software, embedded systems, developer tools, games, SaaS platforms, or self-hosted services, Wardrobe provides a deployment-neutral database that scales from a single executable to a networked server without changing how your application interacts with its data.

## Workspace

```text
wardrobe/
  core/                  Embedded engine, client facade, command model, and protocol
  cli/                   Command-line administration and operations
  server/                Standalone TCP and Unix-socket daemon
  bindings/              C, JavaScript/TypeScript, and Python bindings
  samples/               Rust, C ABI, JavaScript, TypeScript, and Python examples
  utilities/armoire/     Angular and Tauri database administration application
  utilities/benchmark/   Cross-engine performance benchmark
  utilities/scripts/     Scripted CLI workflow
```

## Terminology

The CLI and end-user examples use user-facing structural names:

- `wardrobe`
- `bay`
- `drawer`

The Rust API keeps the older engine-oriented names:

- `database`
- `schema`
- `drawer`

They map directly:

- CLI `wardrobe` = API `database`
- CLI `bay` = API `schema`
- CLI `drawer` = API `drawer`

`tenant` remains a separate routing dimension in both surfaces.

## Current Public API

`wardrobe-core` currently re-exports these public items:

- Core entry points: `WardrobeEngine`, `WardrobeClient`
- Routing types: `StorageCoordinate`, `StorageScope`, `StorageLocator`, `StorageInventory`
- Query and result types: `OperationFilter`, `OperationOptions`, `ReturnShape`, `ReadResult`, `UpsertResult`, `DeleteResult`, `InspectResult`, `QueryModifiers`, `OrderDirection`
- Lifecycle request types: `CreateRequest`, `CreateResult`, `AlterRequest`, `DropRequest`, `CompactRequest`, `CompactMode`, `StatusRequest`, `TypedStatusRequest`, `StatusRequestOutput`, `PermissionRequest`
- Connection and protocol types: `ConnectionTarget`, `DriverKind`, `DEFAULT_NETWORK_PORT`, `ProtocolFrame`, `ProtocolOpcode`, `PROTOCOL_MAGIC`
- Inspection, verification, and recovery types: `DrawerInspectionMetrics`, `CheckReport`, `CheckEntry`, `StorageDiagnosis`, `VacuumReport`, `WalVerification`, `BackupArchive`, `BackupArchiveFile`, `RestoreReport`
- Configuration types: `WardrobeConfig`, `WardrobeEngineBuilder`, `DataConfig`, `NetworkConfig`, `CacheConfig`, `WalConfig`, `TransactionConfig`, `SecurityConfig`
- Lower-level storage types: `Database`, `Drawer`, `DatabaseReader`, `DatabaseWriter`, `Recycler`, `StorageFormat`, `BsonBinaryFormat`, `NativeBinaryIndexFormat`
- Catalog and WAL types: `CATALOG_FILE_NAME`, `CatalogEntry`, `CatalogRegistry`, `CatalogTenantRoute`, `WAL_FILE_NAME`, `WalEntry`, `WalJournal`, `WalOperation`
- Application logging types: `ApplicationLoggingConfig`, `ApplicationLogLevel`, `ApplicationLogFormat`, `ApplicationLogDestination`, `ApplicationLogEvent`

The two main application entry points are:

- `WardrobeEngine` for direct embedded access
- `WardrobeClient` for path, TCP, and Unix socket targets with the same command surface

`WardrobeClient` and `WardrobeEngine` expose the canonical Wardrobe verbs:

- Record operations: `upsert`, `read`, `count`, `delete`
- Maintenance and inspection: `compact`, `inspect`, `status`
- Lifecycle and recovery: `create`, `alter`, `drop`, `backup`, `restore`
- Administrative management: `grant`, `revoke`, plus user creation/removal through `create` and `drop`

`WardrobeEngine` also exposes low-level protocol execution with `execute`, `execute_in_scope`, `execute_for_tenant`, and `execute_command` for server integration.

## Usage

### Embedded Quick Start

```rust
use serde_json::json;
use std::io::{Error, ErrorKind};
use wardrobe_core::{OperationFilter, OperationOptions, ReadResult, WardrobeClient};

fn main() -> std::io::Result<()> {
    let client = WardrobeClient::open("./wardrobe")?;

    let pointer = client
        .upsert(
            json!({
                "_id": "field-service-kit",
                "name": "Field Service Toolkit",
                "category": "maintenance",
                "tags": ["portable", "repair"]
            }),
            OperationFilter::drawer("tool"),
            OperationOptions::default(),
        )?
        .into_pointers()
        .into_iter()
        .next()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "upsert returned no pointer"))?;

    let record = match client.read(
        OperationFilter::pointer(pointer),
        OperationOptions::default(),
    )? {
        ReadResult::Record(record) => record,
        _ => None,
    };
    println!("{record:?}");

    Ok(())
}
```

### Driver-Selecting Client

```rust
use wardrobe_core::WardrobeClient;

fn connect() -> std::io::Result<()> {
    let embedded = WardrobeClient::open("./data")?;
    let embedded_uri = WardrobeClient::open("wardrobe://local/./data")?;
    let network = WardrobeClient::open("wardrobe://localhost:24842")?;

    #[cfg(unix)]
    let socket = WardrobeClient::open("wardrobe+unix:///tmp/wardrobe.sock")?;

    Ok(())
}
```

Supported connection shapes:

- Direct path: `./data`
- Embedded URI: `wardrobe://local/path/to/data`
- File URI: `wardrobe+file://path/to/data`
- TCP URI: `wardrobe://localhost:24842`
- TCP default port: `wardrobe://localhost` uses `24842`
- Unix socket URI: `wardrobe+unix:///tmp/wardrobe.sock`

Use `ConnectionTarget::requires_embedded_engine()` or `WardrobeClient::requires_embedded_engine()` when a binding or host application needs to know whether embedded native storage is required.

### Filtering, Counting, and Pagination

```rust
use serde_json::json;
use wardrobe_core::{
    OperationFilter, OperationOptions, OrderDirection, ReadResult, WardrobeClient,
};

fn query(client: &WardrobeClient) -> std::io::Result<()> {
    let filter = OperationFilter::query_in("device", json!({ "name": "sensor%" }));
    let records = match client.read(
        filter.clone(),
        OperationOptions::new()
            .order_by("name")
            .order_direction(OrderDirection::Ascending)
            .offset(0)
            .limit(25),
    )? {
        ReadResult::Records(records) => records,
        _ => Vec::new(),
    };

    let total = client.count(filter, OperationOptions::default())?;

    println!("matched {} records, returned {}", total, records.len());
    Ok(())
}
```

### Typed Status Results

Rust status constructors encode their output type, so inventory calls return direct values without a result enum or variant wrapper:

```rust
use wardrobe_core::{StatusRequest, WardrobeClient};

fn inventory(client: &WardrobeClient) -> std::io::Result<()> {
    let databases = client.status(StatusRequest::databases())?;
    let schemas = client.status(StatusRequest::schemas("publishing-house"))?;
    let drawers = client.status(StatusRequest::drawers("publishing-house", "public"))?;

    println!("{} databases, {} schemas, {} drawers", databases.len(), schemas.len(), drawers.len());
    Ok(())
}
```

The command protocol keeps the operation envelope, for example `{"status":[...]}`, but the status payload itself is a raw array. JavaScript, TypeScript, and Python status methods return that array directly.

## Language Bindings

| Ecosystem | Server-backed | Embedded |
|---|---|---|
| Rust | `WardrobeClient` with a Wardrobe URI | `WardrobeClient` with a path, or `WardrobeEngine` |
| JavaScript/TypeScript | `@wardrobe/client` | `@wardrobe/embedded` |
| Python | `wardrobe-client` | `wardrobe-embedded` |
| C ABI | `wardrobe-c` | `wardrobe-c` |

The npm packages require Node.js 24 or newer. The Python packages require Python 3.10 or newer. Binding packages are currently prepared for local validation and dry runs; publishing is not part of this release.

## Licensing

Licensing is component-specific:

- Core/client engine, CLI, language bindings, and samples: MIT
- Wardrobe server: Business Source License 1.1, changing to GPL version 2 or later on July 22, 2030
- Armoire: Armoire Source-Available Evaluation License (ASEL); production or non-evaluation commercial use requires a paid commercial license

The license file within each component is authoritative.

## Routed Multi-Tenant Execution

```rust
use serde_json::json;
use wardrobe_core::{Command, OperationFilter, OperationOptions, StorageCoordinate, WardrobeEngine};

fn routed(engine: &WardrobeEngine) -> std::io::Result<()> {
    let scope = StorageCoordinate::new("tenant_a", "production", "public");

    engine.execute(
        scope,
        Command::Upsert {
            payload: json!({
                "_id": "@account:lnk_acme",
                "name": "Acme Manufacturing"
            }),
            filter: OperationFilter::drawer("account"),
            options: OperationOptions::default(),
        },
    )?;

    Ok(())
}
```

## Server Daemon

Run Wardrobe as a TCP-backed daemon:

```text
cargo run -p wardrobe-server -- --data-dir ./data --tcp-bind 127.0.0.1:24842
```

Useful server flags:

- `--data-dir <path>` chooses the root storage directory
- `--tcp-bind <addr:port>` binds the TCP listener; default is `127.0.0.1:24842`
- `--no-tcp` disables the TCP listener
- `--unix-socket <path>` binds a Unix domain socket listener on Unix platforms
- `--check` initializes the engine and exits without blocking
- `--log-level <level>` enables application logs at `trace`, `debug`, `info`, `warn`, `error`, or disables them with `off`
- `--log-format <format>` selects `pretty` or `json`
- `--log-destination <dest>` writes application logs to `stderr`, `stdout`, or `file`
- `--log-file <path>` chooses the file path when `--log-destination file` is used

Application logs are operator-facing diagnostics only. They are separate from Wardrobe's logical WAL and transaction WAL, and they are never used for recovery.

## CLI Usage

`NOTES.txt` and `wardrobe --help` are the authority for the CLI capability set. The package crate remains `wardrobe-cli`, while the installed binary is `wardrobe`. The binary accepts the connection context as the first positional argument, then runs the canonical command families from the help output.

Run a single command:

```text
cargo run -p wardrobe-cli -- <connection> [--pretty] <command> [args]
```

If no command is supplied, the CLI enters an interactive REPL. If standard input is piped in, the CLI executes the piped command instead.

CLI application logging uses the same `--log-level`, `--log-format`, `--log-destination`, and `--log-file` controls as the server. Logging is off by default, and when enabled it writes to stderr by default so JSON command output remains script-friendly.

Examples:

- Embedded structural discovery:
  `cargo run -p wardrobe-cli -- ./data status wardrobes`
- Remote drawer listing:
  `cargo run -p wardrobe-cli -- "wardrobe://127.0.0.1:24842" status drawers inventory/public`
- Backup a bay:
  `cargo run -p wardrobe-cli -- ./data backup inventory/public ./backups/public.wrb`

Canonical command families:

- Structural management
  - `create <type> <path>`
  - `alter <type> <path> <target_field> <?extra_args>`
  - `drop <type> <path> <?target_field> <?extra_args>`
- Document mutations and queries (RUDIC)
  - `read <path> <?json_filter> <?json_options>`
  - `upsert <path> <json_payload> <?json_filter> <?json_options>`
  - `delete <path> <json_filter_or_id>`
  - `inspect <path> <?json_filter> <?json_options>`
  - `count <path> <?json_filter> <?json_options>`
- Backup and disaster recovery
  - `compact <path>`
  - `backup <source_path> <destination_archive_path>`
  - `restore <destination_path> <source_archive_path>`
- Server access control and user administration
  - `create user <json_user_payload>`
  - `drop user <username>`
  - `grant permission <username> <path:rights>`
  - `revoke permission <username> <path:rights>`
  - `status <type> <?path>`

Behavior notes:

- `status wardrobes` and `create wardrobe` map to the Rust `database` lifecycle APIs
- `status bays` and `create bay` map to the Rust `schema` lifecycle APIs
- `create user`, `grant permission`, and `revoke permission` require a remote server-backed target; `WardrobeClient` rejects them for embedded connections
- `compact` can target a wardrobe, a bay, or a single drawer and fans out to the relevant compaction calls
- `backup` and `restore` operate at wardrobe, bay, or drawer scope

Compatibility aliases are intentionally not provided for the canonical CLI vocabulary.

## Application Logging

Wardrobe has three separate log-like systems:

- Application logs: structured operator-facing diagnostics for startup, shutdown, connection handling, command execution, recovery, backup, restore, and compaction activity.
- Logical WAL: durable storage recovery records.
- Transaction WAL: transaction atomicity and transaction recovery records.

Application logs may be configured explicitly by `wardrobe-server`, by the `wardrobe` CLI, or by an embedding host through `ApplicationLoggingConfig` and `init_application_logging`. Embedded `WardrobeEngine::open` does not install or override a global logger by default. Wardrobe also emits application events through `tracing`, so a host application that has already installed a tracing subscriber can observe them without Wardrobe taking ownership of global logging.

Logs include structured fields such as operation, command, drawer, duration, and success state where available. Raw record payloads, credentials, tokens, and user records are not logged by default.

## Current Capabilities

- File-backed drawer storage with separate data, index, metadata, and WAL artifacts
- Versioned WRDB record frames with native positional record payloads, field-name maps, and presence bitmaps; version 1 BSON-backed records remain readable
- Compact WIDX native binary index frames with transparent reading of older BSON-backed index entries
- Record CRUD, JSON filtering, pointer lookup, and count operations
- Primary-key indexing plus ordered B+ tree-style secondary indexes for equality and numeric/string range queries
- Nested document graph hydration and relationship-aware record storage
- Relationship constraints, delete rules, cascade-delete rules, and drawer schema metadata
- Scoped routing across tenant, database, schema, and drawer boundaries
- Write-ahead log verification and recovery for incomplete operations
- Compact maintenance workflows for drawer storage reclamation and migration
- Structural inspection and sanity checking through `inspect` and `status` surfaces
- Archive-based backup and restore at wardrobe, bay, or drawer scope
- Remote access-control administration persisted in `_wardrobe_access_control.json`
- Bounded drawer caching for embedded engine usage

## Sample Application

Run the basic Rust sample crate to execute an end-to-end publishing-house flow that:

- Opens a local embedded engine against `./wardrobe`
- Creates the `publishing-house/public` hierarchy and its publisher, person, and book drawers
- Uses direct typed database, schema, and drawer status arrays
- Stores related publisher, author, editor, and book records
- Exercises filtered reads, pointer reads, counts, temporary record cleanup, and final integrity checks

```text
cargo run -p basic-usage
```

Equivalent embedded examples are available in JavaScript, TypeScript, and Python. They use the repository-root ignored `./wardrobe` storage directory and separate bays named `public_js`, `public_ts`, and `public_py`.

## CLI Sample

Run the shell script sample to drive the CLI through the library workflow:

```text
bash ./utilities/scripts/wardrobe-cli-demo.sh
```

Pass a server connection string to exercise the same workflow remotely:

```text
bash ./utilities/scripts/wardrobe-cli-demo.sh wardrobe://localhost:24842
```

## Testing

Run the test suite with:

```text
cargo test --workspace
```

For a coverage summary, install `cargo-llvm-cov` and run:

```text
cargo llvm-cov --workspace
```
