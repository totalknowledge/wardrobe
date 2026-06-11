mod common;

use common::TempDatabase;
use serde_json::json;
use std::fs;
use wardrobe_core::{DatabaseWriter, Drawer, PlainTextJsonFormat, Recycler, StorageFormat};

fn load_metadata(database_directory: &TempDatabase, drawer_name: &str) -> serde_json::Value {
    let metadata_path = database_directory
        .path
        .join(format!("{}_meta.drw", drawer_name));
    let metadata_contents =
        fs::read_to_string(metadata_path).expect("metadata sidecar should be readable");
    serde_json::from_str(&metadata_contents).expect("metadata sidecar should be valid json")
}

fn write_metadata(
    database_directory: &TempDatabase,
    drawer_name: &str,
    metadata: serde_json::Value,
) {
    fs::write(
        database_directory
            .path
            .join(format!("{}_meta.drw", drawer_name)),
        serde_json::to_vec_pretty(&metadata).expect("metadata should serialize"),
    )
    .expect("metadata should write");
}

fn load_index_records(
    database_directory: &TempDatabase,
    drawer_name: &str,
) -> Vec<serde_json::Value> {
    let index_path = database_directory
        .path
        .join(format!("{}_index.drw", drawer_name));
    let index_contents = fs::read_to_string(index_path).expect("index file should be readable");

    index_contents
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("!!DEAD!!") {
                None
            } else {
                serde_json::from_str(trimmed).ok()
            }
        })
        .collect()
}

#[test]
fn open_rebuilds_secondary_indexes_from_disk() {
    let database_directory = TempDatabase::new("drawer_rebuilds_secondary_indexes");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    {
        let mut drawer = Drawer::open(
            &database_directory.path,
            "weapon",
            "_id",
            vec!["serial".to_string()],
        )
        .expect("drawer should open");

        drawer
            .upsert_record(json!({
                "_id": "@weapon:lnk_a",
                "name": "Axe",
                "serial": "SER-1"
            }))
            .expect("upsert should succeed")
            .expect("record should validate");
    }

    let reopened = Drawer::open(&database_directory.path, "weapon", "_id", Vec::new())
        .expect("drawer should reopen from disk");

    let matches = reopened
        .find_by_secondary_key("serial", "SER-1")
        .expect("secondary lookup should succeed");

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["name"], "Axe");
    assert!(reopened.unique_constraints.contains(&"serial".to_string()));
}

#[test]
fn open_rebuilds_array_form_secondary_offsets_from_disk() {
    let database_directory = TempDatabase::new("drawer_rebuilds_array_offsets");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let data_file = database_directory.path.join("weapon.drw");
    let index_file = database_directory.path.join("weapon_index.drw");

    fs::write(
        &data_file,
        "{\"_id\":\"@weapon:lnk_a\",\"serial\":\"SER-1\"}\n",
    )
    .expect("data file should write");
    fs::write(
        &index_file,
        "{\"f\":\"_id\",\"k\":\"@weapon:lnk_a\",\"o\":0}\n{\"f\":\"serial\",\"k\":\"SER-1\",\"o\":[0]}\n",
    )
    .expect("index file should write");

    let drawer = Drawer::open(
        &database_directory.path,
        "weapon",
        "_id",
        vec!["serial".to_string()],
    )
    .expect("drawer should open");

    let matches = drawer
        .find_by_secondary_key("serial", "SER-1")
        .expect("secondary lookup should succeed");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["_id"], "@weapon:lnk_a");
}

#[test]
fn unique_constraints_are_enforced_and_tombstones_are_recycled() {
    let database_directory = TempDatabase::new("drawer_unique_constraints");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let mut drawer = Drawer::open(
        &database_directory.path,
        "gem",
        "_id",
        vec!["element".to_string()],
    )
    .expect("drawer should open");

    drawer
        .upsert_record(json!({
            "_id": "@gem:lnk_a",
            "element": "Fire",
            "potency": 10
        }))
        .expect("upsert should succeed")
        .expect("record should validate");

    let duplicate = drawer
        .upsert_record(json!({
            "_id": "@gem:lnk_b",
            "element": "Fire",
            "potency": 20
        }))
        .expect("upsert should not fail at io level");

    assert!(duplicate.is_err());
    assert!(
        drawer
            .find_by_secondary_key("element", "Fire")
            .expect("lookup should succeed")
            .len()
            >= 1
    );
}

