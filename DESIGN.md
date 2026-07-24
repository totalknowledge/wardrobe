# Wardrobe Design

This document reflects workspace release `0.26.724`.

## Purpose

Wardrobe is a document database platform designed to preserve one storage model and one client-facing programming model across multiple deployment shapes.

Wardrobe currently runs as:

- Embedded file-backed storage
- TCP server-backed storage
- Unix socket-backed storage

The architectural goal is still simple:

> One data model. One command model. One client surface.

`WardrobeEngine` owns local behavior. `WardrobeClient` selects the transport and exposes the same operational surface across embedded and remote execution.

---

## Product Family

The current workspace is organized around the following deliverables and supporting tools.

| Project | Purpose |
|---|---|
| `wardrobe-core` | Storage engine, command model, protocol types, and client facade |
| `wardrobe-server` | Standalone daemon that executes the shared command surface remotely |
| `wardrobe-cli` crate / `wardrobe` binary | Administrative and operational command-line tooling |
| `@wardrobe/client` / `@wardrobe/embedded` | JavaScript and TypeScript server and embedded packages |
| `wardrobe-client` / `wardrobe-embedded` | Python server and embedded packages |
| `wardrobe-c` | C ABI bridge |
| `armoire` | Angular and Tauri administration application under `utilities/armoire` |
| `wardrobe-benchmark` | Cross-engine performance and storage comparison harness |

The GUI and language bindings build on the same engine and command surface rather than introducing separate storage authorities.

---

## Shared Structural Model

Wardrobe has four main structural layers plus an optional routing dimension:

- `tenant`
- `wardrobe`
- `bay`
- `drawer`
- `record`

The user-facing CLI uses `wardrobe` and `bay`. The Rust API uses the engine terms `database` and `schema`.

The mapping is direct:

- `wardrobe` = `database`
- `bay` = `schema`
- `drawer` = `drawer`

This vocabulary split is presentation-level only. The storage and routing model is the same.

### Tenant

A tenant is a routing scope for isolated execution. It is not the same thing as a filesystem path chosen by the caller.

### Wardrobe / Database

The top-level logical storage boundary for most lifecycle commands.

### Bay / Schema

A namespace inside a wardrobe. Bays are intentionally flat and cannot be nested.

### Drawer

The primary record container. A drawer is closest to a document collection, but it also carries indexes, relationship metadata, delete rules, and optional schema metadata.

### Record

A JSON-like document stored inside a drawer. Record identifiers may be addressed directly or through fully qualified pointers.

---

## Capability Model

The CLI help output and `NOTES.txt` describe the current command families:

- Structural and lifecycle management: `create`, `alter`, `drop`
- Document mutations and queries: `read`, `upsert`, `delete`, `inspect`, `count`
- Maintenance: `compact`, `backup`, `restore`
- Server access control and runtime status: `create user`, `drop user`, `grant permission`, `revoke permission`, `status`

These are not CLI-only concepts. They map onto the same engine and protocol operations used by `WardrobeClient`, `WardrobeEngine`, and `wardrobe-server`.

---

## Command Model

Wardrobe now treats `Command` and `CommandResult` as the shared execution contract across the stack.

- `WardrobeEngine` executes commands locally
- `WardrobeClient` either calls the engine directly or serializes commands over the protocol
- `wardrobe-server` deserializes commands and hands them to the same engine-backed executor
- the `wardrobe` CLI issues the same command families regardless of whether the target is embedded or remote

This is the key reason structural, inspection, backup, restore, schema, and permission workflows can now work against the server instead of only against local files.

---

## Addressing and Routing Rules

Wardrobe accepts either a filesystem path or a Wardrobe URI as the connection context.

Examples:

```text
./data
wardrobe://local/./data
wardrobe://localhost:24842
wardrobe+unix:///tmp/wardrobe.sock
```

Structural paths are expressed as:

```text
wardrobe/bay/drawer
```

Important routing rules:

- When an embedded filesystem target points at a storage root rather than an explicit wardrobe or bay, execution falls back to the default wardrobe and bay names `default` and `default`
- Fully qualified document pointers may bypass positional path arguments entirely:
  `@storage_root/wardrobe_name/bay_name/drawer_name:document_id`
