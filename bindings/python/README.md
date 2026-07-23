# Wardrobe Python Bindings

This directory contains the Python binding packages for Wardrobe.

Both packages are at version `0.26.723`, require Python 3.10 or newer, and use the MIT license.

These packages are binding scaffolds only. Do not publish them until a release story explicitly approves PyPI publication.

## Packages

- `wardrobe-client`: pure Python TCP and Unix socket client for server-backed Wardrobe connections.
- `wardrobe-embedded`: native Python extension for embedded local Wardrobe storage using `wardrobe-core` directly.

The package split is intentional for Python:

- Use `wardrobe-client` when connecting to a running Wardrobe server.
- Use `wardrobe-embedded` when local embedded storage is required.

Both expose the same serialized command operations and `relationship_request` constructor. Database, schema, and drawer status calls return Python lists directly without result-side variant wrappers.

Nested objects and array elements without `_id` are stored inline. An `_id`-only object is a strict reference, while an object with `_id` and additional fields cascade-upserts its target. Mixed arrays classify each element independently; pass `{"hydrate": False}` in read options to retain stored pointer strings.

## Local Validation

```powershell
python -m py_compile bindings/python/wardrobe-client/src/wardrobe_client/*.py
python -m py_compile bindings/python/wardrobe-embedded/src/wardrobe_embedded/*.py
```

The embedded package is configured for `maturin`, but this story does not publish wheels or upload artifacts.

The non-trivial sample under `samples/python` builds or loads the local embedded extension, stores related publishing data under repository-root `./wardrobe`, queries it, verifies pointers, and performs cleanup in the `public_py` bay.
