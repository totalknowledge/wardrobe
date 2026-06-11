# Wardrobe Design

This document describes the current design of Wardrobe and the direction the project appears to be heading. It is meant to be a working design document, not a final specification.

## Purpose

Wardrobe is a small database platform written in Rust. It is currently a flat-file document store using JSON payloads, but the intended direction is a binary-file storage engine with document-style records, indexes, recyclable storage slots, and relationship hydration.

The fantasy naming is part of the model:

- A database is a wardrobe.
- A collection is a drawer.
- A record is an item stored in a drawer.
- A pointer links one item to another item in another drawer.

## Design Constraints

Wardrobe should stay modular as it grows. Each module should have a narrow job, and higher-level behavior should be built by composing those modules instead of letting one layer reach across the whole system.

Core boundaries:

- `reader.rs` owns reading from files.
- `writer.rs` owns writing to files.
- `drawer.rs` owns collection-level behavior for one drawer.
- `database.rs` owns the set of drawers and the storage directory.
- `engine.rs` owns high-level document behavior, including complex object distribution and relationship hydration.

The file layer should not understand the meaning of a `gem`, `weapon`, `character`, or any other domain object. It should only know how to read and write storage blocks. The drawer layer should know how to manage records in one collection. The engine layer should decide how complex JSON objects are split across drawers and later reconstructed.

Complex JSON payloads can contain sub-objects. A sub-object may be supplied in either of these forms:

- A full object that should be inserted or updated in its target drawer.
- A reference to an existing object, either as a Wardrobe pointer string or as an object containing only `_id`.

Arrays are a required part of the design, not a distant extension. Wardrobe should support arrays of scalar values, arrays of pointers, arrays of `_id` reference objects, and arrays of full nested objects. This matters for one-to-many relationships and should be handled before the relationship model becomes too deeply shaped around single object fields.

## Storage Model

Each drawer owns two files:

```text
<drawer>.drw
<drawer>_index.drw
```

The data file stores serialized record payloads. The index file stores metadata entries that map fields and keys to byte offsets in the data file.

Current examples:

```text
gem.drw
gem_index.drw
weapon.drw
weapon_index.drw
character.drw
character_index.drw
```

The current format is plaintext JSON with padding and newline delimiters. The future target is binary storage.

## Workspace Target Architecture

Wardrobe is expected to move from a single Rust crate into a Cargo workspace. The workspace should support a hybrid deployment model:

- An embedded Rust engine library.
- A standalone server daemon.
- A command-line management tool.
- Future cross-language bindings with one public package per language ecosystem.

Target layout:

```text
wardrobe/
  Cargo.toml
  Cargo.lock
  README.md
  DESIGN.md

  core/
    Cargo.toml
    src/

  server/
    Cargo.toml
    src/main.rs

  cli/
    Cargo.toml
    src/main.rs

  bindings/
    nodejs/
```

The repository root `Cargo.toml` should become the workspace configuration. `Cargo.lock` should remain committed so the full workspace builds deterministically.

### Core Crate

The `core` crate should be the single Rust library package. It should contain pure storage mechanics, drawer behavior, memory indexing, recyclers, query orchestration, and the public Rust API for embedded use and future server-backed access.

The core crate should not depend on server transport code, command-line parsing, or language binding packaging.

The driver-selection API belongs inside the core crate as an internal module boundary rather than a separate package boundary. This keeps Rust consumers on one crate while still allowing language bindings to choose their own packaging strategy later.

### Server Crate

The `server` crate should be a standalone daemon that depends on the core crate. It should expose Wardrobe over a protocol boundary such as TCP or IPC.

The server should own network lifecycle, request handling, protocol framing, and process-level operational concerns. It should not duplicate storage logic from the core crate.

### CLI Crate

The `cli` crate should be a command-line management tool. It may use the core crate directly for local file diagnostics and may also act as an administrative client for a running server.

The CLI should cover structural diagnostics, local file inspection, basic database operations, and server administration.

### Bindings Directory

The `bindings` directory is reserved for future language integrations. These bindings should come later, after the core storage model and protocol are more stable, because every public binding creates a maintenance commitment.

Initial binding targets may include Node.js and Python, but those should be added only when there is enough stability to justify maintaining their APIs, packaging, release process, and compatibility promises.

## Driver Pattern

Client libraries should expose a uniform public interface while choosing an internal driver based on the connection string.

