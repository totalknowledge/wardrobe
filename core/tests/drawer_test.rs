mod common;

use common::TempDatabase;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use wardrobe_core::{
    BsonBinaryFormat, DatabaseReader, DatabaseWriter, Drawer, NativeBinaryIndexFormat, Recycler,
    StorageFormat,
};

const INDEX_FIELD_KEY: &str = "f";
const INDEX_VALUE_KEY: &str = "k";
const INDEX_OFFSET_KEY: &str = "o";
const INDEX_LENGTH_KEY: &str = "l";
const INDEX_SIZE_CLASS_KEY: &str = "c";
const INDEX_CRC_KEY: &str = "x";
const INDEX_STATUS_KEY: &str = "s";

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
    let index_reader = DatabaseReader::open_drawer(index_path).expect("index reader should open");
    let mut name_map = std::collections::BTreeMap::new();
    if let Some(map) = field_name_map_from_metadata(database_directory, drawer_name) {
        for (k, v) in map {
            if let Some(s) = v.as_str() {
                name_map.insert(k, s.to_string());
            }
        }
    }
    let mut records = Vec::new();
    index_reader
        .stream_with_offsets(|_offset, slot| {
            let is_dead =
                BsonBinaryFormat::is_tombstone(slot) || NativeBinaryIndexFormat::is_tombstone(slot);
            if !is_dead {
                let entry_opt = if BsonBinaryFormat::is_binary_frame(slot) {
                    BsonBinaryFormat::deserialize_record(slot).ok().flatten()
                } else if NativeBinaryIndexFormat::is_binary_frame(slot) {
                    NativeBinaryIndexFormat::deserialize_index_entry(slot, &name_map)
                        .ok()
                        .flatten()
                } else {
                    None
                };
                if let Some(value) = entry_opt {
                    records.push(value);
                }
            }
        })
        .expect("index should stream");
    if let Some(field_name_map) = field_name_map_from_metadata(database_directory, drawer_name) {
        for record in &mut records {
            if let Some(record_map) = record.as_object_mut() {
                if let Some(logical_field_name) = record_map
                    .get(INDEX_FIELD_KEY)
                    .and_then(serde_json::Value::as_str)
                    .and_then(|stored_field_name| field_name_map.get(stored_field_name))
                    .and_then(serde_json::Value::as_str)
                {
                    record_map.insert(INDEX_FIELD_KEY.to_string(), json!(logical_field_name));
                }
            }
        }
    }
    records
}

fn field_name_map_from_metadata(
    database_directory: &TempDatabase,
    drawer_name: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    load_metadata(database_directory, drawer_name)
        .get("field_name_map")?
        .as_object()
        .cloned()
}

fn compact_primary_index_record(
    key: &str,
    offset: u64,
    payload_len: usize,
    size_class: usize,
    status: u8,
) -> serde_json::Value {
    let mut record = serde_json::Map::new();
    record.insert(
        INDEX_FIELD_KEY.to_string(),
        serde_json::Value::String("_id".to_string()),
    );
    record.insert(
        INDEX_VALUE_KEY.to_string(),
        serde_json::Value::String(key.to_string()),
    );
    record.insert(
        INDEX_OFFSET_KEY.to_string(),
        serde_json::Value::from(offset),
    );
    record.insert(
        INDEX_LENGTH_KEY.to_string(),
        serde_json::Value::from(payload_len as u64),
    );
    record.insert(
        INDEX_SIZE_CLASS_KEY.to_string(),
        serde_json::Value::from(size_class as u64),
    );
    record.insert(INDEX_CRC_KEY.to_string(), serde_json::Value::from(0_u64));
    record.insert(
        INDEX_STATUS_KEY.to_string(),
        serde_json::Value::from(status as u64),
    );
    serde_json::Value::Object(record)
}

fn count_tombstones(path: &std::path::Path) -> usize {
    let reader = DatabaseReader::open_drawer(path).expect("reader should open");
    let mut count = 0usize;
    reader
        .stream_with_offsets(|_offset, slot| {
            if BsonBinaryFormat::is_tombstone(slot) {
                count += 1;
            }
        })
        .expect("stream should succeed");
    count
}