#[test]
fn delete_by_primary_key_tombstones_record_and_evicts_indexes() {
    let database_directory = TempDatabase::new("drawer_delete_by_primary_key");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let mut drawer = Drawer::open(
        &database_directory.path,
        "gem",
        "_id",
        vec!["element".to_string()],
    )
    .expect("drawer should open");

    drawer
        .upsert_record(json!({
            "_id": "@gem:lnk_delete_me",
            "element": "Fire",
            "potency": 10
        }))
        .expect("upsert should succeed")
        .expect("record should validate");

    let deleted = drawer
        .delete_by_primary_key("@gem:lnk_delete_me")
        .expect("delete should succeed")
        .expect("deleted record should be returned");

    assert_eq!(deleted["element"], "Fire");
    assert!(
        drawer
            .find_by_primary_key("@gem:lnk_delete_me")
            .expect("lookup should succeed")
            .is_none()
    );
    assert!(
        drawer
            .find_by_secondary_key("element", "Fire")
            .expect("secondary lookup should succeed")
            .is_empty()
    );
    assert!(
        drawer
            .find_all_records()
            .expect("find all should succeed")
            .is_empty()
    );

    let data_contents = fs::read_to_string(database_directory.path.join("gem.drw"))
        .expect("data file should be readable");
    assert!(data_contents.contains("!!DEAD!!"));

    let index_records = load_index_records(&database_directory, "gem");
    assert!(index_records.iter().any(|record| record["status"] == 0));
}

#[test]
fn find_all_records_skips_tombstoned_lines() {
    let database_directory = TempDatabase::new("drawer_find_all_records");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let mut drawer = Drawer::open(&database_directory.path, "weapon", "_id", Vec::new())
        .expect("drawer should open");

    drawer
        .upsert_record(json!({
            "_id": "@weapon:lnk_a",
            "name": "Blade"
        }))
        .expect("upsert should succeed")
        .expect("record should validate");

    drawer
        .upsert_record(json!({
            "_id": "@weapon:lnk_a",
            "name": "Blade Prime"
        }))
        .expect("replacement should succeed")
        .expect("replacement should validate");

    let records = drawer.find_all_records().expect("find all should succeed");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "Blade Prime");
}

#[test]
fn us_017_open_creates_metadata_sidecar_file_for_drawer() {
    let database_directory = TempDatabase::new("drawer_metadata_sidecar_creation");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let _drawer = Drawer::open(&database_directory.path, "socks", "_id", Vec::new())
        .expect("drawer should open");

    let metadata_path = database_directory.path.join("socks_meta.drw");
    assert!(metadata_path.is_file());

    let metadata = load_metadata(&database_directory, "socks");
    assert_eq!(metadata["format_version"], 1);
    assert_eq!(metadata["primary_key"], "_id");
    assert_eq!(metadata["record_count"], 0);
    assert!(metadata["relationship_constraints"].is_object());
    assert!(metadata["delete_rules"].is_object());
    assert!(metadata.get("blocks").is_none());
    assert!(metadata.get("free_slots").is_none());
    assert!(metadata.get("record_status_map").is_none());
    assert!(metadata.get("payload_lengths").is_none());
    assert!(metadata.get("allocated_size_classes").is_none());
    assert!(metadata.get("integrity_checksums").is_none());
}

#[test]
fn us_017_metadata_sidecar_tracks_record_count_during_standard_writes() {
    let database_directory = TempDatabase::new("drawer_metadata_sidecar_record_count");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let mut drawer = Drawer::open(&database_directory.path, "socks", "_id", Vec::new())
        .expect("drawer should open");
    let initial_metadata = load_metadata(&database_directory, "socks");
    assert_eq!(initial_metadata["record_count"], 0);

    drawer
        .upsert_record(json!({
            "_id": "@socks:lnk_a",
            "color": "Blue"
        }))
        .expect("first upsert should succeed")
        .expect("record should validate");

    drawer
        .upsert_record(json!({
            "_id": "@socks:lnk_a",
            "color": "Green"
        }))
        .expect("second upsert should succeed")
        .expect("record should validate");

    let metadata_after_upserts = load_metadata(&database_directory, "socks");
    assert_eq!(metadata_after_upserts["record_count"], 1);
    assert!(metadata_after_upserts.get("blocks").is_none());
    assert!(metadata_after_upserts.get("free_slots").is_none());

    drawer
        .upsert_record(json!({
            "_id": "@socks:lnk_b",
            "color": "Gold"
        }))
        .expect("third upsert should succeed")
        .expect("record should validate");
    let metadata_after_insert = load_metadata(&database_directory, "socks");
    assert_eq!(metadata_after_insert["record_count"], 2);

    drawer
        .delete_by_primary_key("@socks:lnk_a")
        .expect("delete should succeed")
        .expect("record should exist");
    let metadata_after_delete = load_metadata(&database_directory, "socks");
    assert_eq!(metadata_after_delete["record_count"], 1);
}

