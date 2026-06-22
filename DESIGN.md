# Wardrobe Design

## Purpose

Wardrobe is a Rust-based document database platform designed to operate across multiple deployment models while preserving a single data model, storage model, and client API.

Wardrobe can run as:

- Embedded file-backed storage
- Standalone server-backed storage
- Unix socket-backed storage
- Future in-memory storage

Applications should be able to move between deployment modes without changing their database access code.

The long-term goal is:

> One data model. One client API. Multiple deployment modes.

---

## Product Family

The Wardrobe ecosystem consists of several related projects.

| Project | Purpose |
|---|---|
| `wardrobe-core` | Storage engine, client API, protocol types, command execution |
| `wardrobe-server` | Standalone database daemon |
| `wardrobe-cli` | Administrative command-line tooling |
| `armoire` | Graphical database administration tool |
| `wardrobe-typescript` | TypeScript bindings |
| `wardrobe-python` | Python bindings |
| `wardrobe-java` | Planned Java bindings |
| Additional Bindings | Experimental language integrations |

All clients should expose the same conceptual API and use connection strings to select drivers.

---

## Core Concepts

### Tenant

A logical owner of data.

Examples:

- `customer_a`
- `customer_b`
- `internal`

Tenants are routing concepts rather than physical storage locations.

### Database

A logical database boundary.

Examples:

- `production`
- `staging`
- `analytics`

### Schema

A namespace within a database.

Examples:

- `public`
- `accounting`
- `inventory`

### Drawer

A drawer is Wardrobe's primary storage abstraction.

A drawer is similar to:

- a collection in MongoDB
- a table in a relational database

but is not exactly either.

Drawers store JSON-like records and support relationships, hydration, indexing, validation, and lifecycle management.

Example:

```text
public.user
public.weapon
public.gem
```

### Record

A JSON-like document stored within a drawer.

Example:

```json
{
  "_id": "@user:lnk_bryan",
  "name": "Bryan"
}
```

---

## Automatic Drawer Extraction

One of Wardrobe's defining features is automatic nested-object extraction.

Nested objects are not necessarily stored as opaque JSON blobs.

Wardrobe may automatically:

1. Create records in additional drawers.
2. Generate relationship links.
3. Maintain hydration metadata.

Example input:

```json
{
  "name": "Bryan",
  "address": {
    "city": "Raleigh",
    "state": "NC"
  }
}
```

May become:

```text
user drawer
  -> address drawer
```

with generated relationships between records.

This allows applications to write document-oriented payloads while Wardrobe maintains relationship-aware storage internally.

---

## Storage Hierarchy

```text
Tenant
 └── Database
      └── Schema
           └── Drawer
                └── Record
```

Example:

```text
tenant: acme
database: production
schema: public
drawer: user
```

Fully-qualified location:

```text
acme.production.public.user
```

---

## Deployment Model

The preferred access point is:

```rust
WardrobeClient
```

Applications should not care whether the target is embedded, remote, or in-memory.

Examples:

```text
./data
```

```text
wardrobe://local/./data
```

```text
wardrobe://localhost:24842
```

```text
wardrobe+unix:///tmp/wardrobe.sock
```

Future:

```text
wardrobe+memory://
```

---

## Driver Architecture

Connection targets select a driver implementation.

```rust
pub enum DriverKind {
    Embedded,
    Tcp,
    UnixSocket,
    Memory,
}
```

The client API remains stable regardless of driver.

Example:

```rust
let client = WardrobeClient::open("./data")?;
```

Later:

```rust
let client = WardrobeClient::open(
    "wardrobe://db.company.com:24842"
)?;
```

No application code changes should be required.

---

## Authentication And Access Control

Wardrobe uses different authentication rules depending on the deployment model.

### Embedded Mode

Embedded mode is for local files and uses operating-system level permissions.

If the current process owner has read and write access to the `./data` directory, the user is treated as authenticated.

In embedded mode:

- No database password is required
- No certificate prompt is required
- The client should rely on the local file-system permissions already granted by the OS

This keeps local development and scripting low-friction.

### Remote Mode

Remote mode is for server-backed connections and requires explicit authentication during the connection handshake.

Supported approaches include:

- Mutual TLS (`mTLS`)
- API keys
- An `ADMIN_TOKEN` header when a lightweight shared secret is preferred

Passwords should be avoided for remote automation and CLI workflows.

### Client Behavior

`WardrobeClient` should branch on `DriverKind`:

