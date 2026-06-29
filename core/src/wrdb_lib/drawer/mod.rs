use crate::wrdb_lib::core::reader::DatabaseReader;
use crate::wrdb_lib::core::recycler::Recycler;
use crate::wrdb_lib::core::storage_format::{BsonBinaryFormat, NativeBinaryIndexFormat, StorageFormat};
use crate::wrdb_lib::core::writer::DatabaseWriter;
use crate::wrdb_lib::query;
use crate::wrdb_lib::wal::TransactionCoordinator;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

const DRAWER_METADATA_FORMAT_VERSION: u8 = 1;
const RESERVED_ID_FIELD_TOKEN: &str = "_";
const RESERVED_ID_FIELD_NAME: &str = "_id";
const FIELD_TOKEN_ALPHABET: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
pub(crate) const INDEX_FIELD_KEY: &str = "f";
pub(crate) const INDEX_VALUE_KEY: &str = "k";
pub(crate) const INDEX_OFFSET_KEY: &str = "o";
pub(crate) const INDEX_LENGTH_KEY: &str = "l";
pub(crate) const INDEX_SIZE_CLASS_KEY: &str = "c";
pub(crate) const INDEX_CRC_KEY: &str = "x";
pub(crate) const INDEX_STATUS_KEY: &str = "s";

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;

    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let least_significant_bit = crc & 1;
            crc >>= 1;
            if least_significant_bit != 0 {
                crc ^= 0xedb8_8320;
            }
        }
    }

    !crc
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct DrawerMetadata {
    #[serde(default)]
    pub(crate) format_version: u8,
    #[serde(default)]
    pub(crate) primary_key: String,
    #[serde(default)]
    pub(crate) record_count: usize,
    #[serde(default)]
    pub(crate) unique_constraints: Vec<String>,
    #[serde(default)]
    pub(crate) relationship_constraints: BTreeMap<String, Value>,
    #[serde(default)]
    pub(crate) delete_rules: BTreeMap<String, Value>,
    #[serde(default)]
    pub(crate) cascade_delete_rules: BTreeMap<String, bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) schema: Option<Value>,
    #[serde(default)]
    pub(crate) secondary_index_generation: u64,
    #[serde(default)]
    pub(crate) materialized_secondary_indexes: BTreeMap<String, u64>,
    #[serde(default)]
    pub(crate) field_name_map: BTreeMap<String, String>,
}

impl DrawerMetadata {
    pub(crate) fn load(path: &Path) -> std::io::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }

        let contents = std::fs::read_to_string(path)?;
        if contents.trim().is_empty() {
            return Ok(None);
        }

        match serde_json::from_str(&contents) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(_) => Ok(None),
        }
    }

    fn from_configuration(
        primary_key: &str,
        record_count: usize,
        unique_constraints: &[String],
        relationship_constraints: BTreeMap<String, Value>,
        delete_rules: BTreeMap<String, Value>,
        cascade_delete_rules: BTreeMap<String, bool>,
        schema: Option<Value>,
        secondary_index_generation: u64,
        materialized_secondary_indexes: BTreeMap<String, u64>,
        field_name_map: BTreeMap<String, String>,
    ) -> Self {
        Self {
            format_version: DRAWER_METADATA_FORMAT_VERSION,
            primary_key: primary_key.to_string(),
            record_count,
            unique_constraints: unique_constraints.to_vec(),
            relationship_constraints,
            delete_rules,
            cascade_delete_rules,
            schema,
            secondary_index_generation,
            materialized_secondary_indexes,
            field_name_map,
        }
    }

    #[cfg(test)]
    fn persist(&self, path: &Path) -> std::io::Result<()> {
        let serialized = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, &serialized)?;
        Ok(())
    }
}

const DATA_BLOCK_STATUS_DEAD: u8 = 0;
const DATA_BLOCK_STATUS_LIVE: u8 = 1;

#[derive(Clone, Copy, Debug)]
struct DataBlockIndexEntry {
    payload_len: usize,
    size_class: usize,
    crc: u32,
    status: u8,
}

impl DataBlockIndexEntry {
    fn live(payload: &[u8], size_class: usize) -> Self {
        Self {
            payload_len: payload.len(),
            size_class,
            crc: crc32(payload),
            status: DATA_BLOCK_STATUS_LIVE,
        }
    }

    fn from_index_entry(index_entry: &Value) -> Option<(u64, Self)> {
        let offset = index_entry
            .get(INDEX_OFFSET_KEY)
            .and_then(|value| value.as_u64())?;
        let payload_len = index_entry
            .get(INDEX_LENGTH_KEY)
            .and_then(|value| value.as_u64())? as usize;
        let size_class = index_entry
            .get(INDEX_SIZE_CLASS_KEY)
            .and_then(|value| value.as_u64())? as usize;
        let crc = index_entry
            .get(INDEX_CRC_KEY)
            .and_then(|value| value.as_u64())? as u32;
        let status = index_entry
            .get(INDEX_STATUS_KEY)
            .and_then(|value| value.as_u64())? as u8;

        Some((
            offset,
            Self {
                payload_len,
                size_class,
                crc,
                status,
            },
        ))
    }
}

