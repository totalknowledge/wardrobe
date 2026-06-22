use crate::services::wardrobe_database_service::WardrobeDatabaseService;

#[tauri::command]
pub async fn wardrobe_test_database_access(database_directory: String) -> Result<(), String> {
    WardrobeDatabaseService::test_connection(&database_directory).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn wardrobe_create_source_location(database_directory: String) -> Result<String, String> {
    WardrobeDatabaseService::create_source_location(&database_directory)
        .map_err(|error| error.to_string())
}
