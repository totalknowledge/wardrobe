#[test]
fn library_exports_command_and_service_modules_for_tests() {
    let _service = app_lib::services::wardrobe_database_service::WardrobeDatabaseService;

    let _create_command = app_lib::commands::wardrobe::wardrobe_create_source_location;
    let _test_command = app_lib::commands::wardrobe::wardrobe_test_database_access;
}
