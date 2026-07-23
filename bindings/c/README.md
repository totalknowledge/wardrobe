# Wardrobe C ABI

The MIT-licensed `bindings/c` crate exposes the `wardrobe-c` native library at version `0.26.723` for C and C-compatible consumers.

Exported functions currently include:

- `wardrobe_cabi_version`
- `wardrobe_cabi_status_databases`
- `wardrobe_cabi_execute_command`
- `wardrobe_cabi_relationship_command`
- `wardrobe_cabi_duplicate_path`
- `wardrobe_cabi_free_string`

`wardrobe_cabi_status_databases` returns a `WardrobeCStatus` with success and database-count fields. `wardrobe_cabi_relationship_command` returns a serialized relationship-alter command ready for `wardrobe_cabi_execute_command`. `wardrobe_cabi_execute_command` returns an allocated serialized `CommandResult`; callers release either allocated string with `wardrobe_cabi_free_string`. A status command result contains the direct status payload under the protocol's `status` operation envelope.

The binding is intended as the low-level bridge for C and C-compatible integrations. Higher-level language wrappers can build on top of it without changing the ABI contract.

Build and test it from the repository root:

```sh
cargo build -p wardrobe-c
cargo test -p wardrobe-c
```
