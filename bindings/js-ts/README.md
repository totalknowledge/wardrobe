# Wardrobe JavaScript/TypeScript Package

This directory contains the publish-readiness scaffold for the Wardrobe JavaScript and TypeScript npm package.

The package is intentionally validated with `npm publish --dry-run` only. Do not run `npm publish` until a release story explicitly approves publishing and registry credentials.

## Package Metadata

- npm package: `@wardrobe/database`
- Rust crate: `wardrobe-js-ts`
- version: `0.1.0`
- license: see `LICENSE`
- repository: `https://github.com/totalknowledge/wardrobe`
- TypeScript declarations: `index.d.ts`

## Validation Commands

Run these commands from the repository root:

```powershell
cargo fmt --manifest-path bindings/js-ts/Cargo.toml --package wardrobe-js-ts -- --check
cargo clippy --manifest-path bindings/js-ts/Cargo.toml --all-targets --all-features --no-deps -- -D warnings
cargo test --manifest-path bindings/js-ts/Cargo.toml --all-targets --all-features
Set-Location bindings/js-ts
npm publish --dry-run
```

The dry run verifies npm package contents without uploading anything to the registry.

## Current Surface

The current package surface exposes binding metadata, the canonical Wardrobe operation names, connection-target classification helpers, and internal client/embedded driver modules under this JS/TS binding folder.

Connection strings follow the same embedded, TCP, and Unix socket routing rules as `WardrobeClient`:

- `./data`
- `wardrobe://local/path/to/data`
- `wardrobe+file://path/to/data`
- `file://path/to/data`
- `wardrobe://localhost:24842`
- `wardrobe://localhost`
- `wardrobe+unix:///tmp/wardrobe.sock`

The `client` and `embedded` directories are internal parts of this package, not separate language binding roots under `bindings`.