fn reopened_records_from_file(path: &std::path::Path) -> Vec<serde_json::Value> {
    let reader = DatabaseReader::open_drawer(path).expect("reader should open");
    let mut records = Vec::new();
    let field_name_map = field_name_map_from_data_path(path);
    let native_field_name_map = field_name_map.as_ref().map(|field_name_map| {
        field_name_map
            .iter()
            .filter_map(|(token, logical_name)| {
                logical_name
                    .as_str()
                    .map(|logical_name| (token.clone(), logical_name.to_string()))
            })
            .collect::<BTreeMap<_, _>>()
    });
    reader
        .stream_with_offsets(|_offset, slot| {
            if let Ok(Some(record)) =
                BsonBinaryFormat::deserialize_record_with_map(slot, native_field_name_map.as_ref())
            {
                records.push(record);
            }
        })
        .expect("stream should succeed");
    let Some(field_name_map) = field_name_map else {
        return records;
    };
    records
        .into_iter()
        .map(|record| decode_record_from_field_name_map(record, &field_name_map))
        .collect()
}

fn field_name_map_from_data_path(
    path: &std::path::Path,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let drawer_name = path.file_stem()?.to_str()?;
    let metadata_path = path.with_file_name(format!("{drawer_name}_meta.drw"));
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(metadata_path).ok()?).ok()?;
    metadata.get("field_name_map")?.as_object().cloned()
}

fn decode_record_from_field_name_map(
    value: serde_json::Value,
    field_name_map: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(field_name, field_value)| {
                    let logical_field_name = field_name_map
                        .get(&field_name)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(field_name.as_str())
                        .to_string();
                    (
                        logical_field_name,
                        decode_record_from_field_name_map(field_value, field_name_map),
                    )
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(|value| decode_record_from_field_name_map(value, field_name_map))
                .collect(),
        ),
        other => other,
    }
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

    let data_record =
        BsonBinaryFormat::serialize_record(&json!({"_id": "@weapon:lnk_a", "serial": "SER-1"}))
            .expect("data record should serialize");
    fs::write(&data_file, &data_record).expect("data file should write");

    let mut index_records = Vec::new();
    index_records.extend(
        BsonBinaryFormat::serialize_record(&compact_primary_index_record(
            "@weapon:lnk_a",
            0,
            data_record.len(),
            data_record.len(),
            1,
        ))
        .expect("primary index should serialize"),
    );
    index_records.extend(
        BsonBinaryFormat::serialize_record(&json!({"f": "serial", "k": "SER-1", "o": [0]}))
            .expect("secondary index should serialize"),
    );
    fs::write(&index_file, index_records).expect("index file should write");

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
fn non_unique_schema_indexes_materialize_lazily_for_duplicate_string_and_integer_values() {
    let database_directory = TempDatabase::new("drawer_non_unique_schema_indexes");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    {
        let mut drawer = Drawer::open(&database_directory.path, "book", "_id", Vec::new())
            .expect("drawer should open");
        drawer
            .manage_schema_rule("add", "index", "author_id", json!({ "kind": "index" }))
            .expect("author index should be registered");
        drawer
            .manage_schema_rule("add", "index", "purge_bucket", json!({ "kind": "index" }))
            .expect("purge index should be registered");

        for payload in [
            json!({"_id": "book_a", "author_id": "entity_a", "purge_bucket": 0}),
            json!({"_id": "book_b", "author_id": "entity_a", "purge_bucket": 0}),
            json!({"_id": "book_c", "author_id": "entity_b", "purge_bucket": 1}),
        ] {
            drawer
                .upsert_record(payload)
                .expect("upsert should succeed")
                .expect("record should validate");
        }

        let pre_materialization_index_records = load_index_records(&database_directory, "book");
        assert!(
            pre_materialization_index_records
                .iter()
                .all(|record| record["f"] == "_id")
        );

        assert_eq!(
            drawer
                .find_by_secondary_key("author_id", "entity_a")
                .expect("author lookup should succeed")
                .len(),
            2
        );
        assert_eq!(
            drawer
                .find_by_secondary_key("purge_bucket", "0")
                .expect("purge lookup should succeed")
                .len(),
            2
        );
        drawer.checkpoint().expect("drawer should checkpoint");
    }

    let mut reopened = Drawer::open(&database_directory.path, "book", "_id", Vec::new())
        .expect("drawer should reopen");

    assert_eq!(
        reopened
            .find_by_secondary_key("author_id", "entity_a")
            .expect("reopened author lookup should succeed")
            .len(),
        2
    );
    assert_eq!(
        reopened
            .find_by_secondary_key("purge_bucket", "0")
            .expect("reopened purge lookup should succeed")
            .len(),
        2
    );
    assert!(
        !reopened
            .unique_constraints
            .contains(&"author_id".to_string())
    );
    assert!(
        !reopened
            .unique_constraints
            .contains(&"purge_bucket".to_string())
    );
}

