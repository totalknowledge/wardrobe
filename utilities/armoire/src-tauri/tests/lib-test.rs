#[test]
fn library_exports_command_and_service_modules_for_tests() {
    let _service = armoire_lib::services::wardrobe_database_service::WardrobeDatabaseService;

    let _create_command = armoire_lib::commands::wardrobe::wardrobe_create_source_location;
    let _test_command = armoire_lib::commands::wardrobe::wardrobe_test_database_access;
}