- Cross-boundary traversal is allowed only when the executing context has direct clearance and the request uses explicit fully qualified paths
- When execution is scoped to a tenant, cross-tenant traversal is rejected
- Non-tenant structural inquiries such as `inspect` and `count` aggregate across active tenant sibling files for the targeted drawer

---

## Storage Architecture

Current on-disk storage uses versioned binary files owned by the embedded engine.

Primary artifacts:

- Drawer data files: `*.drw`
- Drawer index files: `*_index.drw`
- Drawer metadata files: `*_meta.drw`
- Catalog registry: `.catalog.drw`
- Logical and transaction WAL files
- Server access-control registry: `_wardrobe_access_control.json`

New data records use version-2 WRDB frames with a native positional value stream and presence bitmap keyed by the drawer field-name map. Version-1 BSON-backed WRDB records remain readable, and compaction rewrites live records using the native format.

Indexes use compact WIDX native binary frames. Readers continue to accept older BSON-backed index entries, while compaction and current writes use the native representation. Materialized secondary indexes use ordered B+ tree-style structures for equality and numeric or string range candidate lookup. Reverse relationship mappings are maintained inside drawer index storage rather than in a separate sidecar file.

The drawer remains the core persistence unit. Indexes, relationship metadata, delete rules, and schema metadata remain attached to that unit.

Tenant storage follows the current documented strategy:

- Tenant data may live in separate files beneath the drawer level
- Drawer-level indexes and metadata remain associated with the drawer namespace so uniqueness checks and aggregate inspection stay fast

The engine remains the source of truth for reading and mutating all of these artifacts.

---

## Inspection, Maintenance, and Recovery

Wardrobe now exposes a fuller operational surface through both the engine and the client:

- `inspect_drawer` for raw size, count, register, and fragmentation metrics
- `check_path` for physical presence and sanity checks
- `diagnose_storage` for high-level storage health summaries
- `compact` / `CompactRequest` for storage reclamation and migration maintenance
- `verify_wal` for write-ahead log inspection
- `backup_archive` and `restore_archive` for scope-aware archive workflows

Backup and restore are structural operations, not just filesystem copies. The engine produces and consumes archive envelopes that can move through the remote protocol as structured data.

---

## Observability

Wardrobe deliberately keeps operator-facing application logs separate from recovery artifacts.

- Application logs describe runtime behavior such as server startup, shutdown, config loading, listener binding, accepted connections, command start/end/failure, recovery activity, backup/restore, and compaction.
- Logical WAL records are durability and recovery data for storage mutations.
- Transaction WAL records are transaction atomicity and transaction recovery data.

Application logs are never replayed for recovery. WAL and transaction WAL files are never treated as human-facing application log streams.

`wardrobe-server` owns logging initialization for daemon mode. The `wardrobe` CLI can opt into the same logging controls while keeping command output on stdout and logs on stderr by default. Embedded `WardrobeEngine` construction does not install global logging; host applications either configure Wardrobe logging explicitly or install their own `tracing` subscriber.

Application events should include structured fields where possible, such as operation, command, drawer, wardrobe/database, bay/schema, tenant, duration, and success/failure state. Events must not log raw record payloads, credentials, tokens, permission payloads, or user records by default.

---

## Schema and Relationship Management

Wardrobe supports schema-adjacent control without abandoning the document model.

The current administrative surface can attach or remove:

- secondary indexes
- keys
- constraints
- triggers
- relationships
- cascade-delete rules

Those mutations flow through `alter` and `drop` in the Rust API and the matching CLI command families.

Nested JSON values without `_id` remain embedded in their parent record, including nested objects and array elements. Only ID-bearing nested objects participate in relationship-aware graph processing:

- full nested objects with `_id` can trigger cascade upsert behavior
- `_id`-only objects are treated as strict references
- removing the field dissolves the relationship edge

---

## Access Control

Embedded and remote modes intentionally differ here.

### Embedded Mode

Embedded mode relies on operating-system filesystem permissions. If the current process can read and write the target path, the engine treats that as sufficient authority for local execution.

### Remote Mode

Remote mode currently exposes built-in server-side authorization management through command execution and the access-control registry file `_wardrobe_access_control.json`.

The supported administrative operations are:

- create user
- drop user
- grant permission
- revoke permission

Permission scopes are normalized as:

```text
wardrobe[/bay[/drawer]]:rights
```

