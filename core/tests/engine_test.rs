mod common;

use common::TempDatabase;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use wardrobe_core::{
    Command, CommandResult, OrderDirection, QueryModifiers, StorageCoordinate, StorageScope,
    WardrobeEngine,
};

fn write_cascade_delete_rules(database: &TempDatabase, drawer_name: &str, fields: &[&str]) {
    let cascade_delete_rules = fields
        .iter()
        .map(|field| ((*field).to_string(), json!(true)))
        .collect::<serde_json::Map<String, serde_json::Value>>();

    let metadata = json!({
        "format_version": 1,
        "primary_key": "_id",
        "unique_constraints": [],
        "relationship_constraints": {},
        "delete_rules": {},
        "cascade_delete_rules": cascade_delete_rules
    });

    fs::write(
        database.path.join(format!("{}_meta.drw", drawer_name)),
        serde_json::to_vec_pretty(&metadata).expect("metadata should serialize"),
    )
    .expect("metadata should write");
}

fn write_drawer_metadata(database: &TempDatabase, drawer_name: &str, metadata: serde_json::Value) {
    fs::write(
        database.path.join(format!("{}_meta.drw", drawer_name)),
        serde_json::to_vec_pretty(&metadata).expect("metadata should serialize"),
    )
    .expect("metadata should write");
}

fn write_wal_record(database: &TempDatabase, record: serde_json::Value) {
    fs::create_dir_all(&database.path).expect("temp dir should create");
    let mut wal_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(database.path.join("wardrobe.wal"))
        .expect("wal should open");
    writeln!(wal_file, "{}", record).expect("wal record should write");
    wal_file.sync_all().expect("wal should sync");
}

fn wal_records(database: &TempDatabase) -> Vec<serde_json::Value> {
    fs::read_to_string(database.path.join("wardrobe.wal"))
        .expect("wal should read")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("wal line should parse"))
        .collect()
}

#[test]
fn find_all_loads_existing_drawer_files_after_restart() {
    let database = TempDatabase::new("find_all_loads_existing_drawer_files");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let engine = WardrobeEngine::open(&database_directory).expect("database should initialize");
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

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("database should reinitialize");
    let weapons = restarted_engine
        .find_all("weapon")
        .expect("weapons should load");

    assert_eq!(weapons.len(), 1);
    assert_eq!(weapons[0]["name"], "Test Sword");
    assert_eq!(weapons[0]["gem"]["element"], "Light");
}

#[test]
fn upsert_rejects_non_object_payload() {
    let database = TempDatabase::new("upsert_rejects_non_object_payload");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("database should initialize");

    let error = engine
        .upsert("gem", json!(["not", "an", "object"]))
        .expect_err("non-object payload should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn find_by_id_returns_none_for_missing_drawer_and_does_not_create_files() {
    let database = TempDatabase::new("find_by_id_missing_drawer");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("database should initialize");

    let result = engine
        .find_by_id("@missing:lnk_any")
        .expect("missing drawer lookup should not fail");

    assert!(result.is_none());
    assert!(!Path::new(&database_directory).join("missing.drw").exists());
    assert!(
        !Path::new(&database_directory)
            .join("missing_index.drw")
            .exists()
    );
}

#[test]
fn find_by_id_rejects_malformed_pointer() {
    let database = TempDatabase::new("find_by_id_rejects_malformed_pointer");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("database should initialize");

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
        let engine = WardrobeEngine::open(&database_directory).expect("database should initialize");
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

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("database should reinitialize");
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
        let engine = WardrobeEngine::open(&database_directory).expect("database should initialize");
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

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("database should reinitialize");
    let weapons = restarted_engine
        .find_all("weapon")
        .expect("find_all should succeed");

    assert_eq!(weapons.len(), 1);
    assert_eq!(weapons[0]["gem"], "@gem:lnk_does_not_exist");
}

#[test]
fn us_013_find_all_auto_loads_drawers_and_hydrates_linked_drawers_on_demand() {
    let database = TempDatabase::new("us_013");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reinitialize");
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
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

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
        let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reinitialize");
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
fn us_018_id_only_subobject_normalizes_raw_ids_and_preserves_child_record() {
    let database = TempDatabase::new("us_018_id_only_raw_reference");
    let database_directory = database.path.to_string_lossy().into_owned();
    let application_gem_id = "existing_gem";
    let wardrobe_gem_id = "@gem:lnk_existing_gem";

    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
    engine
        .upsert(
            "gem",
            json!({
                "_id": wardrobe_gem_id,
                "element": "Solar",
                "potency": 777
            }),
        )
        .expect("gem should upsert");

    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_uses_existing_gem",
                "name": "Sun Pike",
                "gem": {
                    "_id": application_gem_id
                }
            }),
        )
        .expect("weapon should upsert with raw id-only gem reference");

    let gem_file =
        fs::read_to_string(database.path.join("gem.drw")).expect("gem drawer should be readable");
    assert!(!gem_file.contains("!!DEAD!!"));
    assert!(gem_file.contains("\"element\":\"Solar\""));

    let weapon_file = fs::read_to_string(database.path.join("weapon.drw"))
        .expect("weapon drawer should be readable");
    assert!(weapon_file.contains("\"gem\":\"@gem:lnk_existing_gem\""));

    let found_gem = engine
        .find_by_id(wardrobe_gem_id)
        .expect("gem lookup should succeed")
        .expect("gem should still exist");
    assert_eq!(found_gem["element"], "Solar");
    assert_eq!(found_gem["potency"], 777);

    let weapons = engine
        .find_all("weapon")
        .expect("weapon lookup should succeed");
    assert_eq!(weapons.len(), 1);
    assert_eq!(weapons[0]["gem"]["element"], "Solar");
}

#[test]
fn us_018_id_only_subobject_accepts_preformatted_pointers() {
    let database = TempDatabase::new("us_018_preformatted_reference");
    let database_directory = database.path.to_string_lossy().into_owned();
    let gem_id = "@gem:lnk_existing_gem";

    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
    engine
        .upsert(
            "gem",
            json!({
                "_id": gem_id,
                "element": "Nebula",
                "potency": 313
            }),
        )
        .expect("gem should upsert");

    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_preformatted_reference",
                "name": "Star Lance",
                "gem": {
                    "_id": gem_id
                }
            }),
        )
        .expect("weapon should upsert with preformatted reference");

    let weapon_file = fs::read_to_string(database.path.join("weapon.drw"))
        .expect("weapon drawer should be readable");
    assert!(weapon_file.contains("\"gem\":\"@gem:lnk_existing_gem\""));

    let weapons = engine
        .find_all("weapon")
        .expect("weapon lookup should succeed");
    assert_eq!(weapons.len(), 1);
    assert_eq!(weapons[0]["gem"]["element"], "Nebula");
}

