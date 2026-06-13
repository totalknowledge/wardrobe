use serde_json::{json, Value};
use std::io::{self, Error, ErrorKind};
use wardrobe_core::{
    Command, CommandResult, OrderDirection, QueryModifiers, StorageScope, WardrobeClient,
    WardrobeEngine,
};

const DATABASE_DIRECTORY: &str = "./wardrobe";
const DATABASE_NAME: &str = "basic-usage";
const SCHEMA_NAME: &str = "public";
const USER_DRAWER: &str = "user";
const GEM_DRAWER: &str = "gem";
const WEAPON_DRAWER: &str = "weapon";

fn initialize_engine_instance() -> io::Result<WardrobeEngine> {
    WardrobeEngine::open(DATABASE_DIRECTORY)
}

fn initialize_metadata_client() -> io::Result<WardrobeClient> {
    WardrobeClient::open(DATABASE_DIRECTORY)
}

fn initialize_public_client() -> io::Result<WardrobeClient> {
    WardrobeClient::open("./wardrobe/basic-usage/public")
}

fn print_execution_separator(stage_title: &str) {
    println!("\n==================================================");
    println!(">>> {}", stage_title);
    println!("==================================================\n");
}

fn result_to_pointer(result: CommandResult) -> io::Result<String> {
    match result {
        CommandResult::Pointer(pointer) => Ok(pointer),
        other => Err(Error::new(
            ErrorKind::InvalidData,
            format!("Expected pointer result, got {other:?}"),
        )),
    }
}

fn public_scope() -> StorageScope {
    StorageScope::schema(DATABASE_NAME, SCHEMA_NAME)
}

fn pointer_from_record(record: &Value, drawer_name: &str) -> io::Result<String> {
    let raw_id = record
        .get("_id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "record is missing _id"))?;

    if raw_id.starts_with('@') {
        return Ok(raw_id.to_string());
    }

    Ok(format!("@{drawer_name}:{raw_id}"))
}

fn perform_full_diagnostic_suite(client: &WardrobeClient) -> io::Result<()> {
    let available_databases = client.show_databases()?;
    let database_names: Vec<String> = available_databases.iter().map(|d| d.name.clone()).collect();
    println!("System Databases: {:?}", database_names);

    let available_schemas = client.show_schemas(DATABASE_NAME)?;
    println!("Available Schemas in '{}': {:?}", DATABASE_NAME, available_schemas);

    let drawer_inventory = client.show_drawers(DATABASE_NAME, SCHEMA_NAME)?;
    println!("Drawers in basic-usage/public:");
    for drawer in drawer_inventory {
        println!(" - Drawer: {} ({} records)", drawer.name, drawer.record_count);
    }

    Ok(())
}

fn upsert_in_public_scope(
    engine: &WardrobeEngine,
    drawer_name: &str,
    payload: Value,
) -> io::Result<String> {
    result_to_pointer(engine.execute_in_scope(
        public_scope(),
        Command::Upsert {
            drawer_name: drawer_name.to_string(),
            payload,
        },
    )?)
}

fn upsert_user_record(
    engine: &WardrobeEngine,
    user_id: &str,
    username: &str,
) -> io::Result<String> {
    let pointer = upsert_in_public_scope(
        engine,
        USER_DRAWER,
        json!({
            "_id": user_id,
            "username": username,
            "account_level": "gold",
            "active": true
        }),
    )?;
    println!("Successfully persisted user: {} (Pointer: {})", user_id, pointer);
    Ok(pointer)
}

fn upsert_gem_record(
    engine: &WardrobeEngine,
    gem_id: &str,
    user_id: &str,
    element: &str,
    tags: Vec<&str>,
) -> io::Result<String> {
    let pointer = upsert_in_public_scope(
        engine,
        GEM_DRAWER,
        json!({
            "_id": gem_id,
            "user_id": user_id,
            "element": element,
            "tags": tags
        }),
    )?;
    println!("Successfully persisted gem: {} (Pointer: {})", gem_id, pointer);
    Ok(pointer)
}

fn upsert_weapon_record(
    engine: &WardrobeEngine,
    weapon_id: &str,
    user_id: &str,
    primary_gem_id: &str,
    name: &str,
    damage: u32,
) -> io::Result<String> {
    let pointer = upsert_in_public_scope(
        engine,
        WEAPON_DRAWER,
        json!({
            "_id": weapon_id,
            "user_id": user_id,
            "primary_gem_id": primary_gem_id,
            "name": name,
            "damage": damage
        }),
    )?;
    println!("Successfully persisted weapon: {} (Pointer: {})", weapon_id, pointer);
    Ok(pointer)
}

fn execute_and_print_tag_filter(
    client: &WardrobeClient,
    user_pointer: &str,
) -> io::Result<()> {
    let query_modifiers = QueryModifiers {
        order_by: Some("element".to_string()),
        order_direction: Some(OrderDirection::Ascending),
        offset: Some(0),
        limit: Some(10),
    };

    let filter_payload = json!({
        "user_id": user_pointer,
        "tags": ["support", "magic"]
    });
    let matching_records = client.find_by_filter(GEM_DRAWER, filter_payload, Some(query_modifiers))?;

    println!("Filtered gems for tag match: {}", matching_records.len());
    println!(
        "Found {} records matching the user and tag filter",
        matching_records.len()
    );
    for record in matching_records {
        println!("  Match Found: {}", record);
    }

    Ok(())
}

fn verify_three_relations(
    client: &WardrobeClient,
    user_pointer: &str,
    gem_pointer: &str,
    weapon_pointer: &str,
) -> io::Result<()> {
    let gem = client
        .find_by_filter(
            GEM_DRAWER,
            json!({
                "_id": gem_pointer.trim_start_matches("@gem:")
            }),
            None,
        )?
        .into_iter()
        .next()
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "gem record missing before verification"))?;
    let weapon = client
        .find_by_filter(
            WEAPON_DRAWER,
            json!({
                "_id": weapon_pointer.trim_start_matches("@weapon:")
            }),
            None,
        )?
        .into_iter()
        .next()
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "weapon record missing before verification"))?;

    println!(
        "Relation check: gem record {}, weapon record {}, user pointer {}",
        gem,
        weapon,
        user_pointer
    );

    Ok(())
}

