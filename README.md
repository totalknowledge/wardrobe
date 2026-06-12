# Wardrobe

Wardrobe is an embedded Rust document database for local-first applications, developer tooling, test environments, desktop software, and service backends that need structured storage without a separate database server in the hot path.

Wardrobe stores JSON-like records in flat files with file-backed indexes, relationship hydration, schema validation, scoped multi-tenant routing, write-ahead recovery, vacuum compaction, and bounded drawer caching. The long-term direction remains a binary storage engine, but the current API is already useful for a wide range of application workflows.

## Workspace

```text
wardrobe/
  core/                 Embedded engine library crate
  cli/                  Command-line inspection and diagnostics
  server/               Standalone network daemon
  samples/basic-usage/  End-to-end sample application
```

## Current Public API

Wardrobe currently exposes these top-level Rust API items from `wardrobe-core`:

- `WardrobeEngine`
- `Command`
- `CommandResult`
- `WardrobeClient`
- `ConnectionTarget`
- `DriverKind`
- `DEFAULT_NETWORK_PORT`
- `ProtocolFrame`
- `ProtocolOpcode`
- `PROTOCOL_MAGIC`
- `StorageCoordinate`
- `StorageLocator`
- `StorageScope`
- `QueryModifiers`
- `OrderDirection`
- `Database`
- `Drawer`
- `VacuumReport`
- `DatabaseReader`
- `DatabaseWriter`
- `Recycler`
- `StorageFormat`
- `PlainTextJsonFormat`

The main embedded entry points on `WardrobeEngine` are:

- `open`
- `open_with_drawer_cache_limit`
- `new` as a deprecated compatibility alias for `open`
- `upsert`
- `find_all`
- `find_by_filter`
- `count`
- `find_by_id`
- `delete`
- `delete_by_id`
- `vacuum_drawer`
- `migrate_drawer`
- `cached_drawer_count`
- `show_tenants`
- `list_tenants`
- `execute`
- `execute_in_scope`

Supporting execution and routing types include:

- `QueryModifiers` and `OrderDirection` for sorting and pagination
- `StorageCoordinate` for tenant/database/schema routing
- `StorageScope` for database, schema, or drawer isolation modes
- `show_tenants` / `list_tenants` for active tenant namespace discovery
- `Command` and `CommandResult` for a command-driven execution surface

The lower-level exports remain available for advanced use cases such as custom storage experiments, diagnostics, or alternative tooling layers.

`WardrobeClient::open(...)` is the deployment-neutral client entrypoint inside `wardrobe-core`. It accepts a direct file-system path, an embedded URI, a TCP URI, or a Unix socket URI. Embedded paths delegate to the local engine immediately. TCP and Unix socket drivers open their transport connection and exchange framed `Command` / `CommandResult` payloads using Wardrobe's binary protocol framing.

Future language bindings follow the same rule: one public package per ecosystem with internal driver selection from the connection string. Network and Unix socket targets report that they do not require the embedded storage engine artifact, allowing bindings to avoid loading native embedded code unless a local path or file URI is requested.

Embedded language mode is the companion path: direct file-system paths and file-oriented URIs select the embedded driver and may load native binaries built from `wardrobe-core` behind the same public API. The intended package shape remains one public package per ecosystem, even if that package contains separate internal artifacts for embedded and socket-backed execution.

## Usage

### Embedded Quick Start

```rust
use serde_json::json;
use wardrobe_core::WardrobeEngine;

fn main() -> std::io::Result<()> {
    let engine = WardrobeEngine::open("./data")?;

    let weapon_id = engine.upsert(
        "weapon",
        json!({
            "name": "Field Service Toolkit",
            "category": "maintenance",
            "tags": ["portable", "repair"]
        }),
    )?;

    let weapon = engine.find_by_id(&weapon_id)?;
    println!("{weapon:?}");

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

Use `ConnectionTarget::requires_embedded_engine()` or `WardrobeClient::requires_embedded_engine()` when a binding or host application needs to decide whether embedded native storage must be loaded.

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
    let scope = StorageCoordinate::new("tenant_a", "production", "core");

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

### Sample Application

Run the basic sample crate to seed data from `core/tests/common/test_seed.json` and print hydrated drawers:

```text
cargo run -p basic-usage
```

### Server Daemon

Run Wardrobe as a TCP-backed daemon:

```text
cargo run -p wardrobe-server -- --data-dir ./data --tcp-bind 127.0.0.1:24842
```

Useful server flags:

- `--data-dir <path>` chooses the root storage directory.
- `--tcp-bind <addr:port>` binds the TCP listener. The default is `127.0.0.1:24842`.
- `--no-tcp` disables the TCP listener.
- `--unix-socket <path>` binds a Unix domain socket listener on Unix platforms.
- `--check` initializes the engine and exits without blocking.

## Current Capabilities

- Flat-file drawer storage with separate data, index, and metadata sidecar files
- Primary-key indexing and secondary unique-field indexing
- Upsert semantics with tombstoning and size-class slot recycling
- Recursive record hydration across drawers without unsafe code
- Array handling for scalar values, pointer arrays, and nested object arrays
- Schema-less writes or optional per-drawer schema validation through metadata
- Relationship constraints for `1:1`, `M:1`, `1:M`, and `M:M` patterns
- Declarative delete behavior including cascade, restrict, and set-null rules
- Scoped storage routing across database, schema, and drawer isolation models
- Write-ahead log recovery for incomplete upsert and delete operations
- Explicit vacuum compaction for reclaiming fragmented drawer storage
- Lazy and batch schema evolution for legacy drawer layout versions
- Optional LRU drawer caching to bound open file handles and cached drawer state

## Tooling

- `wardrobe-core`: embedded library crate
- `wardrobe-cli`: local inspection and diagnostics
- `wardrobe-server`: standalone daemon with TCP protocol handling
- `basic-usage`: practical sample application

Run the test suite with:

```text
cargo test --workspace
```

For a coverage summary, install `cargo-llvm-cov` and run:

```text
cargo coverage-summary
```
