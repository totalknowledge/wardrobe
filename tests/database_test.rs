mod common;

use common::TempDatabase;
use serde_json::json;
use std::collections::HashMap;
use wardrobe::Database;

#[test]
fn us_001_database_initialization_creates_storage_directory() {
    let database = TempDatabase::new("us_001");

    assert!(!database.path.exists());
    Database::initialize(&database.path).expect("database should initialize");

    assert!(database.path.is_dir());
}

#[test]
fn us_002_opening_a_drawer_creates_data_and_index_files() {
    let database = TempDatabase::new("us_002");
    let mut database_core =
        Database::initialize(&database.path).expect("database should initialize");

    database_core
        .load_drawer("gem", "_id", Vec::new())
        .expect("drawer should load");

    assert!(database.path.join("gem.drw").is_file());
    assert!(database.path.join("gem_index.drw").is_file());
}

#[test]
fn us_012_indexes_rebuild_from_disk_after_restart() {
    let database = TempDatabase::new("us_012");
    let database_directory = database.path.to_string_lossy().into_owned();
    let record_id = "@gem:lnk_restart_gem";

    {
        let mut engine = wardrobe::WardrobeEngine::new(&database_directory).expect("engine should initialize");
        engine
            .upsert(
                "gem",
                json!({
                    "_id": record_id,
                    "element": "Water",
                    "potency": 7300
                }),
            )
            .expect("record should upsert");
    }

    let mut restarted_engine =
        wardrobe::WardrobeEngine::new(&database_directory).expect("engine should reinitialize");
    let found = restarted_engine
        .find_by_id(record_id)
        .expect("lookup should use rebuilt index")
        .expect("record should be found after restart");

    assert_eq!(found["element"].as_str(), Some("Water"));
    assert_eq!(found["potency"].as_u64(), Some(7300));
}

#[test]
fn active_drawer_or_load_from_disk_returns_none_when_files_are_missing() {
    let database_directory = TempDatabase::new("db_missing_files_returns_none");
    let mut database_core =
        Database::initialize(&database_directory.path).expect("db should initialize");

    let drawer = database_core
        .active_drawer_or_load_from_disk("ghost", "_id", Vec::new())
        .expect("read path should not fail");

    assert!(drawer.is_none());
    assert!(!database_directory.path.join("ghost.drw").exists());
    assert!(!database_directory.path.join("ghost_index.drw").exists());
}

#[test]
fn active_drawer_or_load_from_disk_opens_existing_drawer_files() {
    let database_directory = TempDatabase::new("db_loads_existing_drawer");

    {
        let mut setup_db = Database::initialize(&database_directory.path).expect("db should initialize");
        setup_db
            .load_drawer("gem", "_id", Vec::new())
            .expect("drawer should load");
        let drawer = setup_db
            .use_drawer("gem")
            .expect("drawer should be active after load");
        drawer
            .upsert_record(json!({
                "_id": "@gem:lnk_db_existing",
                "element": "Fire"
            }))
            .expect("upsert should succeed")
            .expect("record should validate");
    }

    let mut restarted_db = Database::initialize(&database_directory.path).expect("db should reinitialize");
    let drawer = restarted_db
        .active_drawer_or_load_from_disk("gem", "_id", Vec::new())
        .expect("read load should succeed")
        .expect("existing drawer should be loaded");

    let found = drawer
        .find_by_primary_key("@gem:lnk_db_existing")
        .expect("lookup should succeed")
        .expect("record should exist");
    assert_eq!(found["element"], "Fire");
}

#[test]
fn load_existing_drawers_registers_all_non_index_drawers() {
    let database_directory = TempDatabase::new("db_load_existing_drawers");
    let mut database = Database::initialize(&database_directory.path).expect("db should initialize");

    database
        .load_drawer("weapon", "_id", Vec::new())
        .expect("weapon drawer should load");
    database
        .load_drawer("gem", "_id", Vec::new())
        .expect("gem drawer should load");
    database.close_drawer("weapon");
    database.close_drawer("gem");

    database
        .load_existing_drawers("_id", HashMap::new())
        .expect("existing drawers should load");

    assert!(database.use_drawer("weapon").is_some());
    assert!(database.use_drawer("gem").is_some());
}

#[test]
fn get_all_drawers_reflects_loaded_and_closed_drawers() {
    let database_directory = TempDatabase::new("db_get_all_drawers");
    let mut database = Database::initialize(&database_directory.path).expect("db should initialize");

    database
        .load_drawer("weapon", "_id", Vec::new())
        .expect("weapon drawer should load");
    database
        .load_drawer("gem", "_id", Vec::new())
        .expect("gem drawer should load");

    let loaded = database.get_all_drawers();
    assert_eq!(loaded.len(), 2);

    database.close_drawer("weapon");
    let loaded_after_close = database.get_all_drawers();
    assert_eq!(loaded_after_close.len(), 1);
    assert!(loaded_after_close.contains_key("gem"));
}