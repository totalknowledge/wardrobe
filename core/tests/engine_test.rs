mod common;

use common::TempDatabase;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use wardrobe_core::CatalogRegistry;
use wardrobe_core::{
    BsonBinaryFormat, Command, CommandResult, DatabaseReader, OrderDirection, QueryModifiers,
    StorageCoordinate, StorageFormat, StorageLocator, StorageScope, WardrobeEngine,
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

fn write_legacy_drawer_record(
    database: &TempDatabase,
    drawer_name: &str,
    record: serde_json::Value,
) {
    fs::create_dir_all(&database.path).expect("temp dir should create");

    let data_path = database.path.join(format!("{drawer_name}.drw"));
    let index_path = database.path.join(format!("{drawer_name}_index.drw"));
    let serialized_record =
        serde_json::to_vec(&record).expect("legacy record should serialize as json");
    let mut data_contents = serialized_record.clone();
    data_contents.push(b'\n');
    fs::write(&data_path, data_contents).expect("legacy data file should write");
    let data_offset = 0u64;

    let primary_key = record
        .get("_id")
        .and_then(|value| value.as_str())
        .expect("legacy record should include string primary key");
    let data_size_class = serialized_record.len() + 1;
    let index_record = json!({
        "f": "_id",
        "k": primary_key,
        "o": data_offset,
        "len": serialized_record.len(),
        "class": data_size_class,
        "crc": 0,
        "status": 1
    });
    let serialized_index =
        serde_json::to_vec(&index_record).expect("legacy index should serialize as json");
    let mut index_contents = serialized_index;
    index_contents.push(b'\n');
    fs::write(&index_path, index_contents).expect("legacy index file should write");

    write_drawer_metadata(
        database,
        drawer_name,
        json!({
            "format_version": 0,
            "primary_key": "_id",
            "record_count": 1,
            "unique_constraints": [],
            "relationship_constraints": {},
            "delete_rules": {},
            "cascade_delete_rules": {}
        }),
    );
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

fn drawer_records_from_disk(path: &Path) -> Vec<serde_json::Value> {
    if !path.exists() {
        return Vec::new();
    }

    let reader = DatabaseReader::open_drawer(path).expect("drawer should open");
    let mut records = Vec::new();
    reader
        .stream_with_offsets(|_offset, slot| {
            if let Ok(Some(record)) = BsonBinaryFormat::deserialize_record(slot) {
                records.push(record);
            }
        })
        .expect("drawer should stream");
    records
}

fn drawer_tombstone_count(path: &Path) -> usize {
    if !path.exists() {
        return 0;
    }

    let reader = DatabaseReader::open_drawer(path).expect("drawer should open");
    let mut count = 0usize;
    reader
        .stream_with_offsets(|_offset, slot| {
            if BsonBinaryFormat::is_tombstone(slot) {
                count += 1;
            }
        })
        .expect("drawer should stream");
    count
}

#[test]
fn embedded_engine_opens_direct_storage_path_and_writes_records() {
    let database = TempDatabase::new("embedded_engine_direct_storage_path");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let pointer = engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_embedded_target",
                "element": "Fire"
            }),
        )
        .expect("embedded engine should write directly");

    assert_eq!(pointer, "@gem:embedded_target");
    assert!(database.path.join("gem.drw").is_file());
    assert_eq!(
        engine
            .count("gem", None, None)
            .expect("count should succeed"),
        1
    );
}

#[test]
fn diagnose_storage_reports_recursive_storage_bytes() {
    let database = TempDatabase::new("diagnose_storage_reports_recursive_bytes");
    let nested_directory = database.path.join("nested").join("storage");
    fs::create_dir_all(&nested_directory).expect("nested directory should create");
    fs::write(nested_directory.join("manual.bin"), [1_u8, 2, 3, 4])
        .expect("manual fixture should write");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "diagnose_fire",
                "element": "Fire"
            }),
        )
        .expect("record should upsert");

    let diagnosis = engine
        .diagnose_storage()
        .expect("diagnosis should be available");

    assert_eq!(diagnosis.storage_directory, database_directory);
    assert!(diagnosis.storage_bytes >= 4);
    assert_eq!(diagnosis.drawer_count, 1);
    assert_eq!(diagnosis.drawers, vec!["gem".to_string()]);
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
    assert_eq!(weapons[0]["gem"], "@gem:does_not_exist");
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
        Some("safe_gem")
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

    let gem_records = drawer_records_from_disk(&database.path.join("gem.drw"));
    assert_eq!(drawer_tombstone_count(&database.path.join("gem.drw")), 0);
    assert!(
        gem_records
            .iter()
            .any(|record| record["element"] == "Solar")
    );

    let weapon_records = drawer_records_from_disk(&database.path.join("weapon.drw"));
    assert!(
        weapon_records
            .iter()
            .any(|record| record["gem"] == "@gem:existing_gem")
    );

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

    let weapon_records = drawer_records_from_disk(&database.path.join("weapon.drw"));
    assert!(
        weapon_records
            .iter()
            .any(|record| record["gem"] == "@gem:existing_gem")
    );

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

    let weapon_records = drawer_records_from_disk(&database.path.join("weapon.drw"));
    assert!(
        weapon_records
            .iter()
            .any(|record| record["gem"] == "@gem:full_child_gem")
    );

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
fn us_052_primary_ids_are_stored_clean_while_references_keep_drawer_routing() {
    let database = TempDatabase::new("us_052_clean_primary_ids");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let character_pointer = engine
        .upsert(
            "character",
            json!({
                "_id": "@character:lnk_us_052_owner",
                "name": "Clean Owner"
            }),
        )
        .expect("character should upsert");
    assert_eq!(character_pointer, "@character:us_052_owner");

    let weapon_pointer = engine
        .upsert(
            "weapon",
            json!({
                "_id": "lnk_us_052_weapon",
                "name": "Clean Blade",
                "character": { "_id": "@character:lnk_us_052_owner" }
            }),
        )
        .expect("weapon should upsert");
    assert_eq!(weapon_pointer, "@weapon:us_052_weapon");

    let character_records = drawer_records_from_disk(&database.path.join("character.drw"));
    assert!(
        character_records
            .iter()
            .any(|record| record["_id"] == "us_052_owner")
    );

    let weapon_records = drawer_records_from_disk(&database.path.join("weapon.drw"));
    assert!(
        weapon_records
            .iter()
            .any(|record| record["_id"] == "us_052_weapon")
    );
    assert!(
        weapon_records
            .iter()
            .any(|record| record["character"] == "@character:us_052_owner")
    );

    let legacy_lookup = engine
        .find_by_id("@weapon:lnk_us_052_weapon")
        .expect("legacy lookup should succeed")
        .expect("weapon should exist");
    assert_eq!(legacy_lookup["name"], "Clean Blade");

    let clean_lookup = engine
        .find_by_id("@weapon:us_052_weapon")
        .expect("clean lookup should succeed")
        .expect("weapon should exist");
    assert_eq!(clean_lookup["character"]["name"], "Clean Owner");
}

