use serde_json::Value;
use std::fs;
use std::io;
use std::path::PathBuf;
use wardrobe_core::WardrobeEngine;

fn seed_file_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("basic-usage")
        .join("tests")
        .join("common")
        .join("test_seed.json")
}

fn seed_database(engine: &WardrobeEngine) -> io::Result<()> {
    let seed_contents = fs::read_to_string(seed_file_path())?;
    let seed_value: Value = serde_json::from_str(&seed_contents)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    let seed_map = seed_value.as_object().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Seed file root must be a JSON object mapping drawer names to arrays",
        )
    })?;

    for (drawer_name, records) in seed_map {
        let record_array = records.as_array().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Seed drawer '{drawer_name}' must contain an array of records"),
            )
        })?;

        for record in record_array {
            engine.upsert(drawer_name, record.clone())?;
        }
    }

    Ok(())
}

fn print_drawer(engine: &WardrobeEngine, drawer_name: &str, label: &str) {
    println!("--- [ Drawer: {label} (.drw) ] ---");
    match engine.find_all(drawer_name) {
        Ok(records) if !records.is_empty() => {
            for record in records {
                println!("{record}");
            }
        }
        Ok(_) => println!("({label} drawer is currently empty)"),
        Err(error) => println!("Could not read {drawer_name} drawer: {error}"),
    }
    println!();
}

fn main() -> io::Result<()> {
    let database_directory = "./wardrobe";
    let engine = WardrobeEngine::open(database_directory)?;

    if let Err(error) = seed_database(&engine) {
        println!("Failed to seed database from test_seed.json: {error}");
    }

    println!("=========================================");
    println!("        WARDROBE DATABASE STORAGE        ");
    println!("================================*********\n");

    print_drawer(&engine, "gem", "Gems");
    print_drawer(&engine, "weapon", "Weapons");
    print_drawer(&engine, "character", "Characters");

    println!("=========================================");

    Ok(())
}
