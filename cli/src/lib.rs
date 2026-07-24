use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Error, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use wardrobe_core::{
    AlterRequest, ApplicationLogEvent, ApplicationLogLevel, ApplicationLoggingConfig,
    CompactRequest, ConnectionTarget, CreateRequest, CreateResult, Database, DropRequest,
    InspectResult, OperationFilter, OperationOptions, PermissionRequest, ReadResult, StatusRequest,
    StorageDiagnosis, StorageInventory, VacuumReport, WardrobeClient, emit_application_log,
    init_application_logging, issue_managed_client_certificate, list_managed_certificates,
    managed_identity_certificates, remove_managed_identity, renew_managed_client_certificate,
    revoke_managed_certificate,
};

#[derive(Debug)]
pub struct CliConfig {
    pub connection: String,
    pub pretty: bool,
    pub command_parts: Vec<String>,
    pub logging: ApplicationLoggingConfig,
    pub profile: Option<PathBuf>,
}

impl CliConfig {
    pub fn from_args<I>(args: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let connection;
        let mut pretty = false;
        let mut command_parts = Vec::new();
        let mut logging_level = None;
        let mut logging_format = None;
        let mut logging_destination = None;
        let mut logging_file = None;
        let mut profile = None;

        let mut args = args.into_iter();
        let Some(first_arg) = args.next() else {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "wardrobe requires a target connection context",
            ));
        };
        match first_arg.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--version" | "-v" => {
                print_version();
                std::process::exit(0);
            }
            _ => {
                connection = first_arg;
            }
        }

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--pretty" => {
                    pretty = true;
                }
                "--log-level" => {
                    logging_level = Some(args.next().ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidInput,
                            "--log-level requires trace, debug, info, warn, error, or off",
                        )
                    })?);
                }
                "--log-format" => {
                    logging_format = Some(args.next().ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidInput,
                            "--log-format requires pretty or json",
                        )
                    })?);
                }
                "--log-destination" => {
                    logging_destination = Some(args.next().ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidInput,
                            "--log-destination requires stderr, stdout, or file",
                        )
                    })?);
                }
                "--log-file" => {
                    logging_file = Some(PathBuf::from(args.next().ok_or_else(|| {
                        Error::new(ErrorKind::InvalidInput, "--log-file requires a file path")
                    })?));
                }
                "--profile" => {
                    profile = Some(PathBuf::from(args.next().ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidInput,
                            "--profile requires a client profile path",
                        )
                    })?));
                }
                _ => {
                    command_parts.push(arg);
                }
            }
        }

        Ok(Self {
            connection,
            pretty,
            command_parts,
            logging: ApplicationLoggingConfig::from_parts(
                logging_level.as_deref(),
                logging_format.as_deref(),
                logging_destination.as_deref(),
                logging_file,
            )?,
            profile,
        })
    }
}

const HELP_TEXT: &str = r#"wardrobe:
    -h, --help                  Show this message and exit
    -v, --version               Show the version and exit
    --log-level <level>         Application log level: trace, debug, info, warn, error, off
    --log-format <format>       Application log format: pretty or json
    --log-destination <dest>    Application log destination: stderr, stdout, or file
    --log-file <path>           File path when --log-destination file is used
    --profile <path>            Use an X.509 client profile for a TCP connection

    The first argument to the CLI is always the target connection context.
    It can be a filesystem path (e.g., ./wardrobe) for embedded mode, or a network connection string or socket location
    (e.g., wardrobe://127.0.0.1:24842) for remote execution.

    If a targeted filesystem path does not explicitly point to a database or bay context,
    the engine transparently routes execution into system defaults:
    - Default Wardrobe fallback:         "default"
    - Default Bay fallback:              "default"

    ===========================================================================
    STRUCTURAL COMMANDS
    ===========================================================================
    status <type> <?parent_path>
        Show structural and runtime status information. <type> must be one of: wardrobes, bays, drawers, tenants, wal, storage, path, drawer-names, cached-drawer-count, server, config.
        Example: status bays my_wardrobe

    create <type> <path>
        Provision a new structural resource. <type> must be one of: wardrobe, bay, drawer, user.
        Example: create drawer my_wardrobe/my_bay/user

    drop <type> <path>
        Drop a structural resource. <type> must be one of: wardrobe, bay, drawer, user.
        Example: drop drawer my_wardrobe/my_bay/user

    compact <path>
        Execute a space-reclaiming vacuum process on a drawer or group of drawers.
        Example: compact my_wardrobe/my_bay (compacts all drawers in bay)

    ===========================================================================
    DOCUMENT MUTATIONS & QUERIES (RUDIC)
    ===========================================================================
    upsert <path> <json_payload> <?json_filter> <?json_options>
        Insert a new document or completely overwrite an existing document by its _id.
        Example: upsert my_wardrobe/my_bay/user '{"_id": "user-01", "name": "Marcus"}'

    count <path> <?json_filter> <?json_options>
        Count documents matching an optional criteria filter.
        Example: count my_wardrobe/my_bay/user '{"tool.type": "Hammer"}'

    inspect <path> <?json_filter> <?json_options>
        Expose raw storage metrics (sizes, record counts, tombstone fragmentation percentages).
        Does not output documents.
        Example: inspect my_wardrobe/my_bay/user

    read <path> <?json_filter> <?json_options>
        Retrieve documents matching an optional criteria filter.
        Example: read my_wardrobe/my_bay/user '{"tool._id": "298234789328923489"}'
        Example: read my_wardrobe/my_bay/user '{"_id": "@tool:298234789328923489"}'
        Example: read my_wardrobe/my_bay/user '{"tool.type": "Hammer"}'
        Example: read my_wardrobe/my_bay/user '{"tool.type": {"$in": ["Hammer", "Sword"]}}'
        Example: read my_wardrobe/my_bay/user '{"tool.type": {"$nin": ["Hammer", "Sword"]}}'
        Example: read my_wardrobe/my_bay/user '{"tool.type": {"$exists": true}}'
        Example: read my_wardrobe/my_bay/user '{"tool.type": {"$exists": false}}'
        Example: read my_wardrobe/my_bay/user '{"tool.type": {"$regex": ".*Sword.*"}}'
        Example: read my_wardrobe/my_bay/user '{"tool.type.owner": "@user:8723478929234786234"}'

    delete <path> <json_filter_or_id>
        Delete documents from a drawer matching a specific structural ID or JSON filter criteria.
        Example: delete my_wardrobe/my_bay/user '{"_id": "user-02"}'

    ===========================================================================
    SCHEMA ENGINE & RELATIONSHIP MANAGEMENT
    ===========================================================================
    alter <type> <path> <target_field> <?extra_args>
        Attach a structural modifier, index, rule, or side-effect routine to a field.
        <type> must be one of: index, key, constraint, trigger, relationship, cascade-delete.

        Examples:
        alter index my_wardrobe/my_bay/user tool.type
        alter key my_wardrobe/my_bay/user profile_id secondary
        alter constraint my_wardrobe/my_bay/user email unique
        alter constraint my_wardrobe/my_bay/user age non-null
        alter relationship my_wardrobe/my_bay/user tool_id my_wardrobe/my_bay/tool
        alter cascade-delete my_wardrobe/my_bay/user tool_id
        alter trigger my_wardrobe/my_bay/user on_upsert ./scripts/sync_profile.sh

    drop <type> <path> <target_field> <?extra_args>
        Detach or drop an active modifier, index, rule, or routine from a field context.
        <type> must be one of: index, key, constraint, trigger, relationship, cascade-delete.

        Examples:
        drop index my_wardrobe/my_bay/user tool.type
        drop constraint my_wardrobe/my_bay/user email unique
        drop cascade-delete my_wardrobe/my_bay/user tool_id

    ===========================================================================
    BACKUP & DISASTER RECOVERY
    ===========================================================================
    backup <source_path> <destination_archive_path>
        Snapshot a targeted layer (Wardrobe, Bay, or individual Drawer) into an isolated archive file.
        Example: backup my_wardrobe/my_bay ./backups/bay_snapshot.wrb

    restore <destination_path> <source_archive_path>
        Hydrate and replace a target storage layer from a valid backup archive file.
        Example: restore my_wardrobe/my_bay ./backups/bay_snapshot.wrb

    ===========================================================================
    SERVER ACCESS CONTROL & USER ADMINISTRATION
    ===========================================================================
    create user <json_user_payload>
        Register an authorized administrative or client identity with the Wardrobe instance.
        Example: create user '{"username": "dev_admin", "role": "operator"}'

    drop user <username>
        Remove an authorized administrative or client identity from the Wardrobe instance.
        Example: drop user dev_admin

    grant permission <username> <permission_scope>
        Delegate functional access rights (Read, Update, Delete, Inspect) across a path scope.
        Example: grant permission dev_admin my_wardrobe/my_bay:rud

    revoke permission <username> <permission_scope>
        Strip functional access rights from a user identity.
        Example: revoke permission dev_admin my_wardrobe/my_bay:d

    identity <create|enroll|renew|list|inspect|remove> [identity] [options]
        Manage Wardrobe-issued client identities in the local security directory supplied as the connection context.
        Example: wardrobe ./security identity create adminuser --device desktop --server-name localhost

    certificate <issue|renew|revoke|list> [identity_or_serial] [options]
        Issue, renew, revoke, and list Wardrobe-managed client certificates.
        Example: wardrobe ./security certificate revoke 0123456789abcdef

    ===========================================================================
    CORE ARCHITECTURAL RULES
    ===========================================================================
    * Addressing Resolution: Document IDs can be targeted explicitly or implicitly. Fully
      qualified URI references bypass path parameter dependencies entirely by mapping the
      exact storage boundaries directly into the key token:
      @storage_root/wardrobe_name/bay_name/drawer_name:document_id

    * Cross-Boundary Traversal: Multi-wardrobe or cross-bay queries are explicitly allowed,
      provided the executing context has been granted direct security clearance. These
      requests require the fully qualified path string (wardrobe_name.bay_name.drawer_name)
      to ensure the coordinator engine paths resolve without name collisions.

    * Hierarchical Isolation Constraints: Bays are completely flat structures and cannot
      be nested. When executing inquiries inside an explicitly passed tenant context,
      the core boundary manager will explicitly block and reject any cross-tenant data traversal.

    * Storage Strategy: Tenant data lives inside separate files underneath the drawer level
      (e.g., drawername.tenant.drw), while the indexes and metadata remain per drawer name
      to allow high-performance uniqueness constraints and fast non-tenant aggregation.

    * Non-Tenant Inquiries: Executing a structural inquiry (inspect/count) without passing a
      tenant context instructs the storage engine to aggregate results across all active tenant
      sibling files, returning total global space and metadata metrics.

    * Relational Document Graph Hydration: Nested JSON objects containing an "_id" field
      trigger transactional graph processing. If a full object payload is passed with an "_id",
      the engine initiates a Cascade Upsert across targeted drawers. If only an "_id" field
      is present, the engine treats it as a strict reference link mutation. Omitting the field
      entirely breaks and dissolves the underlying database relationship graph."#;

pub fn print_help() {
    println!("{HELP_TEXT}");
}

pub fn print_version() {
    println!("wardrobe {}", env!("CARGO_PKG_VERSION"));
}

fn cli_log(level: ApplicationLogLevel, message: &'static str, fields: Vec<(&'static str, String)>) {
    emit_application_log(ApplicationLogEvent::new(
        level,
        "wardrobe_cli",
        message,
        fields,
    ));
}

