# Wardrobe

[![coverage](https://github.com/totalknowledge/wardrobe/actions/workflows/coverage.yml/badge.svg)](https://github.com/totalknowledge/wardrobe/actions/workflows/coverage.yml)

Current workspace release: `0.26.725`.

Linux AMD64 downloads:

- Wardrobe CLI: [.deb](https://github.com/totalknowledge/wardrobe/releases/download/v0.26.725/wardrobe-cli_0.26.725_amd64.deb) | [.tar.gz](https://github.com/totalknowledge/wardrobe/releases/download/v0.26.725/wardrobe-cli_0.26.725_amd64.tar.gz)
- Wardrobe Server: [.deb](https://github.com/totalknowledge/wardrobe/releases/download/v0.26.725/wardrobe-server_0.26.725_amd64.deb) | [.tar.gz](https://github.com/totalknowledge/wardrobe/releases/download/v0.26.725/wardrobe-server_0.26.725_amd64.tar.gz)

Wardrobe is a BSON document database with native relationship support, designed to bridge the gap between traditional document stores, relational databases, and graph databases. Complex object graphs are stored naturally, automatically separating embedded documents from related entities while preserving relationships, referential integrity, and intuitive traversal.

It combines the flexibility of BSON documents with built-in referential integrity, relationship traversal, automatic hydration, cascading operations, and document validation without requiring separate graph storage or complex object-relational mapping.

Written in Rust, Wardrobe can run directly inside your application as an embedded database or as a standalone server. The separate embedded and client crates expose the same canonical operation vocabulary without duplicating the storage engine.

Unlike many document databases that treat references as ordinary strings, Wardrobe understands relationships between documents. References can participate in integrity validation, automatic object hydration, virtual relationships, cascading updates and deletes, and efficient traversal while remaining simple fields inside your documents.

Wardrobes, bays, and drawers are separate storage scopes, not a document hierarchy. A wardrobe is analogous to the database scope used by other systems; a bay is a namespace-like scope; and a drawer is the document container, comparable to a MongoDB collection or relational table. Documents live in drawers and can remain embedded or participate in relationships.

Under the hood, Wardrobe stores documents in versioned binary record files backed by native indexes, write-ahead logging, crash recovery, archive-based backup and restore, online compaction, and bounded in-memory caching. The result is a lightweight storage engine that requires no external services while providing capabilities typically associated with much larger database systems.

Whether you're building desktop software, embedded systems, developer tools, games, SaaS platforms, or self-hosted services, Wardrobe provides a deployment-neutral database that scales from a single executable to a networked server without changing how your application interacts with its data.

## Workspace

```text
wardrobe/
  client/                TCP and Unix-socket client, connection model, and protocol framing
  embedded/              Embedded engine, storage, and command model
  cli/                   Command-line administration and operations
  server/                Standalone TCP and Unix-socket daemon
  bindings/              C, JavaScript/TypeScript, and Python bindings
  samples/               Rust, C ABI, JavaScript, TypeScript, and Python examples
  utilities/armoire/     Angular and Tauri database administration application
  utilities/benchmark/   Cross-engine performance benchmark
  utilities/scripts/     Scripted CLI workflow
```

## Terminology

Wardrobe uses one structural vocabulary across the Rust API, CLI, protocol, bindings, and end-user documentation:

- `wardrobe`
- `bay`
- `drawer`

These describe the roles familiar from other database systems:

- a wardrobe is a database-like storage scope
- a bay is a namespace-like storage scope
- a drawer is a collection- or table-like document container

`tenant` remains a separate routing dimension across all surfaces.

## Current Public API

`wardrobe-embedded` exports the embedded engine, shared command model, configuration, catalog, and WAL types:

- Embedded entry point: `WardrobeEngine`
- Routing types: `StorageCoordinate`, `StorageScope`, `StorageLocator`, `StorageInventory`
- Query and result types: `OperationFilter`, `OperationOptions`, `ReturnShape`, `ReadResult`, `UpsertResult`, `DeleteResult`, `InspectResult`, `QueryModifiers`, `OrderDirection`
- Lifecycle request types: `CreateRequest`, `CreateResult`, `AlterRequest`, `DropRequest`, `CompactRequest`, `CompactMode`, `StatusRequest`, `TypedStatusRequest`, `StatusRequestOutput`, `PermissionRequest`
- Inspection, verification, and recovery types: `DrawerInspectionMetrics`, `CheckReport`, `CheckEntry`, `StorageDiagnosis`, `VacuumReport`, `WalVerification`, `BackupArchive`, `BackupArchiveFile`, `RestoreReport`
- Configuration types: `WardrobeConfig`, `WardrobeEngineBuilder`, `DataConfig`, `NetworkConfig`, `CacheConfig`, `WalConfig`, `TransactionConfig`, `SecurityConfig`
- Low-level storage implementation types are not part of the stable application API.
- Catalog and WAL types: `CATALOG_FILE_NAME`, `CatalogEntry`, `CatalogRegistry`, `CatalogTenantRoute`, `WAL_FILE_NAME`, `WalEntry`, `WalJournal`, `WalOperation`
- Application logging types: `ApplicationLoggingConfig`, `ApplicationLogLevel`, `ApplicationLogFormat`, `ApplicationLogDestination`, `ApplicationLogEvent`

`wardrobe-client` exports `WardrobeClient`, `ConnectionTarget`, `DriverKind`, `DEFAULT_NETWORK_PORT`, `ProtocolFrame`, `ProtocolOpcode`, `PROTOCOL_MAGIC`, and the same public command and result model.

The two main application entry points are:

- `WardrobeEngine` for direct embedded access
- `WardrobeClient` for TCP and Unix socket server connections

`WardrobeClient` and `WardrobeEngine` expose the same Rust API, including method signatures, request and result types, and the canonical Wardrobe verbs:

- Record operations: `upsert`, `read`, `count`, `delete`
- Maintenance and inspection: `compact`, `inspect`, `status`
- Lifecycle and recovery: `create`, `alter`, `drop`, `backup`, `restore`
- Administrative management: `grant`, `revoke`, plus user creation/removal through `create` and `drop`

`WardrobeEngine` also exposes low-level protocol execution with `execute`, `execute_in_scope`, `execute_for_tenant`, and `execute_command` for server integration.

## Usage

### Embedded Quick Start

```rust
use serde_json::json;
use std::io::{Error, ErrorKind};
use wardrobe_embedded::{OperationFilter, OperationOptions, ReadResult, WardrobeEngine};

fn main() -> std::io::Result<()> {
    let engine = WardrobeEngine::open("./wardrobe")?;

    let pointer = engine
        .upsert(
            json!({
                "_id": "field-service-kit",
                "name": "Field Service Toolkit",
                "category": "maintenance",
                "tags": ["portable", "repair"]
            }),
            OperationFilter::drawer("tool"),
            OperationOptions::default(),
        )?
        .into_pointers()
        .into_iter()
        .next()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "upsert returned no pointer"))?;

    let record = match engine.read(
        OperationFilter::pointer(pointer),
        OperationOptions::default(),
    )? {
        ReadResult::Record(record) => record,
        _ => None,
    };
    println!("{record:?}");

    Ok(())
}
```

### Server Client

```rust
use wardrobe_client::WardrobeClient;

fn connect() -> std::io::Result<()> {
    let network = WardrobeClient::open_with_profile(
        "wardrobe://localhost:24842",
        "./profiles/adminuser/profile.toml",
    )?;

    #[cfg(unix)]
    let socket = WardrobeClient::open("wardrobe+unix:///tmp/wardrobe.sock")?;

    Ok(())
}
```

Supported connection shapes:

- TCP URI: `wardrobe://localhost:24842`
- TCP default port: `wardrobe://localhost` uses `24842`
- Unix socket URI: `wardrobe+unix:///tmp/wardrobe.sock`

Direct filesystem access belongs to `WardrobeEngine` from `wardrobe-embedded`; `wardrobe-client` does not include or duplicate the embedded engine.

### Filtering, Counting, and Pagination

```rust
use serde_json::json;
use wardrobe_embedded::{
    OperationFilter, OperationOptions, OrderDirection, ReadResult, WardrobeEngine,
};

fn query(engine: &WardrobeEngine) -> std::io::Result<()> {
    let filter = OperationFilter::query_in("device", json!({ "name": "sensor%" }));
    let records = match engine.read(
        filter.clone(),
        OperationOptions::new()
            .order_by("name")
            .order_direction(OrderDirection::Ascending)
          .page(1)
          .page_size(25),
    )? {
        ReadResult::Page(page) => page.records,
        _ => Vec::new(),
    };

    let total = engine.count(filter, OperationOptions::default())?;

    println!("matched {} records, returned {}", total, records.len());
    Ok(())
}
```

### Typed Status Results

Rust status constructors encode their output type, so inventory calls return direct values without a result enum or variant wrapper:

```rust
use wardrobe_embedded::{StatusRequest, WardrobeEngine};

fn inventory(engine: &WardrobeEngine) -> std::io::Result<()> {
    let wardrobes = engine.status(StatusRequest::wardrobes())?;
    let bays = engine.status(StatusRequest::bays("publishing-house"))?;
    let drawers = engine.status(StatusRequest::drawers("publishing-house", "public"))?;

    println!("{} wardrobes, {} bays, {} drawers", wardrobes.len(), bays.len(), drawers.len());
    Ok(())
}
```

The command protocol keeps the operation envelope, for example `{"status":[...]}`, but the status payload itself is a raw array. JavaScript, TypeScript, and Python status methods return that array directly.

### Embedded Values and Relationships

Nested JSON objects and array elements without `_id` remain embedded in their parent record. ID-bearing objects express relationships: an `_id`-only object is a strict reference, while an `_id` plus additional fields cascade-upserts the referenced record. Direct Wardrobe pointer strings are stored as references. Set `OperationOptions::hydrate(false)` to read stored pointers without resolving them.

## Language Bindings

| Ecosystem | Server-backed | Embedded |
|---|---|---|
| Rust | `WardrobeClient` from `wardrobe-client` | `WardrobeEngine` from `wardrobe-embedded` |
| JavaScript/TypeScript | `@wardrobe/client` | `@wardrobe/embedded` |
| Python | `wardrobe-client` | `wardrobe-embedded` |
| C ABI | `wardrobe-c` | `wardrobe-c` |

The npm packages require Node.js 24 or newer. The Python packages require Python 3.10 or newer. Binding packages are currently prepared for local validation and dry runs; publishing is not part of this release.

## Licensing

Licensing is component-specific:

- Embedded engine, client, CLI, language bindings, and samples: MIT
- Wardrobe server: Business Source License 1.1, changing to GPL version 2 or later on July 22, 2030
- Armoire: Armoire Source-Available Evaluation License (ASEL); production or non-evaluation commercial use requires a paid commercial license

The license file within each component is authoritative.

## Routed Multi-Tenant Execution

```rust
use serde_json::json;
use wardrobe_embedded::{Command, OperationFilter, OperationOptions, StorageCoordinate, WardrobeEngine};

fn routed(engine: &WardrobeEngine) -> std::io::Result<()> {
    let scope = StorageCoordinate::new("tenant_a", "production", "public");

    engine.execute(
        scope,
        Command::Upsert {
            payload: json!({
                "_id": "@account:lnk_acme",
                "name": "Acme Manufacturing"
            }),
            filter: OperationFilter::drawer("account"),
            options: OperationOptions::default(),
        },
    )?;

    Ok(())
}
```

## Server Daemon

Run Wardrobe as a TCP-backed daemon:

```text
cargo run -p wardrobe-server -- --data-dir ./data --tcp-bind 127.0.0.1:24842
```

Useful server flags:

- `--data-dir <path>` chooses the root storage directory
- `--tcp-bind <addr:port>` binds the TCP listener; default is `127.0.0.1:24842`
- `--no-tcp` disables the TCP listener
- `--unix-socket <path>` binds a Unix domain socket listener on Unix platforms
- `--check` initializes the engine and exits without blocking
- `--log-level <level>` enables application logs at `trace`, `debug`, `info`, `warn`, `error`, or disables them with `off`
- `--log-format <format>` selects `pretty` or `json`
- `--log-destination <dest>` writes application logs to `stderr`, `stdout`, or `file`
- `--log-file <path>` chooses the file path when `--log-destination file` is used

Application logs are operator-facing diagnostics only. They are separate from Wardrobe's logical WAL and transaction WAL, and they are never used for recovery.

## Certificate Security and Server Bootstrap

Wardrobe separates authentication from authorization. TLS client certificates authenticate a stable URI SAN identity such as `wardrobe:user:adminuser`; the existing `_wardrobe_access_control.json` registry resolves that identity to a Wardrobe user and continues to hold the user's role and permissions. Renewing a certificate keeps the URI SAN unchanged, so it does not replace or invalidate the user's authorization record.

TCP deployments support three security modes:

- `managed`: Wardrobe owns a private CA and issues the server and client certificates.
- `external`: the operator supplies the server identity and one or more trusted client CA bundles.
- `disabled`: TCP is plaintext and has no authentication. It is accepted only on localhost unless `unsafe_allow_remote_disabled = true` or `--unsafe-disable-auth` is explicitly set. Unix sockets use disabled mode and rely on local filesystem permissions.

Embedded use does not open a network listener and therefore does not perform TLS authentication. Filesystem access remains the local authority boundary.

### Step-by-Step Managed Connection Setup with Locally Generated Certificates

This setup uses Wardrobe's managed PKI. `wardrobe-server` creates a private CA and signs the server and client certificates locally; it does not request or depend on a hosted certificate. The commands in this walkthrough use the installed `wardrobe-server` and `wardrobe` binaries from the release packages.

#### 1. Create an instance directory

```text
mkdir wardrobe-instance
cd wardrobe-instance
```

The remaining commands use `./data` for Wardrobe data, `./security` for the CA and server keys, and `./profiles` for client credentials. Keep all of these paths persistent.

#### 2. Generate the local CA and server certificate

Choose the DNS name or IP address that clients will use before running this command. This local-only example uses `localhost`:

```text
wardrobe-server init \
  --data-dir ./data \
  --security-dir ./security \
  --server-name localhost \
  --server-ip 127.0.0.1
```

This creates the private CA under `./security/ca` and a CA-signed server certificate under `./security/server`. Initialization fails rather than replacing an existing CA or server identity. Never copy `ca.key` to a client.

For a LAN connection, replace `localhost` and `127.0.0.1` with the exact DNS name and IP address clients will use. Add every required name or address with another `--server-name` or `--server-ip`.

#### 3. Configure the TCP listener

Create `wardrobe.toml`:

```toml
[data]
directory = "./data"

[network]
tcp_enabled = true
tcp_bind = "127.0.0.1:24842"
unix_socket_enabled = false

[security]
mode = "managed"
security_dir = "./security"
server_names = ["localhost"]
server_ips = ["127.0.0.1"]
```

The names and addresses in this file must match the values used to generate the server certificate. For a LAN server, also change `tcp_bind` to the listening interface, such as `192.168.1.20:24842`.

#### 4. Generate and register the first administrator certificate

Run the local bootstrap command before starting the server:

```text
wardrobe-server bootstrap-admin \
  --data-dir ./data \
  --security-dir ./security \
  --username adminuser \
  --server-name localhost \
  --output ./security/bootstrap/adminuser
```

This single command performs both required operations:

1. It generates a client certificate whose URI identity is `wardrobe:user:adminuser`.
2. It registers that identity in the server's access-control data as the `adminuser` administrator.

It writes `client.crt`, `client.key`, `ca.crt`, and `profile.toml` to `./security/bootstrap/adminuser`. Bootstrap is a local filesystem operation, not an unauthenticated network endpoint.

#### 5. Start the server

Leave this command running:

```text
wardrobe-server ./wardrobe.toml
```

Normal startup only loads the certificate files created above. It never silently generates or replaces them.

#### 6. Connect with the administrator certificate

From another terminal in `wardrobe-instance`:

```text
wardrobe wardrobe://localhost:24842 \
  --profile ./security/bootstrap/adminuser/profile.toml \
  status wardrobes
```

The profile supplies the client certificate and key, the CA used to verify the server, and the TLS server name. Copy the entire profile directory through an approved secret-transfer channel when connecting from another machine; do not copy the managed CA private key.

#### 7. Generate and register another client certificate

On the host that holds `./security`, issue a separate certificate for the user and device:

```text
wardrobe ./security identity create alice \
  --device laptop \
  --server-name localhost \
  --output ./profiles/alice-laptop
```

The first argument is the local managed security directory; this command does not contact the server. It creates a certificate containing the stable URI identity `wardrobe:user:alice`.

Next, use the administrator connection to register that certificate identity with the server:

```text
wardrobe wardrobe://localhost:24842 \
  --profile ./security/bootstrap/adminuser/profile.toml \
  create user '{"username":"alice","role":"operator","certificate_identities":["wardrobe:user:alice"]}'
```

Wardrobe registers the certificate's URI identity, not the certificate file or private key. The private key stays with the client. Move the complete `./profiles/alice-laptop` directory to Alice's machine and verify the connection:

```text
wardrobe wardrobe://localhost:24842 \
  --profile ./profiles/alice-laptop/profile.toml \
  status wardrobes
```

Repeat step 7 for each user/device or service/device pair so every client has its own certificate serial. Use `--service` when issuing a service identity such as `wardrobe:service:nispuk`. Do not share client private keys or profiles.

### Managed Certificate Lifecycle

List or inspect managed identities:

```text
cargo run -p wardrobe-cli -- ./security identity list
cargo run -p wardrobe-cli -- ./security identity inspect adminuser
cargo run -p wardrobe-cli -- ./security certificate list
```

Renew a device certificate while preserving `wardrobe:user:adminuser`:

```text
cargo run -p wardrobe-cli -- ./security identity renew adminuser \
  --device desktop \
  --server-name localhost \
  --output ./profiles/adminuser-desktop
```

The previous active serial for that identity/device is added to `revoked.json`. A certificate can also be renewed by serial:

```text
cargo run -p wardrobe-cli -- ./security certificate renew 0123456789abcdef \
  --server-name localhost
```

Revoke a compromised certificate or remove all active certificates for an identity:

```text
cargo run -p wardrobe-cli -- ./security certificate revoke 0123456789abcdef
cargo run -p wardrobe-cli -- ./security identity remove adminuser
```

Revocation is read for each new TLS connection. Existing connections should be disconnected operationally when immediate cutoff is required.

Server certificates are replaced only through the explicit reissue command:

```text
cargo run -p wardrobe-server -- reissue-server-certificate \
  --security-dir ./security \
  --server-name localhost \
  --server-name wardrobe \
  --server-ip 127.0.0.1 \
  --server-ip ::1
```

Restart the server after reissuing its certificate.

Rotate the managed CA only through the local rotation command:

```text
cargo run -p wardrobe-server -- rotate-ca \
  --security-dir ./security \
  --server-name localhost \
  --server-name wardrobe \
  --server-ip 127.0.0.1 \
  --server-ip ::1
```

Rotation archives the previous CA, creates a new CA, reissues the server certificate, and builds `ca/ca-bundle.crt` with both trust anchors. Wardrobe updates locally tracked client-profile CA bundles so existing client certificates remain usable during the migration. Redistribute the updated bundle to profiles stored on other machines before restarting the server. Renew clients under the new CA, then retain or retire the archived CA according to the deployment's security policy.

### Security Directory Layout

```text
security/
  ca/
    ca.crt
    ca.key
    ca-bundle.crt
    archive/
  server/
    server.crt
    server.key
  bootstrap/
    adminuser/
      client.crt
      client.key
      ca.crt
      profile.toml
  clients/
  certificates.json
  revoked.json
```

Keep `security/` outside container images and source control. Back it up as sensitive state and restrict `ca.key`, `server.key`, and client keys to their owners.

### Docker Bootstrap

Mount data and security as persistent volumes:

```yaml
services:
  wardrobe:
    image: wardrobe-server:0.26.725
    command: ["/config/wardrobe.toml"]
    volumes:
      - wardrobe-data:/data
      - wardrobe-security:/security
      - ./wardrobe.toml:/config/wardrobe.toml:ro

volumes:
  wardrobe-data:
  wardrobe-security:
```

Initialize and bootstrap from inside the container or a one-shot container with the same volumes:

```text
docker compose run --rm wardrobe wardrobe-server init \
  --data-dir /data \
  --security-dir /security \
  --server-name wardrobe \
  --server-name localhost \
  --server-ip 127.0.0.1

docker compose run --rm wardrobe wardrobe-server bootstrap-admin \
  --data-dir /data \
  --security-dir /security \
  --username adminuser \
  --server-name wardrobe \
  --output /security/bootstrap/adminuser
```

Copy `/security/bootstrap/adminuser` to the administrator workstation through an approved secret-transfer channel. Never bake it into the image.

### Local and LAN Deployments

For local development, initialize SANs for `localhost`, `127.0.0.1`, and `::1`; public DNS is not required. For Docker Compose, add the service name such as `wardrobe`. For LAN access, add the exact LAN DNS name and IP before issuing the server certificate, bind the listener to that interface, and keep managed mode enabled:

```toml
[network]
tcp_enabled = true
tcp_bind = "192.168.1.20:24842"

[security]
mode = "managed"
security_dir = "/srv/wardrobe/security"
server_names = ["wardrobe.lan"]
server_ips = ["192.168.1.20"]
```

Clients must use a profile whose `server_name` matches one of those SAN values.

### External or Enterprise PKI

External mode uses the same mutual-TLS authentication and URI SAN mapping but never reads or operates a CA private key:

```toml
[data]
directory = "/srv/wardrobe/data"

[network]
tcp_enabled = true
tcp_bind = "0.0.0.0:24842"
unix_socket_enabled = false

[security]
mode = "external"
security_dir = "/srv/wardrobe/security-state"
server_certificate = "/etc/wardrobe-pki/server.crt"
server_private_key = "/etc/wardrobe-pki/server.key"
trusted_client_ca_bundles = [
  "/etc/wardrobe-pki/client-root-a.crt",
  "/etc/wardrobe-pki/client-root-b.crt",
]
```

The server certificate must contain the DNS/IP SAN used by clients. Each client certificate must be valid for client authentication, chain to one configured client CA, and contain exactly one Wardrobe URI SAN. This works with enterprise PKI, Vault, Smallstep, Kubernetes issuers, SPIFFE/SPIRE certificate delivery, and Active Directory Certificate Services when those certificate requirements are met.

Register the first external administrator certificate locally:

```text
cargo run -p wardrobe-server -- bootstrap-admin \
  --data-dir /srv/wardrobe/data \
  --username adminuser \
  --certificate /secure-transfer/adminuser.crt
```

The certificate must contain `wardrobe:user:adminuser`. The operator retains and distributes the matching private key and creates a client profile:

```toml
identity = "wardrobe:user:adminuser"
server_name = "wardrobe.example.com"
ca_cert = "server-ca.crt"
client_cert = "adminuser.crt"
client_key = "adminuser.key"
```

Relative profile paths are resolved from the directory containing `profile.toml`. Multiple `trusted_client_ca_bundles` allow CA rotation or mixed enterprise and Wardrobe trust during a controlled migration. External certificate renewal and revocation remain the external PKI operator's responsibility; Wardrobe's local serial revocation file can additionally deny a client serial.

### Disabled Mode

Local plaintext development can be configured explicitly:

```toml
[network]
tcp_enabled = true
tcp_bind = "127.0.0.1:24842"

[security]
mode = "disabled"
```

Wardrobe rejects a non-loopback TCP bind in disabled mode. The unsafe override is intentionally conspicuous:

```toml
[security]
mode = "disabled"
unsafe_allow_remote_disabled = true
```

Do not use that override on an untrusted network. Prefer managed or external mode.

### Certificate Troubleshooting

- `certificate identity is not registered`: run local bootstrap for `adminuser`, or ensure the existing user record includes the certificate URI SAN.
- `certificate does not contain a URI SAN`: issue a client certificate with exactly one `wardrobe:user:<name>` or `wardrobe:service:<name>` URI SAN.
- `unknown issuer` or `bad certificate`: verify the client chains to one of `trusted_client_ca_bundles` and the client profile trusts the CA that signed the server.
- `certificate is not valid for name`: set the profile's `server_name` to a DNS/IP SAN in the server certificate, or explicitly reissue the managed server certificate with the required name/IP.
- `certificate has been revoked`: issue or renew a distinct device certificate and update the client profile.
- `managed CA/server file not found`: run `wardrobe-server init` once with the same persistent security volume; startup never repairs or silently replaces missing identity files.
- clients fail after CA rotation: redistribute `ca/ca-bundle.crt` or the refreshed profile `ca.crt` before restarting, then renew each client under the new CA.
- TLS works on one host but not another: check clock synchronization, certificate validity dates, DNS resolution, mounted file paths, and file permissions.

## CLI Usage

`NOTES.txt` and `wardrobe --help` are the authority for the CLI capability set. The package crate remains `wardrobe-cli`, while the installed binary is `wardrobe`. The binary accepts the connection context as the first positional argument, then runs the canonical command families from the help output.

Run a single command:

```text
cargo run -p wardrobe-cli -- <connection> [--pretty] <command> [args]
```

If no command is supplied, the CLI enters an interactive REPL. If standard input is piped in, the CLI executes the piped command instead.

CLI application logging uses the same `--log-level`, `--log-format`, `--log-destination`, and `--log-file` controls as the server. Logging is off by default, and when enabled it writes to stderr by default so JSON command output remains script-friendly.

Use `--profile <profile.toml>` for certificate-authenticated TCP connections. Identity and certificate lifecycle commands target a local managed security directory and do not open a server connection.

Examples:

- Embedded structural discovery:
  `cargo run -p wardrobe-cli -- ./data status wardrobes`
- Remote drawer listing:
  `cargo run -p wardrobe-cli -- "wardrobe://127.0.0.1:24842" status drawers inventory/public`
- Backup a bay:
  `cargo run -p wardrobe-cli -- ./data backup inventory/public ./backups/public.wrb`

Canonical command families:

- Structural management
  - `create <type> <path>`
  - `alter <type> <path> <target_field> <?extra_args>`
  - `drop <type> <path> <?target_field> <?extra_args>`
- Document mutations and queries (RUDIC)
  - `read <path> <?json_filter> <?json_options>`
  - `upsert <path> <json_payload> <?json_filter> <?json_options>`
  - `delete <path> <json_filter_or_id>`
  - `inspect <path> <?json_filter> <?json_options>`
  - `count <path> <?json_filter> <?json_options>`
- Backup and disaster recovery
  - `compact <path>`
  - `backup <source_path> <destination_archive_path>`
  - `restore <destination_path> <source_archive_path>`
- Server access control and user administration
  - `create user <json_user_payload>`
  - `drop user <username>`
  - `grant permission <username> <path:rights>`
  - `revoke permission <username> <path:rights>`
  - `status <type> <?path>`

Behavior notes:

- `status wardrobes` and `create wardrobe` use the Rust wardrobe lifecycle APIs
- `status bays` and `create bay` use the Rust bay lifecycle APIs
- `create user`, `grant permission`, and `revoke permission` require a remote server-backed target; direct embedded administration uses `WardrobeEngine`
- `compact` can target a wardrobe, a bay, or a single drawer and fans out to the relevant compaction calls
- `backup` and `restore` operate at wardrobe, bay, or drawer scope

Compatibility aliases are intentionally not provided for the canonical CLI vocabulary.

## Application Logging

Wardrobe has three separate log-like systems:

- Application logs: structured operator-facing diagnostics for startup, shutdown, connection handling, command execution, recovery, backup, restore, and compaction activity.
- Logical WAL: durable storage recovery records.
- Transaction WAL: transaction atomicity and transaction recovery records.

Application logs may be configured explicitly by `wardrobe-server`, by the `wardrobe` CLI, or by an embedding host through `ApplicationLoggingConfig` and `init_application_logging`. Embedded `WardrobeEngine::open` does not install or override a global logger by default. Wardrobe also emits application events through `tracing`, so a host application that has already installed a tracing subscriber can observe them without Wardrobe taking ownership of global logging.

Logs include structured fields such as operation, command, drawer, duration, and success state where available. Raw record payloads, credentials, tokens, and user records are not logged by default.

## Current Capabilities

- File-backed drawer storage with separate data, index, metadata, and WAL artifacts
- Versioned WRDB record frames with native positional record payloads, field-name maps, and presence bitmaps; version 1 BSON-backed records remain readable
- Compact WIDX native binary index frames with transparent reading of older BSON-backed index entries
- Record CRUD, JSON filtering, pointer lookup, and count operations
- Primary-key indexing plus ordered B+ tree-style secondary indexes for equality and numeric/string range queries
- Nested document graph hydration and relationship-aware record storage
- Relationship constraints, delete rules, cascade-delete rules, and drawer validation metadata
- Scoped routing across tenant, wardrobe, bay, and drawer boundaries
- Write-ahead log verification and recovery for incomplete operations
- Compact maintenance workflows for drawer storage reclamation and migration
- Structural inspection and sanity checking through `inspect` and `status` surfaces
- Archive-based backup and restore at wardrobe, bay, or drawer scope
- Remote access-control administration persisted in `_wardrobe_access_control.json`
- Bounded drawer caching for embedded engine usage

## Sample Application

Run the basic Rust sample crate to execute an end-to-end publishing-house flow that:

- Opens a local embedded engine against `./wardrobe`
- Creates the `publishing-house/public` wardrobe and bay scopes with publisher, person, and book drawers
- Uses direct typed wardrobe, bay, and drawer status arrays
- Stores related publisher, author, editor, and book records
- Exercises filtered reads, pointer reads, counts, temporary record cleanup, and final integrity checks

```text
cargo run -p basic-usage
```

Equivalent embedded examples are available in JavaScript, TypeScript, and Python. They use the repository-root ignored `./wardrobe` storage directory and separate bays named `public_js`, `public_ts`, and `public_py`.

## CLI Sample

Run the shell script sample to drive the CLI through the library workflow:

```text
bash ./utilities/scripts/wardrobe-cli-demo.sh
```

Pass a server connection string to exercise the same workflow remotely:

```text
bash ./utilities/scripts/wardrobe-cli-demo.sh wardrobe://localhost:24842
```

## Testing

Run the test suite with:

```text
cargo test --workspace
```

For a coverage summary, install `cargo-llvm-cov` and run:

```text
cargo llvm-cov --workspace
```