#[test]
fn us_015_index_journal_tracks_blocks_and_reuses_dead_slots_after_reopen() {
    let database_directory = TempDatabase::new("drawer_index_block_journal_recycler");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    {
        let mut drawer = Drawer::open(&database_directory.path, "socks", "_id", Vec::new())
            .expect("drawer should open");

        drawer
            .upsert_record(json!({
                "_id": "@socks:lnk_a",
                "color": "Blue"
            }))
            .expect("first upsert should succeed")
            .expect("record should validate");

        drawer
            .upsert_record(json!({
                "_id": "@socks:lnk_a",
                "color": "Ruby"
            }))
            .expect("second upsert should succeed")
            .expect("record should validate");
    }

    let index_records = load_index_records(&database_directory, "socks");
    assert!(index_records.iter().any(|record| {
        record["status"] == 1
            && record.get("len").is_some()
            && record.get("class").is_some()
            && record.get("crc").is_some()
    }));
    assert!(index_records.iter().any(|record| record["status"] == 0));

    let data_path = database_directory.path.join("socks.drw");
    let len_after_update = fs::metadata(&data_path)
        .expect("data file metadata should read")
        .len();

    {
        let mut reopened = Drawer::open(&database_directory.path, "socks", "_id", Vec::new())
            .expect("drawer should reopen");

        reopened
            .upsert_record(json!({
                "_id": "@socks:lnk_b",
                "color": "Gold"
            }))
            .expect("third upsert should succeed")
            .expect("record should validate");
    }

    let len_after_reuse = fs::metadata(&data_path)
        .expect("data file metadata should read")
        .len();
    let data_contents = fs::read_to_string(&data_path).expect("data file should be readable");

    assert_eq!(len_after_reuse, len_after_update);
    assert!(
        data_contents
            .lines()
            .next()
            .expect("first data line should exist")
            .contains("@socks:lnk_b")
    );
}

#[test]
fn us_046_upsert_reuses_recycler_slot_before_appending() {
    let database_directory = TempDatabase::new("us_046_recycler_upsert_pipeline");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let data_path = database_directory.path.join("socks.drw");
    let mut drawer = Drawer::open(&database_directory.path, "socks", "_id", Vec::new())
        .expect("drawer should open");

    drawer
        .upsert_record(json!({
            "_id": "@socks:lnk_a",
            "color": "Blue"
        }))
        .expect("first upsert should succeed")
        .expect("record should validate");
    let len_after_initial_append = fs::metadata(&data_path)
        .expect("data metadata should read")
        .len();
    assert!(len_after_initial_append > 0);

    drawer
        .delete_by_primary_key("@socks:lnk_a")
        .expect("delete should succeed")
        .expect("record should exist");
    let len_after_delete = fs::metadata(&data_path)
        .expect("data metadata should read")
        .len();

    let deleted_offset = load_index_records(&database_directory, "socks")
        .iter()
        .find(|record| record["k"] == "@socks:lnk_a" && record["status"] == 0)
        .and_then(|record| record["o"].as_u64())
        .expect("dead index record should expose reusable offset");

    drawer
        .upsert_record(json!({
            "_id": "@socks:lnk_b",
            "color": "Gold"
        }))
        .expect("second upsert should succeed")
        .expect("record should validate");
    let len_after_reuse = fs::metadata(&data_path)
        .expect("data metadata should read")
        .len();

    assert_eq!(len_after_reuse, len_after_delete);
    let reused_offset = load_index_records(&database_directory, "socks")
        .iter()
        .rev()
        .find(|record| record["k"] == "@socks:lnk_b" && record["status"] == 1)
        .and_then(|record| record["o"].as_u64())
        .expect("new live index record should expose storage offset");
    assert_eq!(reused_offset, deleted_offset);

    assert_eq!(
        drawer
            .find_by_primary_key("@socks:lnk_b")
            .expect("lookup should succeed")
            .expect("reused record should exist")["color"],
        "Gold"
    );

    drawer
        .upsert_record(json!({
            "_id": "@socks:lnk_c",
            "color": "Violet",
            "description": "larger payload forces append because no matching recycled slot remains"
        }))
        .expect("third upsert should succeed")
        .expect("record should validate");
    let len_after_append = fs::metadata(&data_path)
        .expect("data metadata should read")
        .len();

    assert!(len_after_append > len_after_reuse);
}