#[test]
fn us_111_lazy_secondary_index_is_invalidated_after_write_and_rebuilt_on_next_lookup() {
    let database_directory = TempDatabase::new("us_111_lazy_index_invalidates_after_write");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let mut drawer = Drawer::open(&database_directory.path, "book", "_id", Vec::new())
        .expect("drawer should open");
    drawer
        .manage_schema_rule("add", "index", "author_id", json!({ "kind": "index" }))
        .expect("author index should be registered");
    drawer
        .upsert_record(json!({"_id": "book_a", "author_id": "entity_a"}))
        .expect("upsert should succeed")
        .expect("record should validate");

    assert_eq!(
        drawer
            .find_by_secondary_key("author_id", "entity_a")
            .expect("first lookup should build lazy index")
            .len(),
        1
    );

    drawer
        .upsert_record(json!({"_id": "book_a", "author_id": "entity_b"}))
        .expect("update should succeed")
        .expect("record should validate");

    assert!(
        drawer
            .find_by_secondary_key("author_id", "entity_a")
            .expect("stale lookup should rebuild and scan")
            .is_empty()
    );
    assert_eq!(
        drawer
            .find_by_secondary_key("author_id", "entity_b")
            .expect("rebuilt lookup should use current value")
            .len(),
        1
    );

    drawer.checkpoint().expect("drawer should checkpoint");
    drop(drawer);

    let mut reopened = Drawer::open(&database_directory.path, "book", "_id", Vec::new())
        .expect("drawer should reopen");

    assert!(
        reopened
            .find_by_secondary_key("author_id", "entity_a")
            .expect("stale lookup should stay absent after reopen")
            .is_empty()
    );
    assert_eq!(
        reopened
            .find_by_secondary_key("author_id", "entity_b")
            .expect("rebuilt lookup should survive reopen")
            .len(),
        1
    );
}

#[test]
fn us_111_admin_rebuild_materializes_secondary_index_without_rewriting_drawer_data() {
    let database_directory = TempDatabase::new("us_111_admin_rebuild_secondary_index");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let mut drawer = Drawer::open(&database_directory.path, "book", "_id", Vec::new())
        .expect("drawer should open");
    drawer
        .manage_schema_rule("add", "index", "author_id", json!({ "kind": "index" }))
        .expect("author index should be registered");
    for payload in [
        json!({"_id": "book_a", "author_id": "entity_a"}),
        json!({"_id": "book_b", "author_id": "entity_a"}),
        json!({"_id": "book_c", "author_id": "entity_b"}),
    ] {
        drawer
            .upsert_record(payload)
            .expect("upsert should succeed")
            .expect("record should validate");
    }

    let data_path = database_directory.path.join("book.drw");
    let data_before = fs::read(&data_path).expect("data should read before rebuild");
    assert!(
        load_index_records(&database_directory, "book")
            .iter()
            .all(|record| record["f"] == "_id")
    );

    drawer
        .manage_schema_rule("rebuild", "index", "author_id", json!({}))
        .expect("index rebuild should succeed");

    let data_after = fs::read(&data_path).expect("data should read after rebuild");
    assert_eq!(data_after, data_before);
    assert!(
        load_index_records(&database_directory, "book")
            .iter()
            .any(|record| record["f"] == "author_id")
    );
    assert_eq!(
        drawer
            .find_by_secondary_key("author_id", "entity_a")
            .expect("rebuilt lookup should succeed")
            .len(),
        2
    );
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

    assert_eq!(
        count_tombstones(&database_directory.path.join("gem.drw")),
        1
    );
    assert!(count_tombstones(&database_directory.path.join("gem_index.drw")) >= 1);
    assert!(
        load_index_records(&database_directory, "gem")
            .iter()
            .all(|record| !(record["f"] == "_id" && record["k"] == "@gem:lnk_delete_me"))
    );
}

