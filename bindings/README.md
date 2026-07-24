# Wardrobe Language Bindings

Current binding version: `0.26.725`.

Wardrobe provides separate network and embedded packages where the host ecosystem benefits from keeping native storage artifacts out of server-only applications.

| Ecosystem | Network/server package | Embedded package |
|---|---|---|
| JavaScript/TypeScript | `@wardrobe/client` | `@wardrobe/embedded` |
| Python | `wardrobe-client` | `wardrobe-embedded` |
| C ABI | `wardrobe-c` | `wardrobe-c` |

Rust applications use `WardrobeClient` from `wardrobe-core`; the connection target selects embedded, TCP, or Unix socket execution.

## Network Driver Mode

Network mode is selected by TCP or Unix socket connection strings:

```text
wardrobe://localhost:24842
wardrobe://localhost
wardrobe+unix:///tmp/wardrobe.sock
```

Network packages:

- Use the host language's ordinary socket APIs.
- Avoid loading embedded native storage artifacts.
- Expose the canonical command surface.
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

Embedded mode loads the native storage engine artifact and executes directly in the caller's process.

Embedded packages carry platform-specific native artifacts built from `wardrobe-core`, such as Node-API libraries and Python extensions. They execute commands in the caller's process and do not start or connect to a local Wardrobe server.

Embedded mode is a good fit for:

- Desktop apps.
- Local automation.
- Long-running services that need no external database daemon.
- Electron and Tauri applications.
- Test suites that need disposable local storage.

## Shared Contract

All bindings use the shared serialized `Command` and `CommandResult` model. Rust uses `AlterRequest::relationship`, JavaScript and TypeScript use `relationshipRequest`, Python uses `relationship_request`, and C uses `wardrobe_cabi_relationship_command` to construct the same relationship declaration. Database, schema, and drawer status requests return raw arrays directly; result payloads are not wrapped in `Databases`, `Schemas`, or `Drawers` objects.

Objects and array elements without `_id` remain inline values in the parent record. An `_id`-only object is a strict reference; an object with `_id` and additional fields cascade-upserts its target. Each mixed-array element is classified independently, and non-hydrated reads preserve stored reference pointers.

## C ABI Binding

The C ABI binding lives under `bindings/c` and exposes a narrow native surface for C and C-compatible consumers.

The binding keeps the same high-level deployment split as the rest of the binding strategy:

- embedded/local targets through direct filesystem paths
- network targets through Wardrobe URI connection strings

The C ABI remains a single native package. Its current exports cover version reporting, database status counts, relationship command construction, serialized command execution, path duplication, and string release.

## JavaScript/TypeScript Binding

The JavaScript/TypeScript publish-readiness package lives under `bindings/js-ts`.

These packages are prepared for validation, not publication. Use `npm publish --dry-run` from each package root to inspect package contents without uploading to npm.

The binding exposes separate packages for server and embedded access:

- server package: `@wardrobe/client`
- embedded package: `@wardrobe/embedded`
- Rust validation crate: `wardrobe-js-ts`
- package roots: `bindings/js-ts/client` and `bindings/js-ts/embedded`

Both packages require Node.js 24 or newer. Do not run `npm publish`, add npm credentials, or add registry tokens unless a release story explicitly approves publication.

## Python Bindings

The Python binding packages live under `bindings/python`.

The current Python direction intentionally uses two package names:

- `wardrobe-client` for pure Python TCP and Unix socket client access
- `wardrobe-embedded` for native embedded local storage access through a dedicated PyO3 extension

These packages are bindings only. Do not publish to PyPI, add credentials, or create release automation until a release story explicitly approves publication.

Both packages require Python 3.10 or newer. `wardrobe-embedded` is built with PyO3 and maturin; it does not use the C ABI.

## Samples

The repository includes equivalent non-trivial embedded examples under `samples/javascript`, `samples/typescript`, and `samples/python`. Each creates publishing structures, stores related records, queries and verifies them, performs cleanup, and uses ignored repository-root `./wardrobe` storage.

## Licensing

The C, JavaScript/TypeScript, and Python bindings are MIT licensed. Each distributable package includes its own `LICENSE` file.
