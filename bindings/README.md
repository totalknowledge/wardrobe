# Wardrobe Language Binding Strategy

Wardrobe language bindings should ship as one public package per language ecosystem.
The package should select embedded or network behavior internally from the connection string rather than splitting users across separate install names.

## Package Rule

```text
one npm package
one pip package
one Rust crate
```

The package may contain multiple internal driver implementations, but users should import one public API.

## Network Driver Mode

Network mode is selected by TCP or Unix socket connection strings:

```text
wardrobe://localhost:24842
wardrobe://localhost
wardrobe+unix:///tmp/wardrobe.sock
```

Network mode should:

- Use the host language's ordinary socket APIs.
- Avoid loading embedded native storage artifacts.
- Expose the same public client API as embedded mode where practical.
- Serialize requests through Wardrobe `ProtocolFrame` envelopes carrying `Command` and `CommandResult` payloads.

The Rust `ConnectionTarget` and `WardrobeClient` APIs expose `requires_embedded_engine()` and `uses_socket_transport()` so bindings can decide whether a native embedded artifact is needed before loading it.

## Embedded Driver Mode

Embedded mode is selected by direct paths and file-oriented URIs:

```text
./data
wardrobe://local/path/to/data
wardrobe+file://path/to/data
file://path/to/data
```

Embedded mode should load the native storage engine artifact and execute directly in the caller's process.

Embedded packages may carry one or more native binaries built from the Rust core crate, such as platform-specific Node-API, Python extension, `.dll`, `.so`, or `.dylib` artifacts.
Those artifacts are internal packaging details; the public package name and public client API should remain the same as network mode.

Recommended first binding track:

- Start with one language ecosystem after the Rust protocol and client API stabilize.
- Prefer Node.js with `napi-rs` for desktop and application runtime coverage, or Python if local scripting becomes the first priority.
- Document the build matrix before publishing binaries, including operating system, CPU architecture, and runtime ABI.
- Keep network mode usable without loading the embedded artifact whenever the host package manager and runtime allow lazy loading.

Embedded mode is a good fit for:

- Desktop apps.
- Local automation.
- Long-running services that need no external database daemon.
- Electron and Tauri applications.
- Test suites that need disposable local storage.

## Maintenance Guidance

Do not create separate public packages for network-only and embedded-only usage unless there is a hard ecosystem constraint.
If an ecosystem needs separate internal artifacts for bundling, keep those artifacts behind one public package and one public API.

## C ABI Binding

The C ABI binding lives under `bindings/c` and exposes a narrow native surface for C and C-compatible consumers.

The binding should keep the same high-level deployment split as the rest of the binding strategy:

- embedded/local targets through direct filesystem paths
- network targets through Wardrobe URI connection strings

The C ABI is intended to remain a single public package, with any platform-specific loading or packaging details kept internal to the binding crate.

## JavaScript/TypeScript Binding

The JavaScript/TypeScript publish-readiness package lives under `bindings/js-ts`.

This package is prepared for validation, not publication. Use `npm publish --dry-run` from that directory to inspect the package contents without uploading to npm.

The package keeps the same single public npm package strategy:

- package name: `@wardrobe/database`
- Rust validation crate: `wardrobe-js-ts`
- TypeScript declarations: `index.d.ts`
- runtime entry point: `index.js`
- internal driver modules: `bindings/js-ts/client` and `bindings/js-ts/embedded`

Do not run `npm publish`, add npm credentials, or add registry tokens unless a release story explicitly approves publication.

## Python Bindings

The Python binding packages live under `bindings/python`.

The current Python direction intentionally uses two package names:

- `wardrobe-client` for pure Python TCP and Unix socket client access
- `wardrobe-embedded` for native embedded local storage access through a dedicated PyO3 extension

These packages are bindings only. Do not publish to PyPI, add credentials, or create release automation until a release story explicitly approves publication.