#[test]
fn us_047_recycler_cache_is_built_lazily_from_merged_index() {
    let database_directory = TempDatabase::new("us_047_lazy_recycler_cache");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let data_path = database_directory.path.join("socks.drw");
    let index_path = database_directory.path.join("socks_index.drw");
    let reclaimed_record = json!({
        "_id": "@socks:lnk_reclaimed",
        "color": "Gold"
    });
    let serialized_record =
        PlainTextJsonFormat::serialize_record(&reclaimed_record).expect("record should serialize");
    let target_size_class = Recycler::new().calculate_aligned_size(serialized_record.len());
    let dead_offset = {
        let mut data_writer =
            DatabaseWriter::open_drawer(&data_path).expect("data writer should open");
        let dead_offset = data_writer
            .append_record(&serialized_record, target_size_class)
            .expect("seed record should append");
        data_writer
            .write_tombstone_at_offset(dead_offset, target_size_class)
            .expect("seed record should tombstone");
        dead_offset
    };

    let mut drawer = Drawer::open(&database_directory.path, "socks", "_id", Vec::new())
        .expect("drawer should open before free-list index entry exists");
    let len_before_lazy_scan = fs::metadata(&data_path)
        .expect("data metadata should read")
        .len();

    let dead_index_entry = json!({
        "f": "_id",
        "k": "@socks:lnk_reclaimed",
        "o": dead_offset,
        "len": serialized_record.len(),
        "class": target_size_class,
        "crc": 0,
        "status": 0
    });
    let serialized_index =
        PlainTextJsonFormat::serialize_record(&dead_index_entry).expect("index should serialize");
    let index_size_class = Recycler::new().calculate_aligned_size(serialized_index.len());
    DatabaseWriter::open_drawer(&index_path)
        .expect("index writer should open")
        .append_aligned_index(&serialized_index, index_size_class)
        .expect("dead index entry should append after drawer open");

    drawer
        .upsert_record(reclaimed_record)
        .expect("upsert should succeed")
        .expect("record should validate");

    let len_after_lazy_reuse = fs::metadata(&data_path)
        .expect("data metadata should read")
        .len();
    assert_eq!(len_after_lazy_reuse, len_before_lazy_scan);

    let reused_offset = load_index_records(&database_directory, "socks")
        .iter()
        .rev()
        .find(|record| record["k"] == "@socks:lnk_reclaimed" && record["status"] == 1)
        .and_then(|record| record["o"].as_u64())
        .expect("live record should be written after lazy cache scan");
    assert_eq!(reused_offset, dead_offset);
}

#[test]
fn upsert_record_rejects_missing_primary_key() {
    let database_directory = TempDatabase::new("drawer_missing_primary_key");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let mut drawer = Drawer::open(&database_directory.path, "gem", "_id", Vec::new())
        .expect("drawer should open");

    let result = drawer
        .upsert_record(json!({
            "element": "Wind"
        }))
        .expect("upsert should not fail at io level");

    assert!(result.is_err());
}

#[test]
fn update_same_primary_key_rewrites_the_record() {
    let database_directory = TempDatabase::new("drawer_update_same_primary_key");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let mut drawer = Drawer::open(&database_directory.path, "weapon", "_id", Vec::new())
        .expect("drawer should open");

    drawer
        .upsert_record(json!({
            "_id": "@weapon:lnk_a",
            "name": "Hammer"
        }))
        .expect("initial upsert should succeed")
        .expect("record should validate");

    drawer
        .upsert_record(json!({
            "_id": "@weapon:lnk_a",
            "name": "Hammer Prime"
        }))
        .expect("update should succeed")
        .expect("updated record should validate");

    let current = drawer
        .find_by_primary_key("@weapon:lnk_a")
        .expect("lookup should succeed")
        .expect("record should exist");
    assert_eq!(current["name"], "Hammer Prime");

    let records = drawer.find_all_records().expect("find all should succeed");
    assert_eq!(records.len(), 1);

    let data_file = database_directory.path.join("weapon.drw");
    let data_contents = fs::read_to_string(&data_file).expect("data file should be readable");
    assert!(data_contents.contains("!!DEAD!!"));
}