#[test]
fn us_018_full_subobject_is_upserted_and_parent_stores_reference() {
    let database = TempDatabase::new("us_018_full_subobject");
    let database_directory = database.path.to_string_lossy().into_owned();

    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_full_child",
                "name": "Moon Staff",
                "gem": {
                    "_id": "@gem:lnk_full_child_gem",
                    "element": "Lunar",
                    "potency": 123
                }
            }),
        )
        .expect("weapon should upsert with full child object");

    let weapon_file = fs::read_to_string(database.path.join("weapon.drw"))
        .expect("weapon drawer should be readable");
    assert!(weapon_file.contains("\"gem\":\"@gem:lnk_full_child_gem\""));

    let found_gem = engine
        .find_by_id("@gem:lnk_full_child_gem")
        .expect("gem lookup should succeed")
        .expect("gem should exist");
    assert_eq!(found_gem["element"], "Lunar");

    let weapons = engine
        .find_all("weapon")
        .expect("weapon lookup should succeed");
    assert_eq!(weapons[0]["gem"]["potency"], 123);
}

#[test]
fn us_019_scalar_arrays_are_preserved_on_upsert() {
    let database = TempDatabase::new("us_019_scalar_arrays");
    let database_directory = database.path.to_string_lossy().into_owned();

    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
    engine
        .upsert(
            "character",
            json!({
                "_id": "@character:lnk_array_scalars",
                "name": "Array Keeper",
                "tags": ["tank", "support", "night-watch"],
                "scores": [10, 20, 30]
            }),
        )
        .expect("character should upsert with scalar arrays");

    let character_file = fs::read_to_string(database.path.join("character.drw"))
        .expect("character drawer should be readable");
    assert!(character_file.contains("\"tags\":[\"tank\",\"support\",\"night-watch\"]"));
    assert!(character_file.contains("\"scores\":[10,20,30]"));

    let characters = engine
        .find_all("character")
        .expect("character lookup should succeed");
    assert_eq!(
        characters[0]["tags"],
        json!(["tank", "support", "night-watch"])
    );
    assert_eq!(characters[0]["scores"], json!([10, 20, 30]));
}

#[test]
fn us_019_pointer_arrays_are_hydrated_in_order() {
    let database = TempDatabase::new("us_019_pointer_arrays");
    let database_directory = database.path.to_string_lossy().into_owned();

    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_array_fire",
                "element": "Fire",
                "potency": 10
            }),
        )
        .expect("fire gem should upsert");
    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_array_water",
                "element": "Water",
                "potency": 20
            }),
        )
        .expect("water gem should upsert");

    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_pointer_array",
                "name": "Twin Wand",
                "gems": ["@gem:lnk_array_fire", "@gem:lnk_array_water"]
            }),
        )
        .expect("weapon should upsert with pointer array");

    let weapons = engine
        .find_all("weapon")
        .expect("weapon lookup should succeed");
    assert_eq!(weapons[0]["gems"][0]["element"], "Fire");
    assert_eq!(weapons[0]["gems"][1]["element"], "Water");
}

#[test]
fn us_019_id_only_object_arrays_are_normalized_to_references() {
    let database = TempDatabase::new("us_019_id_only_object_arrays");
    let database_directory = database.path.to_string_lossy().into_owned();

    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_array_existing_fire",
                "element": "Fire",
                "potency": 111
            }),
        )
        .expect("fire gem should upsert");
    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_array_existing_air",
                "element": "Air",
                "potency": 222
            }),
        )
        .expect("air gem should upsert");

    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_id_only_array",
                "name": "Reference Bow",
                "gems": [
                    { "_id": "array_existing_fire" },
                    { "_id": "@gem:lnk_array_existing_air" }
                ]
            }),
        )
        .expect("weapon should upsert with id-only array references");

    let weapon_file = fs::read_to_string(database.path.join("weapon.drw"))
        .expect("weapon drawer should be readable");
    assert!(
        weapon_file.contains(
            "\"gems\":[\"@gem:lnk_array_existing_fire\",\"@gem:lnk_array_existing_air\"]"
        )
    );

    let weapons = engine
        .find_all("weapon")
        .expect("weapon lookup should succeed");
    assert_eq!(weapons[0]["gems"][0]["element"], "Fire");
    assert_eq!(weapons[0]["gems"][1]["element"], "Air");
}

#[test]
fn us_019_full_nested_object_arrays_are_upserted_and_hydrated() {
    let database = TempDatabase::new("us_019_full_nested_object_arrays");
    let database_directory = database.path.to_string_lossy().into_owned();

    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
    engine
        .upsert(
            "character",
            json!({
                "_id": "@character:lnk_nested_array_owner",
                "name": "Grace",
                "weapons": [
                    {
                        "_id": "@weapon:lnk_array_spear",
                        "name": "Spear",
                        "gems": [
                            {
                                "_id": "@gem:lnk_array_storm",
                                "element": "Storm",
                                "potency": 8080
                            }
                        ]
                    },
                    {
                        "_id": "@weapon:lnk_array_shield",
                        "name": "Shield",
                        "gems": [
                            {
                                "_id": "@gem:lnk_array_earth",
                                "element": "Earth",
                                "potency": 4040
                            }
                        ]
                    }
                ]
            }),
        )
        .expect("character should upsert with nested object arrays");

    let character_file = fs::read_to_string(database.path.join("character.drw"))
        .expect("character drawer should be readable");
    assert!(
        character_file
            .contains("\"weapons\":[\"@weapon:lnk_array_spear\",\"@weapon:lnk_array_shield\"]")
    );

    let weapon_file =
        fs::read_to_string(database.path.join("weapon.drw")).expect("weapon drawer readable");
    assert!(weapon_file.contains("\"gems\":[\"@gem:lnk_array_storm\"]"));
    assert!(weapon_file.contains("\"gems\":[\"@gem:lnk_array_earth\"]"));

    let characters = engine
        .find_all("character")
        .expect("character lookup should succeed");
    assert_eq!(characters[0]["weapons"][0]["name"], "Spear");
    assert_eq!(characters[0]["weapons"][0]["gems"][0]["element"], "Storm");
    assert_eq!(characters[0]["weapons"][1]["name"], "Shield");
    assert_eq!(characters[0]["weapons"][1]["gems"][0]["element"], "Earth");
}

#[test]
fn us_020_delete_by_id_tombstones_record_and_hides_future_lookups() {
    let database = TempDatabase::new("us_020_delete_by_id");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
        engine
            .upsert(
                "gem",
                json!({
                    "_id": "@gem:lnk_delete_engine",
                    "element": "Solar",
                    "potency": 777
                }),
            )
            .expect("gem should upsert");

        let deleted = engine
            .delete_by_id("@gem:lnk_delete_engine")
            .expect("delete should succeed");
        assert!(deleted);
        assert!(
            engine
                .find_by_id("@gem:lnk_delete_engine")
                .expect("lookup should succeed")
                .is_none()
        );
        assert!(
            engine
                .find_all("gem")
                .expect("find all should succeed")
                .is_empty()
        );
    }

    let data_contents =
        fs::read_to_string(database.path.join("gem.drw")).expect("gem drawer should be readable");
    assert!(data_contents.contains("!!DEAD!!"));

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reinitialize");
    assert!(
        restarted_engine
            .find_by_id("@gem:lnk_delete_engine")
            .expect("lookup should succeed after restart")
            .is_none()
    );
}

