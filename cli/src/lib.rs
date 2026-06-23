use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Error, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use wardrobe_core::{ConnectionTarget, Database, VacuumReport, WardrobeClient};

#[derive(Debug)]
pub struct CliConfig {
    pub connection: String,
    pub pretty: bool,
    pub command_parts: Vec<String>,
}

impl CliConfig {
    pub fn from_args<I>(args: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let mut connection = "./wardrobe".to_string();
        let mut pretty = false;
        let mut command_parts = Vec::new();

        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--target" | "--connection" | "--data-dir" => {
                    let val = args.next().ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidInput,
                            "--target/--data-dir requires a connection string or path",
                        )
                    })?;
                    connection = val;
                }
                "--pretty" => {
                    pretty = true;
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                "--version" | "-v" => {
                    print_version();
                    std::process::exit(0);
                }
                _ => command_parts.push(arg),
            }
        }

        Ok(Self {
            connection,
            pretty,
            command_parts,
        })
    }
}

const HELP_TEXT: &str = r#"wardrobe-cli:
    -h, --help                  Show this message and exit
    -v, --version               Show the version and exit

    The first argument to the CLI is always the target connection context.
    It can be a filesystem path (e.g., ./wardrobe) for embedded mode, or a network connection string or socket location
    (e.g., wardrobe://127.0.0.1:24842) for remote execution.

    If a targeted filesystem path does not explicitly point to a database or bay context,
    the engine transparently routes execution into system defaults:
    - Default Wardrobe fallback:         "default"
    - Default Bay fallback:              "default"

    ===========================================================================
    STRUCTURAL & LIFECYCLE MANAGEMENT
    ===========================================================================
    show <type> <?parent_path>
        List structural elements. <type> must be one of: wardrobes, bays, drawers, tenants.
        Example: show bays my_wardrobe

    create <type> <path>
        Provision a new structural resource. <type> must be one of: wardrobe, bay, drawer.
        Example: create drawer my_wardrobe/my_bay/user

    check <path>
        Run physical presence and sanity verification checks on a structural layer.
        Example: check my_wardrobe/my_bay/user

    clean <path>
        Execute a space-reclaiming vacuum process on a drawer or group of drawers.
        Example: clean my_wardrobe/my_bay (cleans all drawers in bay)

    ===========================================================================
    DOCUMENT MUTATIONS & QUERIES (RUDI)
    ===========================================================================
    upsert <path> <json_payload>
        Insert a new document or completely overwrite an existing document by its _id.
        Example: upsert my_wardrobe/my_bay/user '{"_id": "user-01", "name": "Marcus"}'

    count <path> <?json_filter>
        Count documents matching an optional criteria filter.
        Example: count my_wardrobe/my_bay/user '{"tool.type": "Hammer"}'

    inspect <path>
        Expose raw storage metrics (sizes, record counts, tombstone fragmentation percentages).
        Does not output data records.
        Example: inspect my_wardrobe/my_bay/user

    records <path> <?json_filter>
        Retrieve a list of documents matching an optional criteria filter.
        Example: records my_wardrobe/my_bay/user '{"tool._id": "298234789328923489"}'
        Example: records my_wardrobe/my_bay/user '{"_id": "@tool:298234789328923489"}'
        Example: records my_wardrobe/my_bay/user '{"tool.type": "Hammer"}'
        Example: records my_wardrobe/my_bay/user '{"tool.type": {"$in": ["Hammer", "Sword"]}}'
        Example: records my_wardrobe/my_bay/user '{"tool.type": {"$nin": ["Hammer", "Sword"]}}'
        Example: records my_wardrobe/my_bay/user '{"tool.type": {"$exists": true}}'
        Example: records my_wardrobe/my_bay/user '{"tool.type": {"$exists": false}}'
        Example: records my_wardrobe/my_bay/user '{"tool.type": {"$regex": ".*Sword.*"}}'
        Example: records my_wardrobe/my_bay/user '{"tool.type.owner": "@user:8723478929234786234"}'

    delete <path> <json_filter_or_id>
        Remove documents from a drawer matching a specific structural ID or JSON filter criteria.
        Example: delete my_wardrobe/my_bay/user '{"_id": "user-02"}'

    ===========================================================================
    SCHEMA ENGINE & RELATIONSHIP MANAGEMENT
    ===========================================================================
    add <type> <path> <target_field> <?extra_args>
        Attach a structural modifier, index, rule, or side-effect routine to a field.
        <type> must be one of: index, key, constraint, trigger, relationship, cascade-delete.

        Examples:
        add index my_wardrobe/my_bay/user tool.type
        add key my_wardrobe/my_bay/user profile_id secondary
        add constraint my_wardrobe/my_bay/user email unique
        add constraint my_wardrobe/my_bay/user age non-null
        add relationship my_wardrobe/my_bay/user tool_id my_wardrobe/my_bay/tool
        add cascade-delete my_wardrobe/my_bay/user tool_id
        add trigger my_wardrobe/my_bay/user on_upsert ./scripts/sync_profile.sh

    remove <type> <path> <target_field> <?extra_args>
        Detach or drop an active modifier, index, rule, or routine from a field context.
        <type> must be one of: index, key, constraint, trigger, relationship, cascade-delete.

        Examples:
        remove index my_wardrobe/my_bay/user tool.type
        remove constraint my_wardrobe/my_bay/user email unique
        remove cascade-delete my_wardrobe/my_bay/user tool_id

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
    add user <json_user_payload>
        Register an authorized administrative or client identity with the Wardrobe instance.
        Example: add user '{"username": "dev_admin", "role": "operator"}'

    grant permission <username> <permission_scope>
        Delegate functional access rights (Read, Update, Delete, Inspect) across a path scope.
        Example: grant permission dev_admin my_wardrobe/my_bay:rud

    revoke permission <username> <permission_scope>
        Strip functional access rights from a user identity.
        Example: revoke permission dev_admin my_wardrobe/my_bay:d

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
    println!("wardrobe-cli {}", env!("CARGO_PKG_VERSION"));
}

