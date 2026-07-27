# Wardrobe Server

`wardrobe-server` is the standalone Wardrobe daemon at version `0.26.725`. It hosts a shared `WardrobeEngine` behind a protocol boundary so multiple clients can connect to the same storage root over TCP or Unix sockets.

The server is licensed under Business Source License 1.1. Its change date is July 22, 2030, and its change license is GPL version 2 or later. See `server/LICENSE` for the authoritative terms.

This README is focused on:

- starting the server
- choosing a data directory
- connecting from Rust code
- understanding the current CLI and API story

## What The Server Does

The server owns:

- process lifecycle
- TCP and Unix-socket listener setup
- command routing into the core engine
- multi-tenant scoped execution
- protocol frame handling for request and response messages

The server does not duplicate storage logic. All record handling lives in `wardrobe-embedded`.

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

## Configuration File

The server can also load a TOML configuration file. The config file may be passed either as the first positional argument or with `--config`:

```text
cargo run -p wardrobe-server -- ./wardrobe.server.toml
cargo run -p wardrobe-server -- --config ./wardrobe.server.toml
```

CLI flags override values from the config file:

```text
cargo run -p wardrobe-server -- ./wardrobe.server.toml --data-dir ./override --tcp-bind 127.0.0.1:24843
```

Minimal config:

```toml
[data]
directory = "./data"

[network]
tcp_enabled = true
tcp_bind = "127.0.0.1:24842"
unix_socket_enabled = false
unix_socket = "/tmp/wardrobe.sock"

[cache]
max_cached_drawers = 128

[wal]
durability = "strict"
checkpoint_size_bytes = 1048576
checkpoint_ops = 1000

[logging]
level = "info"
format = "pretty"
destination = "stderr"
```

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
use wardrobe_client::{OperationFilter, OperationOptions, WardrobeClient};

fn main() -> std::io::Result<()> {
    let client = WardrobeClient::open_with_profile(
        "wardrobe://127.0.0.1:24842",
        "./profiles/adminuser/profile.toml",
    )?;

    let pointers = client.upsert(
        json!({
            "_id": "server_fire",
            "element": "Fire"
        }),
        OperationFilter::drawer("gem"),
        OperationOptions::default(),
    )?;

    let records = client.read(
        OperationFilter::drawer("gem"),
        OperationOptions::default(),
    )?;

    println!("stored: {:?}", pointers.into_pointers());
    println!("records: {}", records.records.len());
    Ok(())
}
```

Connection strings for the network daemon use the `wardrobe://host:port` shape:

- `wardrobe://127.0.0.1:24842`
- `wardrobe://localhost:24842`

## Scoped And Multi-Tenant Commands

The server supports the same command surface as the embedded engine, including:

- canonical data operations: read, upsert, delete, inspect, and count
- filtering and counting
- scoped execution through tenant/database/schema routing
- maintenance commands such as compact, backup, and restore
- status commands such as tenants, wardrobes, bays, and drawers

If your application needs multi-tenant routing, build those requests through the core command types and route them through the client or server protocol.

## Protocol Notes

The daemon speaks Wardrobe's framed binary protocol. At a high level:

- clients send a framed `Command`
- the server executes it against the engine
- the server returns a framed `CommandResult`

Status result payloads are flat. For example, a database inventory result serializes as `{"status":[...]}` rather than `{"status":{"Databases":[...]}}`.

Most users should not work with raw frames directly unless they are writing a custom client or language binding. The recommended path is to use `WardrobeClient`.

Wardrobe is still pre-stable, so the protocol intentionally has no compatibility aliases for removed command names. Remote clients should serialize only the canonical `Command` variants used by the embedded engine.

## CLI And Server

The CLI and the server share the same canonical command vocabulary.

- `wardrobe-server` hosts the shared daemon
- `wardrobe` can target embedded paths or remote `wardrobe://` connections

For Rust application code, use `WardrobeClient` from `wardrobe-client`. Other ecosystems use their server-only client package.

## Typical Workflow

1. Start the server with a chosen storage root.
2. Connect from your application using `WardrobeClient::open("wardrobe://host:24842")`.
3. Issue CRUD or scoped commands through the client API.
4. Use `wardrobe wardrobe://host:24842 ...` for operational CLI workflows when needed.

## Related Crates

- `wardrobe-embedded`: embedded engine and command model
- `wardrobe-client`: Rust TCP and Unix-socket client and protocol framing
- `wardrobe-server`: standalone daemon
- `wardrobe-cli`: package crate that installs the `wardrobe` operational CLI
- `@wardrobe/client`: pure JavaScript/TypeScript TCP and Unix socket client
- `wardrobe-client`: pure Python TCP and Unix socket client

## Current Direction

The server is part of a broader architectural move toward:

- registry-backed storage discovery
- explicit managed database and schema lifecycle
- logical tenant routing
- binary WAL-based integrity and recovery

As those pieces land, the daemon remains the process boundary that turns the embedded engine into a shared database service.