pub fn run_cli_logic(config: CliConfig) -> io::Result<()> {
    init_application_logging(config.logging.clone())?;
    cli_log(
        ApplicationLogLevel::Info,
        "cli_start",
        vec![
            ("operation", "cli_start".to_string()),
            ("connection", config.connection.clone()),
            ("log_level", config.logging.level.as_str().to_string()),
            ("log_format", config.logging.format.as_str().to_string()),
            (
                "log_destination",
                config.logging.destination.as_str().to_string(),
            ),
        ],
    );
    if matches!(
        config.command_parts.first().map(String::as_str),
        Some("identity" | "certificate")
    ) {
        return run_security_command(
            Path::new(&config.connection),
            &config.command_parts,
            config.pretty,
        );
    }

    let client_result = match config.profile {
        Some(profile) => WardrobeClient::open_with_profile(&config.connection, profile),
        None => WardrobeClient::open(&config.connection),
    };
    let client = match client_result {
        Ok(client) => client,
        Err(error) => {
            cli_log(
                ApplicationLogLevel::Error,
                "connection_failure",
                vec![
                    ("operation", "connect".to_string()),
                    ("connection", config.connection.clone()),
                    ("error_kind", format!("{:?}", error.kind())),
                    ("error", error.to_string()),
                    ("success", "false".to_string()),
                ],
            );
            return Err(Error::new(
                ErrorKind::Other,
                format!("Failed to open connection: {error}"),
            ));
        }
    };

    if !config.command_parts.is_empty() {
        return run_command(&client, &config.command_parts, config.pretty);
    }

    let stdin_is_tty = atty::is(atty::Stream::Stdin);
    if !stdin_is_tty {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        let buffer = buffer.trim();
        if !buffer.is_empty() {
            let parts = shell_split(buffer);
            return run_command(&client, &parts, config.pretty);
        }
    }

    repl(&client, config.pretty)
}

pub fn shell_split(input: &str) -> Vec<String> {
    input.split_whitespace().map(|s| s.to_string()).collect()
}

fn format_target(t: &ConnectionTarget) -> String {
    match t {
        ConnectionTarget::EmbeddedPath(p) => format!("embedded:{}", p.display()),
        ConnectionTarget::Network { host, port } => format!("network:{}:{}", host, port),
        ConnectionTarget::UnixSocket { path } => format!("unix:{}", path.display()),
    }
}

fn repl(client: &WardrobeClient, pretty: bool) -> io::Result<()> {
    let mut input = String::new();
    let prompt = format!("wardrobe:{}> ", format_target(client.connection_target()));

    loop {
        print!("{prompt}");
        io::stdout().flush()?;
        input.clear();
        if io::stdin().read_line(&mut input)? == 0 {
            break;
        }
        let line = input.trim();
        if line.is_empty() {
            continue;
        }
        if line == "exit" || line == "quit" {
            break;
        }
        let parts = shell_split(line);
        if let Err(e) = run_command(client, &parts, pretty) {
            eprintln!("Error: {e}");
        }
    }

    Ok(())
}

fn status_drawer_names(client: &WardrobeClient) -> io::Result<Vec<String>> {
    client
        .status(StatusRequest::drawer_names())
        .map_err(client_error)
}

fn status_storage(client: &WardrobeClient) -> io::Result<StorageDiagnosis> {
    client
        .status(StatusRequest::storage())
        .map_err(client_error)
}

fn status_check(client: &WardrobeClient, path: &str) -> io::Result<wardrobe_core::CheckReport> {
    client
        .status(StatusRequest::path(path))
        .map_err(client_error)
}

fn status_tenants(client: &WardrobeClient) -> io::Result<Vec<String>> {
    client
        .status(StatusRequest::tenants())
        .map_err(client_error)
}

fn status_databases(client: &WardrobeClient) -> io::Result<Vec<StorageInventory>> {
    client
        .status(StatusRequest::databases())
        .map_err(client_error)
}

fn status_schemas(client: &WardrobeClient, database_name: &str) -> io::Result<Vec<String>> {
    client
        .status(StatusRequest::schemas(database_name))
        .map_err(client_error)
}

fn status_drawers(
    client: &WardrobeClient,
    database_name: &str,
    schema_name: &str,
) -> io::Result<Vec<StorageInventory>> {
    client
        .status(StatusRequest::drawers(database_name, schema_name))
        .map_err(client_error)
}

fn create_inventory(
    client: &WardrobeClient,
    request: CreateRequest,
) -> io::Result<StorageInventory> {
    match client.create(request).map_err(client_error)? {
        CreateResult::StorageInventory(inventory) => Ok(inventory),
        other => unexpected_create_result("storage inventory", other),
    }
}

fn drawer_delete_filter(drawer_name: impl Into<String>, query: Value) -> OperationFilter {
    OperationFilter::many(vec![
        OperationFilter::drawer(drawer_name.into()),
        OperationFilter::Query(query),
    ])
}

fn operation_filter(path: &str, filter: Option<Value>) -> OperationFilter {
    match filter {
        Some(filter) => OperationFilter::many(vec![
            OperationFilter::from(path.to_string()),
            OperationFilter::from(filter),
        ]),
        None => OperationFilter::from(path.to_string()),
    }
}

fn operation_options(parts: &[String], index: usize, label: &str) -> io::Result<OperationOptions> {
    if let Some(raw_options) = parts.get(index) {
        OperationOptions::from_json(parse_json_arg(raw_options, label)?)
    } else {
        Ok(OperationOptions::default())
    }
}

fn alter_schema_rule(
    client: &WardrobeClient,
    drawer_path: &str,
    action: &str,
    kind: &str,
    field_name: &str,
    payload: Value,
) -> io::Result<Value> {
    if action == "drop" {
        client
            .drop(DropRequest::schema_rule(
                drawer_path,
                kind,
                field_name,
                payload,
            ))
            .map_err(client_error)
    } else {
        client
            .alter(AlterRequest::schema_rule(
                drawer_path,
                "add",
                kind,
                field_name,
                payload,
            ))
            .map_err(client_error)
    }
}

fn administer_user_action(
    client: &WardrobeClient,
    action: &str,
    payload: Value,
) -> io::Result<Value> {
    match action.replace('-', "_").to_ascii_lowercase().as_str() {
        "add" | "add_user" | "create" | "create_user" => client
            .create(CreateRequest::user(payload))
            .map_err(client_error)
            .and_then(|result| match result {
                CreateResult::Admin(response) => Ok(response),
                other => unexpected_create_result("admin response", other),
            }),
        "drop" | "drop_user" | "remove_user" | "delete_user" => client
            .drop(DropRequest::user(payload_username(&payload)?))
            .map_err(client_error),
        "grant" | "grant_permission" => client
            .grant(permission_request_from_payload(payload)?)
            .map_err(client_error),
        "revoke" | "revoke_permission" => client
            .revoke(permission_request_from_payload(payload)?)
            .map_err(client_error),
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("unsupported canonical user admin action: {other}"),
        )),
    }
}

fn permission_request_from_payload(payload: Value) -> io::Result<PermissionRequest> {
    let username = payload_username(&payload)?;
    let permission_scope = payload
        .get("permission_scope")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                "permission payload requires permission_scope",
            )
        })?;
    if let Some(scope) = payload.get("scope").and_then(Value::as_object) {
        let path = scope
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "scope requires path"))?;
        let rights = scope
            .get("rights")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "scope requires rights"))?;
        Ok(PermissionRequest::with_scope(
            username,
            permission_scope,
            path,
            rights,
        ))
    } else {
        Ok(PermissionRequest::new(username, permission_scope))
    }
}

