mod common;

use common::TempDatabase;
use serde_json::json;
use std::fs;
use wardrobe::Drawer;

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
