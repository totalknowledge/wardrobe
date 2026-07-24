# Wardrobe JavaScript/TypeScript Package

This directory contains the publish-readiness workspace for the Wardrobe JavaScript and TypeScript npm packages. Both packages require Node.js 24 or newer and are MIT licensed.

The package is intentionally validated with `npm publish --dry-run` only. Do not run `npm publish` until a release story explicitly approves publishing and registry credentials.

## Package Metadata

- server package: `@wardrobe/client`
- embedded package: `@wardrobe/embedded`
- Rust crate: `wardrobe-js-ts`
- version: `0.26.725`
- license: see `LICENSE`
- repository: `https://github.com/totalknowledge/wardrobe`
- TypeScript declarations: `index.d.ts`

## Validation Commands

Run these commands from the repository root:

```powershell
cargo fmt --manifest-path bindings/js-ts/Cargo.toml --package wardrobe-js-ts -- --check
cargo clippy --manifest-path bindings/js-ts/Cargo.toml --all-targets --all-features --no-deps -- -D warnings
cargo test --manifest-path bindings/js-ts/Cargo.toml --all-targets --all-features
npm publish --dry-run ./bindings/js-ts/client
npm publish --dry-run ./bindings/js-ts/embedded
```

The dry run verifies npm package contents without uploading anything to the registry.

## Current Surface

Both packages expose `WardrobeClient` with `upsert`, `read`, `delete`, `inspect`, `count`, `clean`, `create`, `alter`, `drop`, `backup`, `restore`, `grant`, `revoke`, and `status`, plus `relationshipRequest` for constructing the canonical relationship declaration. `@wardrobe/client` connects to a running server, while `@wardrobe/embedded` opens local storage through the native binding without starting a server.

Nested objects and array elements without `_id` are stored inline. An `_id`-only object is a strict reference, while an object with `_id` and additional fields cascade-upserts its target. Mixed arrays classify each element independently; use `{ hydrate: false }` in read options to retain stored pointer strings.

Database, schema, and drawer status calls return `StorageInventory[]`, `string[]`, and `StorageInventory[]` directly. The TypeScript declarations include overloads for those request shapes.

Connection targets follow the Rust routing syntax, split by package:

- `@wardrobe/embedded`: `./data`, `wardrobe://local/path/to/data`, `wardrobe+file://path/to/data`, or `file://path/to/data`
- `@wardrobe/client`: `wardrobe://localhost:24842`, `wardrobe://localhost`, or `wardrobe+unix:///tmp/wardrobe.sock`

The `client` and `embedded` directories are independently installable npm package roots.

Repository samples use a `file:` dependency on `bindings/js-ts/embedded`, so `npm install` installs the built local binding directly rather than resolving a registry package.