fn payload_username(payload: &Value) -> io::Result<String> {
    payload
        .get("username")
        .or_else(|| payload.get("user"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|username| !username.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                "user admin payload requires a non-empty username",
            )
        })
}

fn unexpected_create_result<T>(expected: &str, actual: CreateResult) -> io::Result<T> {
    Err(Error::new(
        ErrorKind::InvalidData,
        format!("expected {expected}, got {actual:?}"),
    ))
}

struct CertificateCommandOptions {
    device: String,
    output: Option<PathBuf>,
    server_name: String,
    service: bool,
}

fn run_security_command(security_dir: &Path, parts: &[String], pretty: bool) -> io::Result<()> {
    match parts.first().map(String::as_str) {
        Some("identity") => run_identity_command(security_dir, parts, pretty),
        Some("certificate") => run_certificate_command(security_dir, parts, pretty),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            "security command requires identity or certificate",
        )),
    }
}

fn run_identity_command(security_dir: &Path, parts: &[String], pretty: bool) -> io::Result<()> {
    let action = parts.get(1).map(String::as_str).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "identity requires create, enroll, renew, list, inspect, or remove",
        )
    })?;
    match action {
        "list" => print_json(&list_managed_certificates(security_dir)?, pretty),
        "create" | "enroll" | "renew" => {
            let name = parts.get(2).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("identity {action} requires a user or service name"),
                )
            })?;
            let options = certificate_command_options(parts, 3)?;
            let identity = certificate_identity_uri(name, options.service)?;
            let record = if action == "renew" {
                renew_managed_client_certificate(
                    security_dir,
                    &identity,
                    &options.device,
                    options.output.as_deref(),
                    &options.server_name,
                )?
            } else {
                issue_managed_client_certificate(
                    security_dir,
                    &identity,
                    &options.device,
                    options.output.as_deref(),
                    &options.server_name,
                )?
            };
            print_json(&record, pretty)
        }
        "inspect" => {
            let name = parts.get(2).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "identity inspect requires a user or service name",
                )
            })?;
            let options = certificate_command_options(parts, 3)?;
            let identity = certificate_identity_uri(name, options.service)?;
            print_json(
                &managed_identity_certificates(security_dir, &identity)?,
                pretty,
            )
        }
        "remove" => {
            let name = parts.get(2).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "identity remove requires a user or service name",
                )
            })?;
            let options = certificate_command_options(parts, 3)?;
            let identity = certificate_identity_uri(name, options.service)?;
            print_json(
                &json!({
                    "identity": identity,
                    "revoked": remove_managed_identity(security_dir, &identity)?,
                }),
                pretty,
            )
        }
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown identity action: {other}"),
        )),
    }
}

fn run_certificate_command(security_dir: &Path, parts: &[String], pretty: bool) -> io::Result<()> {
    let action = parts.get(1).map(String::as_str).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "certificate requires issue, renew, revoke, or list",
        )
    })?;
    match action {
        "list" => print_json(&list_managed_certificates(security_dir)?, pretty),
        "issue" => {
            let name = parts.get(2).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "certificate issue requires an identity",
                )
            })?;
            let options = certificate_command_options(parts, 3)?;
            let identity = certificate_identity_uri(name, options.service)?;
            let record = issue_managed_client_certificate(
                security_dir,
                &identity,
                &options.device,
                options.output.as_deref(),
                &options.server_name,
            )?;
            print_json(&record, pretty)
        }
        "renew" => {
            let target = parts.get(2).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "certificate renew requires an identity or certificate serial",
                )
            })?;
            let options = certificate_command_options(parts, 3)?;
            let (identity, device, existing_output) =
                certificate_renewal_target(security_dir, target, options.service, &options.device)?;
            let output = options.output.as_deref().or(existing_output.as_deref());
            let record = renew_managed_client_certificate(
                security_dir,
                &identity,
                &device,
                output,
                &options.server_name,
            )?;
            print_json(&record, pretty)
        }
        "revoke" => {
            let serial = parts.get(2).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "certificate revoke requires a certificate serial",
                )
            })?;
            print_json(
                &json!({
                    "serial": serial,
                    "revoked": revoke_managed_certificate(security_dir, serial)?,
                }),
                pretty,
            )
        }
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown certificate action: {other}"),
        )),
    }
}

fn certificate_command_options(
    parts: &[String],
    start: usize,
) -> io::Result<CertificateCommandOptions> {
    let mut options = CertificateCommandOptions {
        device: "default".to_string(),
        output: None,
        server_name: "localhost".to_string(),
        service: false,
    };
    let mut index = start;
    while index < parts.len() {
        match parts[index].as_str() {
            "--device" => {
                index += 1;
                options.device = parts.get(index).cloned().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidInput, "--device requires a name")
                })?;
            }
            "--output" => {
                index += 1;
                options.output = Some(PathBuf::from(parts.get(index).ok_or_else(|| {
                    Error::new(ErrorKind::InvalidInput, "--output requires a path")
                })?));
            }
            "--server-name" => {
                index += 1;
                options.server_name = parts.get(index).cloned().ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        "--server-name requires a DNS name or IP address",
                    )
                })?;
            }
            "--service" => options.service = true,
            unknown => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Unknown certificate option: {unknown}"),
                ));
            }
        }
        index += 1;
    }
    Ok(options)
}

fn certificate_identity_uri(name: &str, service: bool) -> io::Result<String> {
    if name.starts_with("wardrobe:user:") || name.starts_with("wardrobe:service:") {
        return Ok(name.to_string());
    }
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "identity name must contain only letters, digits, dash, underscore, or dot",
        ));
    }
    let kind = if service { "service" } else { "user" };
    Ok(format!("wardrobe:{kind}:{name}"))
}

fn certificate_renewal_target(
    security_dir: &Path,
    target: &str,
    service: bool,
    default_device: &str,
) -> io::Result<(String, String, Option<PathBuf>)> {
    if target.len() >= 16
        && target
            .chars()
            .all(|character| character.is_ascii_hexdigit() || character == ':')
    {
        let normalized_target = normalized_certificate_serial(target);
        if let Some(record) = list_managed_certificates(security_dir)?
            .into_iter()
            .find(|record| normalized_certificate_serial(&record.serial) == normalized_target)
        {
            return Ok((
                record.identity,
                record.device,
                record.profile.parent().map(Path::to_path_buf),
            ));
        }
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("certificate serial '{target}' is not in the managed registry"),
        ));
    }
    Ok((
        certificate_identity_uri(target, service)?,
        default_device.to_string(),
        None,
    ))
}

fn normalized_certificate_serial(serial: &str) -> String {
    serial
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn run_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.is_empty() {
        return Ok(());
    }

    let command_name = parts[0].as_str();
    let started = Instant::now();
    cli_log(
        ApplicationLogLevel::Info,
        "command_start",
        vec![
            ("operation", "command_execute".to_string()),
            ("command", command_name.to_string()),
        ],
    );
    let result = match command_name {
        "status" => run_status_command(client, parts, pretty),
        "inspect" => run_inspect_command(client, parts, pretty),
        "read" => run_read_command(client, parts, pretty),
        "count" => run_count_command(client, parts, pretty),
        "upsert" => run_upsert_command(client, parts, pretty),
        "create" => {
            if parts
                .get(1)
                .map(|target| is_structural_create_target(target))
                .unwrap_or(false)
            {
                run_create_command(client, parts, pretty)
            } else if parts.get(1).map(String::as_str) == Some("user") {
                run_create_user_command(client, parts, pretty)
            } else {
                Err(Error::new(
                    ErrorKind::InvalidInput,
                    "create requires a structural type or user payload; use upsert for documents",
                ))
            }
        }
        "alter" => run_schema_management_command(client, parts, pretty),
        "drop" => run_drop_command(client, parts, pretty),
        "delete" => run_delete_command(client, parts),
        "grant" | "revoke" => run_permission_command(client, parts, pretty),
        "compact" => run_compact_command(client, parts, pretty),
        "backup" => run_backup_command(client, parts, pretty),
        "restore" => run_restore_command(client, parts, pretty),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown command: {}", parts[0]),
        )),
    };
    match &result {
        Ok(()) => cli_log(
            ApplicationLogLevel::Info,
            "command_finish",
            vec![
                ("operation", "command_execute".to_string()),
                ("command", command_name.to_string()),
                ("duration_us", started.elapsed().as_micros().to_string()),
                ("success", "true".to_string()),
            ],
        ),
        Err(error) => cli_log(
            ApplicationLogLevel::Error,
            "command_failure",
            vec![
                ("operation", "command_execute".to_string()),
                ("command", command_name.to_string()),
                ("duration_us", started.elapsed().as_micros().to_string()),
                ("error_kind", format!("{:?}", error.kind())),
                ("error", error.to_string()),
                ("mutation_phase", "unknown".to_string()),
                ("success", "false".to_string()),
            ],
        ),
    }
    result
}