pub fn run_cli_logic(config: CliConfig) -> io::Result<()> {
    let client = WardrobeClient::open(&config.connection)
        .map_err(|e| Error::new(ErrorKind::Other, format!("Failed to open connection: {e}")))?;

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

pub fn run_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.is_empty() {
        return Ok(());
    }

    match parts[0].as_str() {
        "drawers" => {
            if client.requires_embedded_engine() {
                let data_dir = match client.connection_target() {
                    ConnectionTarget::EmbeddedPath(p) => p.clone(),
                    _ => PathBuf::from("./wardrobe"),
                };
                let drawers = load_drawer_names(&data_dir)?;
                for d in drawers {
                    println!("{d}");
                }
                Ok(())
            } else {
                eprintln!(
                    "drawers command is only supported for embedded connections; use show-databases for network targets"
                );
                Ok(())
            }
        }
        "diagnose" => {
            if client.requires_embedded_engine() {
                let data_dir = match client.connection_target() {
                    ConnectionTarget::EmbeddedPath(p) => p.clone(),
                    _ => PathBuf::from("./wardrobe"),
                };
                diagnose(&data_dir)
            } else {
                eprintln!("diagnose is only available for embedded connections");
                Ok(())
            }
        }
        "inspect" => run_inspect_command(client, parts, pretty),
        "records" => run_records_command(client, parts, pretty, false),
        "find" | "get" | "query" => run_records_command(client, parts, pretty, true),
        "count" => run_count_command(client, parts, pretty),
        "upsert" | "insert" => run_upsert_command(client, parts),
        "add" => run_schema_management_command(client, parts, pretty),
        "create" => {
            if parts
                .get(1)
                .map(|target| is_structural_create_target(target))
                .unwrap_or(false)
            {
                run_create_command(client, parts, pretty)
            } else {
                run_upsert_command(client, parts)
            }
        }
        "delete-by-id" => {
            if parts.len() < 2 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "delete-by-id requires a pointer",
                ));
            }
            let deleted = client.delete_by_id(&parts[1]).map_err(client_error)?;
            println!("deleted: {deleted}");
            Ok(())
        }
        "remove"
            if parts
                .get(1)
                .is_some_and(|kind| is_schema_management_type(kind)) =>
        {
            run_schema_management_command(client, parts, pretty)
        }
        "delete" | "remove" => run_delete_command(client, parts),
        "define" => run_define_command(client, parts, pretty),
        "create-db" => {
            if parts.len() < 2 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "create-db requires a database name",
                ));
            }
            let inventory = client.create_database(&parts[1]).map_err(client_error)?;
            print_json(&inventory, pretty)
        }
        "create-schema" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "create-schema requires <database> <schema>",
                ));
            }
            let inventory = client
                .create_schema(&parts[1], &parts[2])
                .map_err(client_error)?;
            print_json(&inventory, pretty)
        }
        "create-drawer" => {
            if parts.len() < 4 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "create-drawer requires <database> <schema> <drawer>",
                ));
            }
            let inventory = client
                .create_drawer(&parts[1], &parts[2], &parts[3])
                .map_err(client_error)?;
            print_json(&inventory, pretty)
        }
        "manage" => run_manage_user_command(client, parts, pretty),
        "auth" | "rbac" => run_manage_user_alias(client, parts, pretty),
        "show" | "ls" | "list" => run_show_command(client, parts, pretty),
        "check" => run_check_command(client, parts),
        "clean" => run_clean_command(client, parts, pretty),
        "backup" => run_backup_command(client, parts, pretty),
        "restore" => run_restore_command(client, parts, pretty),
        "show-databases" => {
            let dbs = client.show_databases().map_err(client_error)?;
            print_json(&dbs, pretty)
        }
        "show-schemas" => {
            if parts.len() < 2 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "show-schemas requires a database name",
                ));
            }
            let schemas = client.show_schemas(&parts[1]).map_err(client_error)?;
            print_json(&schemas, pretty)
        }
        "show-drawers" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "show-drawers requires <database> <schema>",
                ));
            }
            let drawers = client
                .show_drawers(&parts[1], &parts[2])
                .map_err(client_error)?;
            print_json(&drawers, pretty)
        }
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown command: {}", parts[0]),
        )),
    }
}

