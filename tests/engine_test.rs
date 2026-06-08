mod common;

use common::TempDatabase;
use serde_json::json;
use std::path::Path;
use wardrobe::WardrobeEngine;

#[test]
fn find_all_loads_existing_drawer_files_after_restart() {
    let database = TempDatabase::new("find_all_loads_existing_drawer_files");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let mut engine = WardrobeEngine::new(&database_directory).expect("database should initialize");
        engine
            .upsert(
                "weapon",
                json!({
                    "_id": "@weapon:lnk_test_weapon",
                    "name": "Test Sword",
                    "gem": {
                        "_id": "@gem:lnk_test_gem",
                        "element": "Light",
                        "potency": 9001
                    }
                }),
            )
            .expect("weapon should upsert");
    }

    let mut restarted_engine = WardrobeEngine::new(&database_directory).expect("database should reinitialize");
    let weapons = restarted_engine.find_all("weapon").expect("weapons should load");

    assert_eq!(weapons.len(), 1);
    assert_eq!(weapons[0]["name"], "Test Sword");
    assert_eq!(weapons[0]["gem"]["element"], "Light");
}

#[test]
fn upsert_rejects_non_object_payload() {
    let database = TempDatabase::new("upsert_rejects_non_object_payload");
    let database_directory = database.path.to_string_lossy().into_owned();
    let mut engine = WardrobeEngine::new(&database_directory).expect("database should initialize");

    let error = engine
        .upsert("gem", json!(["not", "an", "object"]))
        .expect_err("non-object payload should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn find_by_id_returns_none_for_missing_drawer_and_does_not_create_files() {
    let database = TempDatabase::new("find_by_id_missing_drawer");
    let database_directory = database.path.to_string_lossy().into_owned();
    let mut engine = WardrobeEngine::new(&database_directory).expect("database should initialize");

    let result = engine
        .find_by_id("@missing:lnk_any")
        .expect("missing drawer lookup should not fail");

    assert!(result.is_none());
    assert!(!Path::new(&database_directory).join("missing.drw").exists());
    assert!(!Path::new(&database_directory).join("missing_index.drw").exists());
}

#[test]
fn find_by_id_rejects_malformed_pointer() {
    let database = TempDatabase::new("find_by_id_rejects_malformed_pointer");
    let database_directory = database.path.to_string_lossy().into_owned();
    let mut engine = WardrobeEngine::new(&database_directory).expect("database should initialize");

    let error = engine
        .find_by_id("not-a-pointer")
        .expect_err("malformed pointer should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn find_by_id_hydrates_without_id_fields() {
    let database = TempDatabase::new("find_by_id_hydrates_without_id_fields");
    let database_directory = database.path.to_string_lossy().into_owned();

    let weapon_pointer = {
        let mut engine = WardrobeEngine::new(&database_directory).expect("database should initialize");
        engine
            .upsert(
                "weapon",
                json!({
                    "name": "Edge",
                    "gem": {
                        "element": "Arc",
                        "potency": 1234
                    }
                }),
            )
            .expect("weapon should upsert")
    };

    let mut restarted_engine = WardrobeEngine::new(&database_directory).expect("database should reinitialize");
    let found = restarted_engine
        .find_by_id(&weapon_pointer)
        .expect("lookup should succeed")
        .expect("record should exist");

    assert!(found.get("_id").is_none());
    assert!(found["gem"].get("_id").is_none());
    assert_eq!(found["gem"]["element"], "Arc");
}

#[test]
fn find_all_keeps_pointer_when_target_record_is_missing() {
    let database = TempDatabase::new("find_all_keeps_pointer_when_target_record_is_missing");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let mut engine = WardrobeEngine::new(&database_directory).expect("database should initialize");
        engine
            .upsert(
                "weapon",
                json!({
                    "_id": "@weapon:lnk_has_missing_target",
                    "name": "Fragment",
                    "gem": "@gem:lnk_does_not_exist"
                }),
            )
            .expect("weapon should upsert");
    }

    let mut restarted_engine = WardrobeEngine::new(&database_directory).expect("database should reinitialize");
    let weapons = restarted_engine.find_all("weapon").expect("find_all should succeed");

    assert_eq!(weapons.len(), 1);
    assert_eq!(weapons[0]["gem"], "@gem:lnk_does_not_exist");
}

#[test]
fn us_013_find_all_auto_loads_drawers_and_hydrates_linked_drawers_on_demand() {
    let database = TempDatabase::new("us_013");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let mut engine = WardrobeEngine::new(&database_directory).expect("engine should initialize");
        engine
            .upsert(
                "weapon",
                json!({
                    "_id": "@weapon:lnk_lazy_weapon",
                    "name": "Moonblade",
                    "damage": 120,
                    "gem": {
                        "_id": "@gem:lnk_lazy_gem",
                        "element": "Void",
                        "potency": 4242
                    }
                }),
            )
            .expect("weapon with nested gem should upsert");
    }

    let mut restarted_engine = WardrobeEngine::new(&database_directory).expect("engine should reinitialize");
    let weapons = restarted_engine
        .find_all("weapon")
        .expect("find_all should auto-load weapon drawer from disk");

    assert_eq!(weapons.len(), 1);
    assert_eq!(weapons[0]["name"].as_str(), Some("Moonblade"));
    assert_eq!(weapons[0]["gem"]["element"].as_str(), Some("Void"));
    assert_eq!(weapons[0]["gem"]["potency"].as_u64(), Some(4242));
}

