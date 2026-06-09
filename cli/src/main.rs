use std::collections::HashMap;
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};
use wardrobe_core::{Database, WardrobeEngine};

#[derive(Debug, PartialEq)]
enum CliCommand {
    Drawers,
    Inspect { drawer_name: String },
    Diagnose,
    Records { drawer_name: String },
}

#[derive(Debug)]
struct CliConfig {
    data_dir: PathBuf,
    command: CliCommand,
}

impl CliConfig {
    fn from_args<I>(args: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let mut data_dir = PathBuf::from("./wardrobe");
        let mut command_parts = Vec::new();
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--data-dir" => {
                    let path = args.next().ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidInput,
                            "--data-dir requires a directory path",
                        )
                    })?;
                    data_dir = PathBuf::from(path);
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                _ => command_parts.push(arg),
            }
        }

        let command = match command_parts.as_slice() {
            [] => CliCommand::Diagnose,
            [command] if command == "drawers" => CliCommand::Drawers,
            [command] if command == "diagnose" => CliCommand::Diagnose,
            [command, drawer_name] if command == "inspect" => CliCommand::Inspect {
                drawer_name: drawer_name.to_string(),
            },
            [command, drawer_name] if command == "records" => CliCommand::Records {
                drawer_name: drawer_name.to_string(),
            },
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Usage: wardrobe-cli [--data-dir <path>] <drawers|diagnose|inspect <drawer>|records <drawer>>",
                ));
            }
        };

        Ok(Self { data_dir, command })
    }
}

fn print_help() {
    println!("wardrobe-cli");
    println!("  --data-dir <path>          Storage directory for local Wardrobe files");
    println!("  drawers                    Show known drawers");
    println!("  diagnose                   Run structural diagnostics");
    println!("  inspect <drawer>           Inspect drawer companion files");
    println!("  records <drawer>           Print hydrated records for a drawer");
}

fn main() -> io::Result<()> {
    let config = CliConfig::from_args(std::env::args().skip(1))?;

    match config.command {
        CliCommand::Drawers => show_drawers(&config.data_dir),
        CliCommand::Inspect { drawer_name } => inspect_drawer(&config.data_dir, &drawer_name),
        CliCommand::Diagnose => diagnose(&config.data_dir),
        CliCommand::Records { drawer_name } => print_records(&config.data_dir, &drawer_name),
    }
}

fn show_drawers(data_dir: &Path) -> io::Result<()> {
    let drawers = load_drawer_names(data_dir)?;

    if drawers.is_empty() {
        println!("No drawers found.");
        return Ok(());
    }

    for drawer_name in drawers {
        println!("{drawer_name}");
    }

    Ok(())
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

fn print_records(data_dir: &Path, drawer_name: &str) -> io::Result<()> {
    let data_dir = data_dir.to_string_lossy().to_string();
    let mut engine = WardrobeEngine::new(&data_dir)?;
    let records = engine.find_all(drawer_name)?;

    println!(
        "{}",
        serde_json::to_string_pretty(&records).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to format records as JSON: {error}"),
            )
        })?
    );

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
