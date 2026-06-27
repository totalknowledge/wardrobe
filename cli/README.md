# Wardrobe CLI

Command-line management for Wardrobe embedded and remote targets.

The package crate is still named `wardrobe-cli`, but the installed binary is `wardrobe`.

```sh
cargo build --release -p wardrobe-cli
target/release/wardrobe --help
```

## Connection

All commands accept a connection string via `--target`, `--connection`, or `--data-dir`.
If omitted, the CLI defaults to `./wardrobe`.

```sh
wardrobe --target ./my-db status storage
wardrobe --target wardrobe://localhost:7777 status server
```

## Canonical Commands

Data commands:

```text
read <path> <?json_filter> <?json_options>
upsert <path> <json_payload> <?json_filter> <?json_options>
delete <pointer>
delete <path> <json_filter_or_id> <?json_options>
inspect <path> <?json_filter> <?json_options>
count <path> <?json_filter> <?json_options>
```

Structural commands:

```text
create wardrobe <wardrobe>
create bay <wardrobe>/<bay>
create drawer <wardrobe>/<bay>/<drawer>
create user <json_user_payload>
alter <index|key|constraint|relationship|trigger|cascade-delete> <drawer_path> <field> <?extra_args>
drop wardrobe <wardrobe>
drop bay <wardrobe>/<bay>
drop drawer <wardrobe>/<bay>/<drawer>
drop user <username>
drop <index|key|constraint|relationship|trigger|cascade-delete> <drawer_path> <field> <?extra_args>
```

Maintenance commands:

```text
compact <wardrobe|bay|drawer_path>
backup <source_path> <destination_archive_path>
restore <destination_path> <source_archive_path>
```

Admin/runtime commands:

```text
grant permission <username> <path:rights>
revoke permission <username> <path:rights>
status <tenants|wardrobes|bays|drawers|wal|storage|path|drawer-names|cached-drawer-count|server|config> <?path>
```

## Examples

```sh
wardrobe --target ./my-db create wardrobe catalog
wardrobe --target ./my-db create bay catalog/public
wardrobe --target ./my-db create drawer catalog/public/book
wardrobe --target ./my-db upsert catalog/public/book '{"_id":"book-01","title":"The Lantern Index"}'
wardrobe --target ./my-db read catalog/public/book '{"title":"The Lantern Index"}'
wardrobe --target ./my-db count catalog/public/book
wardrobe --target ./my-db compact catalog/public
wardrobe --target ./my-db backup catalog/public ./backups/public.wrb
wardrobe --target ./my-db restore catalog/public-copy ./backups/public.wrb
```

Schema metadata examples:

```sh
wardrobe --target ./my-db alter index catalog/public/book title
wardrobe --target ./my-db alter relationship catalog/public/book author_id catalog/public/people M:1
wardrobe --target ./my-db drop index catalog/public/book title
```

Remote administration examples:

```sh
wardrobe --target wardrobe://localhost:7777 create user '{"username":"dev_admin","role":"operator"}'
wardrobe --target wardrobe://localhost:7777 grant permission dev_admin catalog/public:rud
wardrobe --target wardrobe://localhost:7777 revoke permission dev_admin catalog/public:d
wardrobe --target wardrobe://localhost:7777 drop user dev_admin
```

This is a breaking CLI vocabulary cleanup. Compatibility aliases are intentionally not provided.
