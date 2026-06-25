# Wardrobe

Wardrobe is a Rust document database that can run embedded or behind a server without forcing application code to change the API it calls. `WardrobeEngine` is the direct embedded engine. `WardrobeClient` is the deployment-neutral facade that selects embedded, TCP, or Unix socket transport from a connection string.

Wardrobe stores JSON-like records in flat files with file-backed indexes, relationship hydration, schema and rule metadata, scoped tenant routing, write-ahead recovery, vacuum compaction, inspection tooling, and archive-based backup and restore workflows.

## Workspace

```text
wardrobe/
  core/                 Embedded engine library crate
  cli/                  Command-line administration and operations
  server/               Standalone network daemon
  samples/cli-script/    Shell script CLI workflow sample
  samples/basic-usage/  Sample application using embedded engine
```

## Terminology

The CLI help and `NOTES.txt` use user-facing structural names:

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
- Command and routing types: `Command`, `CommandResult`, `StorageCoordinate`, `StorageScope`, `StorageLocator`, `StorageInventory`
- Query types: `QueryModifiers`, `OrderDirection`
- Connection and protocol types: `ConnectionTarget`, `DriverKind`, `DEFAULT_NETWORK_PORT`, `ProtocolFrame`, `ProtocolOpcode`, `PROTOCOL_MAGIC`
- Inspection, verification, and recovery types: `DrawerInspectionMetrics`, `CheckReport`, `CheckEntry`, `StorageDiagnosis`, `VacuumReport`, `WalVerification`, `BackupArchive`, `BackupArchiveFile`, `RestoreReport`
- Lower-level storage types: `Database`, `Drawer`, `DatabaseReader`, `DatabaseWriter`, `Recycler`, `StorageFormat`, `BsonBinaryFormat`
- Catalog and WAL types: `CATALOG_FILE_NAME`, `CatalogEntry`, `CatalogRegistry`, `CatalogTenantRoute`, `WAL_FILE_NAME`, `WalEntry`, `WalJournal`, `WalOperation`

The two main application entry points are:

- `WardrobeEngine` for direct embedded access
- `WardrobeClient` for path, TCP, and Unix socket targets with the same command surface

`WardrobeClient` currently exposes:

- Record operations: `upsert`, `find_all`, `find_by_filter`, `count`, `find_by_id`, `delete`, `delete_by_id`, `delete_by_filter`
- Maintenance and inspection: `vacuum_drawer`, `migrate_drawer`, `inspect_drawer`, `check_path`, `diagnose_storage`, `list_drawer_names`, `verify_wal`
- Lifecycle and recovery: `create_database`, `create_schema`, `create_drawer`, `register_tenant_route`, `backup_archive`, `restore_archive`
- Administrative management: `manage_schema`, `manage_user`
- Discovery: `show_tenants`, `show_databases`, `show_schemas`, `show_drawers`, plus `list_*` aliases

`WardrobeEngine` exposes the same data, inspection, lifecycle, and discovery methods locally, and adds direct command execution with `execute`, `execute_in_scope`, `execute_for_tenant`, `execute_command`, and `cached_drawer_count`.

## Usage

### Embedded Quick Start

```rust
use serde_json::json;
use wardrobe_core::WardrobeEngine;

fn main() -> std::io::Result<()> {
    let engine = WardrobeEngine::open("./data")?;

    let pointer = engine.upsert(
        "weapon",
        json!({
            "_id": "field-service-kit",
            "name": "Field Service Toolkit",
            "category": "maintenance",
            "tags": ["portable", "repair"]
        }),
    )?;

    let record = engine.find_by_id(&pointer)?;
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
use wardrobe_core::{OrderDirection, QueryModifiers, WardrobeEngine};

fn query(engine: &WardrobeEngine) -> std::io::Result<()> {
    let records = engine.find_by_filter(
        "device",
        json!({ "name": "sensor%" }),
        Some(QueryModifiers {
            order_by: Some("name".to_string()),
            order_direction: Some(OrderDirection::Ascending),
            offset: Some(0),
            limit: Some(25),
        }),
    )?;

    let total = engine.count("device", Some(json!({ "name": "sensor%" })), None)?;

    println!("matched {} records, returned {}", total, records.len());
    Ok(())
}
```

### Routed Multi-Tenant Execution