fn run_upsert_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "upsert requires <path> <json_payload> <?json_filter> <?json_options>",
        ));
    }
    if parts.len() > 5 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "upsert accepts at most <path> <json_payload> <json_filter> <json_options>",
        ));
    }
    let payload = parse_json_arg(&parts[2], "payload")?;
    let filter = if parts.len() >= 4 {
        Some(parse_json_arg(&parts[3], "upsert filter")?)
    } else {
        None
    };
    let options = operation_options(parts, 4, "upsert options")?;
    let pointers = client
        .upsert(payload, operation_filter(&parts[1], filter), options)
        .map_err(client_error)?
        .into_pointers();
    if pointers.len() == 1 {
        println!("{}", pointers[0]);
    } else {
        print_json(&pointers, pretty)?;
    }
    Ok(())
}

fn run_count_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "count requires a drawer path",
        ));
    }
    if parts.len() > 4 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "count accepts at most <path> <json_filter> <json_options>",
        ));
    }

    let filter = if parts.len() >= 3 {
        operation_filter(&parts[1], Some(parse_json_arg(&parts[2], "count filter")?))
    } else {
        operation_filter(&parts[1], None)
    };
    let options = operation_options(parts, 3, "count options")?;
    let count = client.count(filter, options).map_err(client_error)?;
    print_json(&count, pretty)
}

fn run_read_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() > 4 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "read accepts at most <path> <json_filter> <json_options>",
        ));
    }

    let filter = match parts.len() {
        1 => OperationFilter::none(),
        2 => operation_filter(&parts[1], None),
        _ => operation_filter(&parts[1], Some(parse_json_arg(&parts[2], "query filter")?)),
    };
    let mut options = operation_options(parts, 3, "read options")?;
    if options.hydrate.is_none() {
        options.hydrate = Some(true);
    }

    match client.read(filter, options).map_err(client_error)? {
        ReadResult::Records(mut records) => {
            pub_normalize_record_ids(&mut records);
            print_json(&records, pretty)
        }
        ReadResult::Page(mut page) => {
            pub_normalize_record_ids(&mut page.records);
            print_json(&page, pretty)
        }
        ReadResult::Record(Some(record)) => {
            let mut records = vec![record];
            pub_normalize_record_ids(&mut records);
            print_json(&records.remove(0), pretty)
        }
        ReadResult::Record(None) => print_json(&Value::Null, pretty),
        ReadResult::Pointers(pointers) => print_json(&pointers, pretty),
        ReadResult::Exists(exists) => print_json(&exists, pretty),
    }
}

fn run_inspect_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "inspect requires a drawer path",
        ));
    }
    if parts.len() > 4 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "inspect accepts at most <path> <json_filter> <json_options>",
        ));
    }

    let filter = if parts.len() >= 3 {
        operation_filter(
            &parts[1],
            Some(parse_json_arg(&parts[2], "inspect filter")?),
        )
    } else {
        operation_filter(&parts[1], None)
    };
    let options = operation_options(parts, 3, "inspect options")?;
    match client.inspect(filter, options).map_err(client_error)? {
        InspectResult::Drawer(metrics) => print_json(&metrics, pretty),
        other => print_json(&other, pretty),
    }
}

enum DeleteTarget {
    Id(String),
    Filter(Value),
}

fn run_delete_command(client: &WardrobeClient, parts: &[String]) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "{} requires a pointer or <path> <json_filter_or_id>",
                parts[0]
            ),
        ));
    }
    if parts.len() > 4 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "delete accepts at most <pointer> or <path> <json_filter_or_id> <json_options>",
        ));
    }

    if parts.len() == 2 {
        let deleted = client
            .delete(
                OperationFilter::pointer(&parts[1]),
                None::<OperationOptions>,
            )
            .map_err(client_error)?;
        println!("deleted: {}", deleted.deleted);
        return Ok(());
    }

    match parse_delete_target(&parts[2])? {
        DeleteTarget::Id(record_id) => {
            let options = operation_options(parts, 3, "delete options")?;
            let pointer = pointer_from_record_id(&parts[1], &record_id);
            let deleted = client
                .delete(OperationFilter::pointer(&pointer), options)
                .map_err(client_error)?;
            println!("deleted: {}", deleted.deleted);
        }
        DeleteTarget::Filter(filter) => {
            let options = if parts.len() >= 4 {
                operation_options(parts, 3, "delete options")?
            } else {
                OperationOptions::new().multi(true)
            };
            let (matched, deleted) = delete_by_filter(client, &parts[1], filter, options)?;
            println!("matched: {matched}");
            println!("deleted: {deleted}");
        }
    }
    Ok(())
}

fn parse_delete_target(raw: &str) -> io::Result<DeleteTarget> {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') || trimmed.starts_with('"') {
        let payload = parse_json_arg(raw, "delete filter or id")?;
        match payload {
            Value::String(record_id) => Ok(DeleteTarget::Id(record_id)),
            Value::Object(map) => {
                if map.len() == 1 {
                    if let Some(Value::String(record_id)) = map.get("_id") {
                        return Ok(DeleteTarget::Id(record_id.clone()));
                    }
                }
                Ok(DeleteTarget::Filter(Value::Object(map)))
            }
            _ => Err(Error::new(
                ErrorKind::InvalidInput,
                "delete target must be a structural ID string or JSON object filter",
            )),
        }
    } else {
        Ok(DeleteTarget::Id(trimmed.to_string()))
    }
}

fn delete_by_filter(
    client: &WardrobeClient,
    drawer_name: &str,
    filter: Value,
    options: OperationOptions,
) -> io::Result<(usize, usize)> {
    let operation_filter = drawer_delete_filter(drawer_name, filter);
    let matched = client
        .count(operation_filter.clone(), None::<OperationOptions>)
        .map_err(client_error)?;
    let deleted = client
        .delete(operation_filter, options)
        .map_err(client_error)?
        .deleted;

    Ok((matched, deleted))
}

fn run_schema_management_command(
    client: &WardrobeClient,
    parts: &[String],
    pretty: bool,
) -> io::Result<()> {
    if parts.len() < 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "{} requires <type> <path> <?target_field> <?extra_args>",
                parts[0]
            ),
        ));
    }

    let action = parts[0].as_str();
    let kind = normalize_schema_command_type(&parts[1])?;
    let drawer_path = normalize_drawer_path(&parts[2], "schema command path")?;
    let field_name = parts.get(3).map(String::as_str).unwrap_or("timestamps");
    let payload = schema_management_payload(action, &kind, field_name, parts)?;
    let response = alter_schema_rule(client, &drawer_path, action, &kind, field_name, payload)?;
    print_json(&response, pretty)
}

fn is_schema_management_type(kind: &str) -> bool {
    normalize_schema_command_type(kind).is_ok()
}

fn normalize_schema_command_type(kind: &str) -> io::Result<String> {
    match kind.to_ascii_lowercase().as_str() {
        "index" | "indexes" => Ok("index".to_string()),
        "key" | "keys" => Ok("key".to_string()),
        "constraint" | "constraints" => Ok("constraint".to_string()),
        "trigger" | "triggers" => Ok("trigger".to_string()),
        "relationship" | "relationships" => Ok("relationship".to_string()),
        "cascade-delete" | "cascade_delete" | "cascade" | "delete-rule" | "delete-rules" => {
            Ok("cascade-delete".to_string())
        }
        "timestamp" | "timestamps" => Ok("timestamp".to_string()),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown schema command type: {kind}"),
        )),
    }
}

fn normalize_drawer_path(raw_path: &str, label: &str) -> io::Result<String> {
    let segments = split_structural_path(raw_path, label)?;
    if segments.len() != 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{label} must identify a wardrobe/bay/drawer path"),
        ));
    }
    Ok(segments.join("/"))
}

fn schema_management_payload(
    action: &str,
    kind: &str,
    field_name: &str,
    parts: &[String],
) -> io::Result<Value> {
    if field_name.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "schema command target field cannot be empty",
        ));
    }

    match kind {
        "index" => Ok(json!({ "kind": "index" })),
        "key" => {
            let key_type = parts
                .get(4)
                .map(String::as_str)
                .unwrap_or("secondary")
                .to_ascii_lowercase();
            if key_type != "primary" && key_type != "secondary" {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "key requires primary or secondary as the optional key type",
                ));
            }
            Ok(json!({ "key_type": key_type }))
        }
        "constraint" => {
            let Some(constraint) = parts.get(4) else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "constraint requires a constraint type such as unique or non-null",
                ));
            };
            Ok(json!({ "constraint": constraint }))
        }
        "trigger" => {
            if action == "alter" && parts.len() < 5 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "trigger requires a script path or command",
                ));
            }
            Ok(json!({
                "event": field_name,
                "command": parts.get(4).cloned().unwrap_or_default()
            }))
        }
        "relationship" => {
            if action == "drop" && parts.len() < 5 {
                return Ok(json!({}));
            }
            let Some(target_path) = parts.get(4) else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "relationship requires a target drawer path",
                ));
            };
            let relationship_type = parts.get(5).map(String::as_str).unwrap_or("M:1");
            let target_drawer = normalize_drawer_path(target_path, "relationship target path")?;
            let mut payload = json!({
                "type": relationship_type,
                "target_drawer": target_drawer
            });
            if relationship_type == "1:M" {
                let Some(mapped_by) = parts.get(6) else {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "1:M relationship requires a mapped_by field",
                    ));
                };
                payload["mapped_by"] = Value::String(mapped_by.clone());
            }
            Ok(payload)
        }
        "cascade-delete" => Ok(json!({ "action": "Cascade" })),
        "timestamp" => Ok(json!({ "enabled": action == "alter" })),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown schema command type: {kind}"),
        )),
    }
}

