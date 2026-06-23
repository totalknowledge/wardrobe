use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Error, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use wardrobe_core::{ConnectionTarget, Database, WardrobeClient};

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

pub fn print_help() {
    println!("wardrobe-cli");
    println!("  --target <connection>     Wardrobe connection string (defaults to ./wardrobe)");
    println!("  --pretty                  Pretty-print JSON output (default: compact)");
    println!("  drawers                   Show known drawers (embedded only)");
    println!("  diagnose                  Run structural diagnostics (embedded only)");
    println!("  inspect <drawer>          Inspect drawer companion files (embedded only)");
    println!("  records <drawer>          Print hydrated records for a drawer");
    println!("  upsert <drawer> <json>    Insert or update a record (aliases: insert, create)");
    println!("  find <drawer> <json>      Query records with a JSON filter (aliases: get, query)");
    println!("  delete <drawer> <json>    Delete a record by JSON _id (alias: remove)");
    println!("  define database <name>    Create/register a database (alias: create-db)");
    println!("  define schema <db> <name> Create/register a schema (alias: create-schema)");
    println!(
        "  define drawer <db> <schema> <name> Create/register a drawer (alias: create-drawer)"
    );
    println!("  manage user <action> <json> Send a user admin request to a remote server");
    println!(
        "  show <type>               List tenants/databases/schemas/drawers (aliases: ls, list)"
    );
    println!("  show-databases            List discovered databases (network+embedded)");
    println!("  show-schemas <database>   List schemas for a database");
    println!("  show-drawers <db> <schema> List drawers for a schema");
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
        "inspect" => {
            if parts.len() < 2 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "inspect requires a drawer name",
                ));
            }
            if client.requires_embedded_engine() {
                let data_dir = match client.connection_target() {
                    ConnectionTarget::EmbeddedPath(p) => p.clone(),
                    _ => PathBuf::from("./wardrobe"),
                };
                inspect_drawer(&data_dir, &parts[1])
            } else {
                eprintln!("inspect is only available for embedded connections");
                Ok(())
            }
        }
        "records" => {
            if parts.len() < 2 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "records requires a drawer name",
                ));
            }
            let mut records = client
                .find_all(&parts[1])
                .map_err(|e| Error::new(ErrorKind::Other, format!("client error: {e}")))?;
            if let Ok(raw) = serde_json::to_string(&records) {
                eprintln!("DEBUG-RAW-RECORDS: {raw}");
            }
            pub_normalize_record_ids(&mut records);
            println!(
                "{}",
                serde_json::to_string_pretty(&records).map_err(|e| Error::new(
                    ErrorKind::InvalidData,
                    format!("JSON serialization error: {e}")
                ))?
            );
            Ok(())
        }
        "find" | "get" | "query" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("{} requires a drawer name and JSON filter", parts[0]),
                ));
            }
            let filter = parse_json_arg(&parts[2], "query filter")?;
            let mut records = client
                .find_by_filter(&parts[1], filter, None)
                .map_err(client_error)?;
            pub_normalize_record_ids(&mut records);
            print_json(&records, pretty)
        }
        "upsert" | "insert" | "create" => {
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
        "delete" | "remove" => {
            if parts.len() < 2 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("{} requires a pointer or <drawer> <json>", parts[0]),
                ));
            }
            let pointer = if parts.len() == 2 {
                parts[1].clone()
            } else {
                pointer_from_delete_payload(
                    &parts[1],
                    &parse_json_arg(&parts[2], "delete payload")?,
                )?
            };
            let deleted = client.delete_by_id(&pointer).map_err(client_error)?;
            println!("deleted: {deleted}");
            Ok(())
        }
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
        "database" | "databases" => {
            let dbs = client.show_databases().map_err(client_error)?;
            print_json(&dbs, pretty)
        }
        "schema" | "schemas" => {
            if parts.len() < 3 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("{} schemas requires a database name", parts[0]),
                ));
            }
            let schemas = client.show_schemas(&parts[2]).map_err(client_error)?;
            print_json(&schemas, pretty)
        }
        "drawer" | "drawers" => {
            if parts.len() < 4 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("{} drawers requires <database> <schema>", parts[0]),
                ));
            }
            let drawers = client
                .show_drawers(&parts[2], &parts[3])
                .map_err(client_error)?;
            print_json(&drawers, pretty)
        }
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown show target: {other}"),
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
    if parts.len() < 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{} requires <action> <json>", parts[0]),
        ));
    }

    let payload = parse_json_arg(&parts[2], "user admin payload")?;
    let response = client
        .manage_user(&parts[1], payload)
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

fn pointer_from_delete_payload(drawer_name: &str, payload: &Value) -> io::Result<String> {
    let record_id = payload.get("_id").and_then(Value::as_str).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "delete payload must include a string _id",
        )
    })?;

    if record_id.starts_with('@') {
        Ok(record_id.to_string())
    } else {
        Ok(format!(
            "@{}:{}",
            drawer_name.trim_start_matches('@'),
            record_id.trim_start_matches("lnk_")
        ))
    }
}

fn client_error(error: std::io::Error) -> std::io::Error {
    Error::new(error.kind(), format!("client error: {error}"))
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
    let files = drawer_files(data_dir, drawer_name);
    println!("Drawer: {drawer_name}");
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