#[test]
fn drawer_checkpoint_creates_meta_and_syncs() -> std::io::Result<()> {
    let database_directory = TempDatabase::new("drawer_checkpoint");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");
    let mut drawer = Drawer::open(&database_directory.path, "testdrawer", "_id", Vec::new())?;
    drawer.checkpoint()?;
    let meta_path = database_directory.path.join("testdrawer_meta.drw");
    assert!(meta_path.exists());
    let data_path = database_directory.path.join("testdrawer.drw");
    let _ = fs::remove_file(&data_path);
    let _ = fs::remove_file(&meta_path);
    Ok(())
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
fn us_106_find_all_uses_primary_index_offsets_for_live_storage_order() {
    let database_directory = TempDatabase::new("us_106_find_all_primary_offsets");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");
    let data_path = database_directory.path.join("weapon.drw");

    {
        let mut drawer = Drawer::open(&database_directory.path, "weapon", "_id", Vec::new())
            .expect("drawer should open");
        drawer
            .upsert_record(json!({
                "_id": "@weapon:lnk_first",
                "name": "First Original"
            }))
            .expect("first upsert should succeed")
            .expect("first record should validate");
        drawer
            .upsert_record(json!({
                "_id": "@weapon:lnk_second",
                "name": "Second"
            }))
            .expect("second upsert should succeed")
            .expect("second record should validate");
        drawer
            .upsert_record(json!({
                "_id": "@weapon:lnk_first",
                "name": "First Updated"
            }))
            .expect("replacement should succeed")
            .expect("replacement should validate");
    }

    let orphan_record = BsonBinaryFormat::serialize_record(&json!({
        "_id": "@weapon:lnk_orphan",
        "name": "Unindexed Orphan"
    }))
    .expect("orphan should serialize");
    let orphan_size = Recycler::new().calculate_aligned_size(orphan_record.len());
    let mut writer = DatabaseWriter::open_drawer(&data_path).expect("data writer should open");
    writer
        .append_record(&orphan_record, orphan_size)
        .expect("orphan should append");

    let drawer = Drawer::open(&database_directory.path, "weapon", "_id", Vec::new())
        .expect("drawer should reopen");
    let records = drawer.find_all_records().expect("find all should succeed");

    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["_id"], "@weapon:lnk_second");
    assert_eq!(records[0]["name"], "Second");
    assert_eq!(records[1]["_id"], "@weapon:lnk_first");
    assert_eq!(records[1]["name"], "First Updated");
    assert!(
        !records
            .iter()
            .any(|record| record["_id"] == "@weapon:lnk_orphan")
    );
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
fn us_102_metadata_sidecar_defers_record_count_until_checkpoint() {
    let database_directory = TempDatabase::new("us_102_deferred_metadata_record_count");
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

    assert_eq!(drawer.record_count(), 1);
    let stale_metadata_after_upserts = load_metadata(&database_directory, "socks");
    assert_eq!(stale_metadata_after_upserts["record_count"], 0);

    drawer
        .checkpoint()
        .expect("checkpoint should flush dirty metadata");
    let checkpointed_metadata_after_upserts = load_metadata(&database_directory, "socks");
    assert_eq!(checkpointed_metadata_after_upserts["record_count"], 1);
    assert!(checkpointed_metadata_after_upserts.get("blocks").is_none());
    assert!(
        checkpointed_metadata_after_upserts
            .get("free_slots")
            .is_none()
    );

    drawer
        .upsert_record(json!({
            "_id": "@socks:lnk_b",
            "color": "Gold"
        }))
        .expect("third upsert should succeed")
        .expect("record should validate");
    assert_eq!(drawer.record_count(), 2);
    let stale_metadata_after_insert = load_metadata(&database_directory, "socks");
    assert_eq!(stale_metadata_after_insert["record_count"], 1);

    drawer
        .checkpoint()
        .expect("checkpoint should flush second insert metadata");
    let checkpointed_metadata_after_insert = load_metadata(&database_directory, "socks");
    assert_eq!(checkpointed_metadata_after_insert["record_count"], 2);

    drawer
        .delete_by_primary_key("@socks:lnk_a")
        .expect("delete should succeed")
        .expect("record should exist");
    assert_eq!(drawer.record_count(), 1);
    let stale_metadata_after_delete = load_metadata(&database_directory, "socks");
    assert_eq!(stale_metadata_after_delete["record_count"], 2);

    drawer
        .checkpoint()
        .expect("checkpoint should flush delete metadata");
    let checkpointed_metadata_after_delete = load_metadata(&database_directory, "socks");
    assert_eq!(checkpointed_metadata_after_delete["record_count"], 1);
}

#[test]
fn us_102_open_recovers_record_count_from_index_when_metadata_is_stale() {
    let database_directory = TempDatabase::new("us_102_recover_stale_record_count");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    {
        let mut drawer = Drawer::open(&database_directory.path, "gem", "_id", Vec::new())
            .expect("drawer should open");
        drawer
            .upsert_record(json!({
                "_id": "@gem:lnk_fire",
                "element": "Fire"
            }))
            .expect("upsert should succeed")
            .expect("record should validate");

        let stale_metadata = load_metadata(&database_directory, "gem");
        assert_eq!(stale_metadata["record_count"], 0);
    }

    let reopened = Drawer::open(&database_directory.path, "gem", "_id", Vec::new())
        .expect("drawer should reopen");
    assert_eq!(reopened.record_count(), 1);

    let recovered_metadata = load_metadata(&database_directory, "gem");
    assert_eq!(recovered_metadata["record_count"], 1);
}

#[test]
fn us_102_structural_metadata_persists_immediately_with_dirty_record_count() {
    let database_directory = TempDatabase::new("us_102_structural_metadata_immediate");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let mut drawer = Drawer::open(&database_directory.path, "weapon", "_id", Vec::new())
        .expect("drawer should open");
    drawer
        .upsert_record(json!({
            "_id": "@weapon:lnk_blade",
            "name": "Blade"
        }))
        .expect("upsert should succeed")
        .expect("record should validate");

    let stale_metadata = load_metadata(&database_directory, "weapon");
    assert_eq!(stale_metadata["record_count"], 0);

    drawer
        .register_relationship_constraint(
            "gem_slot",
            json!({
                "type": "M:1",
                "target_drawer": "gem"
            }),
        )
        .expect("relationship metadata should persist");

    let structural_metadata = load_metadata(&database_directory, "weapon");
    assert_eq!(structural_metadata["record_count"], 1);
    assert_eq!(
        structural_metadata["relationship_constraints"]["gem_slot"]["target_drawer"],
        "gem"
    );
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
        record[INDEX_STATUS_KEY] == 1
            && record.get(INDEX_LENGTH_KEY).is_some()
            && record.get(INDEX_SIZE_CLASS_KEY).is_some()
            && record.get(INDEX_CRC_KEY).is_some()
    }));
    assert_eq!(index_records.len(), 1);
    assert!(count_tombstones(&database_directory.path.join("socks_index.drw")) >= 1);

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
    let records_after_reuse = reopened_records_from_file(&data_path);

    assert_eq!(len_after_reuse, len_after_update);
    assert!(
        records_after_reuse
            .iter()
            .any(|record| record["_id"] == "@socks:lnk_b")
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
    let deleted_offset = load_index_records(&database_directory, "socks")
        .iter()
        .find(|record| record[INDEX_VALUE_KEY] == "@socks:lnk_a" && record[INDEX_STATUS_KEY] == 1)
        .and_then(|record| record[INDEX_OFFSET_KEY].as_u64())
        .expect("live index record should expose reusable offset");

    drawer
        .delete_by_primary_key("@socks:lnk_a")
        .expect("delete should succeed")
        .expect("record should exist");
    let len_after_delete = fs::metadata(&data_path)
        .expect("data metadata should read")
        .len();
    assert!(count_tombstones(&database_directory.path.join("socks_index.drw")) >= 1);

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
        .find(|record| record[INDEX_VALUE_KEY] == "@socks:lnk_b" && record[INDEX_STATUS_KEY] == 1)
        .and_then(|record| record[INDEX_OFFSET_KEY].as_u64())
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
    let stored_reclaimed_record = json!({
        "_": "@socks:lnk_reclaimed",
        "a": "Gold"
    });
    let seed_field_name_map = BTreeMap::from([
        ("_".to_string(), "_id".to_string()),
        ("a".to_string(), "color".to_string()),
    ]);
    let serialized_record =
        BsonBinaryFormat::serialize_native_record(&stored_reclaimed_record, &seed_field_name_map)
            .expect("record should serialize");
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

    let dead_index_entry = compact_primary_index_record(
        "@socks:lnk_reclaimed",
        dead_offset,
        serialized_record.len(),
        target_size_class,
        0,
    );
    let serialized_index =
        BsonBinaryFormat::serialize_record(&dead_index_entry).expect("index should serialize");
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
        .find(|record| {
            record[INDEX_VALUE_KEY] == "@socks:lnk_reclaimed" && record[INDEX_STATUS_KEY] == 1
        })
        .and_then(|record| record[INDEX_OFFSET_KEY].as_u64())
        .expect("live record should be written after lazy cache scan");
    assert_eq!(reused_offset, dead_offset);
}

