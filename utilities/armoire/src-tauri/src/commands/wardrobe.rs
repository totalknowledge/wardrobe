use crate::services::wardrobe_database_service::WardrobeDatabaseService;
use wardrobe_embedded::StorageInventory;

#[tauri::command]
pub async fn wardrobe_test_database_access(database_directory: String) -> Result<(), String> {
    WardrobeDatabaseService::test_connection(&database_directory).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn wardrobe_create_source_location(database_directory: String) -> Result<String, String> {
    WardrobeDatabaseService::create_source_location(&database_directory)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn wardrobe_connect_source_location(
    database_directory: String,
    name: Option<String>,
) -> Result<(), String> {
    WardrobeDatabaseService::connect_source_location_with_name(&database_directory, name.as_deref())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn wardrobe_show_wardrobes() -> Result<Vec<StorageInventory>, String> {
    WardrobeDatabaseService::show_wardrobes().map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn wardrobe_create_new_wardrobe(database_name: String) -> Result<(), String> {
    WardrobeDatabaseService::create_new_wardrobe(&database_name).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn wardrobe_show_bays(database_name: String) -> Result<Vec<String>, String> {
    println!(
        "Tauri command: wardrobe_show_bays(database_name: \"{}\")",
        database_name
    );
    let result = WardrobeDatabaseService::show_bays(&database_name).map_err(|error| {
        println!("wardrobe_show_bays error: {}", error);
        error.to_string()
    });
    println!("wardrobe_show_bays result: {:?}", result);
    result
}

#[tauri::command]
pub async fn wardrobe_create_new_bay(
    database_name: String,
    schema_name: String,
) -> Result<(), String> {
    println!(
        "Tauri command: wardrobe_create_new_bay(database_name: \"{}\", schema_name: \"{}\")",
        database_name, schema_name
    );
    let result =
        WardrobeDatabaseService::create_new_bay(&database_name, &schema_name).map_err(|error| {
            println!("wardrobe_create_new_bay error: {}", error);
            error.to_string()
        });
    println!("wardrobe_create_new_bay result: {:?}", result);
    result
}

#[tauri::command]
pub async fn wardrobe_show_drawers(
    database_name: String,
    schema_name: String,
) -> Result<Vec<StorageInventory>, String> {
    println!(
        "Tauri command: wardrobe_show_drawers(database_name: \"{}\", schema_name: \"{}\")",
        database_name, schema_name
    );
    let result =
        WardrobeDatabaseService::show_drawers(&database_name, &schema_name).map_err(|error| {
            println!("wardrobe_show_drawers error: {}", error);
            error.to_string()
        });
    println!("wardrobe_show_drawers result: {:?}", result);
    result
}

#[tauri::command]
pub async fn wardrobe_create_new_drawer(
    database_name: String,
    schema_name: String,
    drawer_name: String,
) -> Result<(), String> {
    println!(
        "Tauri command: wardrobe_create_new_drawer(database_name: \"{}\", schema_name: \"{}\", drawer_name: \"{}\")",
        database_name, schema_name, drawer_name
    );
    let result =
        WardrobeDatabaseService::create_new_drawer(&database_name, &schema_name, &drawer_name)
            .map_err(|error| {
                println!("wardrobe_create_new_drawer error: {}", error);
                error.to_string()
            });
    println!("wardrobe_create_new_drawer result: {:?}", result);
    result
}

#[tauri::command]
pub async fn wardrobe_read_records(
    database_name: String,
    schema_name: String,
    drawer_name: String,
) -> Result<Vec<serde_json::Value>, String> {
    println!(
        "Tauri command: wardrobe_read_records(database_name: \"{}\", schema_name: \"{}\", drawer_name: \"{}\")",
        database_name, schema_name, drawer_name
    );
    let result = WardrobeDatabaseService::read_records(&database_name, &schema_name, &drawer_name)
        .map_err(|error| {
            println!("wardrobe_read_records error: {}", error);
            error.to_string()
        });
    println!("wardrobe_read_records result: {:?}", result);
    result
}

#[tauri::command]
pub async fn wardrobe_create_record(
    database_name: String,
    schema_name: String,
    drawer_name: String,
    payload: serde_json::Value,
) -> Result<(), String> {
    println!(
        "Tauri command: wardrobe_create_record(database_name: \"{}\", schema_name: \"{}\", drawer_name: \"{}\", payload: {:?})",
        database_name, schema_name, drawer_name, payload
    );
    let result =
        WardrobeDatabaseService::create_record(&database_name, &schema_name, &drawer_name, payload)
            .map_err(|error| {
                println!("wardrobe_create_record error: {}", error);
                error.to_string()
            });
    println!("wardrobe_create_record result: {:?}", result);
    result
}

#[tauri::command]
pub async fn armoire_get_saved_connections() -> Result<Vec<serde_json::Value>, String> {
    WardrobeDatabaseService::get_saved_connections().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn armoire_remove_connection(target: String) -> Result<(), String> {
    println!(
        "Tauri command: armoire_remove_connection(target: \"{}\")",
        target
    );
    let result = WardrobeDatabaseService::remove_connection(&target);
    println!("armoire_remove_connection result: {:?}", result);
    result.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn armoire_update_connection_alias(target: String, alias: String) -> Result<(), String> {
    WardrobeDatabaseService::update_connection_alias(&target, &alias).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn armoire_delete_connection_files(target: String, id: String) -> Result<(), String> {
    println!(
        "Tauri command: armoire_delete_connection_files(target: \"{}\", id: \"{}\")",
        target, id
    );
    let result = WardrobeDatabaseService::delete_connection_files(&target, &id);
    println!("armoire_delete_connection_files result: {:?}", result);
    result.map_err(|e| e.to_string())
}