#[test]
fn us_053_inline_routing_registers_polymorphic_alias_and_hydrates_targets() {
    let database = TempDatabase::new("us_053_polymorphic_alias");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_us_053_fire",
                "element": "Fire"
            }),
        )
        .expect("gem should upsert");
    engine
        .upsert(
            "rune",
            json!({
                "_id": "@rune:lnk_us_053_guard",
                "school": "Guard"
            }),
        )
        .expect("rune should upsert");
    engine
        .upsert(
            "artifact",
            json!({
                "_id": "@artifact:lnk_us_053_satchel",
                "name": "Satchel",
                "attachments": [
                    "@gem:lnk_us_053_fire",
                    "@rune:lnk_us_053_guard"
                ]
            }),
        )
        .expect("artifact should upsert");

    let metadata_contents = fs::read_to_string(database.path.join("artifact_meta.drw"))
        .expect("artifact metadata should be readable");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_contents).expect("metadata should parse");
    assert_eq!(
        metadata["relationship_constraints"]["attachments"]["type"],
        "polymorphic"
    );
    assert_eq!(
        metadata["relationship_constraints"]["attachments"]["target_drawers"],
        json!(["gem", "rune"])
    );

    let artifact_records = drawer_records_from_disk(&database.path.join("artifact.drw"));
    assert!(
        artifact_records.iter().any(
            |record| record["attachments"] == json!(["@gem:us_053_fire", "@rune:us_053_guard"])
        )
    );

    let artifacts = engine
        .find_all("artifact")
        .expect("artifact lookup should succeed");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0]["attachments"][0]["element"], "Fire");
    assert_eq!(artifacts[0]["attachments"][1]["school"], "Guard");
}

#[test]
fn us_053_self_referencing_alias_resolves_clean_string_keys() {
    let database = TempDatabase::new("us_053_self_reference_alias");
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
                "spouse": {
                    "target_drawer": "character"
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
                "_id": "alex",
                "name": "Alex"
            }),
        )
        .expect("first character should upsert");
    engine
        .upsert(
            "character",
            json!({
                "_id": "sam",
                "name": "Sam",
                "spouse": "alex"
            }),
        )
        .expect("self-referencing character should upsert");

    let character_records = drawer_records_from_disk(&database.path.join("character.drw"));
    assert!(
        character_records
            .iter()
            .any(|record| record["spouse"] == "@character:alex")
    );

    let sam = engine
        .find_by_id("@character:sam")
        .expect("lookup should succeed")
        .expect("sam should exist");
    assert_eq!(sam["name"], "Sam");
    assert_eq!(sam["spouse"]["name"], "Alex");
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

    let character_records = drawer_records_from_disk(&database.path.join("character.drw"));
    assert!(character_records.iter().any(|record| record["tags"]
        == json!(["tank", "support", "night-watch"])
        && record["scores"] == json!([10, 20, 30])));

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

    let weapon_records = drawer_records_from_disk(&database.path.join("weapon.drw"));
    assert!(weapon_records.iter().any(|record| {
        record["gems"] == json!(["@gem:array_existing_fire", "@gem:array_existing_air"])
    }));

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

    let character_records = drawer_records_from_disk(&database.path.join("character.drw"));
    assert!(
        character_records
            .iter()
            .any(|record| record["weapons"]
                == json!(["@weapon:array_spear", "@weapon:array_shield"]))
    );

    let weapon_records = drawer_records_from_disk(&database.path.join("weapon.drw"));
    assert!(
        weapon_records
            .iter()
            .any(|record| record["gems"] == json!(["@gem:array_storm"]))
    );
    assert!(
        weapon_records
            .iter()
            .any(|record| record["gems"] == json!(["@gem:array_earth"]))
    );

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

    assert!(drawer_tombstone_count(&database.path.join("gem.drw")) >= 1);

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
fn us_020_delete_by_filter_deletes_matching_records_and_returns_count() {
    let database = TempDatabase::new("us_020_delete_by_filter");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "lnk_delete_filter_fire",
                "element": "Fire"
            }),
        )
        .expect("first gem should upsert");
    engine
        .upsert(
            "gem",
            json!({
                "_id": "lnk_delete_filter_water",
                "element": "Water"
            }),
        )
        .expect("second gem should upsert");

    let deleted = engine
        .delete_by_filter("gem", json!({ "element": "Fire" }))
        .expect("delete by filter should succeed");

    assert_eq!(deleted, 1);
    assert!(
        engine
            .find_by_id("@gem:lnk_delete_filter_fire")
            .expect("deleted record lookup should succeed")
            .is_none()
    );
    assert!(
        engine
            .find_by_id("@gem:lnk_delete_filter_water")
            .expect("remaining record lookup should succeed")
            .is_some()
    );
}

#[test]
fn us_054_delete_accepts_explicit_storage_locator() {
    let database = TempDatabase::new("us_054_delete_explicit_locator");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_us_054_explicit",
                "element": "Locator"
            }),
        )
        .expect("gem should upsert");

    let deleted = engine
        .delete(StorageLocator::explicit("gem", "us_054_explicit"))
        .expect("explicit locator delete should succeed");

    assert!(deleted);
    assert!(
        engine
            .find_by_id("@gem:us_054_explicit")
            .expect("lookup should succeed")
            .is_none()
    );
}

#[test]
fn us_054_delete_accepts_inline_storage_locator() {
    let database = TempDatabase::new("us_054_delete_inline_locator");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "us_054_inline",
                "element": "Inline"
            }),
        )
        .expect("gem should upsert");

    let deleted = engine
        .delete(StorageLocator::inline("@gem:us_054_inline"))
        .expect("inline locator delete should succeed");

    assert!(deleted);
    assert!(
        engine
            .find_by_id("@gem:us_054_inline")
            .expect("lookup should succeed")
            .is_none()
    );
}