fn run_create_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "create requires <type> <path>",
        ));
    }

    match parts[1].as_str() {
        "wardrobe" | "wardrobes" | "database" | "databases" => {
            let (wardrobe, _, _) = parse_structural_path(&parts[2], 1, "wardrobe path")?;
            let inventory = create_inventory(client, CreateRequest::database(&wardrobe))?;
            print_json(&inventory, pretty)
        }
        "bay" | "bays" | "schema" | "schemas" => {
            let (wardrobe, bay, _) = parse_structural_path(&parts[2], 2, "bay path")?;
            let inventory = create_inventory(client, CreateRequest::schema(&wardrobe, &bay))?;
            print_json(&inventory, pretty)
        }
        "drawer" | "drawers" => {
            let (wardrobe, bay, drawer) = parse_structural_path(&parts[2], 3, "drawer path")?;
            let inventory =
                create_inventory(client, CreateRequest::drawer(&wardrobe, &bay, &drawer))?;
            print_json(&inventory, pretty)
        }
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown create target: {other}"),
        )),
    }
}

fn run_drop_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "drop requires <type> <path>",
        ));
    }

    if is_schema_management_type(&parts[1]) {
        return run_schema_management_command(client, parts, pretty);
    }

    match parts[1].as_str() {
        "user" | "users" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "drop user requires <username> or <json_user_payload>",
                ));
            }
            let username = if parts[2].trim_start().starts_with('{') {
                let raw_payload = parts[2..].join(" ");
                payload_username(&parse_json_arg(&raw_payload, "user admin payload")?)?
            } else {
                validate_permission_username(&parts[2])?
            };
            let response = client
                .drop(DropRequest::user(username))
                .map_err(client_error)?;
            print_json(&response, pretty)
        }
        "wardrobe" | "wardrobes" | "database" | "databases" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "drop wardrobe requires <path>",
                ));
            }
            let (wardrobe, _, _) = parse_structural_path(&parts[2], 1, "wardrobe path")?;
            let response = client
                .drop(DropRequest::database(wardrobe))
                .map_err(client_error)?;
            print_json(&response, pretty)
        }
        "bay" | "bays" | "schema" | "schemas" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "drop bay requires <wardrobe>/<bay>",
                ));
            }
            let (wardrobe, bay, _) = parse_structural_path(&parts[2], 2, "bay path")?;
            let response = client
                .drop(DropRequest::schema(wardrobe, bay))
                .map_err(client_error)?;
            print_json(&response, pretty)
        }
        "drawer" | "drawers" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "drop drawer requires <wardrobe>/<bay>/<drawer>",
                ));
            }
            let (wardrobe, bay, drawer) = parse_structural_path(&parts[2], 3, "drawer path")?;
            let response = client
                .drop(DropRequest::drawer(wardrobe, bay, drawer))
                .map_err(client_error)?;
            print_json(&response, pretty)
        }
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown drop target: {other}"),
        )),
    }
}

fn run_status_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "status requires tenants, wardrobes, bays, drawers, wal, storage, path, drawer-names, cached-drawer-count, server, or config",
        ));
    }

    match parts[1].as_str() {
        "tenant" | "tenants" => {
            let tenants = status_tenants(client)?;
            print_json(&tenants, pretty)
        }
        "wardrobe" | "wardrobes" | "database" | "databases" => {
            let dbs = status_databases(client)?;
            print_json(&dbs, pretty)
        }
        "bay" | "bays" | "schema" | "schemas" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "status bays requires a wardrobe path",
                ));
            }
            let (wardrobe, _, _) = parse_structural_path(&parts[2], 1, "wardrobe path")?;
            let schemas = status_schemas(client, &wardrobe)?;
            print_json(&schemas, pretty)
        }
        "drawer" | "drawers" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "status drawers requires a bay path",
                ));
            }
            let (wardrobe, bay) = if parts.len() >= 4 && !has_path_separator(&parts[2]) {
                (parts[2].clone(), parts[3].clone())
            } else {
                let (wardrobe, bay, _) = parse_structural_path(&parts[2], 2, "bay path")?;
                (wardrobe, bay)
            };
            let drawers = status_drawers(client, &wardrobe, &bay)?;
            print_json(&drawers, pretty)
        }
        "wal" => {
            let database_name = parts.get(2).cloned();
            let status = client
                .status(StatusRequest::wal(database_name))
                .map_err(client_error)?;
            print_json(&status, pretty)
        }
        "storage" => {
            let storage = status_storage(client)?;
            print_json(&storage, pretty)
        }
        "path" => {
            let Some(path) = parts.get(2) else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "status path requires <path>",
                ));
            };
            let report = status_check(client, path)?;
            print_json(&report, pretty)
        }
        "drawer-names" | "drawer_names" => {
            let drawers = status_drawer_names(client)?;
            print_json(&drawers, pretty)
        }
        "cached-drawer-count" | "cached_drawer_count" => {
            let status = client
                .status(StatusRequest::cached_drawer_count())
                .map_err(client_error)?;
            print_json(&status, pretty)
        }
        "server" => print_json(
            &json!({
                "target": format_target(client.connection_target()),
                "status": "available"
            }),
            pretty,
        ),
        "config" => print_json(
            &json!({
                "target": format_target(client.connection_target())
            }),
            pretty,
        ),
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown status target: {other}"),
        )),
    }
}

#[derive(serde::Serialize)]
struct CompactCommandResult {
    path: String,
    report: VacuumReport,
}

fn run_compact_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "compact requires <path>",
        ));
    }

    let targets = compact_targets(client, &parts[1])?;
    let mut results = Vec::new();
    for path in targets {
        let report = client
            .compact(CompactRequest::drawer(&path))
            .map_err(client_error)?;
        results.push(CompactCommandResult { path, report });
    }
    print_json(&results, pretty)
}

fn compact_targets(client: &WardrobeClient, raw_path: &str) -> io::Result<Vec<String>> {
    let segments = split_structural_path(raw_path, "compact path")?;
    match segments.len() {
        1 => {
            let wardrobe = &segments[0];
            let mut targets = Vec::new();
            for bay in status_schemas(client, wardrobe)? {
                for drawer in status_drawers(client, wardrobe, &bay)? {
                    targets.push(format!("{wardrobe}/{bay}/{}", drawer.name));
                }
            }
            Ok(targets)
        }
        2 => {
            let wardrobe = &segments[0];
            let bay = &segments[1];
            status_drawers(client, wardrobe, bay).map(|drawers| {
                drawers
                    .into_iter()
                    .map(|drawer| format!("{wardrobe}/{bay}/{}", drawer.name))
                    .collect()
            })
        }
        3 => Ok(vec![segments.join("/")]),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            "compact path must identify a wardrobe, bay, or drawer",
        )),
    }
}

const BACKUP_ARCHIVE_FORMAT: &str = "wardrobe-cli-backup-v1";

#[derive(Serialize)]
struct BackupCommandResult {
    source_path: String,
    destination_archive_path: String,
    scope: String,
    file_count: usize,
    byte_count: usize,
}

#[derive(Serialize)]
struct RestoreCommandResult {
    destination_path: String,
    source_archive_path: String,
    scope: String,
    file_count: usize,
    byte_count: usize,
}

fn run_backup_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "backup requires <source_path> <destination_archive_path>",
        ));
    }

    let archive = client.backup(&parts[1]).map_err(client_error)?;
    let byte_count = archive
        .files
        .iter()
        .map(|file| file.bytes_hex.len() / 2)
        .sum::<usize>();
    let source_path = archive.source_path.clone();
    let scope = archive.scope.clone();
    let file_count = archive.files.len();
    let archive_path = PathBuf::from(&parts[2]);
    if let Some(parent) = archive_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let serialized = serde_json::to_vec_pretty(&archive).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("Failed to serialize backup archive: {error}"),
        )
    })?;
    fs::write(&archive_path, serialized)?;

    print_json(
        &BackupCommandResult {
            source_path,
            destination_archive_path: archive_path.display().to_string(),
            scope,
            file_count,
            byte_count,
        },
        pretty,
    )
}

fn run_restore_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "restore requires <destination_path> <source_archive_path>",
        ));
    }

    let archive_path = PathBuf::from(&parts[2]);
    let archive = read_backup_archive(&archive_path)?;
    let report = client.restore(&parts[1], archive).map_err(client_error)?;

    print_json(
        &RestoreCommandResult {
            destination_path: report.destination_path,
            source_archive_path: archive_path.display().to_string(),
            scope: report.scope,
            file_count: report.file_count,
            byte_count: report.byte_count,
        },
        pretty,
    )
}

fn read_backup_archive(path: &Path) -> io::Result<wardrobe_core::BackupArchive> {
    let bytes = fs::read(path)?;
    let archive =
        serde_json::from_slice::<wardrobe_core::BackupArchive>(&bytes).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Invalid backup archive JSON: {error}"),
            )
        })?;

    if archive.format != BACKUP_ARCHIVE_FORMAT {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "Invalid backup archive format: expected {BACKUP_ARCHIVE_FORMAT}, found {}",
                archive.format
            ),
        ));
    }

    Ok(archive)
}

#[derive(Debug, PartialEq)]
struct PermissionScope {
    normalized: String,
    path: String,
    rights: String,
}

fn run_create_user_command(
    client: &WardrobeClient,
    parts: &[String],
    pretty: bool,
) -> io::Result<()> {
    if parts.len() < 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "create user requires <json_user_payload>",
        ));
    }

    let raw_payload = parts[2..].join(" ");
    let payload = parse_user_admin_payload(&raw_payload)?;
    let response = administer_user_action(client, "create_user", payload)?;
    print_json(&response, pretty)
}