fn run_upsert_command(client: &WardrobeClient, parts: &[String]) -> io::Result<()> {
    if parts.len() < 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{} requires a drawer name and JSON payload", parts[0]),
        ));
    }
    let payload = parse_json_arg(&parts[2], "payload")?;
    let pointer = client.upsert(&parts[1], payload).map_err(client_error)?;
    println!("{pointer}");
    Ok(())
}

fn run_count_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "count requires a drawer path",
        ));
    }

    let filter = if parts.len() >= 3 {
        Some(parse_json_arg(&parts[2], "count filter")?)
    } else {
        None
    };
    let count = client
        .count(&parts[1], filter, None)
        .map_err(client_error)?;
    print_json(&count, pretty)
}

fn run_records_command(
    client: &WardrobeClient,
    parts: &[String],
    pretty: bool,
    require_filter: bool,
) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{} requires a drawer path", parts[0]),
        ));
    }
    if require_filter && parts.len() < 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{} requires a drawer path and JSON filter", parts[0]),
        ));
    }

    let mut records = if parts.len() >= 3 {
        let filter = parse_json_arg(&parts[2], "query filter")?;
        client
            .find_by_filter(&parts[1], filter, None)
            .map_err(client_error)?
    } else {
        client.find_all(&parts[1]).map_err(client_error)?
    };
    pub_normalize_record_ids(&mut records);
    print_json(&records, pretty)
}

#[derive(serde::Serialize)]
struct DrawerInspectionMetrics {
    path: String,
    data_bytes: u64,
    index_bytes: u64,
    meta_bytes: u64,
    total_bytes: u64,
    record_count: usize,
    register_file_count: usize,
    tombstone_fragmentation_percent: Option<f64>,
}

fn run_inspect_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "inspect requires a drawer path",
        ));
    }
    if !client.requires_embedded_engine() {
        eprintln!("inspect is only available for embedded connections");
        return Ok(());
    }

    let data_dir = embedded_data_dir(client);
    let target = resolve_inspect_target(&data_dir, &parts[1..])?;
    let metrics = inspect_drawer_metrics(client, &target)?;
    print_json(&metrics, pretty)
}

fn inspect_drawer_metrics(
    client: &WardrobeClient,
    target: &InspectTarget,
) -> io::Result<DrawerInspectionMetrics> {
    let files = drawer_files(&target.data_dir, &target.drawer_name);
    let data_bytes = file_size_or_zero(&files.data)?;
    let index_bytes = file_size_or_zero(&files.index)?;
    let meta_bytes = file_size_or_zero(&files.meta)?;
    let total_bytes = data_bytes
        .saturating_add(index_bytes)
        .saturating_add(meta_bytes);
    let register_file_count = [&files.data, &files.index, &files.meta]
        .iter()
        .filter(|path| path.is_file())
        .count();
    let record_count = client
        .count(&target.label, None, None)
        .map_err(client_error)?;

    Ok(DrawerInspectionMetrics {
        path: target.label.clone(),
        data_bytes,
        index_bytes,
        meta_bytes,
        total_bytes,
        record_count,
        register_file_count,
        tombstone_fragmentation_percent: None,
    })
}