#[test]
fn us_020_delete_by_id_returns_false_for_missing_record_in_existing_drawer() {
    let database = TempDatabase::new("us_020_delete_missing_record");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_existing",
                "element": "Solar"
            }),
        )
        .expect("gem should upsert");

    let deleted = engine
        .delete_by_id("@gem:lnk_missing")
        .expect("delete against existing drawer should succeed");

    assert!(!deleted);
}

#[test]
fn us_020_delete_by_id_errors_for_missing_drawer() {
    let database = TempDatabase::new("us_020_delete_missing_drawer");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let error = engine
        .delete_by_id("@missing:lnk_any")
        .expect_err("missing drawer should error");

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn us_030_cascade_delete_uses_metadata_rules_leaf_first() {
    let database = TempDatabase::new("us_030_cascade_delete");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
        engine
            .upsert(
                "character",
                json!({
                    "_id": "@character:lnk_cascade_owner",
                    "name": "Grace",
                    "weapons": [
                        {
                            "_id": "@weapon:lnk_cascade_spear",
                            "name": "Spear",
                            "gems": [
                                {
                                    "_id": "@gem:lnk_cascade_storm",
                                    "element": "Storm",
                                    "potency": 8080
                                }
                            ]
                        }
                    ]
                }),
            )
            .expect("character graph should upsert");
    }

    write_cascade_delete_rules(&database, "character", &["weapons"]);
    write_cascade_delete_rules(&database, "weapon", &["gems"]);

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reinitialize");
    let deleted = restarted_engine
        .delete_by_id("@character:lnk_cascade_owner")
        .expect("cascade delete should succeed");

    assert!(deleted);
    assert!(
        restarted_engine
            .find_by_id("@character:lnk_cascade_owner")
            .expect("character lookup should succeed")
            .is_none()
    );
    assert!(
        restarted_engine
            .find_by_id("@weapon:lnk_cascade_spear")
            .expect("weapon lookup should succeed")
            .is_none()
    );
    assert!(
        restarted_engine
            .find_by_id("@gem:lnk_cascade_storm")
            .expect("gem lookup should succeed")
            .is_none()
    );
}

#[test]
fn us_030_delete_preserves_links_not_configured_for_cascade() {
    let database = TempDatabase::new("us_030_preserve_non_cascade_links");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
        engine
            .upsert(
                "character",
                json!({
                    "_id": "@character:lnk_preserve_owner",
                    "name": "Grace",
                    "weapon": {
                        "_id": "@weapon:lnk_preserved_weapon",
                        "name": "Spear"
                    }
                }),
            )
            .expect("character graph should upsert");
    }

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reinitialize");
    restarted_engine
        .delete_by_id("@character:lnk_preserve_owner")
        .expect("delete should succeed");

    assert!(
        restarted_engine
            .find_by_id("@character:lnk_preserve_owner")
            .expect("character lookup should succeed")
            .is_none()
    );
    assert!(
        restarted_engine
            .find_by_id("@weapon:lnk_preserved_weapon")
            .expect("weapon lookup should succeed")
            .is_some()
    );
}

#[test]
fn find_by_id_handles_cyclic_links_without_recursive_overflow() {
    let database = TempDatabase::new("find_by_id_handles_cyclic_links");
    let database_directory = database.path.to_string_lossy().into_owned();

    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

#[test]
fn us_031_find_by_filter_matches_exact_properties_and_string_wildcards() {
    let database = TempDatabase::new("us_031_property_and_wildcard_matches");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_031_sunblade",
                "name": "Sunblade",
                "damage": 120
            }),
        )
        .expect("sunblade should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_031_moonblade",
                "name": "Moonblade",
                "damage": 90
            }),
        )
        .expect("moonblade should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_031_storm_spear",
                "name": "Storm Spear",
                "damage": 120
            }),
        )
        .expect("storm spear should upsert");

    let by_damage = engine
        .find_by_filter("weapon", json!({ "damage": 120 }), None)
        .expect("damage filter should succeed");
    assert_eq!(by_damage.len(), 2);

    let by_name = engine
        .find_by_filter("weapon", json!({ "name": "%blade" }), None)
        .expect("wildcard filter should succeed");
    assert_eq!(by_name.len(), 2);
    assert_eq!(by_name[0]["name"], "Sunblade");
    assert_eq!(by_name[1]["name"], "Moonblade");
}

#[test]
fn us_031_find_by_filter_matches_reference_ids_against_stored_pointers() {
    let database = TempDatabase::new("us_031_reference_matches");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_us_031_fire",
                "element": "Fire",
                "potency": 500
            }),
        )
        .expect("fire gem should upsert");
    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_us_031_ice",
                "element": "Ice",
                "potency": 300
            }),
        )
        .expect("ice gem should upsert");

    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_031_flare",
                "name": "Flare",
                "gem": { "_id": "@gem:lnk_us_031_fire" }
            }),
        )
        .expect("flare should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_031_frost",
                "name": "Frost",
                "gem": { "_id": "@gem:lnk_us_031_ice" }
            }),
        )
        .expect("frost should upsert");

    let matched = engine
        .find_by_filter("weapon", json!({ "gem": { "_id": "us_031_fire" } }), None)
        .expect("reference filter should succeed");

    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0]["name"], "Flare");
    assert_eq!(matched[0]["gem"]["element"], "Fire");
}

#[test]
fn us_031_find_by_filter_rejects_non_object_filters() {
    let database = TempDatabase::new("us_031_rejects_non_object_filters");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let error = engine
        .find_by_filter("weapon", json!(["not", "an", "object"]), None)
        .expect_err("non-object filter should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn us_032_count_uses_metadata_fast_path_when_no_filter_is_provided() {
    let database = TempDatabase::new("us_032_count_no_filter");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_032_flare",
                "name": "Flare",
                "gem": "@missing:lnk_unresolved"
            }),
        )
        .expect("first weapon should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_032_frost",
                "name": "Frost"
            }),
        )
        .expect("second weapon should upsert");

    let total = engine
        .count("weapon", None, None)
        .expect("count without filter should succeed");

    assert_eq!(total, 2);
    assert!(!database.path.join("missing.drw").exists());
    assert!(!database.path.join("missing_index.drw").exists());
}

#[test]
fn us_032_count_matches_filter_semantics_without_hydrating_records() {
    let database = TempDatabase::new("us_032_count_filtered_matches");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_us_032_fire",
                "element": "Fire",
                "potency": 500
            }),
        )
        .expect("fire gem should upsert");
    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_us_032_ice",
                "element": "Ice",
                "potency": 300
            }),
        )
        .expect("ice gem should upsert");

    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_032_blaze",
                "name": "Blazeblade",
                "damage": 120,
                "gem": { "_id": "@gem:lnk_us_032_fire" }
            }),
        )
        .expect("blazeblade should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_032_frost",
                "name": "Frostblade",
                "damage": 90,
                "gem": { "_id": "@gem:lnk_us_032_ice" }
            }),
        )
        .expect("frostblade should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_032_storm",
                "name": "Storm Spear",
                "damage": 120,
                "gem": { "_id": "@gem:lnk_us_032_fire" }
            }),
        )
        .expect("storm spear should upsert");

    let wildcard_count = engine
        .count("weapon", Some(json!({ "name": "%blade" })), None)
        .expect("wildcard count should succeed");
    assert_eq!(wildcard_count, 2);

    let reference_count = engine
        .count(
            "weapon",
            Some(json!({ "gem": { "_id": "us_032_fire" } })),
            None,
        )
        .expect("reference count should succeed");
    assert_eq!(reference_count, 2);

    let exact_count = engine
        .count("weapon", Some(json!({ "damage": 90 })), None)
        .expect("exact count should succeed");
    assert_eq!(exact_count, 1);
}