fn run_permission_command(
    client: &WardrobeClient,
    parts: &[String],
    pretty: bool,
) -> io::Result<()> {
    if parts.len() < 4 || parts.get(1).map(String::as_str) != Some("permission") {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "{} requires permission <username> <permission_scope>",
                parts[0]
            ),
        ));
    }
    if parts.len() > 4 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "permission scope must be a single <path>:<rights> argument",
        ));
    }

    let username = validate_permission_username(&parts[2])?;
    let scope = parse_permission_scope(&parts[3])?;
    let request =
        PermissionRequest::with_scope(username, scope.normalized, scope.path, scope.rights);
    let response = match parts[0].as_str() {
        "grant" => client.grant(request).map_err(client_error)?,
        "revoke" => client.revoke(request).map_err(client_error)?,
        _ => unreachable!("permission command only routes grant or revoke"),
    };
    print_json(&response, pretty)
}

fn parse_user_admin_payload(raw: &str) -> io::Result<Value> {
    let payload = parse_json_arg(raw, "user admin payload")?;
    let user = payload.as_object().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "user admin payload must be a JSON object",
        )
    })?;

    let username = user
        .get("username")
        .or_else(|| user.get("user"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if username.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "user admin payload requires a non-empty username",
        ));
    }

    Ok(payload)
}

fn validate_permission_username(username: &str) -> io::Result<String> {
    let username = username.trim();
    if username.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "permission username cannot be empty",
        ));
    }
    if username.chars().any(char::is_whitespace) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "permission username cannot contain whitespace",
        ));
    }
    Ok(username.to_string())
}

fn parse_permission_scope(raw: &str) -> io::Result<PermissionScope> {
    let raw = raw.trim();
    let mut parts = raw.split(':');
    let path_part = parts.next().unwrap_or_default().trim();
    let rights_part = parts.next().unwrap_or_default().trim();
    if parts.next().is_some() || path_part.is_empty() || rights_part.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "permission scope must use <path>:<rights>",
        ));
    }

    let segments = split_structural_path(path_part, "permission scope path")?;
    if segments.len() > 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "permission scope path must identify a wardrobe, bay, or drawer",
        ));
    }

    let mut rights = String::new();
    for right in rights_part.chars().map(|right| right.to_ascii_lowercase()) {
        if !matches!(right, 'r' | 'u' | 'd' | 'i') {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "permission rights must contain only r, u, d, or i",
            ));
        }
        if rights.contains(right) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("permission right '{right}' cannot be repeated"),
            ));
        }
        rights.push(right);
    }

    if rights.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "permission scope requires at least one right",
        ));
    }

    let path = segments.join("/");
    Ok(PermissionScope {
        normalized: format!("{path}:{rights}"),
        path,
        rights,
    })
}

fn parse_json_arg(raw: &str, label: &str) -> io::Result<Value> {
    serde_json::from_str::<Value>(raw).map_err(|e| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("invalid JSON {label}: {e}"),
        )
    })
}

fn pointer_from_record_id(drawer_name: &str, record_id: &str) -> String {
    if record_id.starts_with('@') {
        record_id.to_string()
    } else {
        format!(
            "@{}:{}",
            drawer_name.trim_start_matches('@'),
            record_id.trim_start_matches("lnk_")
        )
    }
}

fn client_error(error: std::io::Error) -> std::io::Error {
    Error::new(error.kind(), format!("client error: {error}"))
}

fn is_structural_create_target(value: &str) -> bool {
    matches!(
        value,
        "wardrobe"
            | "wardrobes"
            | "database"
            | "databases"
            | "bay"
            | "bays"
            | "schema"
            | "schemas"
            | "drawer"
            | "drawers"
    )
}

fn has_path_separator(value: &str) -> bool {
    value.contains('/') || value.contains('\\')
}

fn parse_structural_path(
    raw_path: &str,
    expected_segments: usize,
    label: &str,
) -> io::Result<(String, String, String)> {
    let segments = split_structural_path(raw_path, label)?;
    if segments.len() != expected_segments {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{label} requires {expected_segments} path segment(s)"),
        ));
    }

    Ok((
        segments.first().cloned().unwrap_or_default(),
        segments.get(1).cloned().unwrap_or_default(),
        segments.get(2).cloned().unwrap_or_default(),
    ))
}

fn split_structural_path(raw_path: &str, label: &str) -> io::Result<Vec<String>> {
    let mut segments = Vec::new();
    for segment in raw_path.split(|c| c == '/' || c == '\\') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid {label} segment: {segment}"),
            ));
        }
        segments.push(segment.to_string());
    }

    if segments.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{label} cannot be empty"),
        ));
    }

    Ok(segments)
}

pub fn print_json<T: serde::Serialize>(v: &T, pretty: bool) -> io::Result<()> {
    let out = if pretty {
        serde_json::to_string_pretty(v).map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("JSON serialization error: {e}"),
            )
        })?
    } else {
        serde_json::to_string(v).map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("JSON serialization error: {e}"),
            )
        })?
    };
    println!("{out}");
    Ok(())
}

pub fn load_drawer_names(data_dir: &Path) -> io::Result<Vec<String>> {
    let data_dir = data_dir.to_string_lossy().to_string();
    let mut database = Database::initialize(&data_dir)?;
    database.load_existing_drawers("_id", HashMap::new())?;
    let mut drawer_names = database.get_all_drawers().into_keys().collect::<Vec<_>>();
    drawer_names.sort();
    Ok(drawer_names)
}

pub fn pub_normalize_record_ids(records: &mut Vec<Value>) {
    for record in records.iter_mut() {
        if let Value::Object(map) = record {
            if let Some(Value::String(id)) = map.get("_id") {
                if id.starts_with('@') {
                    if let Some(pos) = id.find(':') {
                        let mut id_part = id[pos + 1..].to_string();
                        if let Some(stripped) = id_part.strip_prefix("lnk_") {
                            id_part = stripped.to_string();
                        }
                        map.insert("_id".to_string(), Value::String(id_part));
                    }
                }
            }
        }
    }
}

pub fn inspect_drawer(data_dir: &Path, drawer_name: &str) -> io::Result<()> {
    let tokens = [drawer_name.to_string()];
    let target = resolve_inspect_target(data_dir, &tokens)?;
    inspect_drawer_target(&target)
}

struct InspectTarget {
    data_dir: PathBuf,
    drawer_name: String,
    label: String,
}

fn resolve_inspect_target(data_dir: &Path, drawer_tokens: &[String]) -> io::Result<InspectTarget> {
    let mut segments = Vec::new();
    for token in drawer_tokens {
        for segment in token.split(|c| c == '/' || c == '\\') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Invalid inspect path segment: {segment}"),
                ));
            }
            segments.push(segment.to_string());
        }
    }

    let drawer_name = segments
        .pop()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "inspect requires a drawer name"))?;

    let mut resolved_data_dir = data_dir.to_path_buf();
    for segment in &segments {
        resolved_data_dir.push(segment);
    }

    let label = if segments.is_empty() {
        drawer_name.clone()
    } else {
        format!("{}/{}", segments.join("/"), drawer_name)
    };

    Ok(InspectTarget {
        data_dir: resolved_data_dir,
        drawer_name,
        label,
    })
}

fn inspect_drawer_target(target: &InspectTarget) -> io::Result<()> {
    let files = drawer_files(&target.data_dir, &target.drawer_name);
    println!("Drawer: {}", target.label);
    print_file_status("data", &files.data)?;
    print_file_status("index", &files.index)?;
    print_file_status("meta", &files.meta)?;
    Ok(())
}

pub fn diagnose(data_dir: &Path) -> io::Result<()> {
    let drawers = load_drawer_names(data_dir)?;
    println!("Storage directory: {}", data_dir.display());
    println!("Drawer count: {}", drawers.len());

    if drawers.is_empty() {
        println!("Status: empty");
        return Ok(());
    }

    let mut issues = Vec::new();
    for drawer_name in drawers {
        let files = drawer_files(data_dir, &drawer_name);
        if !files.data.is_file() {
            issues.push(format!("{drawer_name}: missing data file"));
        }
        if !files.index.is_file() {
            issues.push(format!("{drawer_name}: missing index file"));
        }
        if !files.meta.is_file() {
            issues.push(format!("{drawer_name}: missing meta file"));
        }
    }

    if issues.is_empty() {
        println!("Status: ok");
    } else {
        println!("Status: issues found");
        for issue in issues {
            println!("{issue}");
        }
    }

    Ok(())
}

fn print_file_status(label: &str, path: &Path) -> io::Result<()> {
    if path.is_file() {
        let size = fs::metadata(path)?.len();
        println!("{label}: present ({size} bytes)");
    } else {
        println!("{label}: missing");
    }
    Ok(())
}

fn drawer_files(data_dir: &Path, drawer_name: &str) -> DrawerFiles {
    DrawerFiles {
        data: data_dir.join(format!("{drawer_name}.drw")),
        index: data_dir.join(format!("{drawer_name}_index.drw")),
        meta: data_dir.join(format!("{drawer_name}_meta.drw")),
    }
}