where `rights` is a non-empty combination of:

- `r` read
- `u` update
- `d` delete
- `i` inspect

`WardrobeClient` intentionally refuses `manage_user` calls for embedded targets even though `WardrobeEngine` can execute them locally. That keeps application-facing behavior aligned with the intended deployment boundary while still letting the server use the engine primitive internally.

Transport authentication and hardening can be layered on top of this, but the current implementation's concrete built-in surface is the authorization registry and its command handlers.

---

## Discovery and Lifecycle

Wardrobe supports explicit discovery and provisioning.

Discovery surfaces:

- `status(StatusRequest::tenants())`
- `status(StatusRequest::databases())`
- `status(StatusRequest::schemas(...))`
- `status(StatusRequest::drawers(...))`

These typed Rust requests return their concrete values directly. Database and drawer discovery return `Vec<StorageInventory>`, while schema discovery returns `Vec<String>`. The serialized result is `{"status":[...]}`; status payloads do not carry `Databases`, `Schemas`, or `Drawers` result tags.

Lifecycle surfaces:

- `create(CreateRequest::database(...))`
- `create(CreateRequest::schema(...))`
- `create(CreateRequest::drawer(...))`
- `drop(DropRequest::database(...))`
- `drop(DropRequest::schema(...))`
- `drop(DropRequest::drawer(...))`
- `register_tenant_route`

The CLI presents these as `status`, `create`, and `drop` across wardrobes, bays, drawers, and tenants. The Rust API keeps the database/schema naming but executes the same structural intent.

---

## Transport Layer

The network protocol uses framed request and response messages carrying serialized `Command` and `CommandResult` payloads.

Frame writers emit the fixed header and payload directly. Persistent TCP clients keep `TCP_NODELAY` enabled, and direct socket paths avoid forcing an extra buffered flush per command while retaining magic, opcode, and payload-length validation.

Because Wardrobe is pre-stable, the protocol does not keep aliases for removed command names. Clients should send the canonical variants: `Read`, `Upsert`, `Delete`, `Inspect`, `Count`, `Compact`, `Create`, `Alter`, `Drop`, `Backup`, `Restore`, `Grant`, `Revoke`, and `Status`.

Supported transports:

- TCP
- Unix sockets

`DriverKind` currently has three concrete variants:

- `Embedded`
- `Network`
- `UnixSocket`

The client surface should remain stable even when the deployment target changes.

---

## Language Bindings and Tooling

The delivered bindings intentionally separate server-only packages from embedded native packages so network consumers do not have to load a native storage artifact:

- JavaScript/TypeScript: `@wardrobe/client` and `@wardrobe/embedded`
- Python: `wardrobe-client` and `wardrobe-embedded`
- C ABI: `wardrobe-c`

Both JavaScript/TypeScript packages expose `WardrobeClient`. Python exposes `WardrobeClient` for server connections and `WardrobeEmbedded` for in-process storage. All bindings serialize the shared command model and return flat status arrays for database, schema, and drawer discovery.

The embedded JavaScript, TypeScript, and Python samples run the same publishing-house workflow against repository-root `./wardrobe` storage, using separate `public_js`, `public_ts`, and `public_py` bays.

Armoire is a utility rather than a sample. It lives under `utilities/armoire`, uses the Rust client/engine through Tauri commands, and supports source-location lifecycle, connection persistence, structural discovery, drawer creation, record reads, and record creation.

---

## Licensing Boundaries

The workspace does not use one repository-wide license:

- `wardrobe-core`, `wardrobe-cli`, language bindings, and samples use MIT.
- `wardrobe-server` uses Business Source License 1.1 with a July 22, 2030 change date to GPL version 2 or later.
- Armoire uses the Armoire Source-Available Evaluation License and requires a paid commercial license for production or non-evaluation commercial use.

Each component's license file is authoritative.

---

## Near-Term Direction

Current design priorities are:

- keep the command surface aligned across engine, client, CLI, and server
- continue shifting structural discovery and routing toward catalog-backed behavior
- preserve remote parity for inspection, recovery, schema management, and access control
- preserve binding parity as the package surfaces mature toward publication
- continue measuring equality, range, traversal, mutation, purge, compaction, and storage behavior across the benchmark matrix

The north star has not changed:

- one data model
- one command model
- one client surface
- multiple deployment modes
