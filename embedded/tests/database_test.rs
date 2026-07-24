mod common;

use common::TempDatabase;
use serde_json::json;
use std::collections::HashMap;
use wardrobe_embedded::{Database, OperationFilter, OperationOptions, ReadResult};

fn read_record(
    engine: &wardrobe_embedded::WardrobeEngine,
    filter: OperationFilter,
) -> Option<serde_json::Value> {
    match engine
        .read(filter, None::<OperationOptions>)
        .expect("read should succeed")
    {
        ReadResult::Record(record) => record,
        other => panic!("expected record, got {other:?}"),
    }
}

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
fn database_wal_counters_record_and_reset() {
    let database = TempDatabase::new("us_wal_counters");
    let db = Database::initialize(&database.path).expect("database should initialize");

    db.record_wal_activity(512, 2);
    let (bytes, ops) = db.get_wal_counters();
    assert_eq!(bytes, 512);
    assert_eq!(ops, 2);

    db.reset_wal_counters();
    let (b2, o2) = db.get_wal_counters();
    assert_eq!(b2, 0);
    assert_eq!(o2, 0);

    let (threshold_bytes, threshold_ops) = db.wal_thresholds();
    assert!(threshold_bytes > 0);
    assert!(threshold_ops > 0);
}

#[test]
fn us_012_indexes_rebuild_from_disk_after_restart() {
    let database = TempDatabase::new("us_012");
    let database_directory = database.path.to_string_lossy().into_owned();
    let record_id = "@gem:lnk_restart_gem";

    {
        let engine = wardrobe_embedded::WardrobeEngine::open(&database_directory)
            .expect("engine should initialize");
        engine
            .upsert(
                json!({
                    "_id": record_id,
                    "element": "Water",
                    "potency": 7300
                }),
                OperationFilter::drawer("gem"),
                None::<OperationOptions>,
            )
            .expect("record should upsert");
    }

    let restarted_engine = wardrobe_embedded::WardrobeEngine::open(&database_directory)
        .expect("engine should reinitialize");
    let found = read_record(&restarted_engine, OperationFilter::pointer(record_id))
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
        let mut setup_db =
            Database::initialize(&database_directory.path).expect("db should initialize");
        setup_db
            .load_drawer("gem", "_id", Vec::new())
            .expect("drawer should load");
        let drawer = setup_db
            .use_drawer("gem")
            .expect("drawer should be active after load");
        drawer
            .write()
            .expect("drawer lock should be writable")
            .upsert_record(json!({
                "_id": "@gem:lnk_db_existing",
                "element": "Fire"
            }))
            .expect("upsert should succeed")
            .expect("record should validate");
    }

    let mut restarted_db =
        Database::initialize(&database_directory.path).expect("db should reinitialize");
    let drawer = restarted_db
        .active_drawer_or_load_from_disk("gem", "_id", Vec::new())
        .expect("read load should succeed")
        .expect("existing drawer should be loaded");

    let found = drawer
        .read()
        .expect("drawer lock should be readable")
        .find_by_primary_key("@gem:lnk_db_existing")
        .expect("lookup should succeed")
        .expect("record should exist");
    assert_eq!(found["element"], "Fire");
}

#[test]
fn load_existing_drawers_registers_all_non_index_drawers() {
    let database_directory = TempDatabase::new("db_load_existing_drawers");
    let mut database =
        Database::initialize(&database_directory.path).expect("db should initialize");

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
fn load_existing_drawers_ignores_meta_sidecar_files() {
    let database_directory = TempDatabase::new("db_ignore_meta_sidecars");
    let mut database =
        Database::initialize(&database_directory.path).expect("db should initialize");

    database
        .load_drawer("socks", "_id", Vec::new())
        .expect("socks drawer should load");
    database.close_drawer("socks");

    std::fs::write(
        database_directory.path.join("orphan_meta.drw"),
        "{\"format_version\":1}",
    )
    .expect("orphan metadata file should write");

    database
        .load_existing_drawers("_id", HashMap::new())
        .expect("existing drawers should load");

    assert!(database.use_drawer("socks").is_some());
    assert!(database.use_drawer("orphan_meta").is_none());
}

#[test]
fn get_all_drawers_reflects_loaded_and_closed_drawers() {
    let database_directory = TempDatabase::new("db_get_all_drawers");
    let mut database =
        Database::initialize(&database_directory.path).expect("db should initialize");

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

#[test]
fn us_043_lru_cache_evicts_least_recently_used_drawer_when_limit_is_exceeded() {
    let database_directory = TempDatabase::new("us_043_lru_evicts_oldest_drawer");
    let mut database = Database::initialize_with_cache_limit(&database_directory.path, Some(2))
        .expect("db should initialize");

    database
        .load_drawer("gem", "_id", Vec::new())
        .expect("gem drawer should load");
    database
        .load_drawer("weapon", "_id", Vec::new())
        .expect("weapon drawer should load");
    database
        .use_drawer("gem")
        .expect("gem should be hot in cache");
    database
        .load_drawer("character", "_id", Vec::new())
        .expect("character drawer should load");

    assert_eq!(database.cached_drawer_count(), 2);
    assert!(database.use_drawer("gem").is_some());
    assert!(database.use_drawer("character").is_some());
    assert!(database.use_drawer("weapon").is_none());
}

#[test]
fn us_043_lru_cache_does_not_evict_actively_borrowed_drawers() {
    let database_directory = TempDatabase::new("us_043_lru_keeps_borrowed_drawer");
    let mut database = Database::initialize_with_cache_limit(&database_directory.path, Some(1))
        .expect("db should initialize");

    database
        .load_drawer("gem", "_id", Vec::new())
        .expect("gem drawer should load");
    let borrowed_gem = database
        .use_drawer("gem")
        .expect("gem should be active in cache");

    database
        .load_drawer("weapon", "_id", Vec::new())
        .expect("weapon drawer should load");
    assert_eq!(database.cached_drawer_count(), 2);

    drop(borrowed_gem);
    database
        .load_drawer("character", "_id", Vec::new())
        .expect("character drawer should load");

    assert_eq!(database.cached_drawer_count(), 1);
    assert!(database.use_drawer("character").is_some());
    assert!(database.use_drawer("gem").is_none());
    assert!(database.use_drawer("weapon").is_none());
}

#[test]
fn us_043_cache_limit_rejects_zero_sized_pool() {
    let database_directory = TempDatabase::new("us_043_lru_rejects_zero");
    let Err(error) = Database::initialize_with_cache_limit(&database_directory.path, Some(0))
    else {
        panic!("zero limit should fail");
    };

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}