#[test]
fn us_032_count_rejects_non_object_filters() {
    let database = TempDatabase::new("us_032_count_rejects_non_object_filter");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let error = engine
        .count("weapon", Some(json!(["not", "an", "object"])), None)
        .expect_err("non-object filter should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn us_033_find_by_filter_applies_sorting_before_offset_and_limit() {
    let database = TempDatabase::new("us_033_sort_offset_limit");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    for (id, name, damage) in [
        ("low", "Training Blade", 10),
        ("high", "Sunblade", 40),
        ("mid", "Moonblade", 20),
        ("higher", "Stormblade", 30),
    ] {
        engine
            .upsert(
                "weapon",
                json!({
                    "_id": format!("@weapon:lnk_us_033_{id}"),
                    "name": name,
                    "damage": damage
                }),
            )
            .expect("weapon should upsert");
    }

    let records = engine
        .find_by_filter(
            "weapon",
            json!({ "name": "%blade" }),
            Some(QueryModifiers {
                order_by: Some("damage".to_string()),
                order_direction: Some(OrderDirection::Descending),
                offset: Some(1),
                limit: Some(2),
            }),
        )
        .expect("filtered query should succeed");

    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["name"], "Stormblade");
    assert_eq!(records[1]["name"], "Moonblade");
}

#[test]
fn us_033_sorting_pushes_missing_and_mixed_type_fields_to_the_end() {
    let database = TempDatabase::new("us_033_sort_missing_and_mixed");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    for payload in [
        json!({
            "_id": "@weapon:lnk_us_033_damage_20",
            "name": "Twenty",
            "damage": 20
        }),
        json!({
            "_id": "@weapon:lnk_us_033_damage_text",
            "name": "Text Damage",
            "damage": "unknown"
        }),
        json!({
            "_id": "@weapon:lnk_us_033_damage_missing",
            "name": "Missing Damage"
        }),
        json!({
            "_id": "@weapon:lnk_us_033_damage_30",
            "name": "Thirty",
            "damage": 30
        }),
    ] {
        engine
            .upsert("weapon", payload)
            .expect("weapon should upsert");
    }

    let records = engine
        .find_by_filter(
            "weapon",
            json!({}),
            Some(QueryModifiers {
                order_by: Some("damage".to_string()),
                order_direction: Some(OrderDirection::Descending),
                offset: None,
                limit: None,
            }),
        )
        .expect("query should succeed");

    let names = records
        .iter()
        .map(|record| record["name"].as_str().expect("name should be a string"))
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        vec!["Thirty", "Twenty", "Text Damage", "Missing Damage"]
    );
}

#[test]
fn us_033_count_ignores_query_pagination_modifiers() {
    let database = TempDatabase::new("us_033_count_ignores_modifiers");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    for i in 0..3 {
        engine
            .upsert(
                "weapon",
                json!({
                    "_id": format!("@weapon:lnk_us_033_count_{i}"),
                    "name": format!("Blade {i}"),
                    "damage": i
                }),
            )
            .expect("weapon should upsert");
    }

    let count = engine
        .count(
            "weapon",
            Some(json!({ "name": "Blade %" })),
            Some(QueryModifiers {
                order_by: Some("damage".to_string()),
                order_direction: Some(OrderDirection::Descending),
                offset: Some(1),
                limit: Some(1),
            }),
        )
        .expect("count should succeed");

    assert_eq!(count, 3);
}

#[test]
fn us_034_execute_routes_commands_to_nested_tenant_database_schema_paths() {
    let database = TempDatabase::new("us_034_execute_routes_nested_paths");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");
    let coordinate = StorageCoordinate::new("tenant_1", "production_db", "core_schema");

    let result = engine
        .execute(
            coordinate.clone(),
            Command::Upsert {
                drawer_name: "weapon".to_string(),
                payload: json!({
                    "_id": "@weapon:lnk_us_034_blade",
                    "name": "Tenant Blade"
                }),
            },
        )
        .expect("routed upsert should succeed");

    assert_eq!(
        result,
        CommandResult::Pointer("@weapon:lnk_us_034_blade".to_string())
    );

    assert!(
        database
            .path
            .join("tenant_1")
            .join("production_db")
            .join("core_schema")
            .join("weapon.drw")
            .is_file()
    );
    assert!(!database.path.join("weapon.drw").exists());

    let result = engine
        .execute(
            coordinate,
            Command::FindAll {
                drawer_name: "weapon".to_string(),
            },
        )
        .expect("routed find all should succeed");

    let CommandResult::Records(records) = result else {
        panic!("expected records result");
    };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "Tenant Blade");
}

#[test]
fn us_034_storage_coordinates_isolate_neighboring_tenants() {
    let database = TempDatabase::new("us_034_isolates_neighboring_tenants");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");
    let tenant_a = StorageCoordinate::new("tenant_a", "production_db", "core_schema");
    let tenant_b = StorageCoordinate::new("tenant_b", "production_db", "core_schema");

    for (coordinate, name) in [
        (tenant_a.clone(), "Tenant A Blade"),
        (tenant_b.clone(), "Tenant B Blade"),
    ] {
        engine
            .execute(
                coordinate,
                Command::Upsert {
                    drawer_name: "weapon".to_string(),
                    payload: json!({
                        "_id": "@weapon:lnk_shared_key",
                        "name": name
                    }),
                },
            )
            .expect("routed upsert should succeed");
    }

    let deleted = engine
        .execute(
            tenant_a.clone(),
            Command::Delete {
                pointer: "@weapon:lnk_shared_key".to_string(),
            },
        )
        .expect("routed delete should succeed");
    assert_eq!(deleted, CommandResult::Deleted(true));

    let tenant_a_count = engine
        .execute(
            tenant_a,
            Command::Count {
                drawer_name: "weapon".to_string(),
                filter: None,
                modifiers: None,
            },
        )
        .expect("tenant a count should succeed");
    let tenant_b_count = engine
        .execute(
            tenant_b.clone(),
            Command::Count {
                drawer_name: "weapon".to_string(),
                filter: None,
                modifiers: None,
            },
        )
        .expect("tenant b count should succeed");

    assert_eq!(tenant_a_count, CommandResult::Count(0));
    assert_eq!(tenant_b_count, CommandResult::Count(1));

    let tenant_b_record = engine
        .execute(
            tenant_b,
            Command::FindById {
                pointer: "@weapon:lnk_shared_key".to_string(),
            },
        )
        .expect("tenant b lookup should succeed");

    let CommandResult::Record(Some(record)) = tenant_b_record else {
        panic!("expected tenant b record");
    };
    assert_eq!(record["name"], "Tenant B Blade");
}

