use serde_json::Value;
use std::fs;
use std::io;
use std::path::PathBuf;
use wardrobe_core::WardrobeEngine;

fn seed_file_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("common")
        .join("test_seed.json")
}

fn seed_database(engine: &mut WardrobeEngine) -> io::Result<()> {
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

fn main() -> io::Result<()> {
    let database_directory = "./wardrobe";
    let mut engine = WardrobeEngine::new(database_directory)?;

    if let Err(error) = seed_database(&mut engine) {
        println!("Failed to seed database from test_seed.json: {}", error);
    }

    println!("=========================================");
    println!("        WARDROBE DATABASE STORAGE        ");
    println!("================================*********\n");

    println!("--- [ Drawer: Gems (.drw) ] ---");
    match engine.find_all("gem") {
        Ok(gems) if !gems.is_empty() => {
            for gem in gems {
                println!("{}", gem);
            }
        }
        Ok(_) => println!("(Gems drawer is currently empty)"),
        Err(error) => println!("Could not read gems drawer: {}", error),
    }
    println!();

    println!("--- [ Drawer: Weapons (.drw) ] ---");
    match engine.find_all("weapon") {
        Ok(weapons) if !weapons.is_empty() => {
            for weapon in weapons {
                println!("{}", weapon);
            }
        }
        Ok(_) => println!("(Weapons drawer is currently empty)"),
        Err(error) => println!("Could not read weapons drawer: {}", error),
    }
    println!();

    println!("--- [ Drawer: Characters (.drw) ] ---");
    match engine.find_all("character") {
        Ok(characters) if !characters.is_empty() => {
            for character in characters {
                println!("{}", character);
            }
        }
        Ok(_) => println!("(Characters drawer is currently empty)"),
        Err(error) => println!("Could not read characters drawer: {}", error),
    }
    println!("\n=========================================");

    Ok(())
}
