# Wardrobe

Wardrobe is an experimental database engine written in Rust. The current demo uses fantasy game data, such as gems, weapons, and characters, but the real goal is the storage engine underneath it.

At a high level, Wardrobe is intended to become a fast, single-process NoSQL document store that keeps records in flat files. The current implementation stores JSON payloads in aligned text records, with a path toward storing items in binary files later.

## Project Goals

- Store document-like records in local flat files.
- Organize records into semantic collections called drawers.
- Give each record a stable pointer-style identity, such as `@gem:lnk_...`.
- Maintain file-backed indexes for fast lookup by primary key.
- Support updates without rewriting the whole file.
- Reuse freed disk space through tombstoned slots and size-class recycling.
- Resolve links between records so related objects can be hydrated on read.
- Evolve from the current JSON-line storage format toward a binary storage format.

## What Has Been Implemented

- A Rust crate and demo binary for the Wardrobe engine.
- A `WardrobeEngine` API with `new`, `upsert`, `find_all`, and `find_by_id`.
- Database initialization that creates the storage directory if it does not exist.
- Drawer files using the `.drw` extension.
- Separate data and index files for each drawer.
- Primary key indexes based on the `_id` field.
- Pointer-style IDs generated with UUIDs when `_id` is not supplied.
- Upsert behavior that writes a new version and tombstones the old record after the new index is written.
- Aligned record writes using size classes.
- A `Recycler` for reusing tombstoned slots.
- Basic nested object decomposition during upsert.
- Safe engine-level pointer hydration during reads without raw pointer casting.
- Demo drawers for gems, weapons, and characters.

## Current Shape

The storage layout currently looks like this:

```text
wardrobe/
  gem.drw
  gem_index.drw
  weapon.drw
  weapon_index.drw
  character.drw
  character_index.drw
```

Each drawer has a data file and an index file. The data file stores serialized records. The index file maps lookup keys to byte offsets in the data file.

## Next Design Areas

- Make drawer discovery and loading more explicit so reads work from existing files on disk.
- Formalize secondary indexes and unique constraints.
- Add support for one-to-many relationships through arrays of pointers.
- Add tests around update ordering, tombstoning, recycling, and hydration.
- Define the binary record format that will eventually replace plaintext JSON records.

## Tests And Coverage

Run the behavior suite with:

```text
cargo test
```

`cargo test` does not produce file coverage by itself. To get a coverage summary, install `cargo-llvm-cov` and run the configured alias:

A coverage-summary alias is configured in `.cargo/config.toml`:

```text
cargo coverage-summary
```

The alias runs:

```text
cargo llvm-cov --workspace --summary-only
```

Install the coverage tool with:

```text
cargo install cargo-llvm-cov
```