#[test]
fn delete_by_id_accepts_deep_structural_pointer() {
    let database = TempDatabase::new("bug_005_deep_structural_delete");
    let database_directory = database.path.to_string_lossy().into_owned();
    fs::create_dir_all(database.path.join("basic-usage").join("public"))
        .expect("nested storage path should exist");
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let pointer = engine
        .upsert(
            "basic-usage/public/user",
            json!({
                "_id": "user-02",
                "name": "Marcus"
            }),
        )
        .expect("nested user should upsert");
    assert_eq!(pointer, "@basic-usage/public/user:user-02");

    let deleted = engine
        .delete_by_id("basic-usage/public/user/user-02")
        .expect("structural pointer delete should succeed");

    assert!(deleted);
    assert!(
        engine
            .find_by_id("@basic-usage/public/user:user-02")
            .expect("lookup should succeed")
            .is_none()
    );
}

#[test]
fn us_054_delete_by_id_accepts_tuple_locator_conversion() {
    let database = TempDatabase::new("us_054_delete_tuple_locator");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_us_054_tuple",
                "element": "Tuple"
            }),
        )
        .expect("gem should upsert");

    let deleted = engine
        .delete_by_id(("gem", "lnk_us_054_tuple"))
        .expect("tuple locator delete should succeed");

    assert!(deleted);
    assert!(
        engine
            .find_by_id("@gem:us_054_tuple")
            .expect("lookup should succeed")
            .is_none()
    );
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
    assert_eq!(hydrated["next"]["next"], "@node:a");
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
fn us_104_find_by_filter_intersects_declared_secondary_indexes() {
    let database = TempDatabase::new("us_104_indexed_filter_intersection");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .bulk_upsert(
            "book",
            vec![
                json!({
                    "_id": "book_a",
                    "title": "Indexed Alpha",
                    "author_id": "entity_a",
                    "editor_id": "entity_a",
                    "purge_bucket": 0
                }),
                json!({
                    "_id": "book_b",
                    "title": "Indexed Beta",
                    "author_id": "entity_a",
                    "editor_id": "entity_b",
                    "purge_bucket": 0
                }),
                json!({
                    "_id": "book_c",
                    "title": "Indexed Gamma",
                    "author_id": "entity_a",
                    "editor_id": "entity_a",
                    "purge_bucket": 1
                }),
                json!({
                    "_id": "book_d",
                    "title": "Other Delta",
                    "author_id": "entity_b",
                    "editor_id": "entity_a",
                    "purge_bucket": 0
                }),
            ],
            true,
        )
        .expect("book batch should upsert");

    for field_name in ["author_id", "editor_id", "purge_bucket"] {
        engine
            .manage_schema(
                "book",
                "add",
                "index",
                field_name,
                json!({ "kind": "index" }),
            )
            .expect("index should be registered");
    }

    let mut record_ids = engine
        .find_by_filter(
            "book",
            json!({
                "author_id": "entity_a",
                "editor_id": "entity_a",
                "purge_bucket": 0
            }),
            None,
        )
        .expect("indexed filter should succeed")
        .into_iter()
        .map(|record| {
            record["_id"]
                .as_str()
                .expect("record id should be a string")
                .to_string()
        })
        .collect::<Vec<_>>();
    record_ids.sort();

    assert_eq!(record_ids, vec!["book_a".to_string()]);
    assert_eq!(
        engine
            .count(
                "book",
                Some(json!({
                    "author_id": "entity_a",
                    "editor_id": "entity_a",
                    "purge_bucket": 0
                })),
                None
            )
            .expect("indexed count should succeed"),
        1
    );

    let wildcard_records = engine
        .find_by_filter("book", json!({ "title": "Indexed %" }), None)
        .expect("unsupported wildcard filter should fall back");
    assert_eq!(wildcard_records.len(), 3);
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
        CommandResult::Pointer("@weapon:us_034_blade".to_string())
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

    let weapon_records = drawer_records_from_disk(&database.path.join("tenant_graph_weapon.drw"));
    assert!(
        weapon_records
            .iter()
            .any(|record| record["gem"] == "@tenant_graph_gem:graph_fire")
    );

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
fn us_048_show_tenants_discovers_active_tenant_namespaces() {
    let database = TempDatabase::new("us_048_show_tenants");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");

    for coordinate in [
        StorageCoordinate::new("tenant_alpha", "production", "core"),
        StorageCoordinate::new("tenant_beta", "production", "core"),
    ] {
        engine
            .execute(
                coordinate,
                Command::Upsert {
                    drawer_name: "gem".to_string(),
                    payload: json!({
                        "_id": "route_seed",
                        "element": "Routed"
                    }),
                },
            )
            .expect("coordinate-scoped upsert should succeed");
    }

    engine
        .execute_in_scope(
            StorageScope::schema("main_db", "tenant_schema"),
            Command::Upsert {
                drawer_name: "weapon".to_string(),
                payload: json!({
                    "_id": "schema_seed",
                    "name": "Schema Blade"
                }),
            },
        )
        .expect("schema-scoped upsert should succeed");

    engine
        .execute_in_scope(
            StorageScope::drawer("tenant_drawer"),
            Command::Upsert {
                drawer_name: "character".to_string(),
                payload: json!({
                    "_id": "drawer_seed",
                    "name": "Drawer Tenant"
                }),
            },
        )
        .expect("drawer-scoped upsert should succeed");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "root_seed",
                "element": "Root"
            }),
        )
        .expect("unscoped root drawer should not create a tenant namespace");

    let tenants = engine.show_tenants().expect("tenants should be discovered");

    assert_eq!(
        tenants,
        vec![
            "tenant_alpha".to_string(),
            "tenant_beta".to_string(),
            "tenant_drawer".to_string(),
            "tenant_schema".to_string()
        ]
    );

    let command_result = engine
        .execute_command(Command::ShowTenants)
        .expect("show tenants command should succeed");
    assert_eq!(command_result, CommandResult::Tenants(tenants));
}