#[test]
fn us_013_reads_do_not_auto_create_missing_drawers() {
    let database = TempDatabase::new("us_013_missing_drawers");
    let database_directory = database.path.to_string_lossy().into_owned();
    let mut engine = WardrobeEngine::new(&database_directory).expect("engine should initialize");

    let records = engine
        .find_all("missing")
        .expect("find_all should succeed for missing drawers");
    assert!(records.is_empty());

    let by_id = engine
        .find_by_id("@missing:lnk_example")
        .expect("find_by_id should succeed for missing drawers");
    assert!(by_id.is_none());

    assert!(!database.path.join("missing.drw").exists());
    assert!(!database.path.join("missing_index.drw").exists());
}

#[test]
fn us_014_safe_engine_hydration_resolves_nested_records_after_restart() {
    let database = TempDatabase::new("us_014");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let mut engine = WardrobeEngine::new(&database_directory).expect("engine should initialize");
        engine
            .upsert(
                "character",
                json!({
                    "_id": "@character:lnk_safe_character",
                    "name": "Grace",
                    "weapon": {
                        "_id": "@weapon:lnk_safe_weapon",
                        "name": "Spear",
                        "damage": 64,
                        "gem": {
                            "_id": "@gem:lnk_safe_gem",
                            "element": "Storm",
                            "potency": 8080
                        }
                    }
                }),
            )
            .expect("complex character should upsert");
    }

    let mut restarted_engine = WardrobeEngine::new(&database_directory).expect("engine should reinitialize");
    let characters = restarted_engine
        .find_all("character")
        .expect("characters should hydrate after restart");

    assert_eq!(characters.len(), 1);
    assert_eq!(characters[0]["weapon"]["name"].as_str(), Some("Spear"));
    assert_eq!(
        characters[0]["weapon"]["gem"]["element"].as_str(),
        Some("Storm")
    );
    assert_eq!(
        characters[0]["weapon"]["gem"]["_id"].as_str(),
        Some("@gem:lnk_safe_gem")
    );
}

#[test]
fn find_by_id_handles_cyclic_links_without_recursive_overflow() {
    let database = TempDatabase::new("find_by_id_handles_cyclic_links");
    let database_directory = database.path.to_string_lossy().into_owned();

    let mut engine = WardrobeEngine::new(&database_directory).expect("engine should initialize");
    engine
        .upsert(
            "node",
            json!({
                "_id": "@node:lnk_a",
                "name": "Node A",
                "next": "@node:lnk_b"
            }),
        )
        .expect("first node should upsert");
    engine
        .upsert(
            "node",
            json!({
                "_id": "@node:lnk_b",
                "name": "Node B",
                "next": "@node:lnk_a"
            }),
        )
        .expect("second node should upsert");

    let hydrated = engine
        .find_by_id("@node:lnk_a")
        .expect("lookup should succeed")
        .expect("record should exist");

    assert_eq!(hydrated["name"], "Node A");
    assert_eq!(hydrated["next"]["name"], "Node B");
    assert_eq!(hydrated["next"]["next"], "@node:lnk_a");
}
