# wardrobe-cli

A command-line interface for managing Wardrobe databases. Supports both embedded (file-based) and network connections.

## Installation

```sh
cargo build --release -p wardrobe-cli
```

The compiled binary is placed at `target/release/wardrobe-cli`.

## Connection

All commands accept a connection string via `--target` (aliases: `--connection`, `--data-dir`). If omitted, the CLI defaults to `./wardrobe`.

| Connection type | Format |
|---|---|
| Embedded (local path) | `./my-db` or any filesystem path |
| Network (TCP) | `wardrobe://host:port` |
| Unix socket | `wardrobe+unix:///path/to/socket` |

## Global Flags

| Flag | Description |
|---|---|
| `--target <value>` | Connection string or local path (default: `./wardrobe`) |
| `--connection <value>` | Alias for `--target` |
| `--data-dir <value>` | Alias for `--target` |
| `--pretty` | Pretty-print JSON output (default: compact) |
| `--help`, `-h` | Print usage and exit |

## Commands

### `drawers`

List all known drawers in the embedded database.

> Embedded connections only.

```sh
wardrobe-cli --target ./my-db drawers
```

```
character
gem
weapon
```

---

### `records <drawer>`

Fetch and print all hydrated records from a drawer as JSON.

```sh
wardrobe-cli --target ./my-db records gem
wardrobe-cli --target ./my-db --pretty records gem
```

```json
[
  { "_id": "fire", "element": "Fire" },
  { "_id": "ice",  "element": "Ice"  }
]
```

---

### `upsert <drawer> <json>`

Insert or update a record in a drawer. Prints the assigned record pointer on success.

Aliases: `insert`, `create`.

```sh
wardrobe-cli --target ./wardrobe upsert gem '{"_id":"@gem:lnk_fire","element":"Fire"}'
wardrobe-cli --target ./wardrobe insert gem '{"_id":"@gem:lnk_ice","element":"Ice"}'
```

```
@gem:lnk_fire
```

If `_id` is omitted, Wardrobe generates one automatically.

```sh
wardrobe-cli --target ./my-db upsert gem '{"element":"Wind"}'
```

```
@gem:lnk_<generated-id>
```

---

### `find <drawer> <json>`

Query records in a drawer with a JSON filter. Prints matching hydrated records as JSON.

Aliases: `get`, `query`.

```sh
wardrobe-cli --target ./my-db find gem '{"element":"Fire"}'
wardrobe-cli --target ./my-db --pretty query gem '{"power":88}'
```

---

### `delete <drawer> <json>`

Delete a record identified by the `_id` field in the JSON payload.

Alias: `remove`.

```sh
wardrobe-cli --target ./my-db delete gem '{"_id":"@gem:fire"}'
wardrobe-cli --target ./my-db remove gem '{"_id":"fire"}'
```

---

### `delete-by-id <pointer>`

Delete the record identified by its pointer. Prints `deleted: true` when the record was found and removed, or `deleted: false` when no matching record existed.

```sh
wardrobe-cli --target ./my-db delete-by-id @gem:lnk_fire
```

```
deleted: true
```

---

### `define database <name>`

Create and register a database through the Wardrobe engine catalog.

Alias: `create-db <name>`.

```sh
wardrobe-cli --target ./my-db define database admin_db
wardrobe-cli --target ./my-db create-db admin_db
```

---

### `define schema <database> <name>`

Create and register a schema under an existing database. If the parent database does not exist, the command fails with a non-zero exit status.

Alias: `create-schema <database> <name>`.

```sh
wardrobe-cli --target ./my-db define schema admin_db public
wardrobe-cli --target ./my-db create-schema admin_db public
```

---

### `define drawer <database> <schema> <name>`

Create and register a drawer under an existing database/schema pair.

Alias: `create-drawer <database> <schema> <name>`.

```sh
wardrobe-cli --target ./my-db define drawer admin_db public gem
wardrobe-cli --target ./my-db create-drawer admin_db public gem
```

---

### `manage user <action> <json>`

Send a user administration request through the remote client protocol.

Aliases: `auth <action> <json>`, `rbac <action> <json>`.

Embedded mode rejects this command because local file permissions are the embedded authorization boundary. Remote servers remain responsible for validating privileges and applying credential changes.

```sh
wardrobe-cli --target wardrobe://localhost:7777 manage user grant '{"user":"alice","role":"admin"}'
```

---

### `inspect <drawer>`

Show the presence and byte size of each companion file for a drawer (data, index, meta).

> Embedded connections only.

```sh
wardrobe-cli --target ./my-db inspect gem
```

```
Drawer: gem
data:  present (1024 bytes)
index: present (256 bytes)
meta:  present (128 bytes)
```

---

### `diagnose`

Scan every drawer and report any missing data, index, or meta files.

> Embedded connections only.

```sh
wardrobe-cli --target ./my-db diagnose
```

```
Storage directory: ./my-db
Drawer count: 3
Status: ok
```

If problems are detected:

```
Storage directory: ./my-db
Drawer count: 3
Status: issues found
weapon: missing index file
```

---

### `show <type>`

List catalog and discovery information.

Aliases: `ls`, `list`.

Supported forms:

```sh
wardrobe-cli --target ./my-db show tenants
wardrobe-cli --target ./my-db show databases
wardrobe-cli --target ./my-db list schemas admin_db
wardrobe-cli --target ./my-db ls drawers admin_db public
```

---

### `show-databases`

List all discovered databases available through the current connection.

```sh
wardrobe-cli --target wardrobe://localhost:7777 show-databases
wardrobe-cli --target wardrobe://localhost:7777 --pretty show-databases
```

---

### `show-schemas <database>`

List all schemas within a database.

```sh
wardrobe-cli --target wardrobe://localhost:7777 show-schemas my_database
```

---

### `show-drawers <database> <schema>`

List all drawers within a specific database and schema.

```sh
wardrobe-cli --target wardrobe://localhost:7777 show-drawers my_database public
```

---

## Interactive REPL

When no command is provided on the command line, `wardrobe-cli` enters an interactive REPL. The prompt shows the active connection target.

```sh
wardrobe-cli --target ./my-db
```

```
wardrobe:embedded:./my-db> drawers
character
gem
weapon
wardrobe:embedded:./my-db> records gem
[{"_id":"fire","element":"Fire"}]
wardrobe:embedded:./my-db> exit
```

Type `exit` or `quit` to leave the REPL, or send EOF (`Ctrl+D`).

---

## Piped / Scripted Input

Commands can be piped via stdin. The CLI reads a single command from stdin when it detects a non-TTY input stream.

```sh
echo 'drawers' | wardrobe-cli --target ./my-db
```

```sh
echo 'upsert gem {"element":"Earth"}' | wardrobe-cli --target ./my-db
```

This makes `wardrobe-cli` composable with shell scripts and other tools.

---

## Exit Codes

| Code | Meaning |
|---|---|
| `0` | Success |
| non-zero | Error (invalid arguments, unknown command, I/O failure, etc.) |

Error details are written to `stderr`.