#[test]
fn missing_secondary_lookups_return_empty_results() {
    let database_directory = TempDatabase::new("drawer_missing_secondary_lookup");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let mut drawer = Drawer::open(
        &database_directory.path,
        "gem",
        "_id",
        vec!["element".to_string()],
    )
    .expect("drawer should open");

    drawer
        .upsert_record(json!({
            "_id": "@gem:lnk_a",
            "element": "Fire"
        }))
        .expect("upsert should succeed")
        .expect("record should validate");

    assert!(
        drawer
            .find_by_primary_key("@gem:lnk_missing")
            .expect("lookup should succeed")
            .is_none()
    );
    assert!(
        drawer
            .find_by_secondary_key("element", "Water")
            .expect("lookup should succeed")
            .is_empty()
    );
    assert!(
        drawer
            .find_by_secondary_key("missing_field", "anything")
            .expect("lookup should succeed")
            .is_empty()
    );
}

#[test]
fn updating_secondary_field_removes_stale_index_entries() {
    let database_directory = TempDatabase::new("drawer_secondary_index_cleanup");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    {
        let mut drawer = Drawer::open(
            &database_directory.path,
            "gem",
            "_id",
            vec!["element".to_string()],
        )
        .expect("drawer should open");

        drawer
            .upsert_record(json!({
                "_id": "@gem:lnk_secondary_cleanup",
                "element": "Fire"
            }))
            .expect("initial upsert should succeed")
            .expect("record should validate");

        drawer
            .upsert_record(json!({
                "_id": "@gem:lnk_secondary_cleanup",
                "element": "Water"
            }))
            .expect("update should succeed")
            .expect("updated record should validate");

        assert!(
            drawer
                .find_by_secondary_key("element", "Fire")
                .expect("lookup should succeed")
                .is_empty()
        );

        let water_matches = drawer
            .find_by_secondary_key("element", "Water")
            .expect("lookup should succeed");
        assert_eq!(water_matches.len(), 1);
    }

    let reopened = Drawer::open(
        &database_directory.path,
        "gem",
        "_id",
        vec!["element".to_string()],
    )
    .expect("drawer should reopen");

    assert!(
        reopened
            .find_by_secondary_key("element", "Fire")
            .expect("lookup should succeed")
            .is_empty()
    );
    assert_eq!(
        reopened
            .find_by_secondary_key("element", "Water")
            .expect("lookup should succeed")
            .len(),
        1
    );
}

#[test]
fn us_035_drawer_without_schema_accepts_flexible_json_documents() {
    let database_directory = TempDatabase::new("us_035_schema_less_drawer");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let mut drawer = Drawer::open(&database_directory.path, "artifact", "_id", Vec::new())
        .expect("drawer should open");

    drawer
        .upsert_record(json!({
            "_id": "@artifact:lnk_flexible",
            "name": "Flexible",
            "nested": {
                "free_form": true
            },
            "tags": ["schema-less", 42]
        }))
        .expect("upsert should succeed")
        .expect("schema-less record should validate");

    let records = drawer.find_all_records().expect("records should read");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["nested"]["free_form"], true);
}