```rust
use serde_json::json;
use wardrobe_core::{Command, StorageCoordinate, WardrobeEngine};

fn routed(engine: &WardrobeEngine) -> std::io::Result<()> {
    let scope = StorageCoordinate::new("tenant_a", "production", "public");

    engine.execute(
        scope,
        Command::Upsert {
            drawer_name: "account".to_string(),
            payload: json!({
                "_id": "@account:lnk_acme",
                "name": "Acme Manufacturing"
            }),
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

## CLI Usage

`NOTES.txt` and `wardrobe-cli --help` are the authority for the CLI capability set. The current binary accepts the connection context through `--target`, `--connection`, or `--data-dir`, then runs the canonical command families from the help output.

Run a single command:

```text
cargo run -p wardrobe-cli -- --target <connection> [--pretty] <command> [args]
```

If no command is supplied, the CLI enters an interactive REPL. If standard input is piped in, the CLI executes the piped command instead.

Examples:

- Embedded structural discovery:
  `cargo run -p wardrobe-cli -- --target ./data show wardrobes`
- Remote drawer listing:
  `cargo run -p wardrobe-cli -- --target "wardrobe://127.0.0.1:24842" show drawers inventory/public`
- Backup a bay:
  `cargo run -p wardrobe-cli -- --target ./data backup inventory/public ./backups/public.wrb`

Canonical command families:

- Structural and lifecycle management
  - `show <type> <?parent_path>`
  - `create <type> <path>`
  - `check <path>`
  - `clean <path>`
- Document mutations and queries (RUDI)
  - `upsert <path> <json_payload>`
  - `count <path> <?json_filter>`
  - `inspect <path>`
  - `records <path> <?json_filter>`
  - `delete <path> <json_filter_or_id>`
- Schema engine and relationship management
  - `add <type> <path> <target_field> <?extra_args>`
  - `remove <type> <path> <target_field> <?extra_args>`
- Backup and disaster recovery
  - `backup <source_path> <destination_archive_path>`
  - `restore <destination_path> <source_archive_path>`
- Server access control and user administration
  - `add user <json_user_payload>`
  - `grant permission <username> <path:rights>`
  - `revoke permission <username> <path:rights>`

Behavior notes:

- `show wardrobes` and `create wardrobe` map to the Rust `database` lifecycle APIs
- `show bays` and `create bay` map to the Rust `schema` lifecycle APIs
- `add user`, `grant permission`, and `revoke permission` require a remote server-backed target; `WardrobeClient` rejects them for embedded connections
- `clean` can target a wardrobe, a bay, or a single drawer and fans out to the relevant `vacuum_drawer` calls
- `backup` and `restore` operate at wardrobe, bay, or drawer scope

Compatibility aliases remain available for older workflows, including `list`, `ls`, `find`, `get`, `query`, `insert`, `drawers`, `diagnose`, `show-databases`, `show-schemas`, `show-drawers`, `delete-by-id`, `define`, `manage`, `auth`, and `rbac`.

## Current Capabilities

- Flat-file drawer storage with separate data, index, metadata, and WAL files
- Big-endian BSON-framed binary serialization for drawer data and index payloads
- Record CRUD, JSON filtering, pointer lookup, and count operations
- Primary-key indexing and secondary unique-field indexing
- Nested document graph hydration and relationship-aware record storage
- Relationship constraints, delete rules, cascade-delete rules, and drawer schema metadata
- Scoped routing across tenant, database, schema, and drawer boundaries
- Write-ahead log verification and recovery for incomplete operations
- Explicit drawer vacuuming and migration workflows
- Structural inspection and sanity checking through `inspect`, `check`, and `diagnose` surfaces
- Archive-based backup and restore at wardrobe, bay, or drawer scope
- Remote access-control administration persisted in `_wardrobe_access_control.json`
- Bounded drawer caching for embedded engine usage

## Sample Application

Run the basic sample crate to execute an end-to-end integration flow that:

- Opens a local embedded engine against `./wardrobe`
- Uses `show_drawers("main", "public")` for drawer metadata enumeration
- Upserts a `public.user` parent with multiple `public.gem` children and a `public.weapon` child
- Exercises relation links across drawers
- Filters by array tags and owner via `find_by_filter`
- Cleans up by querying related gems and deleting each via `delete_by_id`

```text
cargo run -p basic-usage
```

## CLI Sample

Run the shell script sample to drive the CLI through the library workflow:

```text
bash ./samples/cli-script/wardrobe-cli-demo.sh
```

Pass a server connection string to exercise the same workflow remotely:

```text
bash ./samples/cli-script/wardrobe-cli-demo.sh wardrobe://localhost:24842
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