Examples:

```text
./data
wardrobe://local/path/to/data
wardrobe+file://path/to/data
wardrobe://localhost:24842
wardrobe+unix:///tmp/wardrobe.sock
```

The direct path, `wardrobe://local/...`, `wardrobe+file://...`, and `file://...` forms activate an embedded driver that calls the compiled core storage engine in-process. The TCP form activates a network driver that serializes commands into protocol frames and sends them to the standalone Wardrobe server. The Unix socket form activates a local socket driver for same-machine client/server use.

The default TCP port is `24842`.

This keeps application code stable while allowing deployment to choose between local embedded storage and client/server storage.

## Foreign Language Package Strategy

Wardrobe should expose one public package per language ecosystem wherever possible. Package users should install one thing, while the package internally chooses between embedded, TCP, and Unix socket drivers from the connection string.

This keeps the ecosystem manageable:

- One Rust crate.
- One npm package when JavaScript bindings arrive.
- One pip package when Python bindings arrive.
- One public API shape per language.

### Internal Driver Modes

Embedded mode:

- Uses the local storage engine in-process.
- Good fit for local-first apps, desktop software, background jobs, and tests.

Client mode:

- Uses TCP or Unix sockets to talk to a running Wardrobe server.
- Does not touch local drawer files directly.
- Good fit for shared server deployments and language runtimes that should not load the embedded engine.

## Record Identity

Records use `_id` as the primary key.

IDs follow a pointer-style format:

```text
@<drawer>:lnk_<uuid>
```

Examples:

```text
@gem:lnk_ab7f2d90ad074e05987817bce6f941c3
@weapon:lnk_040b6a97ccae4949b3eb418876307378
```

When `upsert` receives a payload without `_id`, the engine creates a new ID based on the drawer name and a UUID.

## Main Components

### WardrobeEngine

`WardrobeEngine` is the high-level API. It wraps the lower-level database and drawer types.

Current responsibilities:

- Initialize the database directory.
- Upsert records into drawers.
- Generate IDs when needed.
- Decompose nested objects into linked records.
- Find all records in a drawer.
- Find one record by pointer ID.
- Resolve linked records back into nested objects.
- Distribute complex JSON payloads into the necessary drawers.
- Interpret full sub-objects, `_id`-only sub-objects, pointer strings, and arrays.

### Database

`Database` manages the storage directory and the set of active drawers.

Current responsibilities:

- Create the storage directory.
- Open drawers on demand.
- Return mutable drawer handles.
- Track active drawers in memory.
- Provide drawer access without leaking file read/write details to the engine.

### Drawer

`Drawer` owns the data and index files for one collection.

Current responsibilities:

- Open the data and index files.
- Rebuild in-memory indexes from disk.
- Write and update records.
- Maintain primary and secondary memory indexes.
- Write index log entries.
- Find records by primary key.
- Stream all records in the drawer.
- Tombstone old records.
- Register freed storage slots with recyclers.

Drawers should not orchestrate cross-drawer relationship hydration. They may expose records and indexes for their own collection, but resolving links across drawers belongs at the engine or query-context level.

### DatabaseReader

`DatabaseReader` reads records from disk.

Current responsibilities:

- Read a record at a byte offset.
- Stream a file while reporting each line offset.
- Read raw bytes at an offset.

Reader logic should stay focused on file reads, offsets, and decoding storage blocks. It should not decide how records are distributed across drawers or hydrated into object graphs.

### DatabaseWriter

`DatabaseWriter` writes records to disk.

Current responsibilities:

- Append aligned records.
- Append aligned index entries.
- Overwrite records at known offsets.
- Write tombstone markers.

Writer logic should stay focused on file writes, offsets, alignment, and tombstones. It should not decide collection membership, relationship rules, or JSON decomposition behavior.

### Recycler

`Recycler` tracks freed slots by size class.

Current structure:

```rust
HashMap<usize, Vec<u64>>
```

The key is the aligned byte size. The value is a stack of offsets that can hold a payload of exactly that size class.

## Write Sequence

The current update design follows a safety-oriented order:

1. Write the new payload into either a recycled slot or the end of the data file.
2. Write the index entry pointing to the new payload offset.
3. Update the in-memory primary index.
4. Tombstone the old payload location.
5. Register the old payload location with the data recycler.

The intent is that, if the process stops during an update, the old record and its old index remain valid until the replacement has been written and indexed.