struct DrawerFiles {
    data: PathBuf,
    index: PathBuf,
    meta: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(test_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("wardrobe_cli_unit_{test_name}_{nanos}"))
    }

    #[test]
    fn inspect_target_splits_structured_reference() {
        let root = Path::new("/wardrobe");
        let tokens = [String::from("basic-usage/public/user")];

        let target = resolve_inspect_target(root, &tokens).expect("inspect target");

        assert_eq!(target.data_dir, root.join("basic-usage").join("public"));
        assert_eq!(target.drawer_name, "user");
        assert_eq!(target.label, "basic-usage/public/user");
    }

    #[test]
    fn inspect_target_accepts_split_tokens() {
        let root = Path::new("/wardrobe");
        let tokens = [
            String::from("basic-usage"),
            String::from("public"),
            String::from("user"),
        ];

        let target = resolve_inspect_target(root, &tokens).expect("inspect target");

        assert_eq!(target.data_dir, root.join("basic-usage").join("public"));
        assert_eq!(target.drawer_name, "user");
        assert_eq!(target.label, "basic-usage/public/user");
    }

    #[test]
    fn canonical_parser_helpers_cover_filter_options_and_targets() {
        assert_eq!(
            operation_filter("armory/public/gem", None),
            OperationFilter::Drawer("armory/public/gem".to_string())
        );
        assert_eq!(
            operation_filter("armory/public/gem", Some(json!({"power": 42}))),
            OperationFilter::Many(vec![
                OperationFilter::Drawer("armory/public/gem".to_string()),
                OperationFilter::Query(json!({"power": 42})),
            ])
        );
        assert_eq!(
            drawer_delete_filter("armory/public/gem", json!({})),
            OperationFilter::Many(vec![
                OperationFilter::Drawer("armory/public/gem".to_string()),
                OperationFilter::Query(json!({})),
            ])
        );

        let parts = vec![
            "read".to_string(),
            "armory/public/gem".to_string(),
            "{}".to_string(),
            r#"{"limit":2,"offset":1,"order_by":"power","order_direction":"desc","cursor":"next","page":3,"page_size":5}"#.to_string(),
        ];
        let options = operation_options(&parts, 3, "read options").expect("options parse");
        assert_eq!(options.limit, Some(2));
        assert_eq!(options.offset, Some(1));
        assert_eq!(options.order_by.as_deref(), Some("power"));
        assert_eq!(options.cursor.as_deref(), Some("next"));
        assert_eq!(options.page, Some(3));
        assert_eq!(options.page_size, Some(5));
        assert_eq!(
            options.order_direction,
            Some(wardrobe_core::OrderDirection::Descending)
        );
        assert!(operation_options(&parts, 99, "missing options").is_ok());

        match parse_delete_target("ruby").expect("id target") {
            DeleteTarget::Id(id) => assert_eq!(id, "ruby"),
            DeleteTarget::Filter(_) => panic!("expected id target"),
        }
        match parse_delete_target(r#""sapphire""#).expect("string target") {
            DeleteTarget::Id(id) => assert_eq!(id, "sapphire"),
            DeleteTarget::Filter(_) => panic!("expected id target"),
        }
        match parse_delete_target(r#"{"_id":"emerald"}"#).expect("object id target") {
            DeleteTarget::Id(id) => assert_eq!(id, "emerald"),
            DeleteTarget::Filter(_) => panic!("expected id target"),
        }
        match parse_delete_target(r#"{"power":42}"#).expect("filter target") {
            DeleteTarget::Filter(filter) => assert_eq!(filter, json!({"power": 42})),
            DeleteTarget::Id(_) => panic!("expected filter target"),
        }
        match parse_delete_target(r#"{}"#).expect("empty filter target") {
            DeleteTarget::Filter(filter) => assert_eq!(filter, json!({})),
            DeleteTarget::Id(_) => panic!("expected filter target"),
        }
        assert!(parse_delete_target("[1,2]").is_err());

        assert_eq!(pointer_from_record_id("gem", "@gem:ruby"), "@gem:ruby");
        assert_eq!(pointer_from_record_id("gem", "lnk_ruby"), "@gem:ruby");
        assert_eq!(pointer_from_record_id("@gem", "ruby"), "@gem:ruby");
    }

    #[test]
    fn schema_and_structural_helpers_validate_canonical_forms() {
        assert!(is_schema_management_type("index"));
        assert!(is_schema_management_type("cascade-delete"));
        assert_eq!(normalize_schema_command_type("indexes").unwrap(), "index");
        assert_eq!(normalize_schema_command_type("keys").unwrap(), "key");
        assert_eq!(
            normalize_schema_command_type("constraints").unwrap(),
            "constraint"
        );
        assert_eq!(
            normalize_schema_command_type("triggers").unwrap(),
            "trigger"
        );
        assert_eq!(
            normalize_schema_command_type("relationships").unwrap(),
            "relationship"
        );
        assert_eq!(
            normalize_schema_command_type("cascade_delete").unwrap(),
            "cascade-delete"
        );
        assert!(normalize_schema_command_type("unknown").is_err());

        assert_eq!(
            normalize_drawer_path("armory/public/gem", "drawer").unwrap(),
            "armory/public/gem"
        );
        assert!(normalize_drawer_path("armory/public", "drawer").is_err());

        assert_eq!(
            parse_structural_path("armory", 1, "wardrobe").unwrap(),
            ("armory".to_string(), String::new(), String::new())
        );
        assert_eq!(
            parse_structural_path("armory/public", 2, "bay").unwrap(),
            ("armory".to_string(), "public".to_string(), String::new())
        );
        assert_eq!(
            parse_structural_path("armory/public/gem", 3, "drawer").unwrap(),
            (
                "armory".to_string(),
                "public".to_string(),
                "gem".to_string()
            )
        );
        assert!(split_structural_path("armory//gem", "bad").is_err());
        assert!(split_structural_path("../gem", "bad").is_err());
        assert!(parse_structural_path("armory/public/gem", 2, "bay").is_err());

        let index_payload =
            schema_management_payload("add", "index", "tool.type", &Vec::new()).unwrap();
        assert_eq!(index_payload, json!({"kind": "index"}));
        let key_parts = vec![
            "alter".to_string(),
            "key".to_string(),
            "armory/public/gem".to_string(),
            "id".to_string(),
            "primary".to_string(),
        ];
        assert_eq!(
            schema_management_payload("add", "key", "id", &key_parts).unwrap(),
            json!({"key_type": "primary"})
        );
        assert!(schema_management_payload("add", "key", "id", &vec!["".to_string(); 5]).is_err());
        assert!(schema_management_payload("add", "index", "", &Vec::new()).is_err());

        let constraint_parts = vec![
            "alter".to_string(),
            "constraint".to_string(),
            "armory/public/gem".to_string(),
            "email".to_string(),
            "unique".to_string(),
        ];
        assert_eq!(
            schema_management_payload("add", "constraint", "email", &constraint_parts).unwrap(),
            json!({"constraint": "unique"})
        );
        assert!(schema_management_payload("add", "constraint", "email", &Vec::new()).is_err());

        let trigger_parts = vec![
            "alter".to_string(),
            "trigger".to_string(),
            "armory/public/gem".to_string(),
            "on_upsert".to_string(),
            "script.sh".to_string(),
        ];
        assert_eq!(
            schema_management_payload("add", "trigger", "on_upsert", &trigger_parts).unwrap(),
            json!({"event": "on_upsert", "command": "script.sh"})
        );
        assert!(schema_management_payload("alter", "trigger", "on_upsert", &Vec::new()).is_err());

        let relationship_parts = vec![
            "alter".to_string(),
            "relationship".to_string(),
            "armory/public/user".to_string(),
            "tool_id".to_string(),
            "armory/public/tool".to_string(),
            "1:M".to_string(),
            "owner_id".to_string(),
        ];
        assert_eq!(
            schema_management_payload("add", "relationship", "tool_id", &relationship_parts)
                .unwrap(),
            json!({
                "type": "1:M",
                "target_drawer": "armory/public/tool",
                "mapped_by": "owner_id"
            })
        );
        assert!(
            schema_management_payload("add", "relationship", "tool_id", &relationship_parts[..6])
                .is_err()
        );
        assert_eq!(
            schema_management_payload("add", "cascade-delete", "tool_id", &Vec::new()).unwrap(),
            json!({"action": "Cascade"})
        );
        assert!(schema_management_payload("add", "unknown", "field", &Vec::new()).is_err());
    }

    #[test]
    fn permission_and_user_payload_helpers_cover_validation_paths() {
        assert_eq!(
            payload_username(&json!({"username": " alice "})).unwrap(),
            "alice"
        );
        assert_eq!(payload_username(&json!({"user": "bob"})).unwrap(), "bob");
        assert!(payload_username(&json!({"username": ""})).is_err());

        assert_eq!(
            permission_request_from_payload(json!({
                "username": "alice",
                "permission_scope": "armory/public:rud"
            }))
            .unwrap()
            .into_payload(),
            json!({"username": "alice", "permission_scope": "armory/public:rud"})
        );
        assert_eq!(
            permission_request_from_payload(json!({
                "username": "alice",
                "permission_scope": "scoped",
                "scope": {"path": "armory/public", "rights": "rw"}
            }))
            .unwrap()
            .into_payload(),
            json!({
                "username": "alice",
                "permission_scope": "scoped",
                "scope": {"path": "armory/public", "rights": "rw"}
            })
        );
        assert!(permission_request_from_payload(json!({"username": "alice"})).is_err());
        assert!(
            permission_request_from_payload(
                json!({"username":"alice","permission_scope":"x","scope":{"rights":"r"}})
            )
            .is_err()
        );
        assert!(
            permission_request_from_payload(
                json!({"username":"alice","permission_scope":"x","scope":{"path":"armory"}})
            )
            .is_err()
        );

        assert_eq!(validate_permission_username(" alice ").unwrap(), "alice");
        assert!(validate_permission_username("").is_err());
        assert!(validate_permission_username("alice bob").is_err());

        let scope = parse_permission_scope("armory/public/gem:rud").unwrap();
        assert_eq!(scope.normalized, "armory/public/gem:rud");
        assert_eq!(scope.path, "armory/public/gem");
        assert_eq!(scope.rights, "rud");
        assert!(parse_permission_scope("armory/public/gem").is_err());
        assert!(parse_permission_scope("armory/public/gem:rx").is_err());
        assert!(parse_permission_scope("armory/public/gem/extra:r").is_err());

        assert_eq!(
            parse_user_admin_payload(r#"{"username":"alice","role":"operator"}"#).unwrap(),
            json!({"username":"alice","role":"operator"})
        );
        assert!(parse_user_admin_payload("alice").is_err());
        assert!(parse_user_admin_payload(r#"{"role":"operator"}"#).is_err());
    }

    #[test]
    fn status_and_filesystem_helpers_cover_canonical_runtime_paths() {
        let storage = temp_dir("status_and_files");
        let client = WardrobeClient::open(&storage.to_string_lossy()).expect("client open");

        assert!(run_command(&client, &["status".into(), "server".into()], false).is_ok());
        assert!(run_command(&client, &["status".into(), "config".into()], true).is_ok());
        assert!(run_command(&client, &["status".into(), "tenants".into()], false).is_ok());
        assert!(run_command(&client, &["status".into(), "wal".into()], false).is_ok());
        assert!(
            run_command(
                &client,
                &["status".into(), "wal".into(), "default".into()],
                false,
            )
            .is_ok()
        );
        assert!(
            run_command(
                &client,
                &["status".into(), "cached-drawer-count".into()],
                false
            )
            .is_ok()
        );
        assert!(run_command(&client, &["status".into()], false).is_err());
        assert!(run_command(&client, &["status".into(), "path".into()], false).is_err());
        assert!(run_command(&client, &["status".into(), "unknown".into()], false).is_err());

        fs::write(storage.join("gem.drw"), b"data").expect("write data");
        assert_eq!(drawer_files(&storage, "gem").data, storage.join("gem.drw"));
        assert!(print_file_status("data", &storage.join("gem.drw")).is_ok());
        assert!(print_file_status("meta", &storage.join("gem_meta.drw")).is_ok());
        assert!(load_drawer_names(&storage).is_ok());
        assert!(diagnose(&storage).is_ok());
        assert!(inspect_drawer(&storage, "gem").is_ok());
        assert!(resolve_inspect_target(&storage, &[String::from("../bad")]).is_err());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn local_identity_commands_delegate_to_managed_pki() {
        let root = temp_dir("managed_identity");
        wardrobe_core::initialize_managed_pki(
            &root,
            &["localhost".to_string()],
            &["127.0.0.1".parse().expect("IP should parse")],
        )
        .expect("managed PKI should initialize");
        let config = CliConfig {
            connection: root.display().to_string(),
            pretty: false,
            command_parts: vec![
                "identity".to_string(),
                "create".to_string(),
                "adminuser".to_string(),
                "--device".to_string(),
                "desktop".to_string(),
                "--server-name".to_string(),
                "localhost".to_string(),
            ],
            logging: ApplicationLoggingConfig::default(),
            profile: None,
        };

        run_cli_logic(config).expect("identity command should succeed");
        let records = list_managed_certificates(&root).expect("managed certificates should list");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].identity, "wardrobe:user:adminuser");
        assert_eq!(records[0].device, "desktop");

        let parsed = CliConfig::from_args(vec![
            "wardrobe://localhost:24842".to_string(),
            "--profile".to_string(),
            records[0].profile.display().to_string(),
            "status".to_string(),
            "wardrobes".to_string(),
        ])
        .expect("profile flag should parse");
        assert_eq!(parsed.profile, Some(records[0].profile.clone()));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_identity_and_certificate_commands_cover_lifecycle_and_validation() {
        let root = temp_dir("managed_commands");
        wardrobe_core::initialize_managed_pki(
            &root,
            &["localhost".to_string()],
            &["127.0.0.1".parse().expect("IP should parse")],
        )
        .expect("managed PKI should initialize");

        assert!(run_security_command(&root, &[], false).is_err());
        assert!(run_identity_command(&root, &["identity".into()], false).is_err());
        assert!(
            run_identity_command(&root, &["identity".into(), "unknown".into()], false).is_err()
        );
        assert!(run_identity_command(&root, &["identity".into(), "create".into()], false).is_err());
        assert!(
            run_identity_command(&root, &["identity".into(), "inspect".into()], false).is_err()
        );
        assert!(run_identity_command(&root, &["identity".into(), "remove".into()], false).is_err());

        let service_output = root.join("service-output");
        run_security_command(
            &root,
            &[
                "identity".into(),
                "enroll".into(),
                "sync".into(),
                "--service".into(),
                "--device".into(),
                "worker".into(),
                "--output".into(),
                service_output.display().to_string(),
                "--server-name".into(),
                "localhost".into(),
            ],
            true,
        )
        .expect("service identity should enroll");
        run_identity_command(
            &root,
            &[
                "identity".into(),
                "inspect".into(),
                "sync".into(),
                "--service".into(),
            ],
            false,
        )
        .expect("service identity should inspect");
        run_identity_command(
            &root,
            &[
                "identity".into(),
                "renew".into(),
                "sync".into(),
                "--service".into(),
                "--device".into(),
                "worker".into(),
                "--output".into(),
                service_output.display().to_string(),
            ],
            false,
        )
        .expect("service identity should renew");
        run_identity_command(
            &root,
            &[
                "identity".into(),
                "remove".into(),
                "sync".into(),
                "--service".into(),
            ],
            false,
        )
        .expect("service identity should remove");
        run_identity_command(&root, &["identity".into(), "list".into()], true)
            .expect("identities should list");

        assert!(run_certificate_command(&root, &["certificate".into()], false).is_err());
        assert!(
            run_certificate_command(&root, &["certificate".into(), "issue".into()], false).is_err()
        );
        assert!(
            run_certificate_command(&root, &["certificate".into(), "renew".into()], false).is_err()
        );
        assert!(
            run_certificate_command(&root, &["certificate".into(), "revoke".into()], false)
                .is_err()
        );
        assert!(
            run_certificate_command(&root, &["certificate".into(), "unknown".into()], false)
                .is_err()
        );

        run_certificate_command(
            &root,
            &[
                "certificate".into(),
                "issue".into(),
                "alice".into(),
                "--device".into(),
                "laptop".into(),
            ],
            false,
        )
        .expect("certificate should issue");
        let issued = list_managed_certificates(&root)
            .expect("certificates should list")
            .into_iter()
            .find(|record| record.identity == "wardrobe:user:alice")
            .expect("alice certificate should exist");
        run_certificate_command(
            &root,
            &[
                "certificate".into(),
                "renew".into(),
                issued.serial.clone(),
                "--server-name".into(),
                "localhost".into(),
            ],
            false,
        )
        .expect("certificate serial should renew");
        run_certificate_command(
            &root,
            &[
                "certificate".into(),
                "renew".into(),
                "bob".into(),
                "--device".into(),
                "phone".into(),
            ],
            false,
        )
        .expect("certificate identity should renew");
        let active_bob = list_managed_certificates(&root)
            .expect("certificates should list")
            .into_iter()
            .find(|record| record.identity == "wardrobe:user:bob" && !record.revoked)
            .expect("bob certificate should exist");
        run_certificate_command(
            &root,
            &["certificate".into(), "revoke".into(), active_bob.serial],
            false,
        )
        .expect("certificate should revoke");
        run_certificate_command(&root, &["certificate".into(), "list".into()], true)
            .expect("certificates should list");

        let options = certificate_command_options(
            &[
                "certificate".into(),
                "issue".into(),
                "sync".into(),
                "--service".into(),
            ],
            3,
        )
        .expect("service option should parse");
        assert!(options.service);
        assert_eq!(options.device, "default");
        for option in ["--device", "--output", "--server-name"] {
            assert!(
                certificate_command_options(
                    &[
                        "certificate".into(),
                        "issue".into(),
                        "alice".into(),
                        option.into(),
                    ],
                    3,
                )
                .is_err()
            );
        }
        assert!(
            certificate_command_options(
                &[
                    "certificate".into(),
                    "issue".into(),
                    "alice".into(),
                    "--unknown".into(),
                ],
                3,
            )
            .is_err()
        );
        assert_eq!(
            certificate_identity_uri("wardrobe:user:alice", true).unwrap(),
            "wardrobe:user:alice"
        );
        assert_eq!(
            certificate_identity_uri("sync", true).unwrap(),
            "wardrobe:service:sync"
        );
        assert!(certificate_identity_uri("bad identity", false).is_err());
        assert_eq!(normalized_certificate_serial("AA:bb-12"), "aabb12");
        assert!(
            certificate_renewal_target(
                &root,
                "0123456789abcdef0123456789abcdef",
                false,
                "default",
            )
            .is_err()
        );

        let _ = fs::remove_dir_all(root);
    }
}
