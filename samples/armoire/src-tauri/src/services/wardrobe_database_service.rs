use std::io;
use wardrobe_core::{WardrobeClient, WardrobeEngine};

pub struct WardrobeDatabaseService;

impl WardrobeDatabaseService {
    pub fn test_connection(database_directory: &str) -> io::Result<()> {
        println!("Testing Wardrobe database at: {}", database_directory);

        let engine = WardrobeEngine::open(database_directory)?;
        let client = WardrobeClient::open(database_directory)?;

        let databases = engine.list_databases()?;
        println!("Wardrobe databases:");

        for database in databases {
            println!(" - {}", database.name);

            let schemas = engine.list_schemas(&database.name)?;
            for schema_name in schemas {
                println!("   schema: {}", schema_name);

                let drawers = engine.show_drawers(&database.name, &schema_name)?;
                for drawer in drawers {
                    println!(
                        "     drawer: {} ({} records)",
                        drawer.name, drawer.record_count
                    );
                }
            }
        }

        let available_databases = client.show_databases()?;
        println!("Client can see {} database(s).", available_databases.len());

        Ok(())
    }
}
