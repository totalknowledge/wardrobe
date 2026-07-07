# Wardrobe Python Bindings

This directory contains the Python binding packages for Wardrobe.

These packages are binding scaffolds only. Do not publish them until a release story explicitly approves PyPI publication.

## Packages

- `wardrobe-client`: pure Python TCP and Unix socket client for server-backed Wardrobe connections.
- `wardrobe-embedded`: native Python extension for embedded local Wardrobe storage using `wardrobe-core` directly.

The package split is intentional for Python:

- Use `wardrobe-client` when connecting to a running Wardrobe server.
- Use `wardrobe-embedded` when local embedded storage is required.

## Local Validation

```powershell
python -m py_compile bindings/python/wardrobe-client/src/wardrobe_client/*.py
python -m py_compile bindings/python/wardrobe-embedded/src/wardrobe_embedded/*.py
```

The embedded package is configured for `maturin`, but this story does not publish wheels or upload artifacts.
