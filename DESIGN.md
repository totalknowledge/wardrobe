# Wardrobe Design

## Purpose

Wardrobe is a local-first database engine that is growing from a file-backed document store into a catalog-driven storage system with explicit lifecycle management, tenant routing, and binary durability records.

The current codebase still supports the earlier flat-file engine, but the direction is now clear:

- the engine owns storage behavior
- the catalog owns layout and routing truth
- the CLI becomes an administrative proxy
- the client API stays stable across embedded, network, and socket-backed operation

## Current Layers

### Core Engine

The embedded engine crate is the source of truth for record operations, hydration, filtering, counting, deletion, vacuuming, migration, and discovery.

The current public surface includes:

- `WardrobeEngine`
- `WardrobeClient`
- `Command`
- `CommandResult`
- `StorageCoordinate`
- `StorageScope`
- `StorageLocator`
- `StorageInventory`
- `QueryModifiers`
- `OrderDirection`

### Transport

The protocol layer uses framed request and response messages so network and socket clients can exchange commands without relying on text parsing.

The framing model is shared by:

- TCP clients
- Unix socket clients
- the standalone server daemon

### Storage Files

Today’s layout still uses drawer data files, index files, metadata sidecars, and WAL files.

The next catalog-driven phase is moving toward:

- `.catalog.drw` as the registry of managed entities and paths
- `.wal` as the binary durability log
- drawer data files for payload storage
- drawer index files for live record tracking and recycler state

## Discovery Model

The older engine behavior relied on scanning the filesystem to infer what existed.

That is being replaced by explicit discovery APIs and, eventually, by registry-backed lookup:

- `show_tenants`
- `show_databases`
- `show_schemas`
- `show_drawers`

The immediate goal is to make the admin surface deterministic and queryable.

The longer-term goal is to have the engine initialize from `.catalog.drw` rather than inferring structure from directory layout.

## Multi-Tenancy And Routing

Wardrobe supports several storage scopes because different deployment styles need different isolation levels.

The supported routing shapes are:

- database-level isolation
- schema-level isolation
- drawer-level isolation
- coordinate-based tenant/database/schema routing

The `StorageScope` and `StorageCoordinate` types are the bridge between logical application intent and physical location.

The newer design direction is to treat tenant identity as logical registry data, not as a raw path string.

## Administrative Surface

The CLI is moving from a direct file helper toward an administrative proxy for engine operations.

That means the CLI should eventually ask the engine for:

- database inventory
- schema inventory
- drawer inventory
- tenant inventory
- catalog validation
- WAL verification and recovery

The CLI should not be the authority on what exists on disk.

## Catalog-Driven Architecture

The new stories US-063 through US-066 describe the next major shift.

### Registry Bootstrapping

The engine should initialize from `.catalog.drw` at the storage root.

That registry becomes the source of truth for:

- active databases
- schemas
- drawers
- physical locations
- tenant routing

If a location is not present in the registry, the engine should reject it rather than trying to invent it from the filesystem.

### Explicit Managed Lifecycle

Database, schema, and drawer creation should be explicit operations.

The sequence should be:

1. allocate physical storage
2. persist the catalog transaction

This makes structural changes deliberate and audit-friendly.

### Logical Tenant Routing

Tenant identity should be resolved through the catalog, not through caller-provided paths.

That lets the engine move tenants between shards or storage roots without requiring application changes.

### Binary WAL

Mutating operations should be logged before data files are changed.

That WAL provides:

- crash recovery
- replay
- verification
- catalog/data drift detection

## Drawers And Inventory

Drawer discovery now matters in two forms:

- the user-facing inventory view
- the engine’s live record state

The current design treats drawer inventory as a structured result with:

- drawer name
- record count
- disk size
- companion file count

The live count should come from the merged index state, not from the data file itself.

That keeps administrative reporting fast and consistent with the engine’s own view of truth.

## Documentation

Documentation is part of the product, not an afterthought.

The README should stay aligned with:

- the current public API
- the supported usage shapes
- the storage architecture
- the admin commands
- the evolution toward catalog-driven storage

The design doc and story backlog should be updated whenever the architecture changes in a meaningful way.

## Near-Term Direction

The last stretch of stories points toward a more formal storage system:

- explicit registry loading
- explicit create/delete lifecycle
- logical tenant routing
- binary WAL support
- cleaner CLI administrative commands
- better documentation discipline

The current code is already moving that way; this document is meant to keep the shape visible as it grows.
