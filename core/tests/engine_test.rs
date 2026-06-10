mod common;

use common::TempDatabase;
use serde_json::json;
use std::fs;
use std::path::Path;
use wardrobe_core::{OrderDirection, QueryModifiers, WardrobeEngine};

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

#[test]
fn find_all_loads_existing_drawer_files_after_restart() {
    let database = TempDatabase::new("find_all_loads_existing_drawer_files");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let mut engine =
            WardrobeEngine::open(&database_directory).expect("database should initialize");
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

    let mut restarted_engine =
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
    let mut engine = WardrobeEngine::open(&database_directory).expect("database should initialize");

    let error = engine
        .upsert("gem", json!(["not", "an", "object"]))
        .expect_err("non-object payload should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn find_by_id_returns_none_for_missing_drawer_and_does_not_create_files() {
    let database = TempDatabase::new("find_by_id_missing_drawer");
    let database_directory = database.path.to_string_lossy().into_owned();
    let mut engine = WardrobeEngine::open(&database_directory).expect("database should initialize");

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
    let mut engine = WardrobeEngine::open(&database_directory).expect("database should initialize");

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
        let mut engine =
            WardrobeEngine::open(&database_directory).expect("database should initialize");
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

    let mut restarted_engine =
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
        let mut engine =
            WardrobeEngine::open(&database_directory).expect("database should initialize");
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

    let mut restarted_engine =
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
        let mut engine =
            WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let mut restarted_engine =
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
    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

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
        let mut engine =
            WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let mut restarted_engine =
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

    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
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
        let mut engine =
            WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let mut restarted_engine =
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
    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

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
    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

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
        let mut engine =
            WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let mut restarted_engine =
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
        let mut engine =
            WardrobeEngine::open(&database_directory).expect("engine should initialize");
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

    let mut restarted_engine =
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

    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
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
    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

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
    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

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
    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let error = engine
        .find_by_filter("weapon", json!(["not", "an", "object"]), None)
        .expect_err("non-object filter should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn us_032_count_uses_metadata_fast_path_when_no_filter_is_provided() {
    let database = TempDatabase::new("us_032_count_no_filter");
    let database_directory = database.path.to_string_lossy().into_owned();
    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

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
    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

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
    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let error = engine
        .count("weapon", Some(json!(["not", "an", "object"])), None)
        .expect_err("non-object filter should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn us_033_find_by_filter_applies_sorting_before_offset_and_limit() {
    let database = TempDatabase::new("us_033_sort_offset_limit");
    let database_directory = database.path.to_string_lossy().into_owned();
    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

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
    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

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
    let mut engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

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