#[derive(Debug)]
struct DeleteCandidate {
    key: String,
    offset: u64,
    record: Value,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct DrawerTestMetrics {
    find_all_records_with_migration_calls: usize,
    invalidate_materialized_query_indexes_calls: usize,
    persist_metadata_calls: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VacuumReport {
    pub records_rewritten: usize,
    pub data_bytes_before: u64,
    pub data_bytes_after: u64,
    pub index_bytes_before: u64,
    pub index_bytes_after: u64,
    pub bytes_reclaimed: u64,
}

pub struct Drawer {
    pub name: String,
    pub primary_key: String,
    pub unique_constraints: Vec<String>,

    data_writer: DatabaseWriter,
    data_reader: DatabaseReader,
    index_writer: DatabaseWriter,
    metadata_writer: DatabaseWriter,

    data_recycler: Recycler,
    data_recycler_cache_initialized: bool,
    index_recycler: Recycler,

    primary_memory_index: HashMap<String, u64>,
    secondary_memory_index: HashMap<String, HashMap<String, Vec<u64>>>,
    validated_secondary_indexes: HashSet<String>,
    materialized_secondary_indexes: BTreeMap<String, u64>,
    secondary_index_generation: u64,
    index_file_offsets: HashMap<String, (u64, usize)>,
    data_block_index: HashMap<u64, DataBlockIndexEntry>,
    relationship_constraints: BTreeMap<String, Value>,
    delete_rules: BTreeMap<String, Value>,
    cascade_delete_rules: BTreeMap<String, bool>,
    schema: Option<Value>,
    record_count: usize,
    metadata_dirty: bool,
    metadata_format_version: u8,
    field_name_map: BTreeMap<String, String>,
    #[cfg(test)]
    data_file_path: PathBuf,
    index_file_path: PathBuf,
    #[allow(dead_code)]
    meta_file_path: PathBuf,
    #[cfg(test)]
    test_metrics: DrawerTestMetrics,
}

mod compaction;
pub(crate) mod delete_rules;
mod deletion;
pub(crate) mod hydration;
mod indexing;
mod metadata;
mod migration;
mod mutation;
pub(crate) mod nested_decomposition;
pub(crate) mod relationship;
mod retrieval;
mod schema;
mod storage_blocks;
mod validation;

impl Drop for Drawer {
    fn drop(&mut self) {
        let _ = self.flush_metadata_if_dirty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("wardrobe_drawer_{name}_{nanos}"))
    }

    fn live_index_records(drawer: &mut Drawer) -> Vec<Value> {
        drawer.commit().expect("drawer should commit before read");

        let reader =
            DatabaseReader::open_drawer(&drawer.index_file_path).expect("index reader opens");
        let mut records = Vec::new();
        reader
            .stream_with_offsets(|_, line| {
                let is_dead = BsonBinaryFormat::is_tombstone(line) || NativeBinaryIndexFormat::is_tombstone(line);
                if !is_dead {
                    let record_opt = if BsonBinaryFormat::is_binary_frame(line) {
                        BsonBinaryFormat::deserialize_record(line).expect("index record should deserialize")
                    } else if NativeBinaryIndexFormat::is_binary_frame(line) {
                        NativeBinaryIndexFormat::deserialize_index_entry(line, &drawer.field_name_map)
                            .expect("index record should deserialize")
                    } else {
                        panic!("Unknown index frame magic in test");
                    };
                    records.push(record_opt.expect("index record should contain a value"));
                }
            })
            .expect("index should stream");
        records
    }

    fn live_data_records(drawer: &mut Drawer) -> Vec<Value> {
        drawer.commit().expect("drawer should commit before read");

        let reader =
            DatabaseReader::open_drawer(&drawer.data_file_path).expect("data reader opens");
        let mut records = Vec::new();
        reader
            .stream_with_offsets(|_, line| {
                if !BsonBinaryFormat::is_tombstone(line) {
                    records.push(
                        BsonBinaryFormat::deserialize_record(line)
                            .expect("data record should deserialize")
                            .expect("data record should contain a value"),
                    );
                }
            })
            .expect("data should stream");
        records
    }

    fn assert_record_keys(record: &Value, expected: &[&str]) {
        let object = record.as_object().expect("index record should be object");
        let actual = object
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        let expected = expected
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, expected);
    }

    fn assert_compact_index_records(records: &[Value]) {
        let mut primary_count = 0usize;
        let mut secondary_count = 0usize;

        for record in records {
            let field = record
                .get(INDEX_FIELD_KEY)
                .and_then(Value::as_str)
                .expect("index record should include compact field key");
            if field == RESERVED_ID_FIELD_TOKEN {
                primary_count += 1;
                assert_record_keys(
                    record,
                    &[
                        INDEX_CRC_KEY,
                        INDEX_FIELD_KEY,
                        INDEX_VALUE_KEY,
                        INDEX_LENGTH_KEY,
                        INDEX_OFFSET_KEY,
                        INDEX_STATUS_KEY,
                        INDEX_SIZE_CLASS_KEY,
                    ],
                );
            } else {
                secondary_count += 1;
                assert_record_keys(
                    record,
                    &[INDEX_FIELD_KEY, INDEX_VALUE_KEY, INDEX_OFFSET_KEY],
                );
                assert_eq!(field, "a");
            }
        }

        assert!(
            primary_count > 0,
            "expected at least one primary index record"
        );
        assert!(
            secondary_count > 0,
            "expected at least one secondary index record"
        );
    }

