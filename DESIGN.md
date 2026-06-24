# Wardrobe Design

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

The current workspace is organized around three primary deliverables.

| Project | Purpose |
|---|---|
| `wardrobe-core` | Storage engine, command model, protocol types, and client facade |
| `wardrobe-server` | Standalone daemon that executes the shared command surface remotely |
| `wardrobe-cli` | Administrative and operational command-line tooling |

Supporting projects such as GUI administration tools and language bindings should build on the same engine and command surface rather than inventing separate authorities.

---

## Shared Structural Model

Wardrobe has four main structural layers plus an optional routing dimension:

- `tenant`
- `wardrobe`
- `bay`
- `drawer`
- `record`

The user-facing CLI and `NOTES.txt` use `wardrobe` and `bay`. The Rust API uses the older engine terms `database` and `schema`.

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

`NOTES.txt` is the authority for the CLI capability set. The current command families are:

- Structural and lifecycle management: `show`, `create`, `check`, `clean`
- Document mutations and queries: `upsert`, `count`, `inspect`, `records`, `delete`
- Schema engine and relationship management: `add`, `remove`
- Backup and disaster recovery: `backup`, `restore`
- Server access control and user administration: `add user`, `grant permission`, `revoke permission`

These are not CLI-only concepts. They map onto the same engine and protocol operations used by `WardrobeClient`, `WardrobeEngine`, and `wardrobe-server`.

---

## Command Model

Wardrobe now treats `Command` and `CommandResult` as the shared execution contract across the stack.

- `WardrobeEngine` executes commands locally
- `WardrobeClient` either calls the engine directly or serializes commands over the protocol
- `wardrobe-server` deserializes commands and hands them to the same engine-backed executor
- `wardrobe-cli` issues the same command families regardless of whether the target is embedded or remote

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

Current on-disk storage is flat-file based.

Primary artifacts:

- Drawer data files: `*.drw`
- Drawer index files: `*_index.drw`
- Drawer metadata files: `*_meta.drw`
- Catalog registry: `.catalog.drw`
- Database WAL files: `.wal`
- Server access-control registry: `_wardrobe_access_control.json`

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
- `vacuum_drawer` and `migrate_drawer` for maintenance
- `verify_wal` for write-ahead log inspection
- `backup_archive` and `restore_archive` for scope-aware archive workflows

Backup and restore are structural operations, not just filesystem copies. The engine produces and consumes archive envelopes that can move through the remote protocol as structured data.

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

Those mutations flow through `manage_schema` in the Rust API and the `add` / `remove` command families in the CLI.

Nested JSON objects containing `_id` values still participate in relationship-aware graph processing:

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

- add user
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

- `show_tenants`
- `show_databases`
- `show_schemas`
- `show_drawers`

Lifecycle surfaces:

- `create_database`
- `create_schema`
- `create_drawer`
- `register_tenant_route`

The CLI presents these as `show` and `create` across wardrobes, bays, drawers, and tenants. The Rust API keeps the database/schema naming but executes the same structural intent.

---

## Transport Layer

The network protocol uses framed request and response messages carrying serialized `Command` and `CommandResult` payloads.

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

Future bindings and administration tools should follow the same rules:

- one public client-facing package per ecosystem where possible
- driver selection based on connection target
- reuse of the shared command model instead of custom ad hoc admin APIs

That applies equally to CLI tooling, GUI tooling, and language bindings.

---

## Near-Term Direction

Current design priorities are:

- keep the command surface aligned across engine, client, CLI, and server
- continue shifting structural discovery and routing toward catalog-backed behavior
- preserve remote parity for inspection, recovery, schema management, and access control
- expand bindings and tools on top of the same core execution model

The north star has not changed:

- one data model
- one command model
- one client surface
- multiple deployment modes