#[test]
fn us_049_show_databases_discovers_database_footprints_with_inventory() {
    let database = TempDatabase::new("us_049_show_databases");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");

    engine
        .execute_in_scope(
            StorageScope::database("main_db"),
            Command::Upsert {
                drawer_name: "gem".to_string(),
                payload: json!({
                    "_id": "main_seed",
                    "element": "Main"
                }),
            },
        )
        .expect("database-scoped upsert should succeed");

    engine
        .execute_in_scope(
            StorageScope::schema("analytics_db", "tenant_schema"),
            Command::Upsert {
                drawer_name: "weapon".to_string(),
                payload: json!({
                    "_id": "schema_seed",
                    "name": "Schema Blade"
                }),
            },
        )
        .expect("schema-scoped upsert should succeed");

    engine
        .execute(
            StorageCoordinate::new("tenant_alpha", "production", "core"),
            Command::Upsert {
                drawer_name: "character".to_string(),
                payload: json!({
                    "_id": "coordinate_seed",
                    "name": "Routed Character"
                }),
            },
        )
        .expect("coordinate-scoped upsert should succeed");

    fs::create_dir_all(database.path.join("empty_db")).expect("empty folder should be created");
    fs::create_dir_all(database.path.join("metadata_only"))
        .expect("metadata-only folder should be created");
    fs::write(
        database.path.join("metadata_only").join("orphan_meta.drw"),
        "{}",
    )
    .expect("metadata-only file should write");

    let databases = engine
        .show_databases()
        .expect("databases should be discovered");
    let names: Vec<String> = databases
        .iter()
        .map(|inventory| inventory.name.clone())
        .collect();

    assert_eq!(
        names,
        vec![
            "analytics_db".to_string(),
            "main_db".to_string(),
            "tenant_alpha/production".to_string()
        ]
    );

    for expected_name in &names {
        let inventory = databases
            .iter()
            .find(|inventory| &inventory.name == expected_name)
            .expect("inventory should exist for database");
        assert_eq!(inventory.record_count, 1);
        assert!(
            inventory.disk_size_bytes > 0,
            "database should report disk usage for {expected_name}"
        );
        assert!(
            inventory.register_file_count >= 3,
            "database should report Wardrobe files for {expected_name}"
        );
    }

    let command_result = engine
        .execute_command(Command::ShowDatabases)
        .expect("show databases command should succeed");
    assert_eq!(command_result, CommandResult::Databases(databases));
}

#[test]
fn us_050_show_schemas_discovers_nested_and_flat_namespaces() {
    let database = TempDatabase::new("us_050_show_schemas");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");

    for schema_name in ["audit_schema", "tenant_schema"] {
        engine
            .execute_in_scope(
                StorageScope::schema("main_db", schema_name),
                Command::Upsert {
                    drawer_name: "gem".to_string(),
                    payload: json!({
                        "_id": format!("{schema_name}_seed"),
                        "element": schema_name
                    }),
                },
            )
            .expect("schema-scoped upsert should succeed");
    }

    engine
        .execute_in_scope(
            StorageScope::database("main_db"),
            Command::Upsert {
                drawer_name: "flat_schema.gem".to_string(),
                payload: json!({
                    "_id": "flat_seed",
                    "element": "Flat"
                }),
            },
        )
        .expect("flat schema-prefixed drawer should upsert");

    engine
        .execute_in_scope(
            StorageScope::database("main_db"),
            Command::Upsert {
                drawer_name: "loose_gem".to_string(),
                payload: json!({
                    "_id": "loose_seed",
                    "element": "Loose"
                }),
            },
        )
        .expect("plain database drawer should upsert");

    engine
        .execute(
            StorageCoordinate::new("tenant_alpha", "production", "core"),
            Command::Upsert {
                drawer_name: "weapon".to_string(),
                payload: json!({
                    "_id": "coordinate_seed",
                    "name": "Coordinate Blade"
                }),
            },
        )
        .expect("coordinate-scoped upsert should succeed");

    let schemas = engine
        .show_schemas("main_db")
        .expect("schemas should be discovered");
    assert_eq!(
        schemas,
        vec![
            "audit_schema".to_string(),
            "flat_schema".to_string(),
            "tenant_schema".to_string()
        ]
    );

    let routed_schemas = engine
        .show_schemas("tenant_alpha/production")
        .expect("routed schemas should be discovered");
    assert_eq!(routed_schemas, vec!["core".to_string()]);

    let command_result = engine
        .execute_command(Command::ShowSchemas {
            database_name: "main_db".to_string(),
        })
        .expect("show schemas command should succeed");
    assert_eq!(command_result, CommandResult::Schemas(schemas));
}

#[test]
fn us_051_show_drawers_discovers_scoped_drawers_with_live_counts() {
    let database = TempDatabase::new("us_051_show_drawers");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");
    let schema_scope = StorageScope::schema("main_db", "tenant_schema");

    for (id, element) in [
        ("schema_gem_live", "Live"),
        ("schema_gem_deleted", "Deleted"),
    ] {
        engine
            .execute_in_scope(
                schema_scope.clone(),
                Command::Upsert {
                    drawer_name: "gem".to_string(),
                    payload: json!({
                        "_id": id,
                        "element": element
                    }),
                },
            )
            .expect("schema gem should upsert");
    }

    engine
        .execute_in_scope(
            schema_scope.clone(),
            Command::Delete {
                pointer: "@gem:schema_gem_deleted".to_string(),
            },
        )
        .expect("schema gem should delete");

    engine
        .execute_in_scope(
            schema_scope,
            Command::Upsert {
                drawer_name: "weapon".to_string(),
                payload: json!({
                    "_id": "schema_weapon",
                    "name": "Schema Blade"
                }),
            },
        )
        .expect("schema weapon should upsert");

    engine
        .execute_in_scope(
            StorageScope::database("main_db"),
            Command::Upsert {
                drawer_name: "flat_schema.artifact".to_string(),
                payload: json!({
                    "_id": "flat_artifact",
                    "kind": "Flat"
                }),
            },
        )
        .expect("flat schema drawer should upsert");

    engine
        .execute(
            StorageCoordinate::new("tenant_alpha", "production", "core"),
            Command::Upsert {
                drawer_name: "character".to_string(),
                payload: json!({
                    "_id": "routed_character",
                    "name": "Routed"
                }),
            },
        )
        .expect("routed drawer should upsert");

    let drawers = engine
        .show_drawers("main_db", "tenant_schema")
        .expect("schema drawers should be discovered");
    let drawer_names: Vec<String> = drawers
        .iter()
        .map(|inventory| inventory.name.clone())
        .collect();

    assert_eq!(drawer_names, vec!["gem".to_string(), "weapon".to_string()]);

    let gem_inventory = drawers
        .iter()
        .find(|inventory| inventory.name == "gem")
        .expect("gem drawer should be inventoried");
    assert_eq!(gem_inventory.record_count, 1);
    assert!(gem_inventory.disk_size_bytes > 0);
    assert_eq!(gem_inventory.register_file_count, 3);

    let weapon_inventory = drawers
        .iter()
        .find(|inventory| inventory.name == "weapon")
        .expect("weapon drawer should be inventoried");
    assert_eq!(weapon_inventory.record_count, 1);
    assert_eq!(weapon_inventory.register_file_count, 3);

    let flat_drawers = engine
        .show_drawers("main_db", "flat_schema")
        .expect("flat schema drawers should be discovered");
    assert_eq!(flat_drawers.len(), 1);
    assert_eq!(flat_drawers[0].name, "artifact");
    assert_eq!(flat_drawers[0].record_count, 1);

    let routed_drawers = engine
        .show_drawers("tenant_alpha/production", "core")
        .expect("routed drawers should be discovered");
    assert_eq!(routed_drawers.len(), 1);
    assert_eq!(routed_drawers[0].name, "character");
    assert_eq!(routed_drawers[0].record_count, 1);

    let command_result = engine
        .execute_command(Command::ShowDrawers {
            database_name: "main_db".to_string(),
            schema_name: "tenant_schema".to_string(),
        })
        .expect("show drawers command should succeed");
    assert_eq!(command_result, CommandResult::Drawers(drawers));
}