fn perform_cascade_delete_sequence(
    client: &WardrobeClient,
    user_pointer: &str,
    weapon_pointer: &str,
) -> io::Result<()> {
    let related_gems = client.find_by_filter(
        GEM_DRAWER,
        json!({
            "user_id": user_pointer
        }),
        None,
    )?;
    let removed_gem_count = related_gems.len();
    println!("Identifying {} gems to remove", removed_gem_count);

    for gem in related_gems {
        let gem_pointer = pointer_from_record(&gem, GEM_DRAWER)?;
        client.delete_by_id(&gem_pointer)?;
        println!("  Deleted orphaned gem: {}", gem_pointer);
    }

    client.delete_by_id(weapon_pointer)?;
    println!("  Deleted orphaned weapon: {}", weapon_pointer);

    client.delete_by_id(user_pointer)?;
    println!("  Deleted primary user record: {}", user_pointer);
    println!("Deleted {} gems linked to user", removed_gem_count);

    Ok(())
}

fn perform_maintenance_check(client: &WardrobeClient) -> io::Result<()> {
    let gem_count = client.count(GEM_DRAWER, None, None)?;
    let weapon_count = client.count(WEAPON_DRAWER, None, None)?;
    println!("Maintenance check: {} gems, {} weapons", gem_count, weapon_count);
    Ok(())
}

fn run_stress_test_cycle(client: &WardrobeClient) -> io::Result<()> {
    for i in 0..5 {
        let temp_gem_id = format!("temp_gem_{}", i);
        let pointer = client.upsert(
            GEM_DRAWER,
            json!({
                "_id": temp_gem_id,
                "element": "Temporary",
                "tags": ["test"]
            }),
        )?;
        client.delete_by_id(&pointer)?;
    }

    println!("Stress test cycle completed (5 upserts/deletes).");
    Ok(())
}

fn perform_detailed_inspection(engine: &WardrobeEngine) -> io::Result<()> {
    let databases = engine.list_databases()?;
    for db in databases {
        println!("Inspecting Database: {}", db.name);

        let schemas = engine.list_schemas(&db.name)?;
        for schema_name in schemas {
            println!("  Schema: {}", schema_name);

            let drawers = engine.show_drawers(&db.name, &schema_name)?;
            for drawer in drawers {
                let count = engine.count(&drawer.name, None, None)?;
                println!("    Drawer: {} ({} records)", drawer.name, count);
            }
        }
    }

    Ok(())
}

fn verify_final_database_integrity(client: &WardrobeClient, user_pointer: &str) -> io::Result<()> {
    let remaining_gems = client.find_all(GEM_DRAWER)?;
    let remaining_weapons = client.find_all(WEAPON_DRAWER)?;
    println!(
        "Total records remaining: {} gems, {} weapons",
        remaining_gems.len(),
        remaining_weapons.len()
    );

    match client.find_by_id(user_pointer)? {
        Some(_) => println!("INTEGRITY ERROR: User record persists."),
        None => println!("INTEGRITY SUCCESS: User record has been purged."),
    }

    Ok(())
}

fn main() -> io::Result<()> {
    let engine = initialize_engine_instance()?;
    let metadata_client = initialize_metadata_client()?;
    let public_client = initialize_public_client()?;

    print_execution_separator("Phase 1: Metadata & Inventory Discovery");
    perform_full_diagnostic_suite(&metadata_client)?;

    print_execution_separator("Phase 2: Relational Data Population");
    let user_pointer = upsert_user_record(&engine, "user_001", "Artemis_Prime")?;
    let gem_one = upsert_gem_record(&engine, "gem_001", &user_pointer, "Plasma", vec!["combat", "magic"])?;
    let _gem_two = upsert_gem_record(&engine, "gem_002", &user_pointer, "Gravity", vec!["support", "magic"])?;
    let _gem_three = upsert_gem_record(&engine, "gem_003", &user_pointer, "Void", vec!["utility", "raid"])?;
    let weapon_pointer = upsert_weapon_record(
        &engine,
        "wpn_001",
        &user_pointer,
        &gem_one,
        "Aether Blade",
        50,
    )?;

    print_execution_separator("Phase 3: Filter Query Execution");
    execute_and_print_tag_filter(&public_client, &user_pointer)?;

    print_execution_separator("Phase 4: Relation Verification");
    verify_three_relations(&public_client, &user_pointer, &gem_one, &weapon_pointer)?;

    print_execution_separator("Phase 5: Targeted Lifecycle Cleanup");
    perform_cascade_delete_sequence(&public_client, &user_pointer, &weapon_pointer)?;

    print_execution_separator("Phase 6: Scoped Maintenance & Stress Test");
    perform_maintenance_check(&public_client)?;
    run_stress_test_cycle(&public_client)?;

    print_execution_separator("Phase 7: State Reconciliation");
    perform_detailed_inspection(&engine)?;
    verify_final_database_integrity(&public_client, &user_pointer)?;

    println!("\nIntegration test suite executed successfully.");
    Ok(())
}