fn file_size_or_zero(path: &Path) -> io::Result<u64> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
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

    if parts.len() == 2 {
        let deleted = client.delete_by_id(&parts[1]).map_err(client_error)?;
        println!("deleted: {deleted}");
        return Ok(());
    }

    match parse_delete_target(&parts[2])? {
        DeleteTarget::Id(record_id) => {
            let pointer = pointer_from_record_id(&parts[1], &record_id);
            let deleted = client.delete_by_id(&pointer).map_err(client_error)?;
            println!("deleted: {deleted}");
        }
        DeleteTarget::Filter(filter) => {
            let (matched, deleted) = delete_by_filter(client, &parts[1], filter)?;
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
) -> io::Result<(usize, usize)> {
    let records = client
        .find_by_filter(drawer_name, filter, None)
        .map_err(client_error)?;
    let matched = records.len();
    let mut deleted = 0;

    for record in records {
        let record_id = record_id_for_delete(&record)?;
        let pointer = pointer_from_record_id(drawer_name, &record_id);
        if client.delete_by_id(&pointer).map_err(client_error)? {
            deleted += 1;
        }
    }

    Ok((matched, deleted))
}

fn record_id_for_delete(record: &Value) -> io::Result<String> {
    record
        .get("_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "record matched for deletion did not include a string _id",
            )
        })
}

fn run_schema_management_command(
    client: &WardrobeClient,
    parts: &[String],
    pretty: bool,
) -> io::Result<()> {
    if parts.len() < 4 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "{} requires <type> <path> <target_field> <?extra_args>",
                parts[0]
            ),
        ));
    }

    let action = parts[0].as_str();
    let kind = normalize_schema_command_type(&parts[1])?;
    let drawer_path = normalize_drawer_path(&parts[2], "schema command path")?;
    let field_name = &parts[3];
    let payload = schema_management_payload(action, &kind, field_name, parts)?;
    let response = client
        .manage_schema(&drawer_path, action, &kind, field_name, payload)
        .map_err(client_error)?;
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
            let key_type = parts.get(4).map(String::as_str).unwrap_or("secondary");
            if !matches!(key_type, "primary" | "secondary") {
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
            if action == "add" && parts.len() < 5 {
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
            if action == "remove" && parts.len() < 5 {
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
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown schema command type: {kind}"),
        )),
    }
}

fn run_define_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "define requires database, schema, or drawer",
        ));
    }

    match parts[1].as_str() {
        "database" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "define database requires a database name",
                ));
            }
            let inventory = client.create_database(&parts[2]).map_err(client_error)?;
            print_json(&inventory, pretty)
        }
        "schema" => {
            if parts.len() < 4 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "define schema requires <database> <schema>",
                ));
            }
            let inventory = client
                .create_schema(&parts[2], &parts[3])
                .map_err(client_error)?;
            print_json(&inventory, pretty)
        }
        "drawer" => {
            if parts.len() < 5 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "define drawer requires <database> <schema> <drawer>",
                ));
            }
            let inventory = client
                .create_drawer(&parts[2], &parts[3], &parts[4])
                .map_err(client_error)?;
            print_json(&inventory, pretty)
        }
        "tenant-route" => {
            if parts.len() < 5 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "define tenant-route requires <tenant> <database> <location>",
                ));
            }
            let inventory = client
                .register_tenant_route(&parts[2], &parts[3], &parts[4])
                .map_err(client_error)?;
            print_json(&inventory, pretty)
        }
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown define target: {other}"),
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
            let inventory = client.create_database(&wardrobe).map_err(client_error)?;
            print_json(&inventory, pretty)
        }
        "bay" | "bays" | "schema" | "schemas" => {
            let (wardrobe, bay, _) = parse_structural_path(&parts[2], 2, "bay path")?;
            let inventory = client
                .create_schema(&wardrobe, &bay)
                .map_err(client_error)?;
            print_json(&inventory, pretty)
        }
        "drawer" | "drawers" => {
            let (wardrobe, bay, drawer) = parse_structural_path(&parts[2], 3, "drawer path")?;
            let inventory = client
                .create_drawer(&wardrobe, &bay, &drawer)
                .map_err(client_error)?;
            print_json(&inventory, pretty)
        }
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown create target: {other}"),
        )),
    }
}

