#[test]
fn services_module_exposes_wardrobe_database_service() {
    let _service = armoire_lib::services::wardrobe_database_service::WardrobeDatabaseService;
}