- `DriverKind::Embedded` bypasses password and certificate prompts and validates local file-system access
- `DriverKind::Tcp` requires an explicit handshake before operations are allowed

The goal is to remove unnecessary barriers for embedded use while keeping remote access explicit and secure.

---

## Engine Layer

The engine owns:

- Storage behavior
- Record operations
- Drawer management
- Hydration
- Validation
- Indexing
- WAL recovery
- Scoped execution
- Discovery operations

The engine remains the source of truth for database behavior.

---

## Extension Layer

The Wardrobe engine supports an internal extension layer for lifecycle hooks.

These hooks allow the engine to intercept and shape record operations without pushing that logic into application code.

Examples of hooks include:

- `on_read`
- `on_upsert`

Supported extension behaviors include:

- Server-side data transformation
- Validation before records reach the WAL
- Cross-drawer hydration during document insertion

Example uses:

- Normalizing JSON records before storage
- Rejecting invalid payloads early
- Automatically creating relationship links for nested or related documents

The extension layer should remain internal to the engine and should not weaken the single client API or the deployment model boundaries.

---

## Transport Layer

The protocol layer uses framed request and response messages.

Supported transports:

- TCP
- Unix sockets

Remote transports should integrate with the explicit authentication handshake described above.

Future transports may be added without changing the command model.

Clients exchange:

```text
Command
CommandResult
```

payloads over the protocol.

---

## Administrative Surfaces

### CLI

The CLI is evolving into an administrative proxy.

Administrative operations should be routed through engine APIs rather than direct filesystem inspection.

Examples:

- Tenant inventory
- Database inventory
- Schema inventory
- Drawer inventory
- Catalog validation
- WAL verification

### Armoire

Armoire is the graphical administration experience for Wardrobe.

Armoire should build on the same administrative APIs exposed through the engine and protocol layers.

Armoire is not a separate authority on storage state.

The engine remains the source of truth.

---

## Storage Architecture

Current storage consists of:

- Drawer data files
- Drawer index files
- Metadata files
- WAL files

The long-term architecture is catalog-driven.

Target files:

```text
.catalog.drw
*.drw
*.idx
*.wal
```

The engine owns all storage behavior.

External tooling should interact through engine APIs whenever possible.

---

## Catalog-Driven Architecture

Filesystem discovery is being replaced by catalog-driven discovery.

The catalog becomes the authoritative source of truth for:

- Tenants
- Databases
- Schemas
- Drawers
- Physical storage locations
- Routing metadata

Objects not present in the catalog should not be considered valid database structures.

---

## Discovery APIs

Current discovery operations:

- `show_tenants`
- `show_databases`
- `show_schemas`
- `show_drawers`

Future discovery should read from the catalog rather than filesystem scans.

Inventory results should include:

- Name
- Record count
- Storage size
- File count
- Health status
- Catalog state

---

## Multi-Tenancy And Routing

Supported routing models:

- Database isolation
- Schema isolation
- Drawer isolation
- Tenant / Database / Schema coordinates

Routing is represented by:

- `StorageCoordinate`
- `StorageScope`

Tenant identity should be logical metadata rather than caller-controlled paths.

This allows future tenant relocation and sharding.

---

## Explicit Lifecycle Management

Structural operations should be explicit.

Examples:

- Create database
- Drop database
- Create schema
- Drop schema
- Create drawer
- Drop drawer

Expected sequence:

1. Validate request.
2. Write WAL entry.
3. Apply storage changes.
4. Commit catalog transaction.
5. Finalize operation.

This provides auditability and recovery guarantees.

---

## Binary WAL

Mutating operations should be recorded before storage changes occur.

The WAL provides:

- Crash recovery
- Replay
- Verification
- Drift detection
- Integrity validation

The WAL applies to:

- Data mutations
- Catalog mutations

---

## Language Bindings

All bindings should follow the same connection-driven model.

Example:

```python
client = Wardrobe.open("./data")
```

```typescript
const client = await Wardrobe.open("./data");
```

Connection strings determine driver selection.

Priority bindings:

1. TypeScript
2. Python

Future bindings:

- Java
- Dart
- D
- Additional ecosystem integrations

---

## Near-Term Direction

Current priorities include:

- Catalog-driven discovery
- Explicit lifecycle management
- Binary WAL improvements
- Administrative API consolidation
- Armoire development
- TypeScript bindings
- Python bindings
- Memory driver experimentation

The architectural goal remains unchanged:

- One data model
- One client API
- Multiple deployment modes
- Consistent behavior across languages
