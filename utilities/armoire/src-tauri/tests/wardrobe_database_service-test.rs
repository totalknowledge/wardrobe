use std::time::{SystemTime, UNIX_EPOCH};

use armoire_lib::services::wardrobe_database_service::WardrobeDatabaseService;

fn temp_database_path(test_name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();

    std::env::temp_dir().join(format!("armoire_{test_name}_{nanos}"))
}

fn missing_relative_database_name(test_name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();

    format!("armoire_missing_{test_name}_{nanos}")
}

#[test]
fn create_source_location_creates_and_initializes_directory() {
    let path = temp_database_path("service_create_source_location");

    let source_location = WardrobeDatabaseService::create_source_location(&path.to_string_lossy())
        .expect("source location should create");
    let source_location = std::path::PathBuf::from(source_location);

    assert!(source_location.exists());
    assert!(source_location.is_dir());

    let _ = std::fs::remove_dir_all(source_location);
}

#[test]
fn test_connection_accepts_initialized_source_location() {
    let path = temp_database_path("service_test_connection");
    let source_location = WardrobeDatabaseService::create_source_location(&path.to_string_lossy())
        .expect("source location should create");

    WardrobeDatabaseService::test_connection(&source_location)
        .expect("initialized source location should connect");

    let _ = std::fs::remove_dir_all(source_location);
}

#[test]
fn test_connection_rejects_missing_directory() {
    let path = missing_relative_database_name("service_connection");

    let error = WardrobeDatabaseService::test_connection(&path)
        .expect_err("missing source location should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(error.to_string().contains("was not found"));
}