    #[test]
    fn metadata_crc_and_index_helpers_cover_round_trips() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);

        let metadata_path = temp_dir("metadata").join("gem_meta.drw");
        assert!(
            DrawerMetadata::load(&metadata_path)
                .expect("missing metadata should load")
                .is_none()
        );
        std::fs::create_dir_all(metadata_path.parent().unwrap()).expect("dir should create");
        std::fs::write(&metadata_path, "   ").expect("empty metadata should write");
        assert!(
            DrawerMetadata::load(&metadata_path)
                .expect("empty metadata should load")
                .is_none()
        );
        std::fs::write(&metadata_path, "{bad-json").expect("invalid metadata should write");
        assert!(
            DrawerMetadata::load(&metadata_path)
                .expect("invalid metadata should be ignored")
                .is_none()
        );

        let metadata = DrawerMetadata::from_configuration(
            "_id",
            2,
            &["slug".to_string()],
            BTreeMap::from([("owner".to_string(), json!({"type": "N:1"}))]),
            BTreeMap::from([("owner".to_string(), json!({"action": "Cascade"}))]),
            BTreeMap::from([("owner".to_string(), true)]),
            Some(json!({"type": "object"})),
            3,
            BTreeMap::from([("element".to_string(), 3)]),
            BTreeMap::from([
                (
                    RESERVED_ID_FIELD_TOKEN.to_string(),
                    RESERVED_ID_FIELD_NAME.to_string(),
                ),
                ("a".to_string(), "element".to_string()),
            ]),
        );
        metadata.persist(&metadata_path).expect("metadata persists");
        let loaded = DrawerMetadata::load(&metadata_path)
            .expect("metadata should load")
            .expect("metadata should exist");
        assert_eq!(loaded.primary_key, "_id");
        assert_eq!(loaded.record_count, 2);
        assert_eq!(loaded.materialized_secondary_indexes["element"], 3);
        assert_eq!(loaded.field_name_map["a"], "element");

        let block = DataBlockIndexEntry::live(b"payload", 16);
        let index_entry =
            Drawer::index_entry_value("element", "Fire", Value::from(42_u64), Some(block));
        assert_record_keys(
            &index_entry,
            &[
                INDEX_CRC_KEY,
                INDEX_FIELD_KEY,
                INDEX_VALUE_KEY,
                INDEX_LENGTH_KEY,
                INDEX_OFFSET_KEY,
                INDEX_STATUS_KEY,
                INDEX_SIZE_CLASS_KEY,
            ],
        );
        let (offset, parsed_block) =
            DataBlockIndexEntry::from_index_entry(&index_entry).expect("index should parse");
        assert_eq!(offset, 42);
        assert_eq!(parsed_block.payload_len, 7);
        assert_eq!(parsed_block.size_class, 16);
        assert_eq!(parsed_block.status, DATA_BLOCK_STATUS_LIVE);
        let mut incomplete_index_entry = Map::new();
        incomplete_index_entry.insert(INDEX_OFFSET_KEY.to_string(), Value::from(1_u64));
        assert!(
            DataBlockIndexEntry::from_index_entry(&Value::Object(incomplete_index_entry)).is_none()
        );

        let secondary_entry =
            Drawer::index_entry_value("element", "Fire", Value::from(42_u64), None);
        assert_record_keys(
            &secondary_entry,
            &[INDEX_FIELD_KEY, INDEX_VALUE_KEY, INDEX_OFFSET_KEY],
        );

        let mut compact_payload = Vec::new();
        assert_eq!(
            Drawer::append_compact_payload(&mut compact_payload, b"abc"),
            0
        );
        assert_eq!(
            Drawer::append_compact_payload(&mut compact_payload, b"def"),
            3
        );
        let compact_index_start = compact_payload.len();
        let (index_offset, index_len) =
            Drawer::append_compact_index_entry(&mut compact_payload, &index_entry, &loaded.field_name_map)
                .expect("compact index entry should append");
        assert_eq!(index_offset, compact_index_start as u64);
        assert!(index_len > 0);

        assert_eq!(Drawer::offsets_index_value(&[1, 3, 5]), json!([1, 3, 5]));
        assert_eq!(
            Drawer::intersect_sorted_offsets(vec![1, 2, 4, 7], vec![2, 3, 4, 8]),
            vec![2, 4]
        );

        let _ = std::fs::remove_dir_all(metadata_path.parent().unwrap());
    }

    #[test]
    fn primary_and_secondary_index_records_use_compact_field_names() {
        let path = temp_dir("compact_index_records");
        std::fs::create_dir_all(&path).expect("drawer directory should create");
        let mut drawer =
            Drawer::open(&path, "gem", "_id", vec!["element".to_string()]).expect("drawer opens");

        drawer
            .upsert_record(json!({"_id": "ruby", "element": "fire"}))
            .expect("ruby upsert should write")
            .expect("ruby upsert should validate");
        drawer
            .upsert_record(json!({"_id": "sapphire", "element": "water"}))
            .expect("sapphire upsert should write")
            .expect("sapphire upsert should validate");

        let records = live_index_records(&mut drawer);
        assert_compact_index_records(&records);

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn compaction_rewrites_index_records_with_compact_field_names() {
        let path = temp_dir("compact_index_after_vacuum");
        std::fs::create_dir_all(&path).expect("drawer directory should create");
        let mut drawer =
            Drawer::open(&path, "gem", "_id", vec!["element".to_string()]).expect("drawer opens");

        drawer
            .upsert_record(json!({"_id": "ruby", "element": "fire"}))
            .expect("ruby upsert should write")
            .expect("ruby upsert should validate");
        drawer
            .upsert_record(json!({"_id": "sapphire", "element": "water"}))
            .expect("sapphire upsert should write")
            .expect("sapphire upsert should validate");
        drawer
            .delete_by_primary_key("ruby")
            .expect("delete should write");

        let report = drawer.vacuum().expect("vacuum should compact");
        assert_eq!(report.records_rewritten, 1);

        let records = live_index_records(&mut drawer);
        assert_compact_index_records(&records);

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn drawer_reopens_from_compact_index_format() {
        let path = temp_dir("compact_index_reopen");
        std::fs::create_dir_all(&path).expect("drawer directory should create");
        {
            let mut drawer = Drawer::open(&path, "gem", "_id", vec!["element".to_string()])
                .expect("drawer opens");
            drawer
                .upsert_record(json!({"_id": "ruby", "element": "fire"}))
                .expect("ruby upsert should write")
                .expect("ruby upsert should validate");
            drawer.commit().expect("drawer should commit");
        }

        let mut reopened = Drawer::open(&path, "gem", "_id", vec!["element".to_string()])
            .expect("drawer should reopen compact index");
        let record = reopened
            .find_by_primary_key("ruby")
            .expect("primary read should work")
            .expect("record should exist");
        assert_eq!(record["element"], "fire");
        let secondary_records = reopened
            .find_by_secondary_key("element", "fire")
            .expect("secondary read should work");
        assert_eq!(secondary_records.len(), 1);

        let records = live_index_records(&mut reopened);
        assert_compact_index_records(&records);

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn field_name_encoding_maps_records_and_reuses_stable_tokens() {
        let path = temp_dir("field_name_encoding_tokens");
        std::fs::create_dir_all(&path).expect("drawer directory should create");
        let mut drawer = Drawer::open(&path, "person", "_id", Vec::new()).expect("drawer opens");

        drawer
            .upsert_record(json!({
                "_id": "person-1",
                "name": "Bob",
                "age": 56,
                "weight": 210
            }))
            .expect("upsert should write")
            .expect("upsert should validate");

        let id_token = drawer.stored_field_name("_id");
        let name_token = drawer.stored_field_name("name");
        let age_token = drawer.stored_field_name("age");
        let weight_token = drawer.stored_field_name("weight");

        assert_eq!(id_token, RESERVED_ID_FIELD_TOKEN);
        assert_ne!(name_token, "name");
        assert_ne!(age_token, "age");
        assert_ne!(weight_token, "weight");

        let raw_records = live_data_records(&mut drawer);
        assert_eq!(raw_records.len(), 1);
        assert_eq!(raw_records[0][&id_token], "person-1");
        assert_eq!(raw_records[0][&name_token], "Bob");
        assert_eq!(raw_records[0][&age_token], 56);
        assert_eq!(raw_records[0][&weight_token], 210);
        assert!(raw_records[0].get("_id").is_none());
        assert!(raw_records[0].get("name").is_none());
        assert!(raw_records[0].get("age").is_none());
        assert!(raw_records[0].get("weight").is_none());

        let decoded = drawer
            .find_by_primary_key("person-1")
            .expect("read should decode")
            .expect("record should exist");
        assert_eq!(decoded["_id"], "person-1");
        assert_eq!(decoded["name"], "Bob");
        assert_eq!(decoded["age"], 56);
        assert_eq!(decoded["weight"], 210);

        drawer
            .upsert_record(json!({
                "_id": "person-2",
                "name": "Ada",
                "age": 37,
                "weight": 130,
                "height": 64
            }))
            .expect("second upsert should write")
            .expect("second upsert should validate");

        assert_eq!(drawer.stored_field_name("name"), name_token);
        assert_eq!(drawer.stored_field_name("age"), age_token);
        assert_eq!(drawer.stored_field_name("weight"), weight_token);
        assert_ne!(drawer.stored_field_name("height"), "height");

        let metadata = DrawerMetadata::load(&drawer.meta_file_path)
            .expect("metadata should load")
            .expect("metadata should exist");
        assert_eq!(metadata.field_name_map[RESERVED_ID_FIELD_TOKEN], "_id");
        assert_eq!(metadata.field_name_map[&name_token], "name");
        assert_eq!(metadata.field_name_map[&age_token], "age");
        assert_eq!(metadata.field_name_map[&weight_token], "weight");

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn logical_filters_and_indexed_queries_work_with_encoded_storage() {
        let path = temp_dir("field_name_encoding_filter_index");
        std::fs::create_dir_all(&path).expect("drawer directory should create");
        let mut drawer = Drawer::open(&path, "book", "_id", Vec::new()).expect("drawer opens");

        drawer
            .upsert_record(json!({"_id": "book-1", "name": "Dune", "genre": "sci-fi"}))
            .expect("first upsert should write")
            .expect("first upsert should validate");
        drawer
            .upsert_record(json!({"_id": "book-2", "name": "Emma", "genre": "classic"}))
            .expect("second upsert should write")
            .expect("second upsert should validate");
        drawer.record_schema_extension("indexes", "name", json!({"type": "hash"}));
        drawer
            .materialize_query_index("name")
            .expect("query index should materialize");

        let name_token = drawer.stored_field_name("name");
        let index_records = live_index_records(&mut drawer);
        assert!(
            index_records.iter().any(|record| {
                record.get(INDEX_FIELD_KEY).and_then(Value::as_str) == Some(name_token.as_str())
            }),
            "index file should store the encoded field token"
        );
        assert!(
            index_records.iter().all(|record| {
                record.get(INDEX_FIELD_KEY).and_then(Value::as_str) != Some("name")
            }),
            "index file should not leak logical field names"
        );

        let mut filter = Map::new();
        filter.insert("name".to_string(), json!("Dune"));
        let offsets = drawer
            .indexed_candidate_offsets(&filter)
            .expect("logical filter should use encoded index")
            .expect("indexed candidates should be available");
        assert_eq!(offsets.len(), 1);

        let records = drawer
            .records_matching_filter_candidates(&filter, None)
            .expect("logical filter should match decoded records");
        assert_eq!(
            records,
            vec![json!({"_id": "book-1", "name": "Dune", "genre": "sci-fi"})]
        );

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn legacy_drawer_without_field_map_reads_and_compacts_to_encoded_storage() {
        let path = temp_dir("field_name_encoding_legacy_compact");
        std::fs::create_dir_all(&path).expect("drawer directory should create");
        let data_path = path.join("gem.drw");
        let index_path = path.join("gem_index.drw");
        let meta_path = path.join("gem_meta.drw");

        let legacy_record = json!({"_id": "ruby", "name": "Ruby"});
        let serialized_record =
            BsonBinaryFormat::serialize_record(&legacy_record).expect("legacy record serializes");
        let mut data_writer = DatabaseWriter::open_drawer(&data_path).expect("data writer opens");
        let data_offset = data_writer
            .append_record(&serialized_record, serialized_record.len())
            .expect("legacy record writes");
        let legacy_block = DataBlockIndexEntry::live(&serialized_record, serialized_record.len());
        let primary_index_entry =
            Drawer::index_entry_value("_id", "ruby", Value::from(data_offset), Some(legacy_block));
        let serialized_index =
            BsonBinaryFormat::serialize_record(&primary_index_entry).expect("index serializes");
        let mut index_writer =
            DatabaseWriter::open_drawer(&index_path).expect("index writer opens");
        index_writer
            .append_record(&serialized_index, serialized_index.len())
            .expect("legacy index writes");
        std::fs::write(
            &meta_path,
            serde_json::to_vec_pretty(&json!({
                "format_version": DRAWER_METADATA_FORMAT_VERSION,
                "primary_key": "_id",
                "record_count": 1,
                "unique_constraints": []
            }))
            .expect("legacy metadata serializes"),
        )
        .expect("legacy metadata writes");

        let mut drawer = Drawer::open(&path, "gem", "_id", Vec::new()).expect("legacy opens");
        let legacy_read = drawer
            .find_by_primary_key("ruby")
            .expect("legacy read should succeed")
            .expect("legacy record should exist");
        assert_eq!(legacy_read, legacy_record);

        drawer
            .upsert_record(json!({"_id": "sapphire", "name": "Sapphire", "color": "Blue"}))
            .expect("new upsert should write")
            .expect("new upsert should validate");
        let name_token = drawer.stored_field_name("name");
        let color_token = drawer.stored_field_name("color");

        drawer
            .vacuum()
            .expect("vacuum should encode legacy records");
        let raw_records = live_data_records(&mut drawer);
        assert_eq!(raw_records.len(), 2);
        assert!(raw_records.iter().all(|record| record.get("_id").is_none()));
        assert!(
            raw_records
                .iter()
                .all(|record| record.get("name").is_none())
        );
        assert!(
            raw_records
                .iter()
                .any(|record| record.get(&name_token).and_then(Value::as_str) == Some("Ruby"))
        );
        assert!(
            raw_records
                .iter()
                .any(|record| record.get(&color_token).and_then(Value::as_str) == Some("Blue"))
        );

        let reopened = Drawer::open(&path, "gem", "_id", Vec::new()).expect("reopen succeeds");
        let decoded = reopened.find_all_records().expect("records decode");
        assert!(decoded.contains(&json!({"_id": "ruby", "name": "Ruby"})));
        assert!(decoded.contains(&json!({
            "_id": "sapphire",
            "name": "Sapphire",
            "color": "Blue"
        })));

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn filter_delete_uses_indexed_offsets_and_batches_side_effects() {
        let path = temp_dir("set_based_indexed_delete");
        std::fs::create_dir_all(&path).expect("drawer directory should create");
        let mut drawer =
            Drawer::open(&path, "book", "_id", vec!["serial".to_string()]).expect("drawer opens");

        for record in [
            json!({"_id": "book-0", "serial": "serial-0", "purge_bucket": 0}),
            json!({"_id": "book-1", "serial": "serial-1", "purge_bucket": 0}),
            json!({"_id": "book-2", "serial": "serial-2", "purge_bucket": 0}),
            json!({"_id": "book-3", "serial": "serial-3", "purge_bucket": 1}),
            json!({"_id": "book-4", "serial": "serial-4", "purge_bucket": 1}),
        ] {
            drawer
                .upsert_record(record)
                .expect("upsert should write")
                .expect("upsert should validate");
        }

        drawer.record_schema_extension("indexes", "purge_bucket", json!({"type": "hash"}));
        drawer
            .materialize_query_index("purge_bucket")
            .expect("query index should materialize");

        let deleted_offsets = ["book-0", "book-1", "book-2"]
            .into_iter()
            .map(|key| {
                drawer
                    .primary_memory_index
                    .get(key)
                    .copied()
                    .expect("primary offset should exist")
            })
            .collect::<Vec<_>>();

        let duplicate_offset = deleted_offsets[0];
        drawer
            .secondary_memory_index
            .get_mut("purge_bucket")
            .expect("purge index should exist")
            .get_mut("0")
            .expect("purge bucket should exist")
            .push(duplicate_offset);

        drawer.reset_test_metrics();
        let mut filter = Map::new();
        filter.insert("purge_bucket".to_string(), json!(0));

        let deleted = drawer
            .delete_by_filter_set_based(&filter, None)
            .expect("indexed delete should succeed");

        assert_eq!(deleted, 3);
        assert_eq!(drawer.record_count(), 2);
        assert_eq!(drawer.test_metrics.find_all_records_with_migration_calls, 0);
        assert_eq!(
            drawer
                .test_metrics
                .invalidate_materialized_query_indexes_calls,
            1
        );
        assert_eq!(drawer.test_metrics.persist_metadata_calls, 1);

        for key in ["book-0", "book-1", "book-2"] {
            assert!(!drawer.primary_memory_index.contains_key(key));
        }
        for key in ["book-3", "book-4"] {
            assert!(drawer.primary_memory_index.contains_key(key));
        }

        for offset in deleted_offsets {
            assert!(
                drawer
                    .data_reader
                    .read_record_at_offset(offset)
                    .expect("tombstoned offset should read")
                    .is_none()
            );
            assert!(!drawer.data_block_index.contains_key(&offset));
        }

        let serial_index = drawer
            .secondary_memory_index
            .get("serial")
            .expect("serial index should remain materialized");
        for serial in ["serial-0", "serial-1", "serial-2"] {
            assert!(
                !serial_index.contains_key(serial),
                "empty secondary index bucket should be removed"
            );
        }
        for serial in ["serial-3", "serial-4"] {
            assert!(serial_index.contains_key(serial));
        }
        assert!(
            !drawer.secondary_memory_index.contains_key("purge_bucket"),
            "query index should be invalidated once after the batch"
        );

        let metadata = DrawerMetadata::load(&drawer.meta_file_path)
            .expect("metadata should load")
            .expect("metadata should exist");
        assert_eq!(metadata.record_count, 2);

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn primary_key_batch_delete_deduplicates_keys_and_flushes_metadata_once() {
        let path = temp_dir("set_based_primary_delete");
        std::fs::create_dir_all(&path).expect("drawer directory should create");
        let mut drawer = Drawer::open(&path, "gem", "_id", vec![]).expect("drawer opens");

        for record in [
            json!({"_id": "ruby", "element": "fire"}),
            json!({"_id": "sapphire", "element": "water"}),
            json!({"_id": "emerald", "element": "earth"}),
        ] {
            drawer
                .upsert_record(record)
                .expect("upsert should write")
                .expect("upsert should validate");
        }
        drawer.commit().expect("baseline metadata should persist");
        drawer.reset_test_metrics();

        let deleted = drawer
            .delete_by_primary_keys_set_based(vec![
                "ruby".to_string(),
                "ruby".to_string(),
                "sapphire".to_string(),
                "missing".to_string(),
            ])
            .expect("batch delete should succeed");

        assert_eq!(deleted, 2);
        assert_eq!(drawer.record_count(), 1);
        assert_eq!(
            drawer
                .test_metrics
                .invalidate_materialized_query_indexes_calls,
            1
        );
        assert_eq!(drawer.test_metrics.persist_metadata_calls, 1);
        assert!(!drawer.primary_memory_index.contains_key("ruby"));
        assert!(!drawer.primary_memory_index.contains_key("sapphire"));
        assert!(drawer.primary_memory_index.contains_key("emerald"));

        let metadata = DrawerMetadata::load(&drawer.meta_file_path)
            .expect("metadata should load")
            .expect("metadata should exist");
        assert_eq!(metadata.record_count, 1);

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn schema_normalization_validation_and_index_key_helpers_cover_edges() {
        assert_eq!(Drawer::normalize_schema_kind("indexes").unwrap(), "index");
        assert_eq!(Drawer::normalize_schema_kind("keys").unwrap(), "key");
        assert_eq!(
            Drawer::normalize_schema_kind("constraints").unwrap(),
            "constraint"
        );
        assert_eq!(
            Drawer::normalize_schema_kind("triggers").unwrap(),
            "trigger"
        );
        assert_eq!(
            Drawer::normalize_schema_kind("delete-rules").unwrap(),
            "cascade-delete"
        );
        assert!(Drawer::normalize_schema_kind("unknown").is_err());

        assert!(Drawer::validate_schema_field("field").is_ok());
        assert!(Drawer::validate_schema_field("  ").is_err());
        assert_eq!(
            Drawer::constraint_type(&json!({"constraint": "unique"})).unwrap(),
            "unique"
        );
        assert_eq!(
            Drawer::constraint_type(&json!({"constraint_type": "required"})).unwrap(),
            "required"
        );
        assert_eq!(
            Drawer::constraint_type(&json!({"type": "non-null"})).unwrap(),
            "non-null"
        );
        assert!(Drawer::constraint_type(&json!({})).is_err());
        assert!(Drawer::is_unique_constraint("UNIQUE"));
        assert!(Drawer::is_required_constraint("non_null"));
        assert!(Drawer::is_required_constraint("required"));
        assert!(!Drawer::is_required_constraint("unique"));

        assert_eq!(
            Drawer::secondary_index_key(&Value::String("Fire".to_string())),
            Some("Fire".to_string())
        );
        assert_eq!(
            Drawer::secondary_index_key(&Value::from(7)),
            Some("7".to_string())
        );
        assert_eq!(
            Drawer::secondary_index_key(&Value::Bool(true)),
            Some("true".to_string())
        );
        assert_eq!(Drawer::secondary_index_key(&Value::Null), None);
        assert_eq!(
            Drawer::equality_filter_index_key(&Value::String("Water".to_string())),
            Some("Water".to_string())
        );
        assert_eq!(
            Drawer::equality_filter_index_key(&Value::String("Wa%".to_string())),
            None
        );
        assert_eq!(
            Drawer::equality_filter_index_key(&Value::Bool(false)),
            Some("false".to_string())
        );

        assert!(Drawer::value_matches_type(&json!([]), "array"));
        assert!(Drawer::value_matches_type(&json!(true), "boolean"));
        assert!(Drawer::value_matches_type(&json!(7), "integer"));
        assert!(Drawer::value_matches_type(&Value::Null, "null"));
        assert!(Drawer::value_matches_type(&json!(7.5), "number"));
        assert!(Drawer::value_matches_type(&json!({}), "object"));
        assert!(Drawer::value_matches_type(&json!("x"), "string"));
        assert!(!Drawer::value_matches_type(&json!("x"), "integer"));

        assert!(Drawer::validate_type_rule(&json!("x"), &json!("string"), "$").is_ok());
        assert!(Drawer::validate_type_rule(&json!("x"), &json!("integer"), "$").is_err());
        assert!(Drawer::validate_type_rule(&json!(1), &json!(["string", "integer"]), "$").is_ok());
        assert!(
            Drawer::validate_type_rule(&json!(true), &json!(["string", "integer"]), "$").is_err()
        );

        let schema = json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": {"type": "string", "minLength": 2, "maxLength": 4},
                "age": {"type": "integer", "minimum": 0, "maximum": 120},
                "element": {"enum": ["Fire", "Water"]}
            },
            "additionalProperties": false
        });
        assert!(
            Drawer::validate_value_against_schema(
                &json!({"name": "Ava", "age": 42, "element": "Fire"}),
                &schema,
                "$",
            )
            .is_ok()
        );
        assert!(Drawer::validate_value_against_schema(&json!({"age": 42}), &schema, "$").is_err());
        assert!(
            Drawer::validate_value_against_schema(&json!({"name": "A"}), &schema, "$").is_err()
        );
        assert!(
            Drawer::validate_value_against_schema(&json!({"name": "Avery"}), &schema, "$").is_err()
        );
        assert!(
            Drawer::validate_value_against_schema(&json!({"name": "Ava", "age": -1}), &schema, "$")
                .is_err()
        );
        assert!(
            Drawer::validate_value_against_schema(
                &json!({"name": "Ava", "element": "Air"}),
                &schema,
                "$",
            )
            .is_err()
        );
        assert!(
            Drawer::validate_value_against_schema(
                &json!({"name": "Ava", "extra": true}),
                &schema,
                "$",
            )
            .is_err()
        );
    }

    #[test]
    fn relationship_legacy_and_delete_rule_helpers_cover_validation_paths() {
        let owner_rule = json!({"type": "N:1", "target_drawer": "owner"});
        assert_eq!(Drawer::relationship_type(&owner_rule), Some("N:1"));
        assert_eq!(
            Drawer::relationship_target_drawer(&owner_rule),
            Some("owner")
        );
        assert_eq!(
            Drawer::pointer_drawer_name("@team_owner:lnk_1"),
            Some("team_owner")
        );
        assert!(Drawer::pointer_drawer_name("@owner:bad:extra").is_none());
        assert!(Drawer::pointer_matches_target_drawer("team_owner", "owner"));
        assert!(!Drawer::pointer_matches_target_drawer("teamowner", "owner"));

        assert!(
            Drawer::validate_reference_field("owner", &json!("@team_owner:lnk_1"), &owner_rule)
                .is_none()
        );
        assert!(
            Drawer::validate_reference_field("owner", &json!(42), &owner_rule)
                .expect("non-string should fail")
                .contains("pointer string")
        );
        assert!(
            Drawer::validate_reference_field("owner", &json!("@team_role:1"), &owner_rule)
                .expect("wrong target should fail")
                .contains("expected target drawer")
        );

        assert!(
            Drawer::validate_many_to_many_field(
                "links",
                &json!(["@team_owner:1", "@owner:2"]),
                &owner_rule,
            )
            .is_none()
        );
        assert!(
            Drawer::validate_many_to_many_field("links", &json!("@owner:1"), &owner_rule)
                .expect("non-array should fail")
                .contains("must be an array")
        );
        assert!(
            Drawer::validate_many_to_many_field("links", &json!([42]), &owner_rule)
                .expect("non-string array item should fail")
                .contains("only pointer strings")
        );

        assert!(Drawer::delete_rule_is_cascade(&json!("Cascade")));
        assert!(Drawer::delete_rule_is_cascade(
            &json!({"action": "Cascade"})
        ));
        assert!(!Drawer::delete_rule_is_cascade(
            &json!({"action": "Restrict"})
        ));

        assert_eq!(
            Drawer::try_parse_legacy_pointer("@gem:lnk_fire"),
            Some(("gem".to_string(), "fire".to_string()))
        );
        assert!(Drawer::try_parse_legacy_pointer("gem:fire").is_none());
        assert_eq!(Drawer::clean_legacy_identifier("@gem:lnk_fire"), "fire");
        assert_eq!(Drawer::clean_legacy_identifier("@lnk_water"), "water");
        assert_eq!(
            Drawer::format_legacy_pointer("@gem", "lnk_fire"),
            "@gem:fire"
        );

        let mut record = json!({
            "_id": "@gem:lnk_fire",
            "owner": "@owner:lnk_alice",
            "nested": {
                "pointer": "@owner:lnk_bob",
                "_id": "lnk_inner"
            },
            "links": ["@owner:lnk_cara", "unchanged"]
        });
        assert!(Drawer::migrate_legacy_value(&mut record, None));
        assert_eq!(record["_id"], "fire");
        assert_eq!(record["owner"], "@owner:alice");
        assert_eq!(record["nested"]["pointer"], "@owner:bob");
        assert_eq!(record["nested"]["_id"], "inner");
        assert_eq!(record["links"][0], "@owner:cara");
        assert!(!Drawer::migrate_legacy_value(
            &mut json!({"stable": true}),
            None
        ));
    }

    #[test]
    fn drawer_schema_extension_helpers_update_schema_state() {
        let path = temp_dir("schema_extension");
        std::fs::create_dir_all(&path).expect("drawer directory should create");
        let mut drawer = Drawer::open(&path, "gem", "_id", Vec::new()).expect("drawer opens");

        drawer.add_required_field("element");
        drawer.add_required_field("element");
        assert_eq!(
            drawer.schema.as_ref().unwrap()["required"],
            json!(["element"])
        );
        drawer.remove_required_field("missing");
        drawer.remove_required_field("element");
        assert_eq!(drawer.schema.as_ref().unwrap()["required"], json!([]));

        drawer.record_schema_extension("indexes", "element", json!({"type": "hash"}));
        drawer.record_schema_extension("triggers", "on_upsert", json!({"command": "hook"}));
        assert!(drawer.schema_has_index("element"));
        assert_eq!(
            Drawer::schema_extension_fields(drawer.schema.as_ref(), "triggers"),
            vec!["on_upsert".to_string()]
        );
        drawer.remove_schema_extension("indexes", "element");
        assert!(!drawer.schema_has_index("element"));

        drawer.schema = Some(Value::String("not-an-object".to_string()));
        drawer
            .ensure_schema_object()
            .insert("type".to_string(), Value::String("object".to_string()));
        assert_eq!(drawer.schema.as_ref().unwrap()["type"], "object");

        let _ = std::fs::remove_dir_all(path);
    }
}
