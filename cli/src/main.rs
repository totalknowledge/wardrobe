use std::collections::HashMap;
use std::fs;
use std::io::{self, Error, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use wardrobe_core::{Database, WardrobeClient, ConnectionTarget};
use atty::Stream;
use serde_json::Value;

#[derive(Debug)]
struct CliConfig {
    connection: String,
    pretty: bool,
    command_parts: Vec<String>,
}

impl CliConfig {
    fn from_args<I>(args: I) -> io::Result<Self>
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
                        Error::new(ErrorKind::InvalidInput, "--target/--data-dir requires a connection string or path")
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

fn print_help() {
    println!("wardrobe-cli");
    println!("  --target <connection>     Wardrobe connection string (defaults to ./wardrobe)");
    println!("  --pretty                  Pretty-print JSON output (default: compact)");
    println!("  drawers                   Show known drawers (embedded only)");
    println!("  diagnose                  Run structural diagnostics (embedded only)");
    println!("  inspect <drawer>          Inspect drawer companion files (embedded only)");
    println!("  records <drawer>          Print hydrated records for a drawer");
    println!("  show-databases            List discovered databases (network+embedded)");
    println!("  show-schemas <database>   List schemas for a database");
}

fn main() -> io::Result<()> {
    let config = CliConfig::from_args(std::env::args().skip(1))?;

    let client = WardrobeClient::open(&config.connection)
        .map_err(|e| Error::new(ErrorKind::Other, format!("Failed to open connection: {e}")))?;

    // If stdin is piped, read a single command from it and execute immediately
    let stdin_is_tty = atty::is(Stream::Stdin);
    if !stdin_is_tty {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        let buffer = buffer.trim();
        if !buffer.is_empty() {
            let parts = shell_split(buffer);
            return run_command(&client, &parts, config.pretty);
        }
    }

    // If command arguments were provided, execute once and exit
    if !config.command_parts.is_empty() {
        return run_command(&client, &config.command_parts, config.pretty);
    }

    // Otherwise, enter interactive REPL
    repl(&client, config.pretty)
}

fn shell_split(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .map(|s| s.to_string())
        .collect()
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
            // EOF
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

fn run_command(client: &WardrobeClient, parts: &[String], pretty: bool) -> io::Result<()> {
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
                eprintln!("drawers command is only supported for embedded connections; use show-databases for network targets");
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
                return Err(Error::new(ErrorKind::InvalidInput, "inspect requires a drawer name"));
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
                return Err(Error::new(ErrorKind::InvalidInput, "records requires a drawer name"));
            }
            let mut records = client
                .find_all(&parts[1])
                .map_err(|e| Error::new(ErrorKind::Other, format!("client error: {e}")))?;
            // debug raw
            if let Ok(raw) = serde_json::to_string(&records) {
                eprintln!("DEBUG-RAW-RECORDS: {raw}");
            }
            normalize_record_ids(&mut records);
            println!("{}", serde_json::to_string_pretty(&records).map_err(|e| Error::new(ErrorKind::InvalidData, format!("JSON serialization error: {e}")))?);
            Ok(())
        }
        "upsert" => {
            if parts.len() < 3 {
                return Err(Error::new(ErrorKind::InvalidInput, "upsert requires a drawer name and JSON payload"));
            }
            let payload = serde_json::from_str::<Value>(&parts[2]).map_err(|e| Error::new(ErrorKind::InvalidInput, format!("invalid JSON payload: {e}")))?;
            let pointer = client
                .upsert(&parts[1], payload)
                .map_err(|e| Error::new(ErrorKind::Other, format!("client error: {e}")))?;
            println!("{pointer}");
            Ok(())
        }
        "delete-by-id" => {
            if parts.len() < 2 {
                return Err(Error::new(ErrorKind::InvalidInput, "delete-by-id requires a pointer"));
            }
            let deleted = client
                .delete_by_id(&parts[1])
                .map_err(|e| Error::new(ErrorKind::Other, format!("client error: {e}")))?;
            println!("deleted: {deleted}");
            Ok(())
        }
        "show-databases" => {
            let dbs = client
                .show_databases()
                .map_err(|e| Error::new(ErrorKind::Other, format!("client error: {e}")))?;
            print_json(&dbs, pretty)
        }
        "show-schemas" => {
            if parts.len() < 2 {
                return Err(Error::new(ErrorKind::InvalidInput, "show-schemas requires a database name"));
            }
            let schemas = client
                .show_schemas(&parts[1])
                .map_err(|e| Error::new(ErrorKind::Other, format!("client error: {e}")))?;
            print_json(&schemas, pretty)
        }
        "show-drawers" => {
            if parts.len() < 3 {
                return Err(Error::new(ErrorKind::InvalidInput, "show-drawers requires <database> <schema>"));
            }
            let drawers = client
                .show_drawers(&parts[1], &parts[2])
                .map_err(|e| Error::new(ErrorKind::Other, format!("client error: {e}")))?;
            print_json(&drawers, pretty)
        }
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unknown command: {}", parts[0]),
        )),
    }
}

fn print_json<T: serde::Serialize>(v: &T, pretty: bool) -> io::Result<()> {
    let out = if pretty {
        serde_json::to_string_pretty(v).map_err(|e| Error::new(ErrorKind::InvalidData, format!("JSON serialization error: {e}")))?
    } else {
        serde_json::to_string(v).map_err(|e| Error::new(ErrorKind::InvalidData, format!("JSON serialization error: {e}")))?
    };
    println!("{out}");
    Ok(())
}

fn load_drawer_names(data_dir: &Path) -> io::Result<Vec<String>> {
    let data_dir = data_dir.to_string_lossy().to_string();
    let mut database = Database::initialize(&data_dir)?;
    database.load_existing_drawers("_id", HashMap::new())?;
    let mut drawer_names = database.get_all_drawers().into_keys().collect::<Vec<_>>();
    drawer_names.sort();
    Ok(drawer_names)
}

fn normalize_record_ids(records: &mut Vec<Value>) {
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

fn inspect_drawer(data_dir: &Path, drawer_name: &str) -> io::Result<()> {
    let files = drawer_files(data_dir, drawer_name);

    println!("Drawer: {drawer_name}");
    print_file_status("data", &files.data)?;
    print_file_status("index", &files.index)?;
    print_file_status("meta", &files.meta)?;

    Ok(())
}

fn diagnose(data_dir: &Path) -> io::Result<()> {
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
