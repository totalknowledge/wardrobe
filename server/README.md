# Wardrobe Server

`wardrobe-server` is the standalone Wardrobe daemon. It hosts a shared `WardrobeEngine` behind a protocol boundary so multiple clients can connect to the same storage root over TCP.

This README is focused on:

- starting the server
- choosing a data directory
- connecting from Rust code
- understanding the current CLI and API story

## What The Server Does

The server owns:

- process lifecycle
- TCP listener setup
- command routing into the core engine
- multi-tenant scoped execution
- protocol frame handling for request and response messages

The server does not duplicate storage logic. All record handling still lives in `wardrobe-core`.

## Build And Run

From the workspace root:

```text
cargo run -p wardrobe-server -- --data-dir ./data --tcp-bind 127.0.0.1:24842
```

This starts the daemon with:

- storage root at `./data`
- TCP listener on `127.0.0.1:24842`

To cap simultaneously active connection workers without shutting down the listener:

```text
cargo run -p wardrobe-server -- --data-dir ./data --connection-pool-limit 16
```

For a quick startup validation without staying resident:

```text
cargo run -p wardrobe-server -- --data-dir ./data --check
```

That mode is useful for verifying that the data directory can be initialized successfully.

## Storage Root

The server points at a single root directory. Under that root, the engine can manage:

- direct local drawers
- database-scoped folders
- schema-scoped folders
- tenant/database/schema routed layouts

The exact physical layout is still evolving, but the server always delegates those routing decisions to the core engine.

## Connecting From Rust

The simplest client entry point is `WardrobeClient`.

```rust
use serde_json::json;
use wardrobe_core::{ReadRequest, ReadResult, WardrobeClient};

fn main() -> std::io::Result<()> {
    let client = WardrobeClient::open("wardrobe://127.0.0.1:24842")?;

    let pointers = client.upsert(
        "gem",
        json!({
            "_id": "server_fire",
            "element": "Fire"
        }),
    )?;

    let records = match client.read(ReadRequest::all("gem"))? {
        ReadResult::Records(records) => records,
        _ => Vec::new(),
    };

    println!("stored: {pointers:?}");
    println!("records: {}", records.len());
    Ok(())
}
```

Connection strings for the network daemon use the `wardrobe://host:port` shape:

- `wardrobe://127.0.0.1:24842`
- `wardrobe://localhost:24842`

## Scoped And Multi-Tenant Commands

The server supports the same command surface as the embedded engine, including:

- ordinary CRUD operations
- filtering and counting
- scoped execution through tenant/database/schema routing
- maintenance commands such as compaction and migration
- status commands such as tenants, wardrobes, bays, and drawers

If your application needs multi-tenant routing, build those requests through the core command types and route them through the client or server protocol.

## Protocol Notes

The daemon speaks Wardrobe's framed binary protocol. At a high level:

- clients send a framed `Command`
- the server executes it against the engine
- the server returns a framed `CommandResult`

Most users should not work with raw frames directly unless they are writing a custom client or language binding. The recommended path is to use `WardrobeClient`.

## CLI And Server

The CLI and the server share the same canonical command vocabulary.

- `wardrobe-server` hosts the shared daemon
- `wardrobe` can target embedded paths or remote `wardrobe://` connections

For application code, the stable path remains `WardrobeClient` in `wardrobe-core`.

## Typical Workflow

1. Start the server with a chosen storage root.
2. Connect from your application using `WardrobeClient::open("wardrobe://host:24842")`.
3. Issue CRUD or scoped commands through the client API.
4. Use `wardrobe --target wardrobe://host:24842 ...` for operational CLI workflows when needed.

## Related Crates

- `wardrobe-core`: embedded engine, client API, command types, protocol types
- `wardrobe-server`: standalone daemon
- `wardrobe-cli`: package crate that installs the `wardrobe` operational CLI

## Current Direction

The server is part of a broader architectural move toward:

- registry-backed storage discovery
- explicit managed database and schema lifecycle
- logical tenant routing
- binary WAL-based integrity and recovery

As those pieces land, the daemon remains the process boundary that turns the embedded engine into a shared database service.
