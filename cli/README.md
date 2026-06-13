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

```sh
wardrobe-cli --target ./wardrobe upsert gem '{"_id":"@gem:lnk_fire","element":"Fire"}'
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

### `delete-by-id <pointer>`

Delete the record identified by its pointer. Prints `deleted: true` when the record was found and removed, or `deleted: false` when no matching record existed.

```sh
wardrobe-cli --target ./my-db delete-by-id @gem:lnk_fire
```

```
deleted: true
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
