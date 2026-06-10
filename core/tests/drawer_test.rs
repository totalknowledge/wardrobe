mod common;

use common::TempDatabase;
use serde_json::json;
use std::fs;
use wardrobe_core::Drawer;

fn load_metadata(database_directory: &TempDatabase, drawer_name: &str) -> serde_json::Value {
    let metadata_path = database_directory
        .path
        .join(format!("{}_meta.drw", drawer_name));
    let metadata_contents =
        fs::read_to_string(metadata_path).expect("metadata sidecar should be readable");
    serde_json::from_str(&metadata_contents).expect("metadata sidecar should be valid json")
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

    let mut reopened = Drawer::open(&database_directory.path, "weapon", "_id", Vec::new())
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

    let mut drawer = Drawer::open(
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
fn us_015_index_journal_tracks_blocks_and_rebuilds_recycler_on_open() {
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

    let mut reopened = Drawer::open(
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