#[test]
fn us_035_drawer_schema_rejects_missing_required_or_wrong_type_fields() {
    let database_directory = TempDatabase::new("us_035_schema_rejects_invalid");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");
    write_metadata(
        &database_directory,
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
                    "name": { "type": "string", "minLength": 3 },
                    "damage": { "type": "integer", "minimum": 0 },
                    "rarity": { "enum": ["common", "rare"] }
                },
                "additionalProperties": false
            }
        }),
    );

    let mut drawer = Drawer::open(&database_directory.path, "weapon", "_id", Vec::new())
        .expect("drawer should open");

    let wrong_type = drawer
        .upsert_record(json!({
            "_id": "@weapon:lnk_wrong_type",
            "name": "Blade",
            "damage": "large"
        }))
        .expect("upsert should not fail at io level");
    assert!(
        wrong_type
            .expect_err("wrong type should fail validation")
            .contains("$.damage must be of type integer")
    );

    let missing_required = drawer
        .upsert_record(json!({
            "_id": "@weapon:lnk_missing_required",
            "name": "Blade"
        }))
        .expect("upsert should not fail at io level");
    assert!(
        missing_required
            .expect_err("missing field should fail validation")
            .contains("$.damage is required by schema")
    );

    let extra_field = drawer
        .upsert_record(json!({
            "_id": "@weapon:lnk_extra",
            "name": "Blade",
            "damage": 10,
            "weight": 3
        }))
        .expect("upsert should not fail at io level");
    assert!(
        extra_field
            .expect_err("extra field should fail validation")
            .contains("$.weight is not allowed by schema")
    );

    let invalid_enum = drawer
        .upsert_record(json!({
            "_id": "@weapon:lnk_invalid_enum",
            "name": "Blade",
            "damage": 10,
            "rarity": "legendary"
        }))
        .expect("upsert should not fail at io level");
    assert!(
        invalid_enum
            .expect_err("enum should fail validation")
            .contains("$.rarity must match one of the declared enum values")
    );

    drawer
        .upsert_record(json!({
            "_id": "@weapon:lnk_valid",
            "name": "Blade",
            "damage": 10,
            "rarity": "rare"
        }))
        .expect("valid record should write")
        .expect("valid record should pass schema");

    assert_eq!(drawer.record_count(), 1);
    let metadata = load_metadata(&database_directory, "weapon");
    assert!(metadata.get("schema").is_some());
    assert_eq!(metadata["record_count"], 1);
}

#[test]
fn us_042_vacuum_compacts_live_records_and_rebuilds_indexes() {
    let database_directory = TempDatabase::new("us_042_vacuum_compacts_drawer");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let data_path = database_directory.path.join("gem.drw");
    let mut drawer = Drawer::open(
        &database_directory.path,
        "gem",
        "_id",
        vec!["element".to_string()],
    )
    .expect("drawer should open");

    drawer
        .upsert_record(json!({
            "_id": "@gem:lnk_a",
            "element": "Fire",
            "description": "A very long ember payload that leaves visible slack after the update"
        }))
        .expect("upsert should succeed")
        .expect("record should validate");
    drawer
        .upsert_record(json!({
            "_id": "@gem:lnk_b",
            "element": "Water",
            "description": "This record will be removed before vacuuming"
        }))
        .expect("upsert should succeed")
        .expect("record should validate");
    drawer
        .upsert_record(json!({
            "_id": "@gem:lnk_a",
            "element": "Light",
            "description": "small"
        }))
        .expect("update should succeed")
        .expect("record should validate");
    drawer
        .delete_by_primary_key("@gem:lnk_b")
        .expect("delete should succeed")
        .expect("record should delete");

    let before_contents = fs::read_to_string(&data_path).expect("data file should read");
    let before_len = fs::metadata(&data_path)
        .expect("data metadata should read")
        .len();
    assert!(before_contents.contains("!!DEAD!!"));

    let report = drawer.vacuum().expect("vacuum should succeed");

    let after_contents = fs::read_to_string(&data_path).expect("data file should read");
    let after_len = fs::metadata(&data_path)
        .expect("data metadata should read")
        .len();

    assert_eq!(report.records_rewritten, 1);
    assert_eq!(report.data_bytes_before, before_len);
    assert_eq!(report.data_bytes_after, after_len);
    assert!(report.bytes_reclaimed > 0);
    assert!(after_len < before_len);
    assert!(!after_contents.contains("!!DEAD!!"));
    assert!(after_contents.lines().all(|line| line == line.trim_end()));

    assert_eq!(
        drawer
            .find_by_primary_key("@gem:lnk_a")
            .expect("lookup should succeed")
            .expect("record should exist")["element"],
        "Light"
    );
    assert!(
        drawer
            .find_by_primary_key("@gem:lnk_b")
            .expect("lookup should succeed")
            .is_none()
    );
    assert_eq!(
        drawer
            .find_by_secondary_key("element", "Light")
            .expect("secondary lookup should succeed")
            .len(),
        1
    );
    assert!(
        drawer
            .find_by_secondary_key("element", "Fire")
            .expect("secondary lookup should succeed")
            .is_empty()
    );

    let metadata = load_metadata(&database_directory, "gem");
    assert_eq!(metadata["record_count"], 1);

    drop(drawer);
    let reopened = Drawer::open(
        &database_directory.path,
        "gem",
        "_id",
        vec!["element".to_string()],
    )
    .expect("drawer should reopen");

    assert_eq!(reopened.record_count(), 1);
    assert_eq!(
        reopened
            .find_by_secondary_key("element", "Light")
            .expect("secondary lookup should survive restart")
            .len(),
        1
    );
}