#[test]
fn us_034_routed_nested_objects_and_hydration_stay_inside_coordinate() {
    let database = TempDatabase::new("us_034_nested_hydration_stays_routed");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");
    let coordinate = StorageCoordinate::new("tenant_nested", "production_db", "core_schema");

    engine
        .execute(
            coordinate.clone(),
            Command::Upsert {
                drawer_name: "weapon".to_string(),
                payload: json!({
                    "_id": "@weapon:lnk_us_034_staff",
                    "name": "Routed Staff",
                    "gem": {
                        "_id": "@gem:lnk_us_034_gem",
                        "element": "Route",
                        "potency": 700
                    }
                }),
            },
        )
        .expect("routed nested upsert should succeed");

    assert!(
        database
            .path
            .join("tenant_nested")
            .join("production_db")
            .join("core_schema")
            .join("gem.drw")
            .is_file()
    );
    assert!(!database.path.join("gem.drw").exists());

    let result = engine
        .execute(
            coordinate,
            Command::FindByFilter {
                drawer_name: "weapon".to_string(),
                filter: json!({ "gem": { "_id": "us_034_gem" } }),
                modifiers: None,
            },
        )
        .expect("routed filtered query should succeed");

    let CommandResult::Records(records) = result else {
        panic!("expected records result");
    };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["gem"]["element"], "Route");
}

#[test]
fn us_034_storage_coordinate_rejects_path_traversal_segments() {
    let database = TempDatabase::new("us_034_rejects_path_traversal");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");

    let error = engine
        .execute(
            StorageCoordinate::new("tenant", "..", "schema"),
            Command::Count {
                drawer_name: "weapon".to_string(),
                filter: None,
                modifiers: None,
            },
        )
        .expect_err("path traversal coordinate should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn us_036_database_level_isolation_uses_independent_open_paths() {
    let tenant_a_database = TempDatabase::new("us_036_database_tenant_a");
    let tenant_b_database = TempDatabase::new("us_036_database_tenant_b");
    let tenant_a_directory = tenant_a_database.path.to_string_lossy().into_owned();
    let tenant_b_directory = tenant_b_database.path.to_string_lossy().into_owned();

    let tenant_a_engine =
        WardrobeEngine::open(&tenant_a_directory).expect("tenant a should initialize");
    let tenant_b_engine =
        WardrobeEngine::open(&tenant_b_directory).expect("tenant b should initialize");

    tenant_a_engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_shared_database_key",
                "name": "Tenant A Blade"
            }),
        )
        .expect("tenant a weapon should upsert");
    tenant_b_engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_shared_database_key",
                "name": "Tenant B Blade"
            }),
        )
        .expect("tenant b weapon should upsert");

    let tenant_a_record = tenant_a_engine
        .find_by_id("@weapon:lnk_shared_database_key")
        .expect("tenant a lookup should succeed")
        .expect("tenant a record should exist");
    let tenant_b_record = tenant_b_engine
        .find_by_id("@weapon:lnk_shared_database_key")
        .expect("tenant b lookup should succeed")
        .expect("tenant b record should exist");

    assert_eq!(tenant_a_record["name"], "Tenant A Blade");
    assert_eq!(tenant_b_record["name"], "Tenant B Blade");
    assert!(tenant_a_database.path.join("weapon.drw").is_file());
    assert!(tenant_b_database.path.join("weapon.drw").is_file());
}

#[test]
fn us_036_schema_level_isolation_uses_nested_database_schema_folders() {
    let database = TempDatabase::new("us_036_schema_level");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let schema_scope = StorageScope::schema("main_db", "tenant_1");

    {
        let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");
        engine
            .execute_in_scope(
                schema_scope.clone(),
                Command::Upsert {
                    drawer_name: "gem".to_string(),
                    payload: json!({
                        "_id": "@gem:lnk_schema_fire",
                        "element": "Schema Fire"
                    }),
                },
            )
            .expect("schema-scoped upsert should succeed");
    }

    assert!(
        database
            .path
            .join("main_db")
            .join("tenant_1")
            .join("gem.drw")
            .is_file()
    );
    assert!(!database.path.join("gem.drw").exists());
    assert!(!database.path.join("main_db").join("gem.drw").exists());

    let restarted_engine = WardrobeEngine::open(&storage_pool).expect("engine should reinitialize");
    let count = restarted_engine
        .execute_in_scope(
            schema_scope.clone(),
            Command::Count {
                drawer_name: "gem".to_string(),
                filter: None,
                modifiers: None,
            },
        )
        .expect("schema-scoped count should succeed");
    assert_eq!(count, CommandResult::Count(1));

    let records = restarted_engine
        .execute_in_scope(
            schema_scope,
            Command::FindAll {
                drawer_name: "gem".to_string(),
            },
        )
        .expect("schema-scoped find_all should succeed");

    let CommandResult::Records(records) = records else {
        panic!("expected records result");
    };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["element"], "Schema Fire");
}

#[test]
fn us_036_drawer_level_isolation_uses_prefixed_drawer_files() {
    let database = TempDatabase::new("us_036_drawer_level");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let tenant_a_scope = StorageScope::drawer("tenant1");
    let tenant_b_scope = StorageScope::drawer("tenant2");
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");

    for (scope, name) in [
        (tenant_a_scope.clone(), "Tenant 1 Gem"),
        (tenant_b_scope.clone(), "Tenant 2 Gem"),
    ] {
        engine
            .execute_in_scope(
                scope,
                Command::Upsert {
                    drawer_name: "gem".to_string(),
                    payload: json!({
                        "_id": "@gem:lnk_shared_drawer_key",
                        "element": name
                    }),
                },
            )
            .expect("drawer-scoped upsert should succeed");
    }

    assert!(database.path.join("tenant1_gem.drw").is_file());
    assert!(database.path.join("tenant2_gem.drw").is_file());
    assert!(!database.path.join("gem.drw").exists());

    let tenant_a_record = engine
        .execute_in_scope(
            tenant_a_scope,
            Command::FindById {
                pointer: "@gem:lnk_shared_drawer_key".to_string(),
            },
        )
        .expect("tenant 1 lookup should succeed");
    let tenant_b_record = engine
        .execute_in_scope(
            tenant_b_scope,
            Command::FindById {
                pointer: "@gem:lnk_shared_drawer_key".to_string(),
            },
        )
        .expect("tenant 2 lookup should succeed");

    let CommandResult::Record(Some(tenant_a_record)) = tenant_a_record else {
        panic!("expected tenant 1 record");
    };
    let CommandResult::Record(Some(tenant_b_record)) = tenant_b_record else {
        panic!("expected tenant 2 record");
    };

    assert_eq!(tenant_a_record["element"], "Tenant 1 Gem");
    assert_eq!(tenant_b_record["element"], "Tenant 2 Gem");
}