## Tombstoning And Recycling

Deleted or outdated slots are marked with:

```text
!!DEAD!!
```

The slot is then registered with the appropriate recycler. There are separate recyclers for data records and index records:

- `data_recycler`
- `index_recycler`

This keeps data slot reuse separate from index slot reuse.

## Hydration

Hydration is the process of replacing pointer strings with the records they point to.

Example stored record:

```json
{
  "_id": "@weapon:lnk_040b6a97ccae4949b3eb418876307378",
  "name": "Axe",
  "damage": 50,
  "gem": "@gem:lnk_24968e50a7684f9ebe3ab34259d85541"
}
```

Example hydrated record:

```json
{
  "_id": "@weapon:lnk_040b6a97ccae4949b3eb418876307378",
  "name": "Axe",
  "damage": 50,
  "gem": {
    "_id": "@gem:lnk_24968e50a7684f9ebe3ab34259d85541",
    "element": "Air",
    "potency": 9700
  }
}
```

Current hydration recognizes string values that look like Wardrobe pointers:

```text
@<drawer>:lnk_<id>
```

It then looks up the target drawer and replaces the pointer with the referenced object when possible.

## Nested Object Decomposition

When `upsert` receives a nested JSON object, the engine currently treats that nested object as a child record. The field name becomes the target drawer name.

For example, upserting a weapon with a nested `gem` object can create or update a record in the `gem` drawer, then store a pointer to that gem in the weapon record.

This is an early form of document decomposition and relationship creation.

The intended design is broader than the current implementation:

- If a nested object contains more than `_id`, the engine should upsert that object into the appropriate drawer.
- If a nested object contains only `_id`, the engine should treat it as a reference to an existing record.
- If a field contains a Wardrobe pointer string, the engine should preserve it as a reference.
- If a field contains an array, the engine should inspect each item and normalize nested objects or references while preserving scalar values.

This allows callers to submit complex JSON documents without manually splitting every related object into separate drawer writes.

## Known Design Questions

### Drawer Loading

`find_all` currently searches drawers already loaded in memory. A future design should decide whether reads should:

- Automatically open drawers from disk.
- Require explicit drawer registration.
- Maintain a schema or manifest of known drawers.

### Borrowing And Hydration

Current hydration uses unsafe pointer casting to work around Rust borrowing limits while resolving links across drawers. This works as an experiment, but the design should move toward a safer approach.

Possible directions:

- Separate read-only lookup state from mutable writer state.
- Build a temporary immutable registry for hydration.
- Resolve relationships through engine-level orchestration instead of drawer-level recursion.
- Use interior mutability carefully if shared lookup state is required.

### Index Entry Sizes

The index recycler tracks slots by size class, but replacement and tombstoning need to know the exact old slot size. The design should make old index entry size explicit so index tombstones do not accidentally use the new entry size.

### Secondary Indexes

The drawer structure has a secondary index map and unique constraint list, but this feature is not fully exercised by the high-level engine yet.

Future design should define:

- Non-unique secondary indexes.
- Unique secondary indexes.
- Query APIs for secondary fields.
- Index rebuild behavior on startup.
- Index update behavior when indexed fields change.

### Binary Storage

The project is expected to move from aligned plaintext JSON lines to binary files.

Open decisions:

- Whether each record has a fixed-size header.
- How payload length and size class are stored.
- Whether payloads remain JSON internally or move to a binary encoding.
- How tombstones are represented.
- How indexes refer to data offsets and lengths.
- Whether checksums or version markers are included.

## Future Relationship Goals

Wardrobe should support standard relationship patterns:

- One-to-one links through a single pointer string.
- One-to-many links through arrays of pointer strings.
- One-to-many links through arrays of full objects or `_id` reference objects during upsert.
- Recursive hydration for nested relationships.
- Array hydration for lists of pointers.
- Optional partial hydration for performance.
- Cycle detection to avoid infinite recursive reads.

## Testing Priorities

The next tests should focus on storage correctness:

- Upsert creates a record and index entry.
- Upsert with the same `_id` updates the record.
- Old records are tombstoned only after the new index is written.
- Recycled slots are reused only when the size class matches.
- Existing drawers can be loaded from disk and queried.
- Linked records hydrate correctly.
- Missing pointers remain as pointers instead of crashing.
- Array relationships hydrate correctly once implemented.
