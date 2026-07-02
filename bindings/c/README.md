# Wardrobe C ABI

The `bindings/c` crate exposes a narrow C-callable surface for Wardrobe consumers that need a native ABI.

Exported functions currently include:

- `wardrobe_cabi_version`
- `wardrobe_cabi_status_databases`
- `wardrobe_cabi_duplicate_path`
- `wardrobe_cabi_free_string`

The binding is intended as the low-level bridge for C and C-compatible integrations. Higher-level language wrappers can build on top of it without changing the ABI contract.