#[test]
fn us_036_drawer_level_nested_records_and_filters_stay_namespaced() {
    let database = TempDatabase::new("us_036_drawer_level_nested");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let scope = StorageScope::drawer("tenant_graph");
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");

    engine
        .execute_in_scope(
            scope.clone(),
            Command::Upsert {
                drawer_name: "weapon".to_string(),
                payload: json!({
                    "_id": "@weapon:lnk_graph_staff",
                    "name": "Graph Staff",
                    "gem": {
                        "_id": "@gem:lnk_graph_fire",
                        "element": "Graph Fire",
                        "potency": 999
                    }
                }),
            },
        )
        .expect("drawer-scoped nested upsert should succeed");

    assert!(database.path.join("tenant_graph_weapon.drw").is_file());
    assert!(database.path.join("tenant_graph_gem.drw").is_file());
    assert!(!database.path.join("weapon.drw").exists());
    assert!(!database.path.join("gem.drw").exists());

    let weapon_file = fs::read_to_string(database.path.join("tenant_graph_weapon.drw"))
        .expect("weapon drawer should be readable");
    assert!(weapon_file.contains("\"gem\":\"@tenant_graph_gem:lnk_graph_fire\""));

    let result = engine
        .execute_in_scope(
            scope,
            Command::FindByFilter {
                drawer_name: "weapon".to_string(),
                filter: json!({ "gem": { "_id": "graph_fire" } }),
                modifiers: None,
            },
        )
        .expect("drawer-scoped reference filter should succeed");

    let CommandResult::Records(records) = result else {
        panic!("expected records result");
    };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "Graph Staff");
    assert_eq!(records[0]["gem"]["element"], "Graph Fire");
}

#[test]
fn us_037_one_to_one_blocks_duplicate_pointer_and_many_to_one_allows_shared_targets() {
    let database = TempDatabase::new("us_037_relationship_cardinality");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    write_drawer_metadata(
        &database,
        "weapon",
        json!({
            "format_version": 1,
            "primary_key": "_id",
            "record_count": 0,
            "unique_constraints": [],
            "relationship_constraints": {
                "gem_slot": {
                    "type": "1:1",
                    "target_drawer": "gem"
                },
                "faction_id": {
                    "type": "M:1",
                    "target_drawer": "faction"
                }
            },
            "delete_rules": {},
            "cascade_delete_rules": {}
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_037_sword",
                "name": "Constraint Sword",
                "gem_slot": { "_id": "us_037_fire" },
                "faction_id": "@faction:lnk_us_037_order"
            }),
        )
        .expect("first constrained weapon should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_037_spear",
                "name": "Constraint Spear",
                "gem_slot": { "_id": "us_037_water" },
                "faction_id": "@faction:lnk_us_037_order"
            }),
        )
        .expect("many-to-one faction target should allow duplicate references");

    let weapon_file = fs::read_to_string(database.path.join("weapon.drw"))
        .expect("weapon drawer should be readable");
    assert!(weapon_file.contains("\"gem_slot\":\"@gem:lnk_us_037_fire\""));

    let duplicate_error = engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_037_axe",
                "name": "Constraint Axe",
                "gem_slot": "@gem:lnk_us_037_fire",
                "faction_id": "@faction:lnk_us_037_order"
            }),
        )
        .expect_err("duplicate one-to-one pointer should fail");

    assert_eq!(duplicate_error.kind(), std::io::ErrorKind::InvalidData);
    assert!(duplicate_error.to_string().contains("1:1 relationship"));
    assert_eq!(
        engine
            .count("weapon", None, None)
            .expect("count should succeed"),
        2
    );
}

#[test]
fn us_037_relationship_constraints_reject_wrong_target_drawer_pointers() {
    let database = TempDatabase::new("us_037_wrong_target_drawer");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    write_drawer_metadata(
        &database,
        "weapon",
        json!({
            "format_version": 1,
            "primary_key": "_id",
            "record_count": 0,
            "unique_constraints": [],
            "relationship_constraints": {
                "faction_id": {
                    "type": "M:1",
                    "target_drawer": "faction"
                }
            },
            "delete_rules": {},
            "cascade_delete_rules": {}
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let error = engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_037_wrong_target",
                "name": "Wrong Target",
                "faction_id": "@gem:lnk_us_037_not_a_faction"
            }),
        )
        .expect_err("wrong target drawer should fail validation");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error
            .to_string()
            .contains("expected target drawer 'faction'")
    );
}

#[test]
fn us_038_one_to_many_virtual_relationships_populate_child_arrays_on_read() {
    let database = TempDatabase::new("us_038_virtual_one_to_many");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    write_drawer_metadata(
        &database,
        "character",
        json!({
            "format_version": 1,
            "primary_key": "_id",
            "record_count": 0,
            "unique_constraints": [],
            "relationship_constraints": {
                "equipped_weapons": {
                    "type": "1:M",
                    "target_drawer": "weapon",
                    "mapped_by": "character"
                }
            },
            "delete_rules": {},
            "cascade_delete_rules": {}
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "character",
            json!({
                "_id": "@character:lnk_us_038_mech",
                "name": "Mech Pilot"
            }),
        )
        .expect("character should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_038_lance",
                "name": "Pilot Lance",
                "character": { "_id": "@character:lnk_us_038_mech" }
            }),
        )
        .expect("first child weapon should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_038_blade",
                "name": "Pilot Blade",
                "character": { "_id": "@character:lnk_us_038_mech" }
            }),
        )
        .expect("second child weapon should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_038_other",
                "name": "Other Blade",
                "character": { "_id": "@character:lnk_us_038_other" }
            }),
        )
        .expect("unrelated child weapon should upsert");

    let characters = engine
        .find_all("character")
        .expect("characters should read with virtual relationships");

    assert_eq!(characters.len(), 1);
    let equipped_weapons = characters[0]["equipped_weapons"]
        .as_array()
        .expect("virtual relationship should hydrate as an array");
    let weapon_names = equipped_weapons
        .iter()
        .map(|weapon| weapon["name"].as_str().expect("weapon should have name"))
        .collect::<Vec<_>>();

    assert_eq!(weapon_names, vec!["Pilot Lance", "Pilot Blade"]);
}