#[test]
fn us_063_engine_bootstraps_registry_from_catalog_and_validates_locations() {
    let database = TempDatabase::new("us_063_catalog_bootstrap");
    let storage_pool = database.path.to_string_lossy().into_owned();

    let mut registry = CatalogRegistry::new();
    registry.register_drawer(
        "catalog_db",
        "core",
        "gem",
        database
            .path
            .join("catalog_db")
            .join("core")
            .join("gem.drw")
            .to_string_lossy()
            .into_owned(),
    );
    registry.register_drawer(
        "tenant_alpha/production",
        "inventory",
        "weapon",
        database
            .path
            .join("tenant_alpha")
            .join("production")
            .join("inventory")
            .join("weapon.drw")
            .to_string_lossy()
            .into_owned(),
    );
    registry
        .persist_to_root(&database.path)
        .expect("catalog should persist");

    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");

    let databases = engine
        .show_databases()
        .expect("catalog databases should load");
    let database_names: Vec<String> = databases
        .iter()
        .map(|inventory| inventory.name.clone())
        .collect();
    assert_eq!(
        database_names,
        vec![
            "catalog_db".to_string(),
            "tenant_alpha/production".to_string()
        ]
    );

    assert_eq!(
        engine
            .show_schemas("catalog_db")
            .expect("catalog schemas should load"),
        vec!["core".to_string()]
    );
    assert_eq!(
        engine
            .show_drawers("catalog_db", "core")
            .expect("catalog drawers should load")
            .into_iter()
            .map(|inventory| inventory.name)
            .collect::<Vec<_>>(),
        vec!["gem".to_string()]
    );

    let error = engine
        .execute_in_scope(
            StorageScope::schema("catalog_db", "core"),
            Command::FindAll {
                drawer_name: "missing".to_string(),
            },
        )
        .expect_err("unregistered drawer should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(error.to_string().contains("InvalidLocation"));
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

    let weapon_records = drawer_records_from_disk(&database.path.join("weapon.drw"));
    assert!(
        weapon_records
            .iter()
            .any(|record| record["gem_slot"] == "@gem:us_037_fire")
    );

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

#[test]
fn us_042_engine_vacuum_drawer_compacts_storage_and_preserves_hydration() {
    let database = TempDatabase::new("us_042_engine_vacuum_drawer");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_vacuum_keep",
                "name": "Very Long Vacuum Blade Name",
                "description": "large payload that should leave dead padded space after update",
                "gem": {
                    "_id": "@gem:lnk_vacuum_gem",
                    "element": "Light",
                    "potency": 9001
                }
            }),
        )
        .expect("weapon should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_vacuum_delete",
                "name": "Deleted Blade"
            }),
        )
        .expect("second weapon should upsert");
    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_vacuum_keep",
                "name": "Compact Blade",
                "gem": { "_id": "@gem:lnk_vacuum_gem" }
            }),
        )
        .expect("weapon should update");
    engine
        .delete_by_id("@weapon:lnk_vacuum_delete")
        .expect("weapon should delete");

    let data_path = database.path.join("weapon.drw");
    let before_len = fs::metadata(&data_path)
        .expect("weapon data metadata should read")
        .len();
    assert!(drawer_tombstone_count(&data_path) >= 1);

    let report = engine
        .vacuum_drawer("weapon")
        .expect("vacuum should succeed");

    let after_len = fs::metadata(&data_path)
        .expect("weapon data metadata should read")
        .len();

    assert_eq!(report.records_rewritten, 1);
    assert_eq!(report.data_bytes_before, before_len);
    assert_eq!(report.data_bytes_after, after_len);
    assert!(report.bytes_reclaimed > 0);
    assert!(after_len < before_len);
    assert_eq!(drawer_tombstone_count(&data_path), 0);

    let weapons = engine
        .find_all("weapon")
        .expect("weapons should read after vacuum");
    assert_eq!(weapons.len(), 1);
    assert_eq!(weapons[0]["name"], "Compact Blade");
    assert_eq!(weapons[0]["gem"]["element"], "Light");

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reopen after vacuum");
    let weapon = restarted_engine
        .find_by_id("@weapon:lnk_vacuum_keep")
        .expect("lookup should succeed")
        .expect("weapon should exist");
    assert_eq!(weapon["gem"]["potency"], 9001);
}