fn run_show_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "{} requires tenants, databases, schemas, or drawers",
                parts[0]
            ),
        ));
    }

    match parts[1].as_str() {
        "tenant" | "tenants" => {
            let tenants = client.show_tenants().map_err(client_error)?;
            print_json(&tenants, pretty)
        }
        "wardrobe" | "wardrobes" | "database" | "databases" => {
            let dbs = client.show_databases().map_err(client_error)?;
            print_json(&dbs, pretty)
        }
        "bay" | "bays" | "schema" | "schemas" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("{} bays requires a wardrobe path", parts[0]),
                ));
            }
            let (wardrobe, _, _) = parse_structural_path(&parts[2], 1, "wardrobe path")?;
            let schemas = client.show_schemas(&wardrobe).map_err(client_error)?;
            print_json(&schemas, pretty)
        }
        "drawer" | "drawers" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("{} drawers requires a bay path", parts[0]),
                ));
            }
            let (wardrobe, bay) = if parts.len() >= 4 && !has_path_separator(&parts[2]) {
                (parts[2].clone(), parts[3].clone())
            } else {
                let (wardrobe, bay, _) = parse_structural_path(&parts[2], 2, "bay path")?;
                (wardrobe, bay)
            };
            let drawers = client.show_drawers(&wardrobe, &bay).map_err(client_error)?;
            print_json(&drawers, pretty)
        }
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown show target: {other}"),
        )),
    }
}

fn run_check_command(client: &WardrobeClient, parts: &[String]) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(ErrorKind::InvalidInput, "check requires <path>"));
    }
    if !client.requires_embedded_engine() {
        eprintln!("check is only available for embedded connections");
        return Ok(());
    }

    let data_dir = embedded_data_dir(client);
    let segments = split_structural_path(&parts[1], "check path")?;
    let logical_path = segments.join("/");

    println!("Path: {logical_path}");
    match segments.len() {
        1 => {
            println!("Type: wardrobe");
            print_path_status("directory", &data_dir.join(&segments[0]))?;
        }
        2 => {
            println!("Type: bay");
            print_path_status("directory", &data_dir.join(&segments[0]).join(&segments[1]))?;
        }
        3 => {
            println!("Type: drawer");
            let files = drawer_files(&data_dir, &logical_path);
            print_file_status("data", &files.data)?;
            print_file_status("index", &files.index)?;
            print_file_status("meta", &files.meta)?;
        }
        _ => {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "check path must identify a wardrobe, bay, or drawer",
            ));
        }
    }

    Ok(())
}

#[derive(serde::Serialize)]
struct CleanResult {
    path: String,
    report: VacuumReport,
}

fn run_clean_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
    if parts.len() < 2 {
        return Err(Error::new(ErrorKind::InvalidInput, "clean requires <path>"));
    }

    let targets = clean_targets(client, &parts[1])?;
    let mut results = Vec::new();
    for path in targets {
        let report = client.vacuum_drawer(&path).map_err(client_error)?;
        results.push(CleanResult { path, report });
    }
    print_json(&results, pretty)
}

fn clean_targets(client: &WardrobeClient, raw_path: &str) -> io::Result<Vec<String>> {
    let segments = split_structural_path(raw_path, "clean path")?;
    match segments.len() {
        1 => {
            let wardrobe = &segments[0];
            let mut targets = Vec::new();
            for bay in client.show_schemas(wardrobe).map_err(client_error)? {
                for drawer in client.show_drawers(wardrobe, &bay).map_err(client_error)? {
                    targets.push(format!("{wardrobe}/{bay}/{}", drawer.name));
                }
            }
            Ok(targets)
        }
        2 => {
            let wardrobe = &segments[0];
            let bay = &segments[1];
            client
                .show_drawers(wardrobe, bay)
                .map_err(client_error)
                .map(|drawers| {
                    drawers
                        .into_iter()
                        .map(|drawer| format!("{wardrobe}/{bay}/{}", drawer.name))
                        .collect()
                })
        }
        3 => Ok(vec![segments.join("/")]),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            "clean path must identify a wardrobe, bay, or drawer",
        )),
    }
}

const BACKUP_ARCHIVE_FORMAT: &str = "wardrobe-cli-backup-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackupScope {
    Wardrobe,
    Bay,
    Drawer,
}