#[test]
fn us_038_many_to_many_pointer_arrays_validate_targets_and_hydrate() {
    let database = TempDatabase::new("us_038_many_to_many");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    write_drawer_metadata(
        &database,
        "character",
        json!({
            "format_version": 1,
            "primary_key": "_id",
            "record_count": 0,
            "unique_constraints": [],
            "relationship_constraints": {
                "shared_skills": {
                    "type": "M:M",
                    "target_drawer": "skill"
                }
            },
            "delete_rules": {},
            "cascade_delete_rules": {}
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    for (id, name) in [("dash", "Dash"), ("guard", "Guard")] {
        engine
            .upsert(
                "skill",
                json!({
                    "_id": format!("@skill:lnk_us_038_{id}"),
                    "name": name
                }),
            )
            .expect("skill should upsert");
    }

    engine
        .upsert(
            "character",
            json!({
                "_id": "@character:lnk_us_038_skilled",
                "name": "Skilled Character",
                "shared_skills": [
                    "@skill:lnk_us_038_dash",
                    "@skill:lnk_us_038_guard"
                ]
            }),
        )
        .expect("many-to-many pointer array should upsert");

    let error = engine
        .upsert(
            "character",
            json!({
                "_id": "@character:lnk_us_038_wrong_skill",
                "name": "Wrong Skill Character",
                "shared_skills": [
                    "@gem:lnk_us_038_wrong_target"
                ]
            }),
        )
        .expect_err("wrong many-to-many pointer target should fail");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("expected target drawer 'skill'"));

    let characters = engine
        .find_all("character")
        .expect("character should read with hydrated many-to-many pointers");
    assert_eq!(characters.len(), 1);
    assert_eq!(characters[0]["shared_skills"][0]["name"], "Dash");
    assert_eq!(characters[0]["shared_skills"][1]["name"], "Guard");
}

#[test]
fn us_039_cascade_delete_rule_removes_child_records_tracking_parent_pointer() {
    let database = TempDatabase::new("us_039_cascade_delete_rule");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    write_drawer_metadata(
        &database,
        "character",
        json!({
            "format_version": 1,
            "primary_key": "_id",
            "record_count": 0,
            "unique_constraints": [],
            "relationship_constraints": {
                "equipped_weapons": {
                    "type": "1:M",
                    "target_drawer": "weapon",
                    "mapped_by": "character"
                }
            },
            "delete_rules": {
                "equipped_weapons": {
                    "action": "Cascade"
                }
            },
            "cascade_delete_rules": {}
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "character",
            json!({
                "_id": "@character:lnk_us_039_cascade_parent",
                "name": "Cascade Parent"
            }),
        )
        .expect("character should upsert");
    for (id, name) in [("blade", "Cascade Blade"), ("lance", "Cascade Lance")] {
        engine
            .upsert(
                "weapon",
                json!({
                    "_id": format!("@weapon:lnk_us_039_cascade_{id}"),
                    "name": name,
                    "character": { "_id": "@character:lnk_us_039_cascade_parent" }
                }),
            )
            .expect("weapon should upsert");
    }

    let deleted = engine
        .delete_by_id("@character:lnk_us_039_cascade_parent")
        .expect("cascade delete should succeed");

    assert!(deleted);
    assert_eq!(
        engine
            .count("character", None, None)
            .expect("character count should succeed"),
        0
    );
    assert_eq!(
        engine
            .count("weapon", None, None)
            .expect("weapon count should succeed"),
        0
    );
}

#[test]
fn us_039_restrict_delete_rule_aborts_before_cascade_mutations() {
    let database = TempDatabase::new("us_039_restrict_delete_rule");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    write_drawer_metadata(
        &database,
        "character",
        json!({
            "format_version": 1,
            "primary_key": "_id",
            "record_count": 0,
            "unique_constraints": [],
            "relationship_constraints": {
                "equipped_weapons": {
                    "type": "1:M",
                    "target_drawer": "weapon",
                    "mapped_by": "character"
                },
                "critical_weapons": {
                    "type": "1:M",
                    "target_drawer": "weapon",
                    "mapped_by": "character"
                }
            },
            "delete_rules": {
                "equipped_weapons": {
                    "action": "Cascade"
                },
                "critical_weapons": {
                    "action": "Restrict"
                }
            },
            "cascade_delete_rules": {}
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "character",
            json!({
                "_id": "@character:lnk_us_039_restrict_parent",
                "name": "Restrict Parent"
            }),
        )
        .expect("character should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_039_restrict_child",
                "name": "Restrict Child",
                "character": { "_id": "@character:lnk_us_039_restrict_parent" }
            }),
        )
        .expect("weapon should upsert");

    let error = engine
        .delete_by_id("@character:lnk_us_039_restrict_parent")
        .expect_err("restrict rule should block delete");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("Delete restricted"));
    assert_eq!(
        engine
            .count("character", None, None)
            .expect("character count should succeed"),
        1
    );
    assert_eq!(
        engine
            .count("weapon", None, None)
            .expect("weapon count should succeed"),
        1
    );
}

#[test]
fn us_039_set_null_delete_rule_clears_child_pointer_and_preserves_child_record() {
    let database = TempDatabase::new("us_039_set_null_delete_rule");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    write_drawer_metadata(
        &database,
        "character",
        json!({
            "format_version": 1,
            "primary_key": "_id",
            "record_count": 0,
            "unique_constraints": [],
            "relationship_constraints": {
                "assigned_weapons": {
                    "type": "1:M",
                    "target_drawer": "weapon",
                    "mapped_by": "character"
                }
            },
            "delete_rules": {
                "assigned_weapons": {
                    "action": "SetNull"
                }
            },
            "cascade_delete_rules": {}
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "character",
            json!({
                "_id": "@character:lnk_us_039_set_null_parent",
                "name": "SetNull Parent"
            }),
        )
        .expect("character should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_us_039_set_null_child",
                "name": "SetNull Child",
                "character": { "_id": "@character:lnk_us_039_set_null_parent" }
            }),
        )
        .expect("weapon should upsert");

    let deleted = engine
        .delete_by_id("@character:lnk_us_039_set_null_parent")
        .expect("set-null delete should succeed");

    assert!(deleted);
    assert_eq!(
        engine
            .count("character", None, None)
            .expect("character count should succeed"),
        0
    );
    assert_eq!(
        engine
            .count("weapon", None, None)
            .expect("weapon count should succeed"),
        1
    );

    let weapon = engine
        .find_by_id("@weapon:lnk_us_039_set_null_child")
        .expect("weapon lookup should succeed")
        .expect("weapon should remain");
    assert_eq!(weapon["name"], "SetNull Child");
    assert!(weapon.get("character").is_none());
}

#[test]
fn us_040_shared_engine_allows_concurrent_reader_threads() {
    let database = TempDatabase::new("us_040_concurrent_readers");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = Arc::new(WardrobeEngine::open(&database_directory).expect("engine should open"));

    for i in 0..12 {
        engine
            .upsert(
                "gem",
                json!({
                    "_id": format!("@gem:lnk_us_040_reader_{i}"),
                    "element": if i % 2 == 0 { "Fire" } else { "Water" },
                    "potency": i
                }),
            )
            .expect("seed gem should upsert");
    }

    let reader_count = 8;
    let barrier = Arc::new(Barrier::new(reader_count));
    let handles = (0..reader_count)
        .map(|thread_index| {
            let engine = Arc::clone(&engine);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();

                let total = engine
                    .count("gem", None, None)
                    .expect("count should succeed concurrently");
                assert_eq!(total, 12);

                let filtered = engine
                    .find_by_filter("gem", json!({ "element": "Fire" }), None)
                    .expect("filter should succeed concurrently");
                assert_eq!(filtered.len(), 6);

                let found = engine
                    .find_by_id(&format!("@gem:lnk_us_040_reader_{}", thread_index % 4))
                    .expect("find_by_id should succeed concurrently")
                    .expect("gem should exist");
                assert!(found["element"] == "Fire" || found["element"] == "Water");
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().expect("reader thread should not panic");
    }
}

#[test]
fn us_040_shared_engine_serializes_competing_writer_threads() {
    let database = TempDatabase::new("us_040_competing_writers");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = Arc::new(WardrobeEngine::open(&database_directory).expect("engine should open"));

    for i in 0..10 {
        engine
            .upsert(
                "gem",
                json!({
                    "_id": format!("@gem:lnk_us_040_delete_{i}"),
                    "element": "Old",
                    "potency": i
                }),
            )
            .expect("seed gem should upsert");
    }

    let writer_count = 20;
    let barrier = Arc::new(Barrier::new(writer_count));
    let handles = (0..writer_count)
        .map(|thread_index| {
            let engine = Arc::clone(&engine);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();

                if thread_index < 10 {
                    let deleted = engine
                        .delete_by_id(&format!("@gem:lnk_us_040_delete_{thread_index}"))
                        .expect("delete should succeed concurrently");
                    assert!(deleted);
                } else {
                    engine
                        .upsert(
                            "gem",
                            json!({
                                "_id": format!("@gem:lnk_us_040_insert_{thread_index}"),
                                "element": "New",
                                "potency": thread_index
                            }),
                        )
                        .expect("upsert should succeed concurrently");
                }
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().expect("writer thread should not panic");
    }

    let total = engine
        .count("gem", None, None)
        .expect("final count should succeed");
    assert_eq!(total, 10);

    let new_records = engine
        .find_by_filter("gem", json!({ "element": "New" }), None)
        .expect("new records should query");
    assert_eq!(new_records.len(), 10);
}

#[test]
fn us_041_mutations_append_durable_wal_begin_and_commit_records() {
    let database = TempDatabase::new("us_041_mutation_wal");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should open");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_us_041_logged",
                "element": "Logged",
                "potency": 41
            }),
        )
        .expect("logged upsert should succeed");

    let records = wal_records(&database);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["event"], "begin");
    assert_eq!(records[0]["operation"]["type"], "upsert");
    assert_eq!(records[0]["operation"]["drawer_name"], "gem");
    assert_eq!(
        records[0]["operation"]["payload"]["_id"],
        "@gem:lnk_us_041_logged"
    );
    assert_eq!(records[1]["event"], "commit");
    assert_eq!(records[1]["tx_id"], records[0]["tx_id"]);
}

#[test]
fn us_041_open_replays_incomplete_upsert_intention_from_wal() {
    let database = TempDatabase::new("us_041_replay_upsert");
    write_wal_record(
        &database,
        json!({
            "event": "begin",
            "tx_id": "manual-upsert",
            "operation": {
                "type": "upsert",
                "drawer_name": "gem",
                "payload": {
                    "_id": "@gem:lnk_us_041_replayed",
                    "element": "Replay",
                    "potency": 4100
                }
            }
        }),
    );

    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should recover");
    let found = engine
        .find_by_id("@gem:lnk_us_041_replayed")
        .expect("lookup should succeed")
        .expect("replayed record should exist");

    assert_eq!(found["element"], "Replay");
    assert_eq!(found["potency"], 4100);

    let records = wal_records(&database);
    assert!(
        records
            .iter()
            .any(|record| record["event"] == "commit" && record["tx_id"] == "manual-upsert")
    );
}

#[test]
fn us_041_open_replays_incomplete_cascading_delete_intention_from_wal() {
    let database = TempDatabase::new("us_041_replay_cascade_delete");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    write_drawer_metadata(
        &database,
        "character",
        json!({
            "format_version": 1,
            "primary_key": "_id",
            "record_count": 0,
            "unique_constraints": [],
            "relationship_constraints": {
                "equipped_weapons": {
                    "type": "1:M",
                    "target_drawer": "weapon",
                    "mapped_by": "character"
                }
            },
            "delete_rules": {
                "equipped_weapons": {
                    "action": "Cascade"
                }
            },
            "cascade_delete_rules": {}
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let engine = WardrobeEngine::open(&database_directory).expect("engine should open");
        engine
            .upsert(
                "character",
                json!({
                    "_id": "@character:lnk_us_041_cascade_parent",
                    "name": "Wal Cascade Parent"
                }),
            )
            .expect("character should upsert");
        engine
            .upsert(
                "weapon",
                json!({
                    "_id": "@weapon:lnk_us_041_cascade_child",
                    "name": "Wal Cascade Child",
                    "character": { "_id": "@character:lnk_us_041_cascade_parent" }
                }),
            )
            .expect("weapon should upsert");
    }

    write_wal_record(
        &database,
        json!({
            "event": "begin",
            "tx_id": "manual-cascade-delete",
            "operation": {
                "type": "delete_by_id",
                "pointer": "@character:lnk_us_041_cascade_parent"
            }
        }),
    );

    let recovered_engine =
        WardrobeEngine::open(&database_directory).expect("engine should recover delete");

    assert_eq!(
        recovered_engine
            .count("character", None, None)
            .expect("character count should succeed"),
        0
    );
    assert_eq!(
        recovered_engine
            .count("weapon", None, None)
            .expect("weapon count should succeed"),
        0
    );

    let records = wal_records(&database);
    assert!(records.iter().any(
        |record| record["event"] == "commit" && record["tx_id"] == "manual-cascade-delete"
    ));
}

#[test]
fn us_041_failed_mutations_append_abort_and_do_not_replay_on_open() {
    let database = TempDatabase::new("us_041_abort_failed_mutation");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let engine = WardrobeEngine::open(&database_directory).expect("engine should open");
        let error = engine
            .upsert("gem", json!(["not", "an", "object"]))
            .expect_err("invalid mutation should fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    let records = wal_records(&database);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["event"], "begin");
    assert_eq!(records[1]["event"], "abort");
    assert_eq!(records[1]["tx_id"], records[0]["tx_id"]);

    let recovered_engine =
        WardrobeEngine::open(&database_directory).expect("aborted wal should not replay");
    assert_eq!(
        recovered_engine
            .count("gem", None, None)
            .expect("count should succeed"),
        0
    );
}

#[test]
fn us_035_engine_rejects_schema_violations_as_invalid_data() {
    let database = TempDatabase::new("us_035_engine_schema_invalid_data");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    write_drawer_metadata(
        &database,
        "weapon",
        json!({
            "format_version": 1,
            "primary_key": "_id",
            "record_count": 0,
            "unique_constraints": [],
            "relationship_constraints": {},
            "delete_rules": {},
            "cascade_delete_rules": {},
            "schema": {
                "type": "object",
                "required": ["_id", "name", "damage"],
                "properties": {
                    "_id": { "type": "string" },
                    "name": { "type": "string" },
                    "damage": { "type": "integer" }
                }
            }
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let error = engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_schema_invalid",
                "name": "Schema Blade",
                "damage": "heavy"
            }),
        )
        .expect_err("schema violation should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error
            .to_string()
            .contains("$.damage must be of type integer")
    );
    assert_eq!(
        engine
            .count("weapon", None, None)
            .expect("count should succeed"),
        0
    );

    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_schema_valid",
                "name": "Schema Blade",
                "damage": 42
            }),
        )
        .expect("valid schema record should write");
    assert_eq!(
        engine
            .count("weapon", None, None)
            .expect("count should succeed"),
        1
    );
}
