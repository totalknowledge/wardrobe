#[test]
fn services_module_exposes_wardrobe_database_service() {
    let _service = app_lib::services::wardrobe_database_service::WardrobeDatabaseService;
}