#[test]
fn us_042_vacuum_command_compacts_routed_drawer_scope() {
    let database = TempDatabase::new("us_042_routed_vacuum_command");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");
    let coordinate = StorageCoordinate::new("tenant_vacuum", "prod", "core");

    engine
        .execute(
            coordinate.clone(),
            Command::Upsert {
                drawer_name: "gem".to_string(),
                payload: json!({
                    "_id": "@gem:lnk_routed_keep",
                    "element": "Long Element Value",
                    "potency": 1
                }),
            },
        )
        .expect("routed gem should upsert");
    engine
        .execute(
            coordinate.clone(),
            Command::Upsert {
                drawer_name: "gem".to_string(),
                payload: json!({
                    "_id": "@gem:lnk_routed_keep",
                    "element": "Air",
                    "potency": 2
                }),
            },
        )
        .expect("routed gem should update");

    let scoped_data_path = database
        .path
        .join("tenant_vacuum")
        .join("prod")
        .join("core")
        .join("gem.drw");
    assert!(drawer_tombstone_count(&scoped_data_path) >= 1);

    let result = engine
        .execute(
            coordinate.clone(),
            Command::Vacuum {
                drawer_name: "gem".to_string(),
            },
        )
        .expect("routed vacuum should succeed");

    let CommandResult::Vacuumed(report) = result else {
        panic!("expected vacuum report");
    };

    assert_eq!(report.records_rewritten, 1);
    assert!(report.bytes_reclaimed > 0);
    assert!(!database.path.join("gem.drw").exists());
    assert!(drawer_tombstone_count(&scoped_data_path) == 0);

    let result = engine
        .execute(
            coordinate,
            Command::FindAll {
                drawer_name: "gem".to_string(),
            },
        )
        .expect("routed find should succeed");
    let CommandResult::Records(records) = result else {
        panic!("expected records");
    };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["element"], "Air");
}

#[test]
fn us_072_find_all_rejects_legacy_newline_record_layout() {
    let database = TempDatabase::new("us_044_lazy_schema_evolution");
    let database_directory = database.path.to_string_lossy().into_owned();
    write_legacy_drawer_record(
        &database,
        "gem",
        json!({
            "_id": "@gem:lnk_lazy_fire",
            "element": "Fire",
            "socket": "@socket:lnk_alpha"
        }),
    );

    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
    let error = engine
        .find_all("gem")
        .expect_err("legacy newline records should be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error
            .to_string()
            .contains("Legacy newline-delimited records are no longer supported")
    );
}

#[test]
fn us_072_batch_migration_rejects_legacy_newline_storage_partition() {
    let database = TempDatabase::new("us_044_batch_schema_evolution");
    let database_directory = database.path.to_string_lossy().into_owned();
    write_legacy_drawer_record(
        &database,
        "weapon",
        json!({
            "_id": "@weapon:lnk_batch_blade",
            "name": "Batch Blade",
            "gem": "@gem:lnk_batch_gem"
        }),
    );

    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
    let error = engine
        .migrate_drawer("weapon")
        .expect_err("legacy newline records should be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error
            .to_string()
            .contains("Legacy newline-delimited records are no longer supported")
    );
}

#[test]
fn us_043_engine_reloads_evicted_drawers_from_disk_with_cache_limit() {
    let database = TempDatabase::new("us_043_engine_lru_reloads_evicted_drawers");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open_with_drawer_cache_limit(&database_directory, 1)
        .expect("engine should initialize");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_lru_gem",
                "element": "Light",
                "potency": 9001
            }),
        )
        .expect("gem should upsert");
    assert_eq!(
        engine
            .cached_drawer_count()
            .expect("cache count should read"),
        1
    );

    engine
        .upsert(
            "weapon",
            json!({
                "_id": "@weapon:lnk_lru_weapon",
                "name": "Cache Blade",
                "gem": { "_id": "@gem:lnk_lru_gem" }
            }),
        )
        .expect("weapon should upsert");
    assert_eq!(
        engine
            .cached_drawer_count()
            .expect("cache count should read"),
        1
    );

    let gem = engine
        .find_by_id("@gem:lnk_lru_gem")
        .expect("evicted gem drawer should reload")
        .expect("gem should exist");
    assert_eq!(gem["element"], "Light");
    assert_eq!(
        engine
            .cached_drawer_count()
            .expect("cache count should read"),
        1
    );

    let weapons = engine
        .find_all("weapon")
        .expect("evicted weapon drawer should reload");
    assert_eq!(weapons.len(), 1);
    assert_eq!(weapons[0]["name"], "Cache Blade");
    assert_eq!(weapons[0]["gem"]["potency"], 9001);
    assert_eq!(
        engine
            .cached_drawer_count()
            .expect("cache count should read"),
        1
    );
}