impl BackupScope {
    fn from_segment_count(segment_count: usize, label: &str) -> io::Result<Self> {
        match segment_count {
            1 => Ok(Self::Wardrobe),
            2 => Ok(Self::Bay),
            3 => Ok(Self::Drawer),
            _ => Err(Error::new(
                ErrorKind::InvalidInput,
                format!("{label} must identify a wardrobe, bay, or drawer"),
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Wardrobe => "wardrobe",
            Self::Bay => "bay",
            Self::Drawer => "drawer",
        }
    }

    fn expected_segments(self) -> usize {
        match self {
            Self::Wardrobe => 1,
            Self::Bay => 2,
            Self::Drawer => 3,
        }
    }
}

#[derive(Debug)]
struct StructuralBackupTarget {
    scope: BackupScope,
    segments: Vec<String>,
    logical_path: String,
    storage_path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct BackupArchive {
    format: String,
    source_path: String,
    scope: String,
    files: Vec<BackupArchiveFile>,
}

#[derive(Serialize, Deserialize)]
struct BackupArchiveFile {
    path: String,
    bytes_hex: String,
}

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

    let data_dir = embedded_data_dir_for_recovery(client, "backup")?;
    let target = structural_backup_target(&data_dir, &parts[1], "backup source path")?;
    let files = collect_backup_archive_files(&target)?;
    let byte_count = files
        .iter()
        .map(|file| file.bytes_hex.len() / 2)
        .sum::<usize>();
    let archive = BackupArchive {
        format: BACKUP_ARCHIVE_FORMAT.to_string(),
        source_path: target.logical_path.clone(),
        scope: target.scope.as_str().to_string(),
        files,
    };

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
            source_path: target.logical_path,
            destination_archive_path: archive_path.display().to_string(),
            scope: target.scope.as_str().to_string(),
            file_count: archive.files.len(),
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

    let data_dir = embedded_data_dir_for_recovery(client, "restore")?;
    let target = structural_backup_target(&data_dir, &parts[1], "restore destination path")?;
    let archive_path = PathBuf::from(&parts[2]);
    let archive = read_backup_archive(&archive_path)?;
    validate_archive_scope(&archive, &target)?;
    let decoded_files = decoded_restore_files(&archive, &target)?;
    let byte_count = decoded_files
        .iter()
        .map(|(_, bytes)| bytes.len())
        .sum::<usize>();

    clear_restore_target(&data_dir, &target)?;
    for (relative_path, bytes) in &decoded_files {
        let destination = target.storage_path.join(relative_path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(destination, bytes)?;
    }
    register_restored_catalog(client, &target)?;

    print_json(
        &RestoreCommandResult {
            destination_path: target.logical_path,
            source_archive_path: archive_path.display().to_string(),
            scope: target.scope.as_str().to_string(),
            file_count: decoded_files.len(),
            byte_count,
        },
        pretty,
    )
}

fn embedded_data_dir_for_recovery(client: &WardrobeClient, command: &str) -> io::Result<PathBuf> {
    if !client.requires_embedded_engine() {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("{command} is only available for embedded connections"),
        ));
    }

    Ok(embedded_data_dir(client))
}

fn structural_backup_target(
    data_dir: &Path,
    raw_path: &str,
    label: &str,
) -> io::Result<StructuralBackupTarget> {
    let segments = split_structural_path(raw_path, label)?;
    let scope = BackupScope::from_segment_count(segments.len(), label)?;
    let storage_path = match scope {
        BackupScope::Wardrobe | BackupScope::Bay => segments
            .iter()
            .fold(data_dir.to_path_buf(), |path, segment| path.join(segment)),
        BackupScope::Drawer => data_dir.join(&segments[0]).join(&segments[1]),
    };

    Ok(StructuralBackupTarget {
        scope,
        logical_path: segments.join("/"),
        segments,
        storage_path,
    })
}

fn collect_backup_archive_files(
    target: &StructuralBackupTarget,
) -> io::Result<Vec<BackupArchiveFile>> {
    match target.scope {
        BackupScope::Wardrobe | BackupScope::Bay => {
            if !target.storage_path.is_dir() {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!(
                        "{} '{}' does not exist",
                        target.scope.as_str(),
                        target.logical_path
                    ),
                ));
            }

            let mut files = Vec::new();
            collect_directory_archive_files(
                &target.storage_path,
                &target.storage_path,
                &mut files,
            )?;
            Ok(files)
        }
        BackupScope::Drawer => {
            let drawer_name = target.segments.last().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "drawer backup requires a drawer path",
                )
            })?;
            let drawer_files = drawer_files(&target.storage_path, drawer_name);
            let mut files = Vec::new();
            for path in [&drawer_files.data, &drawer_files.index, &drawer_files.meta] {
                if path.is_file() {
                    let relative_path = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or_else(|| {
                            Error::new(
                                ErrorKind::InvalidData,
                                format!("Invalid drawer file name: {}", path.display()),
                            )
                        })?
                        .to_string();
                    files.push(BackupArchiveFile {
                        path: relative_path,
                        bytes_hex: encode_hex(&fs::read(path)?),
                    });
                }
            }

            if files.is_empty() {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!(
                        "drawer '{}' does not have any storage files",
                        target.logical_path
                    ),
                ));
            }

            Ok(files)
        }
    }
}