#[test]
fn bug_001_repeat_upserts_tombstone_stale_index_entries_and_reuse_after_reopen() {
    let database_directory = TempDatabase::new("bug_001_index_recycler_repeat_upsert");
    fs::create_dir_all(&database_directory.path).expect("temp dir should create");

    let index_path = database_directory.path.join("socks_index.drw");
    let count_index_tombstones = || count_tombstones(&index_path);

    {
        let mut drawer = Drawer::open(&database_directory.path, "socks", "_id", Vec::new())
            .expect("drawer should open");

        for color in ["Blue", "Gold"] {
            drawer
                .upsert_record(json!({
                    "_id": format!("@socks:lnk_bug_001_{color}"),
                    "color": color,
                    "rerun": true
                }))
                .expect("initial upsert should succeed")
                .expect("record should validate");
        }

        for color in ["Blue", "Gold"] {
            drawer
                .upsert_record(json!({
                    "_id": format!("@socks:lnk_bug_001_{color}"),
                    "color": color,
                    "rerun": true
                }))
                .expect("repeat upsert should succeed")
                .expect("record should validate");
        }
    }

    assert_eq!(load_index_records(&database_directory, "socks").len(), 2);
    assert_eq!(count_index_tombstones(), 2);
    let index_len_after_tombstones = fs::metadata(&index_path)
        .expect("index metadata should read")
        .len();

    {
        let mut reopened = Drawer::open(&database_directory.path, "socks", "_id", Vec::new())
            .expect("drawer should reopen");

        for color in ["Blue", "Gold"] {
            reopened
                .upsert_record(json!({
                    "_id": format!("@socks:lnk_bug_001_{color}"),
                    "color": color,
                    "rerun": true
                }))
                .expect("post-reopen upsert should succeed")
                .expect("record should validate");
        }
    }

    assert_eq!(load_index_records(&database_directory, "socks").len(), 2);
    let tombstones_after_reuse = count_index_tombstones();
    assert!(tombstones_after_reuse >= 2);
    assert!(tombstones_after_reuse < 4);
    let index_len_after_reuse = fs::metadata(&index_path)
        .expect("index metadata should read")
        .len();
    assert!(index_len_after_reuse >= index_len_after_tombstones);
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
    assert!(count_tombstones(&data_file) >= 1);
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
    drawer
        .checkpoint()
        .expect("checkpoint should flush valid record count");
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

    let before_len = fs::metadata(&data_path)
        .expect("data metadata should read")
        .len();
    assert!(count_tombstones(&data_path) >= 1);

    let report = drawer.vacuum().expect("vacuum should succeed");

    let after_len = fs::metadata(&data_path)
        .expect("data metadata should read")
        .len();

    assert_eq!(report.records_rewritten, 1);
    assert_eq!(report.data_bytes_before, before_len);
    assert_eq!(report.data_bytes_after, after_len);
    assert!(report.bytes_reclaimed > 0);
    assert!(after_len < before_len);
    assert_eq!(count_tombstones(&data_path), 0);

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

    drawer
        .checkpoint()
        .expect("checkpoint should flush secondary index test metadata");
    let metadata = load_metadata(&database_directory, "gem");
    assert_eq!(metadata["record_count"], 1);

    drop(drawer);
    let mut reopened = Drawer::open(
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