#[test]
fn us_043_engine_rejects_zero_drawer_cache_limit() {
    let database = TempDatabase::new("us_043_engine_rejects_zero_limit");
    let database_directory = database.path.to_string_lossy().into_owned();

    let Err(error) = WardrobeEngine::open_with_drawer_cache_limit(&database_directory, 0) else {
        panic!("zero cache limit should fail");
    };

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn engine_appends_to_wal_on_write() -> std::io::Result<()> {
    let database = TempDatabase::new("engine_wal");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine opens");
    let _ = engine.upsert("character", json!({"name":"hero"}))?;
    let wal_path = database.path.join("wardrobe.wal");
    assert!(wal_path.exists());
    let metadata = fs::metadata(&wal_path)?;
    assert!(metadata.len() > 0);
    Ok(())
}

#[test]
fn us_060_ops_threshold_triggers_checkpoint_and_truncates_wal() -> std::io::Result<()> {
    let database = TempDatabase::new("us_060_ops_threshold_checkpoint");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine =
        WardrobeEngine::open_with_wal_checkpoint_thresholds(&database_directory, 1_048_576, 2)
            .expect("engine opens with WAL thresholds");

    let pointer = engine.upsert("gem", json!({"_id": "fire", "element": "Fire"}))?;
    assert_eq!(pointer, "@gem:fire");

    let wal_path = database.path.join("wardrobe.wal");
    let wal_meta_path = database.path.join("wardrobe.wal.meta");
    assert!(wal_path.exists());
    assert!(wal_meta_path.exists());
    assert_eq!(fs::metadata(&wal_path)?.len(), 0);

    drop(engine);
    let reopened = WardrobeEngine::open(&database_directory).expect("engine reopens");
    let record = reopened
        .find_by_id("@gem:fire")
        .expect("record lookup succeeds")
        .expect("record should survive checkpoint");
    assert_eq!(record["element"], "Fire");
    Ok(())
}

#[test]
fn us_060_byte_threshold_triggers_checkpoint_and_truncates_wal() -> std::io::Result<()> {
    let database = TempDatabase::new("us_060_byte_threshold_checkpoint");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open_with_wal_checkpoint_thresholds(&database_directory, 1, 1000)
        .expect("engine opens with WAL thresholds");

    engine.upsert("gem", json!({"_id": "water", "element": "Water"}))?;

    let wal_path = database.path.join("wardrobe.wal");
    let wal_meta_path = database.path.join("wardrobe.wal.meta");
    assert!(wal_path.exists());
    assert!(wal_meta_path.exists());
    assert_eq!(fs::metadata(&wal_path)?.len(), 0);

    drop(engine);
    let reopened = WardrobeEngine::open(&database_directory).expect("engine reopens");
    let record = reopened
        .find_by_id("@gem:water")
        .expect("record lookup succeeds")
        .expect("record should survive checkpoint");
    assert_eq!(record["element"], "Water");
    Ok(())
}

#[test]
fn us_060_wal_checkpoint_thresholds_reject_zero_values() {
    let database = TempDatabase::new("us_060_zero_threshold_rejected");
    let database_directory = database.path.to_string_lossy().into_owned();

    let zero_size =
        match WardrobeEngine::open_with_wal_checkpoint_thresholds(&database_directory, 0, 1000) {
            Ok(_) => panic!("zero byte threshold should fail"),
            Err(error) => error,
        };
    assert_eq!(zero_size.kind(), std::io::ErrorKind::InvalidInput);

    let zero_ops = match WardrobeEngine::open_with_wal_checkpoint_thresholds(
        &database_directory,
        1_048_576,
        0,
    ) {
        Ok(_) => panic!("zero operation threshold should fail"),
        Err(error) => error,
    };
    assert_eq!(zero_ops.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn us_064_managed_database_schema_and_drawer_lifecycle_updates_catalog() {
    let database = TempDatabase::new("us_064_managed_lifecycle");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should open");

    let database_inventory = engine
        .create_database("managed_db")
        .expect("database should be created");
    assert_eq!(database_inventory.name, "managed_db");
    assert!(database.path.join("managed_db").exists());

    let missing_parent = engine
        .create_schema("missing_db", "core")
        .expect_err("schema creation should require a registered database");
    assert_eq!(missing_parent.kind(), std::io::ErrorKind::NotFound);

    let schema_inventory = engine
        .create_schema("managed_db", "core")
        .expect("schema should be created");
    assert_eq!(schema_inventory.name, "core");
    assert!(database.path.join("managed_db").join("core").exists());

    let drawer_inventory = engine
        .create_drawer("managed_db", "core", "gem")
        .expect("drawer should be created");
    assert_eq!(drawer_inventory.name, "gem");
    assert!(
        database
            .path
            .join("managed_db")
            .join("core")
            .join("gem.drw")
            .exists()
    );

    let command_result = engine
        .execute_command(Command::DefineDrawer {
            database_name: "managed_db".to_string(),
            schema_name: "core".to_string(),
            drawer_name: "weapon".to_string(),
        })
        .expect("define drawer command should route through engine boundary");
    assert!(matches!(
        command_result,
        CommandResult::StorageInventory(inventory) if inventory.name == "weapon"
    ));

    let reopened = WardrobeEngine::open(&storage_pool).expect("engine should reopen");
    let databases = reopened
        .show_databases()
        .expect("catalog databases should load");
    assert_eq!(
        databases
            .iter()
            .map(|inventory| inventory.name.as_str())
            .collect::<Vec<_>>(),
        vec!["managed_db"]
    );
    assert_eq!(
        reopened
            .show_schemas("managed_db")
            .expect("catalog schemas should load"),
        vec!["core".to_string()]
    );
    assert_eq!(
        reopened
            .show_drawers("managed_db", "core")
            .expect("catalog drawers should load")
            .iter()
            .map(|inventory| inventory.name.as_str())
            .collect::<Vec<_>>(),
        vec!["gem", "weapon"]
    );
}
#[test]
fn us_065_logical_tenant_routes_to_catalog_defined_location() {
    let database = TempDatabase::new("us_065_logical_tenant_route");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let routed_database = database
        .path
        .join("shards")
        .join("tenant_a")
        .join("production");
    let routed_drawer = routed_database.join("core").join("gem.drw");
    std::fs::create_dir_all(routed_database.join("core"))
        .expect("tenant schema directory should exist");
    std::fs::File::create(&routed_drawer).expect("tenant drawer should exist");
    std::fs::File::create(routed_database.join("core").join("gem_index.drw"))
        .expect("tenant index should exist");

    let mut registry = CatalogRegistry::new();
    registry.register_tenant_route("tenant_a", "production", "shards/tenant_a/production");
    registry.register_schema("production", "core");
    registry.register_drawer(
        "production",
        "core",
        "gem",
        routed_drawer.to_string_lossy().into_owned(),
    );
    registry
        .persist_to_root(&database.path)
        .expect("catalog should persist");

    let engine = WardrobeEngine::open(&storage_pool).expect("engine should open");
    assert_eq!(
        engine.show_tenants().expect("tenants should load"),
        vec!["tenant_a".to_string()]
    );

    let result = engine
        .execute_in_scope(
            StorageScope::tenant("tenant_a", "production", "core"),
            Command::Upsert {
                drawer_name: "gem".to_string(),
                payload: json!({
                    "_id": "tenant_fire",
                    "element": "Fire"
                }),
            },
        )
        .expect("tenant scoped upsert should route");
    assert!(matches!(result, CommandResult::Pointer(pointer) if pointer == "@gem:tenant_fire"));

    assert!(routed_drawer.exists());
    assert!(
        routed_drawer
            .metadata()
            .expect("tenant drawer metadata should load")
            .len()
            > 0
    );
    assert!(
        !database
            .path
            .join("production")
            .join("core")
            .join("gem.drw")
            .exists()
    );

    let records = engine
        .execute_command(Command::ExecuteForTenant {
            tenant_id: "tenant_a".to_string(),
            database_name: "production".to_string(),
            schema_name: "core".to_string(),
            command: Box::new(Command::FindAll {
                drawer_name: "gem".to_string(),
            }),
        })
        .expect("tenant command should route");
    assert!(matches!(
        records,
        CommandResult::Records(records)
            if records.len() == 1 && records[0]["_id"] == "tenant_fire"
    ));

    let missing_tenant = engine
        .execute_for_tenant(
            "tenant_b",
            "production",
            "core",
            Command::FindAll {
                drawer_name: "gem".to_string(),
            },
        )
        .expect_err("missing tenant should fail");
    assert_eq!(missing_tenant.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn us_066_binary_wal_logs_mutating_commands() {
    let database = TempDatabase::new("us_066_binary_wal");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");

    let empty_verification = engine.verify_wal(None).expect("empty wal should verify");
    assert_eq!(empty_verification.entry_count, 0);
    assert_eq!(empty_verification.last_sequence, None);

    let result = engine
        .execute_command(Command::Upsert {
            drawer_name: "gem".to_string(),
            payload: json!({
                "_id": "wal_fire",
                "element": "Fire"
            }),
        })
        .expect("upsert command should succeed");
    assert!(matches!(result, CommandResult::Pointer(pointer) if pointer == "@gem:wal_fire"));

    let root_wal = database.path.join(".wal");
    assert!(root_wal.exists());

    let verification = engine.verify_wal(None).expect("wal should verify");
    assert_eq!(verification.entry_count, 1);
    assert_eq!(verification.last_sequence, Some(1));

    let command_result = engine
        .execute_command(Command::VerifyWal {
            database_name: None,
        })
        .expect("wal verification command should succeed");
    assert_eq!(command_result, CommandResult::WalVerification(verification));

    engine
        .execute(
            StorageCoordinate::new("tenant_wal", "production", "core"),
            Command::Upsert {
                drawer_name: "gem".to_string(),
                payload: json!({
                    "_id": "tenant_wal_fire",
                    "element": "Routed Fire"
                }),
            },
        )
        .expect("coordinate upsert should succeed");

    let routed_wal = database
        .path
        .join("tenant_wal")
        .join("production")
        .join("core")
        .join(".wal");
    assert!(routed_wal.exists());

    let routed_verification = engine
        .verify_wal(Some("tenant_wal/production/core"))
        .expect("routed wal should verify");
    assert_eq!(routed_verification.entry_count, 1);
    assert_eq!(routed_verification.last_sequence, Some(1));
}

#[test]
fn us_101_bulk_upsert_returns_ordered_pointers() {
    let database = TempDatabase::new("us_101_bulk_upsert_ordered_pointers");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let result = engine
        .execute_command(Command::BulkUpsert {
            drawer_name: "gem".to_string(),
            records: vec![
                json!({"_id": "bulk_fire", "element": "Fire"}),
                json!({"_id": "bulk_water", "element": "Water"}),
            ],
            atomic: true,
        })
        .expect("bulk upsert should succeed");

    assert_eq!(
        result,
        CommandResult::Pointers(vec![
            "@gem:bulk_fire".to_string(),
            "@gem:bulk_water".to_string()
        ])
    );
    assert_eq!(
        engine
            .count("gem", None, None)
            .expect("count should succeed"),
        2
    );
}

#[test]
fn us_101_bulk_upsert_normalizes_plain_relationship_ids() {
    let database = TempDatabase::new("us_101_bulk_upsert_relationship_ids");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    write_drawer_metadata(
        &database,
        "book",
        json!({
            "format_version": 1,
            "primary_key": "_id",
            "record_count": 0,
            "unique_constraints": [],
            "relationship_constraints": {
                "author_id": {
                    "type": "M:1",
                    "target_drawer": "entity"
                },
                "editor_id": {
                    "type": "M:1",
                    "target_drawer": "entity"
                }
            },
            "delete_rules": {},
            "cascade_delete_rules": {}
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .bulk_upsert(
            "entity",
            vec![
                json!({"_id": "entity_00000000", "display_name": "Author"}),
                json!({"_id": "entity_00000001", "display_name": "Editor"}),
            ],
            true,
        )
        .expect("entity batch should upsert");

    let pointers = engine
        .bulk_upsert(
            "book",
            vec![json!({
                "_id": "book_00000000",
                "title": "Bulk Relationship Book",
                "author_id": "entity_00000000",
                "editor_id": "entity_00000001"
            })],
            true,
        )
        .expect("book batch should normalize relationship ids");

    assert_eq!(pointers, vec!["@book:book_00000000".to_string()]);
    let book_records = drawer_records_from_disk(&database.path.join("book.drw"));
    assert_eq!(book_records[0]["author_id"], "@entity:entity_00000000");
    assert_eq!(book_records[0]["editor_id"], "@entity:entity_00000001");
}

#[test]
fn us_101_atomic_bulk_upsert_rejects_invalid_batch_without_writes() {
    let database = TempDatabase::new("us_101_atomic_bulk_upsert_rollback");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    write_drawer_metadata(
        &database,
        "gem",
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
                "required": ["element"],
                "properties": {
                    "_id": { "type": "string" },
                    "element": { "type": "string" }
                },
                "additionalProperties": false
            }
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let error = engine
        .bulk_upsert(
            "gem",
            vec![
                json!({"_id": "bulk_fire", "element": "Fire"}),
                json!({"_id": "bulk_broken"}),
            ],
            true,
        )
        .expect_err("invalid atomic batch should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        engine
            .count("gem", None, None)
            .expect("count should succeed"),
        0
    );

    drop(engine);
    let reopened = WardrobeEngine::open(&database_directory).expect("engine should reopen");
    assert_eq!(
        reopened
            .count("gem", None, None)
            .expect("count should succeed"),
        0
    );
}

#[test]
fn us_102_engine_transactions_flush_dirty_metadata_on_commit() {
    let database = TempDatabase::new("us_102_engine_metadata_commit_flush");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .bulk_upsert(
            "gem",
            vec![
                json!({"_id": "fire", "element": "Fire"}),
                json!({"_id": "water", "element": "Water"}),
            ],
            true,
        )
        .expect("bulk upsert should commit");

    let metadata_contents = fs::read_to_string(database.path.join("gem_meta.drw"))
        .expect("metadata should read after bulk upsert");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_contents).expect("metadata should parse after bulk upsert");
    assert_eq!(metadata["record_count"], 2);

    engine
        .upsert("gem", json!({"_id": "fire", "element": "Flame"}))
        .expect("update should commit");
    let metadata_contents = fs::read_to_string(database.path.join("gem_meta.drw"))
        .expect("metadata should read after update");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_contents).expect("metadata should parse after update");
    assert_eq!(metadata["record_count"], 2);

    assert!(engine.delete("@gem:water").expect("delete should commit"));
    let metadata_contents = fs::read_to_string(database.path.join("gem_meta.drw"))
        .expect("metadata should read after delete");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_contents).expect("metadata should parse after delete");
    assert_eq!(metadata["record_count"], 1);
}