fn collect_directory_archive_files(
    base_dir: &Path,
    current_dir: &Path,
    files: &mut Vec<BackupArchiveFile>,
) -> io::Result<()> {
    let mut entries = fs::read_dir(current_dir)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();

    for path in entries {
        if path.is_dir() {
            collect_directory_archive_files(base_dir, &path, files)?;
        } else if path.is_file() {
            let relative_path = path.strip_prefix(base_dir).map_err(|error| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Failed to compute backup relative path: {error}"),
                )
            })?;
            files.push(BackupArchiveFile {
                path: archive_path_string(relative_path)?,
                bytes_hex: encode_hex(&fs::read(&path)?),
            });
        }
    }

    Ok(())
}

fn read_backup_archive(path: &Path) -> io::Result<BackupArchive> {
    let bytes = fs::read(path)?;
    let archive = serde_json::from_slice::<BackupArchive>(&bytes).map_err(|error| {
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

fn validate_archive_scope(
    archive: &BackupArchive,
    target: &StructuralBackupTarget,
) -> io::Result<()> {
    if archive.scope != target.scope.as_str() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "restore destination '{}' is a {}, but archive contains a {} backup",
                target.logical_path,
                target.scope.as_str(),
                archive.scope
            ),
        ));
    }

    if target.segments.len() != target.scope.expected_segments() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "restore destination path does not match archive scope",
        ));
    }

    Ok(())
}

fn decoded_restore_files(
    archive: &BackupArchive,
    target: &StructuralBackupTarget,
) -> io::Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut files = Vec::new();
    for file in &archive.files {
        let relative_path = restore_relative_path(archive, target, &file.path)?;
        let bytes = decode_hex(&file.bytes_hex)?;
        files.push((relative_path, bytes));
    }

    Ok(files)
}

fn restore_relative_path(
    archive: &BackupArchive,
    target: &StructuralBackupTarget,
    raw_path: &str,
) -> io::Result<PathBuf> {
    let relative_path = validate_archive_relative_path(raw_path)?;
    if target.scope != BackupScope::Drawer {
        return Ok(relative_path);
    }

    let source_drawer = archive.source_path.split('/').next_back().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "drawer archive is missing source path",
        )
    })?;
    let destination_drawer = target.segments.last().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "drawer restore requires a destination drawer path",
        )
    })?;
    let file_name = relative_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Invalid drawer archive file path: {raw_path}"),
            )
        })?;

    if relative_path.components().count() != 1 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "drawer backup archive cannot contain nested file paths",
        ));
    }

    let mapped_name = if file_name == format!("{source_drawer}.drw") {
        format!("{destination_drawer}.drw")
    } else if file_name == format!("{source_drawer}_index.drw") {
        format!("{destination_drawer}_index.drw")
    } else if file_name == format!("{source_drawer}_meta.drw") {
        format!("{destination_drawer}_meta.drw")
    } else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("Unexpected drawer archive file: {file_name}"),
        ));
    };

    Ok(PathBuf::from(mapped_name))
}

fn clear_restore_target(data_dir: &Path, target: &StructuralBackupTarget) -> io::Result<()> {
    match target.scope {
        BackupScope::Wardrobe | BackupScope::Bay => {
            ensure_restore_path_is_under_data_dir(data_dir, &target.storage_path)?;
            if target.storage_path.exists() {
                fs::remove_dir_all(&target.storage_path)?;
            }
        }
        BackupScope::Drawer => {
            let drawer_name = target.segments.last().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "drawer restore requires a drawer path",
                )
            })?;
            ensure_restore_path_is_under_data_dir(data_dir, &target.storage_path)?;
            let files = drawer_files(&target.storage_path, drawer_name);
            for path in [&files.data, &files.index, &files.meta] {
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
        }
    }

    Ok(())
}

fn register_restored_catalog(
    client: &WardrobeClient,
    target: &StructuralBackupTarget,
) -> io::Result<()> {
    let wardrobe = &target.segments[0];
    client.create_database(wardrobe).map_err(client_error)?;

    match target.scope {
        BackupScope::Wardrobe => {
            for bay in restored_bay_names(&target.storage_path)? {
                client.create_schema(wardrobe, &bay).map_err(client_error)?;
                let bay_path = target.storage_path.join(&bay);
                for drawer in restored_drawer_names(&bay_path)? {
                    client
                        .create_drawer(wardrobe, &bay, &drawer)
                        .map_err(client_error)?;
                }
            }
        }
        BackupScope::Bay => {
            let bay = &target.segments[1];
            client.create_schema(wardrobe, bay).map_err(client_error)?;
            for drawer in restored_drawer_names(&target.storage_path)? {
                client
                    .create_drawer(wardrobe, bay, &drawer)
                    .map_err(client_error)?;
            }
        }
        BackupScope::Drawer => {
            let bay = &target.segments[1];
            let drawer = &target.segments[2];
            client.create_schema(wardrobe, bay).map_err(client_error)?;
            client
                .create_drawer(wardrobe, bay, drawer)
                .map_err(client_error)?;
        }
    }

    Ok(())
}

