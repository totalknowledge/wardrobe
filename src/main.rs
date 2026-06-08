#[path = "wrdb_lib/mod.rs"]
pub mod wrdb_lib;

pub mod engine;

pub use engine::WardrobeEngine;
pub use wrdb_lib::database::Database;
pub use wrdb_lib::drawer::Drawer;
pub use wrdb_lib::reader::DatabaseReader;
pub use wrdb_lib::recycler::Recycler;
pub use wrdb_lib::writer::DatabaseWriter;

use serde_json::json;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() -> io::Result<()> {
    let database_directory = "./wardrobe";
    let mut engine = WardrobeEngine::new(database_directory)?;

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let random_potency = 5000 + (nanos % 5000) as u64;

    let gem_payload = json!({
        "_id": "@gem:lnk_ab7f2d90ad074e05987817bce6f941c3",
        "element": "Fire",
        "potency": random_potency
    });

    if let Err(error) = engine.upsert("gem", gem_payload) {
        println!("Failed to run startup upsert: {}", error);
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

