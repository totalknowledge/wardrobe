# Wardrobe CLI

Command-line management for Wardrobe embedded and remote targets.

The package crate is named `wardrobe-cli`, the installed binary is `wardrobe`, and the current MIT-licensed release is `0.26.723`.

```sh
cargo build --release -p wardrobe-cli
target/release/wardrobe --help
```

## Connection

The first positional argument is always the connection target. Use a filesystem path for embedded mode or a Wardrobe URI for remote mode.

```sh
wardrobe ./my-db status storage
wardrobe wardrobe://localhost:24842 status server
```

If no command follows the target, the CLI starts an interactive shell. Piped standard input executes a command non-interactively. Use `--pretty` for formatted JSON, and use `--log-level`, `--log-format`, `--log-destination`, and `--log-file` for application logging.

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
wardrobe ./my-db create wardrobe catalog
wardrobe ./my-db create bay catalog/public
wardrobe ./my-db create drawer catalog/public/book
wardrobe ./my-db upsert catalog/public/book '{"_id":"book-01","title":"The Lantern Index"}'
wardrobe ./my-db read catalog/public/book '{"title":"The Lantern Index"}'
wardrobe ./my-db count catalog/public/book
wardrobe ./my-db compact catalog/public
wardrobe ./my-db backup catalog/public ./backups/public.wrb
wardrobe ./my-db restore catalog/public-copy ./backups/public.wrb
```

Schema metadata examples:

```sh
wardrobe ./my-db alter index catalog/public/book title
wardrobe ./my-db alter relationship catalog/public/book author_id catalog/public/people M:1
wardrobe ./my-db drop index catalog/public/book title
```

Remote administration examples:

```sh
wardrobe wardrobe://localhost:24842 create user '{"username":"dev_admin","role":"operator"}'
wardrobe wardrobe://localhost:24842 grant permission dev_admin catalog/public:rud
wardrobe wardrobe://localhost:24842 revoke permission dev_admin catalog/public:d
wardrobe wardrobe://localhost:24842 drop user dev_admin
```

`status wardrobes`, `status bays`, and `status drawers` print raw JSON arrays. The arrays are not wrapped in result objects named after the requested status type.

Compatibility aliases are intentionally not provided for removed command vocabularies.