fn restored_bay_names(wardrobe_path: &Path) -> io::Result<Vec<String>> {
    if !wardrobe_path.exists() {
        return Ok(Vec::new());
    }

    let mut names = fs::read_dir(wardrobe_path)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(ToOwned::to_owned)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    names.sort();
    Ok(names)
}

fn restored_drawer_names(bay_path: &Path) -> io::Result<Vec<String>> {
    if !bay_path.exists() {
        return Ok(Vec::new());
    }

    let mut names = fs::read_dir(bay_path)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("drw") {
                return None;
            }
            let stem = path.file_stem().and_then(|stem| stem.to_str())?;
            if stem.ends_with("_index") || stem.ends_with("_meta") {
                return None;
            }
            Some(stem.to_string())
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    Ok(names)
}

fn archive_path_string(path: &Path) -> io::Result<String> {
    let mut segments = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(value) => {
                let Some(segment) = value.to_str() else {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("Archive path is not valid UTF-8: {}", path.display()),
                    ));
                };
                segments.push(segment.to_string());
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!("Invalid archive relative path: {}", path.display()),
                ));
            }
        }
    }

    Ok(segments.join("/"))
}

fn validate_archive_relative_path(raw_path: &str) -> io::Result<PathBuf> {
    if raw_path.trim().is_empty() || raw_path.contains('\\') || Path::new(raw_path).is_absolute() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("Invalid archive relative path: {raw_path}"),
        ));
    }

    let mut path = PathBuf::new();
    for segment in raw_path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Invalid archive relative path segment: {segment}"),
            ));
        }
        path.push(segment);
    }

    Ok(path)
}

fn ensure_restore_path_is_under_data_dir(data_dir: &Path, target: &Path) -> io::Result<()> {
    let data_dir = absolute_lexical_path(data_dir)?;
    let target = absolute_lexical_path(target)?;
    if target == data_dir || !target.starts_with(&data_dir) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "Refusing to restore outside the embedded storage root: {}",
                target.display()
            ),
        ));
    }

    Ok(())
}

fn absolute_lexical_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_hex(raw: &str) -> io::Result<Vec<u8>> {
    if raw.len() % 2 != 0 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Invalid backup archive hex payload length",
        ));
    }

    let mut bytes = Vec::with_capacity(raw.len() / 2);
    let raw_bytes = raw.as_bytes();
    for pair in raw_bytes.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> io::Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            "Invalid backup archive hex payload",
        )),
    }
}

fn run_manage_user_command(
    client: &WardrobeClient,
    parts: &[String],
    pretty: bool,
) -> io::Result<()> {
    if parts.len() < 2 || parts[1] != "user" {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "manage requires user <action> <json>",
        ));
    }
    if parts.len() < 4 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "manage user requires <action> <json>",
        ));
    }

    let payload = parse_json_arg(&parts[3], "user admin payload")?;
    let response = client
        .manage_user(&parts[2], payload)
        .map_err(client_error)?;
    print_json(&response, pretty)
}

fn run_manage_user_alias(
    client: &WardrobeClient,
    parts: &[String],
    pretty: bool,
) -> io::Result<()> {
    let (action_index, payload_index) = if parts.get(1).map(String::as_str) == Some("user") {
        (2, 3)
    } else {
        (1, 2)
    };

    if parts.len() <= payload_index {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{} requires <action> <json>", parts[0]),
        ));
    }

    let payload = parse_json_arg(&parts[payload_index], "user admin payload")?;
    let response = client
        .manage_user(&parts[action_index], payload)
        .map_err(client_error)?;
    print_json(&response, pretty)
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

fn embedded_data_dir(client: &WardrobeClient) -> PathBuf {
    match client.connection_target() {
        ConnectionTarget::EmbeddedPath(p) => p.clone(),
        _ => PathBuf::from("./wardrobe"),
    }
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

fn print_path_status(label: &str, path: &Path) -> io::Result<()> {
    if path.is_dir() {
        println!("{label}: present");
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
}
