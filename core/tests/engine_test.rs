mod common;

use common::TempDatabase;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use wardrobe_core::CatalogRegistry;
use wardrobe_core::{
    AlterRequest, BsonBinaryFormat, Command, CommandResult, CompactMode, CompactRequest,
    CreateRequest, CreateResult, DatabaseReader, DeleteResult, NativeBinaryIndexFormat,
    OperationFilter, OperationOptions, OrderDirection, QueryModifiers, ReadResult, ReturnShape,
    StatusRequest, StatusResult, StorageCoordinate, StorageFormat, StorageInventory,
    StorageLocator, StorageScope, UpsertResult, WAL_FILE_NAME, WalJournal, WalOperation,
    WardrobeConfig, WardrobeEngine, application_logging_is_configured,
    shutdown_application_logging,
};

const INDEX_FIELD_KEY: &str = "f";
const INDEX_VALUE_KEY: &str = "k";
const INDEX_OFFSET_KEY: &str = "o";
const INDEX_LENGTH_KEY: &str = "l";
const INDEX_SIZE_CLASS_KEY: &str = "c";
const INDEX_CRC_KEY: &str = "x";
const INDEX_STATUS_KEY: &str = "s";

fn upsert_command(payload: serde_json::Value, filter: impl Into<OperationFilter>) -> Command {
    Command::Upsert {
        payload,
        filter: filter.into(),
        options: OperationOptions::default(),
    }
}

fn read_command(filter: impl Into<OperationFilter>) -> Command {
    Command::Read {
        filter: filter.into(),
        options: OperationOptions::default(),
    }
}

fn read_record_command(filter: impl Into<OperationFilter>) -> Command {
    Command::Read {
        filter: filter.into(),
        options: OperationOptions::new().return_shape(ReturnShape::Record),
    }
}

fn count_command(filter: impl Into<OperationFilter>) -> Command {
    Command::Count {
        filter: filter.into(),
        options: OperationOptions::default(),
    }
}

fn delete_command(filter: impl Into<OperationFilter>) -> Command {
    Command::Delete {
        filter: filter.into(),
        options: OperationOptions::default(),
    }
}

fn write_cascade_delete_rules(database: &TempDatabase, drawer_name: &str, fields: &[&str]) {
    let cascade_delete_rules = fields
        .iter()
        .map(|field| ((*field).to_string(), json!(true)))
        .collect::<serde_json::Map<String, serde_json::Value>>();

    let mut metadata = json!({
        "format_version": 1,
        "primary_key": "_id",
        "unique_constraints": [],
        "relationship_constraints": {},
        "delete_rules": {},
        "cascade_delete_rules": cascade_delete_rules
    });
    preserve_existing_field_name_map(database, drawer_name, &mut metadata);

    fs::write(
        database.path.join(format!("{}_meta.drw", drawer_name)),
        serde_json::to_vec_pretty(&metadata).expect("metadata should serialize"),
    )
    .expect("metadata should write");
}

fn write_drawer_metadata(
    database: &TempDatabase,
    drawer_name: &str,
    mut metadata: serde_json::Value,
) {
    preserve_existing_field_name_map(database, drawer_name, &mut metadata);
    fs::write(
        database.path.join(format!("{}_meta.drw", drawer_name)),
        serde_json::to_vec_pretty(&metadata).expect("metadata should serialize"),
    )
    .expect("metadata should write");
}

fn preserve_existing_field_name_map(
    database: &TempDatabase,
    drawer_name: &str,
    metadata: &mut serde_json::Value,
) {
    if metadata.get("field_name_map").is_some() {
        return;
    }

    let metadata_path = database.path.join(format!("{}_meta.drw", drawer_name));
    let Ok(existing_metadata_contents) = fs::read(&metadata_path) else {
        return;
    };
    let Ok(existing_metadata) =
        serde_json::from_slice::<serde_json::Value>(&existing_metadata_contents)
    else {
        return;
    };
    let Some(field_name_map) = existing_metadata.get("field_name_map").cloned() else {
        return;
    };
    if let Some(metadata_map) = metadata.as_object_mut() {
        metadata_map.insert("field_name_map".to_string(), field_name_map);
    }
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
    let mut index_record = serde_json::Map::new();
    index_record.insert(INDEX_FIELD_KEY.to_string(), json!("_id"));
    index_record.insert(INDEX_VALUE_KEY.to_string(), json!(primary_key));
    index_record.insert(INDEX_OFFSET_KEY.to_string(), json!(data_offset));
    index_record.insert(INDEX_LENGTH_KEY.to_string(), json!(serialized_record.len()));
    index_record.insert(INDEX_SIZE_CLASS_KEY.to_string(), json!(data_size_class));
    index_record.insert(INDEX_CRC_KEY.to_string(), json!(0));
    index_record.insert(INDEX_STATUS_KEY.to_string(), json!(1));
    let serialized_index =
        BsonBinaryFormat::serialize_record(&serde_json::Value::Object(index_record))
            .expect("legacy fixture index should serialize using compact binary format");
    fs::write(&index_path, serialized_index).expect("legacy index file should write");

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
    let payload = serde_json::to_vec(&record).expect("wal record should serialize");
    WalJournal::at_database_path(&database.path)
        .append(wal_record_operation(&record), "transaction", &payload)
        .expect("wal record should append");
}

fn wal_records(database: &TempDatabase) -> Vec<serde_json::Value> {
    WalJournal::at_database_path(&database.path)
        .read_entries()
        .expect("wal should read")
        .into_iter()
        .filter(|entry| entry.scope == "transaction")
        .filter_map(|entry| serde_json::from_slice(&entry.payload).ok())
        .collect()
}

fn wal_record_operation(record: &serde_json::Value) -> WalOperation {
    match record.get("event").and_then(serde_json::Value::as_str) {
        Some("begin") => match record
            .get("operation")
            .and_then(|operation| operation.get("type"))
            .and_then(serde_json::Value::as_str)
        {
            Some("delete_by_id") | Some("delete_by_filter") => WalOperation::Delete,
            _ => WalOperation::Upsert,
        },
        _ => WalOperation::Maintenance,
    }
}

fn drawer_records_from_disk(path: &Path) -> Vec<serde_json::Value> {
    if !path.exists() {
        return Vec::new();
    }

    let is_index = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.ends_with("_index"));

    let native_field_name_map = drawer_field_name_map_from_disk(path).map(|field_name_map| {
        field_name_map
            .iter()
            .filter_map(|(token, logical_name)| {
                logical_name
                    .as_str()
                    .map(|logical_name| (token.clone(), logical_name.to_string()))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    });
    let mut name_map = std::collections::BTreeMap::new();
    if is_index {
        if let Some(map) = native_field_name_map.as_ref() {
            for (k, v) in map {
                name_map.insert(k.clone(), v.clone());
            }
        }
    }

    let reader = DatabaseReader::open_drawer(path).expect("drawer should open");
    let mut records = Vec::new();
    reader
        .stream_with_offsets(|_offset, slot| {
            let is_dead =
                BsonBinaryFormat::is_tombstone(slot) || NativeBinaryIndexFormat::is_tombstone(slot);
            if !is_dead {
                let entry_opt = if BsonBinaryFormat::is_binary_frame(slot) {
                    BsonBinaryFormat::deserialize_record_with_map(
                        slot,
                        native_field_name_map.as_ref(),
                    )
                    .ok()
                    .flatten()
                } else if NativeBinaryIndexFormat::is_binary_frame(slot) {
                    NativeBinaryIndexFormat::deserialize_index_entry(slot, &name_map)
                        .ok()
                        .flatten()
                } else {
                    None
                };
                if let Some(record) = entry_opt {
                    records.push(record);
                }
            }
        })
        .expect("drawer should stream");

    if is_index {
        return records;
    }

    let Some(field_name_map) = drawer_field_name_map_from_disk(path) else {
        return records;
    };
    records
        .into_iter()
        .map(|record| decode_drawer_record_from_disk(record, &field_name_map))
        .collect()
}

fn drawer_field_name_map_from_disk(
    path: &Path,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut drawer_name = path.file_stem()?.to_str()?.to_string();
    if drawer_name.ends_with("_index") {
        drawer_name = drawer_name.replace("_index", "");
    }
    let metadata_path = path.with_file_name(format!("{drawer_name}_meta.drw"));
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(metadata_path).ok()?).ok()?;
    metadata.get("field_name_map")?.as_object().cloned()
}

fn decode_drawer_record_from_disk(
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
                        decode_drawer_record_from_disk(field_value, field_name_map),
                    )
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(|value| decode_drawer_record_from_disk(value, field_name_map))
                .collect(),
        ),
        other => other,
    }
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

fn reverse_relationship_index(database: &TempDatabase) -> serde_json::Value {
    let index_path = database.path.join(".reverse_relationships.json");
    let bytes = fs::read(index_path).expect("reverse relationship index should exist");
    serde_json::from_slice(&bytes).expect("reverse relationship index should parse")
}

fn reverse_relationship_entries(
    database: &TempDatabase,
    parent_pointer: &str,
) -> Vec<serde_json::Value> {
    reverse_relationship_index(database)["references"]
        .get(parent_pointer)
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

struct ReadTarget {
    filter: OperationFilter,
    options: OperationOptions,
}

impl From<OperationFilter> for ReadTarget {
    fn from(filter: OperationFilter) -> Self {
        Self {
            filter,
            options: OperationOptions::default(),
        }
    }
}

impl From<(OperationFilter, OperationOptions)> for ReadTarget {
    fn from((filter, options): (OperationFilter, OperationOptions)) -> Self {
        Self { filter, options }
    }
}

fn read_records<T>(engine: &WardrobeEngine, target: T) -> std::io::Result<Vec<serde_json::Value>>
where
    T: Into<ReadTarget>,
{
    let target = target.into();
    match engine.read(target.filter, target.options)? {
        ReadResult::Records(records) => Ok(records),
        other => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("expected records, got {other:?}"),
        )),
    }
}

fn read_record<T>(engine: &WardrobeEngine, target: T) -> std::io::Result<Option<serde_json::Value>>
where
    T: Into<ReadTarget>,
{
    let target = target.into();
    match engine.read(target.filter, target.options)? {
        ReadResult::Record(record) => Ok(record),
        other => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("expected record, got {other:?}"),
        )),
    }
}

fn upsert_one(
    engine: &WardrobeEngine,
    drawer_name: &str,
    payload: serde_json::Value,
) -> std::io::Result<String> {
    engine
        .upsert(
            payload,
            OperationFilter::drawer(drawer_name),
            None::<OperationOptions>,
        )
        .map(|result| {
            result
                .into_pointers()
                .pop()
                .expect("single-object upsert should return one pointer")
        })
}

fn upsert_batch(
    engine: &WardrobeEngine,
    drawer_name: &str,
    records: Vec<serde_json::Value>,
) -> std::io::Result<Vec<String>> {
    engine
        .upsert(
            serde_json::Value::Array(records),
            OperationFilter::drawer(drawer_name),
            None::<OperationOptions>,
        )
        .map(|result| result.into_pointers())
}

fn create_inventory(result: CreateResult) -> StorageInventory {
    match result {
        CreateResult::StorageInventory(inventory) => inventory,
        other => panic!("expected storage inventory, got {other:?}"),
    }
}

fn status_tenants(engine: &WardrobeEngine) -> std::io::Result<Vec<String>> {
    match engine.status(StatusRequest::tenants())? {
        StatusResult::Tenants(tenants) => Ok(tenants),
        other => Err(unexpected_status(other)),
    }
}

fn status_databases(engine: &WardrobeEngine) -> std::io::Result<Vec<StorageInventory>> {
    match engine.status(StatusRequest::databases())? {
        StatusResult::Databases(databases) => Ok(databases),
        other => Err(unexpected_status(other)),
    }
}

fn status_schemas(engine: &WardrobeEngine, database_name: &str) -> std::io::Result<Vec<String>> {
    match engine.status(StatusRequest::schemas(database_name))? {
        StatusResult::Schemas(schemas) => Ok(schemas),
        other => Err(unexpected_status(other)),
    }
}

fn status_drawers(
    engine: &WardrobeEngine,
    database_name: &str,
    schema_name: &str,
) -> std::io::Result<Vec<StorageInventory>> {
    match engine.status(StatusRequest::drawers(database_name, schema_name))? {
        StatusResult::Drawers(drawers) => Ok(drawers),
        other => Err(unexpected_status(other)),
    }
}

fn status_storage(engine: &WardrobeEngine) -> std::io::Result<wardrobe_core::StorageDiagnosis> {
    match engine.status(StatusRequest::storage())? {
        StatusResult::Storage(diagnosis) => Ok(diagnosis),
        other => Err(unexpected_status(other)),
    }
}

fn status_wal(
    engine: &WardrobeEngine,
    database_name: Option<&str>,
) -> std::io::Result<wardrobe_core::WalVerification> {
    match engine.status(StatusRequest::wal(database_name))? {
        StatusResult::Wal(verification) => Ok(verification),
        other => Err(unexpected_status(other)),
    }
}

fn status_cached_drawer_count(engine: &WardrobeEngine) -> std::io::Result<usize> {
    match engine.status(StatusRequest::cached_drawer_count())? {
        StatusResult::CachedDrawerCount(count) => Ok(count),
        other => Err(unexpected_status(other)),
    }
}

fn unexpected_status(result: StatusResult) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("unexpected status result {result:?}"),
    )
}

#[test]
fn embedded_engine_opens_direct_storage_path_and_writes_records() {
    let database = TempDatabase::new("embedded_engine_direct_storage_path");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let pointer = engine
        .upsert(
            json!({
                "_id": "@gem:lnk_embedded_target",
                "element": "Fire"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("embedded engine should write directly");

    assert_eq!(pointer, vec!["@gem:embedded_target".to_string()]);
    assert!(database.path.join("gem.drw").is_file());
    assert_eq!(
        engine
            .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
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
            json!({
                "_id": "diagnose_fire",
                "element": "Fire"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("record should upsert");

    let diagnosis = status_storage(&engine).expect("diagnosis should be available");

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
                json!({
                    "_id": "@weapon:lnk_test_weapon",
                    "name": "Test Sword",
                    "gem": {
                        "_id": "@gem:lnk_test_gem",
                        "element": "Light",
                        "potency": 9001
                    }
                }),
                OperationFilter::drawer("weapon"),
                None::<OperationOptions>,
            )
            .expect("weapon should upsert");
    }

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("database should reinitialize");
    let weapons = read_records(&restarted_engine, OperationFilter::drawer("weapon"))
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
        .upsert(
            json!(["not", "an", "object"]),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect_err("non-object payload should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn find_by_id_returns_none_for_missing_drawer_and_does_not_create_files() {
    let database = TempDatabase::new("find_by_id_missing_drawer");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("database should initialize");

    let result = read_record(&engine, OperationFilter::pointer("@missing:lnk_any"))
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

    let error = read_record(&engine, OperationFilter::pointer("not-a-pointer"))
        .expect_err("malformed pointer should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn find_by_id_hydrates_without_id_fields() {
    let database = TempDatabase::new("find_by_id_hydrates_without_id_fields");
    let database_directory = database.path.to_string_lossy().into_owned();

    let weapon_pointer = {
        let engine = WardrobeEngine::open(&database_directory).expect("database should initialize");
        upsert_one(
            &engine,
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
    let found = read_record(&restarted_engine, OperationFilter::pointer(&weapon_pointer))
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
                json!({
                    "_id": "@weapon:lnk_has_missing_target",
                    "name": "Fragment",
                    "gem": "@gem:lnk_does_not_exist"
                }),
                OperationFilter::drawer("weapon"),
                None::<OperationOptions>,
            )
            .expect("weapon should upsert");
    }

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("database should reinitialize");
    let weapons = read_records(&restarted_engine, OperationFilter::drawer("weapon"))
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
                OperationFilter::drawer("weapon"),
                None::<OperationOptions>,
            )
            .expect("weapon with nested gem should upsert");
    }

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reinitialize");
    let weapons = read_records(&restarted_engine, OperationFilter::drawer("weapon"))
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

    let records = read_records(&engine, OperationFilter::drawer("missing"))
        .expect("find_all should succeed for missing drawers");
    assert!(records.is_empty());

    let by_id = read_record(&engine, OperationFilter::pointer("@missing:lnk_example"))
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
                OperationFilter::drawer("character"),
                None::<OperationOptions>,
            )
            .expect("complex character should upsert");
    }

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reinitialize");
    let characters = read_records(&restarted_engine, OperationFilter::drawer("character"))
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
            json!({
                "_id": wardrobe_gem_id,
                "element": "Solar",
                "potency": 777
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("gem should upsert");

    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_uses_existing_gem",
                "name": "Sun Pike",
                "gem": {
                    "_id": application_gem_id
                }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
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

    let found_gem = read_record(&engine, OperationFilter::pointer(wardrobe_gem_id))
        .expect("gem lookup should succeed")
        .expect("gem should still exist");
    assert_eq!(found_gem["element"], "Solar");
    assert_eq!(found_gem["potency"], 777);

    let weapons = read_records(&engine, OperationFilter::drawer("weapon"))
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
            json!({
                "_id": gem_id,
                "element": "Nebula",
                "potency": 313
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("gem should upsert");

    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_preformatted_reference",
                "name": "Star Lance",
                "gem": {
                    "_id": gem_id
                }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("weapon should upsert with preformatted reference");

    let weapon_records = drawer_records_from_disk(&database.path.join("weapon.drw"));
    assert!(
        weapon_records
            .iter()
            .any(|record| record["gem"] == "@gem:existing_gem")
    );

    let weapons = read_records(&engine, OperationFilter::drawer("weapon"))
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
            json!({
                "_id": "@weapon:lnk_full_child",
                "name": "Moon Staff",
                "gem": {
                    "_id": "@gem:lnk_full_child_gem",
                    "element": "Lunar",
                    "potency": 123
                }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("weapon should upsert with full child object");

    let weapon_records = drawer_records_from_disk(&database.path.join("weapon.drw"));
    assert!(
        weapon_records
            .iter()
            .any(|record| record["gem"] == "@gem:full_child_gem")
    );

    let found_gem = read_record(&engine, OperationFilter::pointer("@gem:lnk_full_child_gem"))
        .expect("gem lookup should succeed")
        .expect("gem should exist");
    assert_eq!(found_gem["element"], "Lunar");

    let weapons = read_records(&engine, OperationFilter::drawer("weapon"))
        .expect("weapon lookup should succeed");
    assert_eq!(weapons[0]["gem"]["potency"], 123);
}

#[test]
fn us_121_field_name_encoding_preserves_relationship_hydration() {
    let database = TempDatabase::new("us_121_encoded_relationship_hydration");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    upsert_one(
        &engine,
        "gem",
        json!({
            "_id": "encoded_fire",
            "element": "Fire",
            "clarity": "flawless"
        }),
    )
    .expect("gem should upsert");
    upsert_one(
        &engine,
        "weapon",
        json!({
            "_id": "encoded_blade",
            "name": "Encoded Blade",
            "gem": "@gem:encoded_fire"
        }),
    )
    .expect("weapon should upsert");

    let raw_weapon_records = drawer_records_from_disk(&database.path.join("weapon.drw"));
    assert!(
        raw_weapon_records
            .iter()
            .any(|record| record["gem"] == "@gem:encoded_fire")
    );

    let weapon = read_record(&engine, OperationFilter::pointer("@weapon:encoded_blade"))
        .expect("weapon lookup should succeed")
        .expect("weapon should exist");
    assert_eq!(weapon["name"], "Encoded Blade");
    assert_eq!(weapon["gem"]["element"], "Fire");
    assert_eq!(weapon["gem"]["clarity"], "flawless");
}

#[test]
fn us_052_primary_ids_are_stored_clean_while_references_keep_drawer_routing() {
    let database = TempDatabase::new("us_052_clean_primary_ids");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let character_pointer = upsert_one(
        &engine,
        "character",
        json!({
            "_id": "@character:lnk_us_052_owner",
            "name": "Clean Owner"
        }),
    )
    .expect("character should upsert");
    assert_eq!(character_pointer, "@character:us_052_owner");

    let weapon_pointer = upsert_one(
        &engine,
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

    let legacy_lookup = read_record(
        &engine,
        OperationFilter::pointer("@weapon:lnk_us_052_weapon"),
    )
    .expect("legacy lookup should succeed")
    .expect("weapon should exist");
    assert_eq!(legacy_lookup["name"], "Clean Blade");

    let clean_lookup = read_record(&engine, OperationFilter::pointer("@weapon:us_052_weapon"))
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
            json!({
                "_id": "@gem:lnk_us_053_fire",
                "element": "Fire"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("gem should upsert");
    engine
        .upsert(
            json!({
                "_id": "@rune:lnk_us_053_guard",
                "school": "Guard"
            }),
            OperationFilter::drawer("rune"),
            None::<OperationOptions>,
        )
        .expect("rune should upsert");
    engine
        .upsert(
            json!({
                "_id": "@artifact:lnk_us_053_satchel",
                "name": "Satchel",
                "attachments": [
                    "@gem:lnk_us_053_fire",
                    "@rune:lnk_us_053_guard"
                ]
            }),
            OperationFilter::drawer("artifact"),
            None::<OperationOptions>,
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

    let artifacts = read_records(&engine, OperationFilter::drawer("artifact"))
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
            json!({
                "_id": "alex",
                "name": "Alex"
            }),
            OperationFilter::drawer("character"),
            None::<OperationOptions>,
        )
        .expect("first character should upsert");
    engine
        .upsert(
            json!({
                "_id": "sam",
                "name": "Sam",
                "spouse": "alex"
            }),
            OperationFilter::drawer("character"),
            None::<OperationOptions>,
        )
        .expect("self-referencing character should upsert");

    let character_records = drawer_records_from_disk(&database.path.join("character.drw"));
    assert!(
        character_records
            .iter()
            .any(|record| record["spouse"] == "@character:alex")
    );

    let sam = read_record(&engine, OperationFilter::pointer("@character:sam"))
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
            json!({
                "_id": "@character:lnk_array_scalars",
                "name": "Array Keeper",
                "tags": ["tank", "support", "night-watch"],
                "scores": [10, 20, 30]
            }),
            OperationFilter::drawer("character"),
            None::<OperationOptions>,
        )
        .expect("character should upsert with scalar arrays");

    let character_records = drawer_records_from_disk(&database.path.join("character.drw"));
    assert!(character_records.iter().any(|record| record["tags"]
        == json!(["tank", "support", "night-watch"])
        && record["scores"] == json!([10, 20, 30])));

    let characters = read_records(&engine, OperationFilter::drawer("character"))
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
            json!({
                "_id": "@gem:lnk_array_fire",
                "element": "Fire",
                "potency": 10
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("fire gem should upsert");
    engine
        .upsert(
            json!({
                "_id": "@gem:lnk_array_water",
                "element": "Water",
                "potency": 20
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("water gem should upsert");

    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_pointer_array",
                "name": "Twin Wand",
                "gems": ["@gem:lnk_array_fire", "@gem:lnk_array_water"]
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("weapon should upsert with pointer array");

    let weapons = read_records(&engine, OperationFilter::drawer("weapon"))
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
            json!({
                "_id": "@gem:lnk_array_existing_fire",
                "element": "Fire",
                "potency": 111
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("fire gem should upsert");
    engine
        .upsert(
            json!({
                "_id": "@gem:lnk_array_existing_air",
                "element": "Air",
                "potency": 222
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("air gem should upsert");

    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_id_only_array",
                "name": "Reference Bow",
                "gems": [
                    { "_id": "array_existing_fire" },
                    { "_id": "@gem:lnk_array_existing_air" }
                ]
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("weapon should upsert with id-only array references");

    let weapon_records = drawer_records_from_disk(&database.path.join("weapon.drw"));
    assert!(weapon_records.iter().any(|record| {
        record["gems"] == json!(["@gem:array_existing_fire", "@gem:array_existing_air"])
    }));

    let weapons = read_records(&engine, OperationFilter::drawer("weapon"))
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
            OperationFilter::drawer("character"),
            None::<OperationOptions>,
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

    let characters = read_records(&engine, OperationFilter::drawer("character"))
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
                json!({
                    "_id": "@gem:lnk_delete_engine",
                    "element": "Solar",
                    "potency": 777
                }),
                OperationFilter::drawer("gem"),
                None::<OperationOptions>,
            )
            .expect("gem should upsert");

        let deleted = engine
            .delete(
                OperationFilter::pointer("@gem:lnk_delete_engine"),
                None::<OperationOptions>,
            )
            .expect("delete should succeed");
        assert_eq!(deleted, 1);
        assert!(
            read_record(&engine, OperationFilter::pointer("@gem:lnk_delete_engine"))
                .expect("lookup should succeed")
                .is_none()
        );
        assert!(
            read_records(&engine, OperationFilter::drawer("gem"))
                .expect("find all should succeed")
                .is_empty()
        );
    }

    assert!(drawer_tombstone_count(&database.path.join("gem.drw")) >= 1);

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reinitialize");
    assert!(
        read_record(
            &restarted_engine,
            OperationFilter::pointer("@gem:lnk_delete_engine")
        )
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
            json!({
                "_id": "@gem:lnk_existing",
                "element": "Solar"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("gem should upsert");

    let deleted = engine
        .delete(
            OperationFilter::pointer("@gem:lnk_missing"),
            None::<OperationOptions>,
        )
        .expect("delete against existing drawer should succeed");

    assert_eq!(deleted, 0);
}

#[test]
fn us_020_delete_by_filter_deletes_matching_records_and_returns_count() {
    let database = TempDatabase::new("us_020_delete_by_filter");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    engine
        .upsert(
            json!({
                "_id": "lnk_delete_filter_fire",
                "element": "Fire"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("first gem should upsert");
    engine
        .upsert(
            json!({
                "_id": "lnk_delete_filter_water",
                "element": "Water"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("second gem should upsert");

    let deleted = engine
        .delete(
            OperationFilter::query_in("gem", json!({ "element": "Fire" })),
            OperationOptions::new().multi(true),
        )
        .expect("delete by filter should succeed");

    assert_eq!(deleted, 1);
    assert!(
        read_record(
            &engine,
            OperationFilter::pointer("@gem:lnk_delete_filter_fire")
        )
        .expect("deleted record lookup should succeed")
        .is_none()
    );
    assert!(
        read_record(
            &engine,
            OperationFilter::pointer("@gem:lnk_delete_filter_water")
        )
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
            json!({
                "_id": "@gem:lnk_us_054_explicit",
                "element": "Locator"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("gem should upsert");

    let deleted = engine
        .delete(
            StorageLocator::explicit("gem", "us_054_explicit"),
            None::<OperationOptions>,
        )
        .expect("explicit locator delete should succeed");

    assert_eq!(deleted, 1);
    assert!(
        read_record(&engine, OperationFilter::pointer("@gem:us_054_explicit"))
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
            json!({
                "_id": "us_054_inline",
                "element": "Inline"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("gem should upsert");

    let deleted = engine
        .delete(
            StorageLocator::inline("@gem:us_054_inline"),
            None::<OperationOptions>,
        )
        .expect("inline locator delete should succeed");

    assert_eq!(deleted, 1);
    assert!(
        read_record(&engine, OperationFilter::pointer("@gem:us_054_inline"))
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
            json!({
                "_id": "user-02",
                "name": "Marcus"
            }),
            OperationFilter::drawer("basic-usage/public/user"),
            None::<OperationOptions>,
        )
        .expect("nested user should upsert");
    assert_eq!(
        pointer,
        vec!["@basic-usage/public/user:user-02".to_string()]
    );

    let deleted = engine
        .delete(
            OperationFilter::pointer("basic-usage/public/user/user-02"),
            None::<OperationOptions>,
        )
        .expect("structural pointer delete should succeed");

    assert_eq!(deleted, 1);
    assert!(
        read_record(
            &engine,
            OperationFilter::pointer("@basic-usage/public/user:user-02")
        )
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
            json!({
                "_id": "@gem:lnk_us_054_tuple",
                "element": "Tuple"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("gem should upsert");

    let deleted = engine
        .delete(("gem", "lnk_us_054_tuple"), None::<OperationOptions>)
        .expect("tuple locator delete should succeed");

    assert_eq!(deleted, 1);
    assert!(
        read_record(&engine, OperationFilter::pointer("@gem:us_054_tuple"))
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
        .delete(
            OperationFilter::pointer("@missing:lnk_any"),
            None::<OperationOptions>,
        )
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
                OperationFilter::drawer("character"),
                None::<OperationOptions>,
            )
            .expect("character graph should upsert");
    }

    write_cascade_delete_rules(&database, "character", &["weapons"]);
    write_cascade_delete_rules(&database, "weapon", &["gems"]);

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reinitialize");
    let deleted = restarted_engine
        .delete(
            OperationFilter::pointer("@character:lnk_cascade_owner"),
            None::<OperationOptions>,
        )
        .expect("cascade delete should succeed");

    assert_eq!(deleted, 1);
    assert!(
        read_record(
            &restarted_engine,
            OperationFilter::pointer("@character:lnk_cascade_owner")
        )
        .expect("character lookup should succeed")
        .is_none()
    );
    assert!(
        read_record(
            &restarted_engine,
            OperationFilter::pointer("@weapon:lnk_cascade_spear")
        )
        .expect("weapon lookup should succeed")
        .is_none()
    );
    assert!(
        read_record(
            &restarted_engine,
            OperationFilter::pointer("@gem:lnk_cascade_storm")
        )
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
                json!({
                    "_id": "@character:lnk_preserve_owner",
                    "name": "Grace",
                    "weapon": {
                        "_id": "@weapon:lnk_preserved_weapon",
                        "name": "Spear"
                    }
                }),
                OperationFilter::drawer("character"),
                None::<OperationOptions>,
            )
            .expect("character graph should upsert");
    }

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reinitialize");
    restarted_engine
        .delete(
            OperationFilter::pointer("@character:lnk_preserve_owner"),
            None::<OperationOptions>,
        )
        .expect("delete should succeed");

    assert!(
        read_record(
            &restarted_engine,
            OperationFilter::pointer("@character:lnk_preserve_owner")
        )
        .expect("character lookup should succeed")
        .is_none()
    );
    assert!(
        read_record(
            &restarted_engine,
            OperationFilter::pointer("@weapon:lnk_preserved_weapon")
        )
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
            json!({
                "_id": "@node:lnk_a",
                "name": "Node A",
                "next": "@node:lnk_b"
            }),
            OperationFilter::drawer("node"),
            None::<OperationOptions>,
        )
        .expect("first node should upsert");
    engine
        .upsert(
            json!({
                "_id": "@node:lnk_b",
                "name": "Node B",
                "next": "@node:lnk_a"
            }),
            OperationFilter::drawer("node"),
            None::<OperationOptions>,
        )
        .expect("second node should upsert");

    let hydrated = read_record(&engine, OperationFilter::pointer("@node:lnk_a"))
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
            json!({
                "_id": "@weapon:lnk_us_031_sunblade",
                "name": "Sunblade",
                "damage": 120
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("sunblade should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_031_moonblade",
                "name": "Moonblade",
                "damage": 90
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("moonblade should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_031_storm_spear",
                "name": "Storm Spear",
                "damage": 120
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("storm spear should upsert");

    let by_damage = read_records(
        &engine,
        OperationFilter::query_in("weapon", json!({ "damage": 120 })),
    )
    .expect("damage filter should succeed");
    assert_eq!(by_damage.len(), 2);

    let by_name = read_records(
        &engine,
        OperationFilter::query_in("weapon", json!({ "name": "%blade" })),
    )
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
            json!({
                "_id": "@gem:lnk_us_031_fire",
                "element": "Fire",
                "potency": 500
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("fire gem should upsert");
    engine
        .upsert(
            json!({
                "_id": "@gem:lnk_us_031_ice",
                "element": "Ice",
                "potency": 300
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("ice gem should upsert");

    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_031_flare",
                "name": "Flare",
                "gem": { "_id": "@gem:lnk_us_031_fire" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("flare should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_031_frost",
                "name": "Frost",
                "gem": { "_id": "@gem:lnk_us_031_ice" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("frost should upsert");

    let matched = read_records(
        &engine,
        OperationFilter::query_in("weapon", json!({ "gem": { "_id": "us_031_fire" } })),
    )
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

    let error = read_records(
        &engine,
        OperationFilter::query_in("weapon", json!(["not", "an", "object"])),
    )
    .expect_err("non-object filter should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn us_104_find_by_filter_intersects_declared_secondary_indexes() {
    let database = TempDatabase::new("us_104_indexed_filter_intersection");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    upsert_batch(
        &engine,
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
    )
    .expect("book batch should upsert");

    for field_name in ["author_id", "editor_id", "purge_bucket"] {
        engine
            .alter(AlterRequest::schema_rule(
                "book",
                "add",
                "index",
                field_name,
                json!({ "kind": "index" }),
            ))
            .expect("index should be registered");
    }

    let mut record_ids = read_records(
        &engine,
        OperationFilter::query_in(
            "book",
            json!({
                "author_id": "entity_a",
                "editor_id": "entity_a",
                "purge_bucket": 0
            }),
        ),
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
                OperationFilter::query_in(
                    "book",
                    json!({
                        "author_id": "entity_a",
                        "editor_id": "entity_a",
                        "purge_bucket": 0
                    })
                ),
                None::<OperationOptions>
            )
            .expect("indexed count should succeed"),
        1
    );

    let wildcard_records = read_records(
        &engine,
        OperationFilter::query_in("book", json!({ "title": "Indexed %" })),
    )
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
            json!({
                "_id": "@weapon:lnk_us_032_flare",
                "name": "Flare",
                "gem": "@missing:lnk_unresolved"
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("first weapon should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_032_frost",
                "name": "Frost"
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("second weapon should upsert");

    let total = engine
        .count(OperationFilter::drawer("weapon"), None::<OperationOptions>)
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
            json!({
                "_id": "@gem:lnk_us_032_fire",
                "element": "Fire",
                "potency": 500
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("fire gem should upsert");
    engine
        .upsert(
            json!({
                "_id": "@gem:lnk_us_032_ice",
                "element": "Ice",
                "potency": 300
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("ice gem should upsert");

    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_032_blaze",
                "name": "Blazeblade",
                "damage": 120,
                "gem": { "_id": "@gem:lnk_us_032_fire" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("blazeblade should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_032_frost",
                "name": "Frostblade",
                "damage": 90,
                "gem": { "_id": "@gem:lnk_us_032_ice" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("frostblade should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_032_storm",
                "name": "Storm Spear",
                "damage": 120,
                "gem": { "_id": "@gem:lnk_us_032_fire" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("storm spear should upsert");

    let wildcard_count = engine
        .count(
            OperationFilter::query_in("weapon", json!({ "name": "%blade" })),
            None::<OperationOptions>,
        )
        .expect("wildcard count should succeed");
    assert_eq!(wildcard_count, 2);

    let reference_count = engine
        .count(
            OperationFilter::query_in("weapon", json!({ "gem": { "_id": "us_032_fire" } })),
            None::<OperationOptions>,
        )
        .expect("reference count should succeed");
    assert_eq!(reference_count, 2);

    let exact_count = engine
        .count(
            OperationFilter::query_in("weapon", json!({ "damage": 90 })),
            None::<OperationOptions>,
        )
        .expect("exact count should succeed");
    assert_eq!(exact_count, 1);
}

#[test]
fn us_032_count_rejects_non_object_filters() {
    let database = TempDatabase::new("us_032_count_rejects_non_object_filter");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let error = engine
        .count(
            OperationFilter::query_in("weapon", json!(["not", "an", "object"])),
            None::<OperationOptions>,
        )
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
                json!({
                    "_id": format!("@weapon:lnk_us_033_{id}"),
                    "name": name,
                    "damage": damage
                }),
                OperationFilter::drawer("weapon"),
                None::<OperationOptions>,
            )
            .expect("weapon should upsert");
    }

    let records = read_records(
        &engine,
        (
            OperationFilter::query_in("weapon", json!({ "name": "%blade" })),
            OperationOptions::from(QueryModifiers {
                order_by: Some("damage".to_string()),
                order_direction: Some(OrderDirection::Descending),
                offset: Some(1),
                limit: Some(2),
            }),
        ),
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
            .upsert(
                payload,
                OperationFilter::drawer("weapon"),
                None::<OperationOptions>,
            )
            .expect("weapon should upsert");
    }

    let records = read_records(
        &engine,
        (
            OperationFilter::query_in("weapon", json!({})),
            OperationOptions::from(QueryModifiers {
                order_by: Some("damage".to_string()),
                order_direction: Some(OrderDirection::Descending),
                offset: None,
                limit: None,
            }),
        ),
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
                json!({
                    "_id": format!("@weapon:lnk_us_033_count_{i}"),
                    "name": format!("Blade {i}"),
                    "damage": i
                }),
                OperationFilter::drawer("weapon"),
                None::<OperationOptions>,
            )
            .expect("weapon should upsert");
    }

    let count = engine
        .count(
            OperationFilter::query_in("weapon", json!({ "name": "Blade %" })),
            OperationOptions::from(QueryModifiers {
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
            upsert_command(
                json!({
                    "_id": "@weapon:lnk_us_034_blade",
                    "name": "Tenant Blade"
                }),
                OperationFilter::drawer("weapon"),
            ),
        )
        .expect("routed upsert should succeed");

    assert_eq!(
        result,
        CommandResult::Upsert(UpsertResult::Pointers(vec![
            "@weapon:us_034_blade".to_string()
        ]))
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
        .execute(coordinate, read_command(OperationFilter::drawer("weapon")))
        .expect("routed find all should succeed");

    let CommandResult::Read(ReadResult::Records(records)) = result else {
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
                upsert_command(
                    json!({
                        "_id": "@weapon:lnk_shared_key",
                        "name": name
                    }),
                    OperationFilter::drawer("weapon"),
                ),
            )
            .expect("routed upsert should succeed");
    }

    let deleted = engine
        .execute(
            tenant_a.clone(),
            delete_command(OperationFilter::pointer("@weapon:lnk_shared_key")),
        )
        .expect("routed delete should succeed");
    assert_eq!(deleted, CommandResult::Delete(DeleteResult { deleted: 1 }));

    let tenant_a_count = engine
        .execute(tenant_a, count_command(OperationFilter::drawer("weapon")))
        .expect("tenant a count should succeed");
    let tenant_b_count = engine
        .execute(
            tenant_b.clone(),
            count_command(OperationFilter::drawer("weapon")),
        )
        .expect("tenant b count should succeed");

    assert_eq!(tenant_a_count, CommandResult::Count(0));
    assert_eq!(tenant_b_count, CommandResult::Count(1));

    let tenant_b_record = engine
        .execute(
            tenant_b,
            read_record_command(OperationFilter::pointer("@weapon:lnk_shared_key")),
        )
        .expect("tenant b lookup should succeed");

    let CommandResult::Read(ReadResult::Record(Some(record))) = tenant_b_record else {
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
            upsert_command(
                json!({
                    "_id": "@weapon:lnk_us_034_staff",
                    "name": "Routed Staff",
                    "gem": {
                        "_id": "@gem:lnk_us_034_gem",
                        "element": "Route",
                        "potency": 700
                    }
                }),
                OperationFilter::drawer("weapon"),
            ),
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
            read_command(OperationFilter::query_in(
                "weapon",
                json!({ "gem": { "_id": "us_034_gem" } }),
            )),
        )
        .expect("routed filtered query should succeed");

    let CommandResult::Read(ReadResult::Records(records)) = result else {
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
            count_command(OperationFilter::drawer("weapon")),
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
            json!({
                "_id": "@weapon:lnk_shared_database_key",
                "name": "Tenant A Blade"
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("tenant a weapon should upsert");
    tenant_b_engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_shared_database_key",
                "name": "Tenant B Blade"
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("tenant b weapon should upsert");

    let tenant_a_record = read_record(
        &tenant_a_engine,
        OperationFilter::pointer("@weapon:lnk_shared_database_key"),
    )
    .expect("tenant a lookup should succeed")
    .expect("tenant a record should exist");
    let tenant_b_record = read_record(
        &tenant_b_engine,
        OperationFilter::pointer("@weapon:lnk_shared_database_key"),
    )
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
                upsert_command(
                    json!({
                        "_id": "@gem:lnk_schema_fire",
                        "element": "Schema Fire"
                    }),
                    OperationFilter::drawer("gem"),
                ),
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
            count_command(OperationFilter::drawer("gem")),
        )
        .expect("schema-scoped count should succeed");
    assert_eq!(count, CommandResult::Count(1));

    let records = restarted_engine
        .execute_in_scope(schema_scope, read_command(OperationFilter::drawer("gem")))
        .expect("schema-scoped find_all should succeed");

    let CommandResult::Read(ReadResult::Records(records)) = records else {
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
                upsert_command(
                    json!({
                        "_id": "@gem:lnk_shared_drawer_key",
                        "element": name
                    }),
                    OperationFilter::drawer("gem"),
                ),
            )
            .expect("drawer-scoped upsert should succeed");
    }

    assert!(database.path.join("tenant1_gem.drw").is_file());
    assert!(database.path.join("tenant2_gem.drw").is_file());
    assert!(!database.path.join("gem.drw").exists());

    let tenant_a_record = engine
        .execute_in_scope(
            tenant_a_scope,
            read_record_command(OperationFilter::pointer("@gem:lnk_shared_drawer_key")),
        )
        .expect("tenant 1 lookup should succeed");
    let tenant_b_record = engine
        .execute_in_scope(
            tenant_b_scope,
            read_record_command(OperationFilter::pointer("@gem:lnk_shared_drawer_key")),
        )
        .expect("tenant 2 lookup should succeed");

    let CommandResult::Read(ReadResult::Record(Some(tenant_a_record))) = tenant_a_record else {
        panic!("expected tenant 1 record");
    };
    let CommandResult::Read(ReadResult::Record(Some(tenant_b_record))) = tenant_b_record else {
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
            upsert_command(
                json!({
                    "_id": "@weapon:lnk_graph_staff",
                    "name": "Graph Staff",
                    "gem": {
                        "_id": "@gem:lnk_graph_fire",
                        "element": "Graph Fire",
                        "potency": 999
                    }
                }),
                OperationFilter::drawer("weapon"),
            ),
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
            read_command(OperationFilter::query_in(
                "weapon",
                json!({ "gem": { "_id": "graph_fire" } }),
            )),
        )
        .expect("drawer-scoped reference filter should succeed");

    let CommandResult::Read(ReadResult::Records(records)) = result else {
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
                upsert_command(
                    json!({
                        "_id": "route_seed",
                        "element": "Routed"
                    }),
                    OperationFilter::drawer("gem"),
                ),
            )
            .expect("coordinate-scoped upsert should succeed");
    }

    engine
        .execute_in_scope(
            StorageScope::schema("main_db", "tenant_schema"),
            upsert_command(
                json!({
                    "_id": "schema_seed",
                    "name": "Schema Blade"
                }),
                OperationFilter::drawer("weapon"),
            ),
        )
        .expect("schema-scoped upsert should succeed");

    engine
        .execute_in_scope(
            StorageScope::drawer("tenant_drawer"),
            upsert_command(
                json!({
                    "_id": "drawer_seed",
                    "name": "Drawer Tenant"
                }),
                OperationFilter::drawer("character"),
            ),
        )
        .expect("drawer-scoped upsert should succeed");

    engine
        .upsert(
            json!({
                "_id": "root_seed",
                "element": "Root"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("unscoped root drawer should not create a tenant namespace");

    let tenants = status_tenants(&engine).expect("tenants should be discovered");

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
        .execute_command(Command::Status(StatusRequest::tenants()))
        .expect("show tenants command should succeed");
    assert_eq!(
        command_result,
        CommandResult::Status(StatusResult::Tenants(tenants))
    );
}

#[test]
fn us_049_show_databases_discovers_database_footprints_with_inventory() {
    let database = TempDatabase::new("us_049_show_databases");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");

    engine
        .execute_in_scope(
            StorageScope::database("main_db"),
            upsert_command(
                json!({
                    "_id": "main_seed",
                    "element": "Main"
                }),
                OperationFilter::drawer("gem"),
            ),
        )
        .expect("database-scoped upsert should succeed");

    engine
        .execute_in_scope(
            StorageScope::schema("analytics_db", "tenant_schema"),
            upsert_command(
                json!({
                    "_id": "schema_seed",
                    "name": "Schema Blade"
                }),
                OperationFilter::drawer("weapon"),
            ),
        )
        .expect("schema-scoped upsert should succeed");

    engine
        .execute(
            StorageCoordinate::new("tenant_alpha", "production", "core"),
            upsert_command(
                json!({
                    "_id": "coordinate_seed",
                    "name": "Routed Character"
                }),
                OperationFilter::drawer("character"),
            ),
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

    let databases = status_databases(&engine).expect("databases should be discovered");
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
        .execute_command(Command::Status(StatusRequest::databases()))
        .expect("show databases command should succeed");
    assert_eq!(
        command_result,
        CommandResult::Status(StatusResult::Databases(databases))
    );
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
                upsert_command(
                    json!({
                        "_id": format!("{schema_name}_seed"),
                        "element": schema_name
                    }),
                    OperationFilter::drawer("gem"),
                ),
            )
            .expect("schema-scoped upsert should succeed");
    }

    engine
        .execute_in_scope(
            StorageScope::database("main_db"),
            upsert_command(
                json!({
                    "_id": "flat_seed",
                    "element": "Flat"
                }),
                OperationFilter::drawer("flat_schema.gem"),
            ),
        )
        .expect("flat schema-prefixed drawer should upsert");

    engine
        .execute_in_scope(
            StorageScope::database("main_db"),
            upsert_command(
                json!({
                    "_id": "loose_seed",
                    "element": "Loose"
                }),
                OperationFilter::drawer("loose_gem"),
            ),
        )
        .expect("plain database drawer should upsert");

    engine
        .execute(
            StorageCoordinate::new("tenant_alpha", "production", "core"),
            upsert_command(
                json!({
                    "_id": "coordinate_seed",
                    "name": "Coordinate Blade"
                }),
                OperationFilter::drawer("weapon"),
            ),
        )
        .expect("coordinate-scoped upsert should succeed");

    let schemas = status_schemas(&engine, "main_db").expect("schemas should be discovered");
    assert_eq!(
        schemas,
        vec![
            "audit_schema".to_string(),
            "flat_schema".to_string(),
            "tenant_schema".to_string()
        ]
    );

    let routed_schemas = status_schemas(&engine, "tenant_alpha/production")
        .expect("routed schemas should be discovered");
    assert_eq!(routed_schemas, vec!["core".to_string()]);

    let command_result = engine
        .execute_command(Command::Status(StatusRequest::schemas("main_db")))
        .expect("show schemas command should succeed");
    assert_eq!(
        command_result,
        CommandResult::Status(StatusResult::Schemas(schemas))
    );
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
                upsert_command(
                    json!({
                        "_id": id,
                        "element": element
                    }),
                    OperationFilter::drawer("gem"),
                ),
            )
            .expect("schema gem should upsert");
    }

    engine
        .execute_in_scope(
            schema_scope.clone(),
            delete_command(OperationFilter::pointer("@gem:schema_gem_deleted")),
        )
        .expect("schema gem should delete");

    engine
        .execute_in_scope(
            schema_scope,
            upsert_command(
                json!({
                    "_id": "schema_weapon",
                    "name": "Schema Blade"
                }),
                OperationFilter::drawer("weapon"),
            ),
        )
        .expect("schema weapon should upsert");

    engine
        .execute_in_scope(
            StorageScope::database("main_db"),
            upsert_command(
                json!({
                    "_id": "flat_artifact",
                    "kind": "Flat"
                }),
                OperationFilter::drawer("flat_schema.artifact"),
            ),
        )
        .expect("flat schema drawer should upsert");

    engine
        .execute(
            StorageCoordinate::new("tenant_alpha", "production", "core"),
            upsert_command(
                json!({
                    "_id": "routed_character",
                    "name": "Routed"
                }),
                OperationFilter::drawer("character"),
            ),
        )
        .expect("routed drawer should upsert");

    let drawers = status_drawers(&engine, "main_db", "tenant_schema")
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

    let flat_drawers = status_drawers(&engine, "main_db", "flat_schema")
        .expect("flat schema drawers should be discovered");
    assert_eq!(flat_drawers.len(), 1);
    assert_eq!(flat_drawers[0].name, "artifact");
    assert_eq!(flat_drawers[0].record_count, 1);

    let routed_drawers = status_drawers(&engine, "tenant_alpha/production", "core")
        .expect("routed drawers should be discovered");
    assert_eq!(routed_drawers.len(), 1);
    assert_eq!(routed_drawers[0].name, "character");
    assert_eq!(routed_drawers[0].record_count, 1);

    let command_result = engine
        .execute_command(Command::Status(StatusRequest::drawers(
            "main_db",
            "tenant_schema",
        )))
        .expect("show drawers command should succeed");
    assert_eq!(
        command_result,
        CommandResult::Status(StatusResult::Drawers(drawers))
    );
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

    let databases = status_databases(&engine).expect("catalog databases should load");
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
        status_schemas(&engine, "catalog_db").expect("catalog schemas should load"),
        vec!["core".to_string()]
    );
    assert_eq!(
        status_drawers(&engine, "catalog_db", "core")
            .expect("catalog drawers should load")
            .into_iter()
            .map(|inventory| inventory.name)
            .collect::<Vec<_>>(),
        vec!["gem".to_string()]
    );

    let error = engine
        .execute_in_scope(
            StorageScope::schema("catalog_db", "core"),
            read_command(OperationFilter::drawer("missing")),
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
            json!({
                "_id": "@weapon:lnk_us_037_sword",
                "name": "Constraint Sword",
                "gem_slot": { "_id": "us_037_fire" },
                "faction_id": "@faction:lnk_us_037_order"
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("first constrained weapon should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_037_spear",
                "name": "Constraint Spear",
                "gem_slot": { "_id": "us_037_water" },
                "faction_id": "@faction:lnk_us_037_order"
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
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
            json!({
                "_id": "@weapon:lnk_us_037_axe",
                "name": "Constraint Axe",
                "gem_slot": "@gem:lnk_us_037_fire",
                "faction_id": "@faction:lnk_us_037_order"
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect_err("duplicate one-to-one pointer should fail");

    assert_eq!(duplicate_error.kind(), std::io::ErrorKind::InvalidData);
    assert!(duplicate_error.to_string().contains("1:1 relationship"));
    assert_eq!(
        engine
            .count(OperationFilter::drawer("weapon"), None::<OperationOptions>)
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
            json!({
                "_id": "@weapon:lnk_us_037_wrong_target",
                "name": "Wrong Target",
                "faction_id": "@gem:lnk_us_037_not_a_faction"
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
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
            json!({
                "_id": "@character:lnk_us_038_mech",
                "name": "Mech Pilot"
            }),
            OperationFilter::drawer("character"),
            None::<OperationOptions>,
        )
        .expect("character should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_038_lance",
                "name": "Pilot Lance",
                "character": { "_id": "@character:lnk_us_038_mech" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("first child weapon should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_038_blade",
                "name": "Pilot Blade",
                "character": { "_id": "@character:lnk_us_038_mech" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("second child weapon should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_038_other",
                "name": "Other Blade",
                "character": { "_id": "@character:lnk_us_038_other" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("unrelated child weapon should upsert");

    let characters = read_records(&engine, OperationFilter::drawer("character"))
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
                json!({
                    "_id": format!("@skill:lnk_us_038_{id}"),
                    "name": name
                }),
                OperationFilter::drawer("skill"),
                None::<OperationOptions>,
            )
            .expect("skill should upsert");
    }

    engine
        .upsert(
            json!({
                "_id": "@character:lnk_us_038_skilled",
                "name": "Skilled Character",
                "shared_skills": [
                    "@skill:lnk_us_038_dash",
                    "@skill:lnk_us_038_guard"
                ]
            }),
            OperationFilter::drawer("character"),
            None::<OperationOptions>,
        )
        .expect("many-to-many pointer array should upsert");

    let error = engine
        .upsert(
            json!({
                "_id": "@character:lnk_us_038_wrong_skill",
                "name": "Wrong Skill Character",
                "shared_skills": [
                    "@gem:lnk_us_038_wrong_target"
                ]
            }),
            OperationFilter::drawer("character"),
            None::<OperationOptions>,
        )
        .expect_err("wrong many-to-many pointer target should fail");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("expected target drawer 'skill'"));

    let characters = read_records(&engine, OperationFilter::drawer("character"))
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
            json!({
                "_id": "@character:lnk_us_039_cascade_parent",
                "name": "Cascade Parent"
            }),
            OperationFilter::drawer("character"),
            None::<OperationOptions>,
        )
        .expect("character should upsert");
    for (id, name) in [("blade", "Cascade Blade"), ("lance", "Cascade Lance")] {
        engine
            .upsert(
                json!({
                    "_id": format!("@weapon:lnk_us_039_cascade_{id}"),
                    "name": name,
                    "character": { "_id": "@character:lnk_us_039_cascade_parent" }
                }),
                OperationFilter::drawer("weapon"),
                None::<OperationOptions>,
            )
            .expect("weapon should upsert");
    }

    let deleted = engine
        .delete(
            OperationFilter::pointer("@character:lnk_us_039_cascade_parent"),
            None::<OperationOptions>,
        )
        .expect("cascade delete should succeed");

    assert_eq!(deleted, 1);
    assert_eq!(
        engine
            .count(
                OperationFilter::drawer("character"),
                None::<OperationOptions>
            )
            .expect("character count should succeed"),
        0
    );
    assert_eq!(
        engine
            .count(OperationFilter::drawer("weapon"), None::<OperationOptions>)
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
            json!({
                "_id": "@character:lnk_us_039_restrict_parent",
                "name": "Restrict Parent"
            }),
            OperationFilter::drawer("character"),
            None::<OperationOptions>,
        )
        .expect("character should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_039_restrict_child",
                "name": "Restrict Child",
                "character": { "_id": "@character:lnk_us_039_restrict_parent" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("weapon should upsert");

    let error = engine
        .delete(
            OperationFilter::pointer("@character:lnk_us_039_restrict_parent"),
            None::<OperationOptions>,
        )
        .expect_err("restrict rule should block delete");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("Delete restricted"));
    assert_eq!(
        engine
            .count(
                OperationFilter::drawer("character"),
                None::<OperationOptions>
            )
            .expect("character count should succeed"),
        1
    );
    assert_eq!(
        engine
            .count(OperationFilter::drawer("weapon"), None::<OperationOptions>)
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
            json!({
                "_id": "@character:lnk_us_039_set_null_parent",
                "name": "SetNull Parent"
            }),
            OperationFilter::drawer("character"),
            None::<OperationOptions>,
        )
        .expect("character should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_039_set_null_child",
                "name": "SetNull Child",
                "character": { "_id": "@character:lnk_us_039_set_null_parent" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("weapon should upsert");

    let deleted = engine
        .delete(
            OperationFilter::pointer("@character:lnk_us_039_set_null_parent"),
            None::<OperationOptions>,
        )
        .expect("set-null delete should succeed");

    assert_eq!(deleted, 1);
    assert_eq!(
        engine
            .count(
                OperationFilter::drawer("character"),
                None::<OperationOptions>
            )
            .expect("character count should succeed"),
        0
    );
    assert_eq!(
        engine
            .count(OperationFilter::drawer("weapon"), None::<OperationOptions>)
            .expect("weapon count should succeed"),
        1
    );

    let weapon = read_record(
        &engine,
        OperationFilter::pointer("@weapon:lnk_us_039_set_null_child"),
    )
    .expect("weapon lookup should succeed")
    .expect("weapon should remain");
    assert_eq!(weapon["name"], "SetNull Child");
    assert!(weapon.get("character").is_none());
}

#[test]
fn us_129_reverse_relationship_index_tracks_insert_update_delete_and_restart() {
    let database = TempDatabase::new("us_129_reverse_index_lifecycle");
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
                    "target_drawer": "entity",
                    "reverse": true
                }
            },
            "delete_rules": {},
            "cascade_delete_rules": {}
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
        upsert_one(
            &engine,
            "entity",
            json!({"_id": "entity_a", "display_name": "Author A"}),
        )
        .expect("first entity should upsert");
        upsert_one(
            &engine,
            "entity",
            json!({"_id": "entity_b", "display_name": "Author B"}),
        )
        .expect("second entity should upsert");
        upsert_one(
            &engine,
            "book",
            json!({
                "_id": "book_001",
                "title": "Reverse Index Book",
                "author_id": "@entity:entity_a"
            }),
        )
        .expect("book should upsert");

        let first_author_refs = reverse_relationship_entries(&database, "@entity:entity_a");
        assert_eq!(first_author_refs.len(), 1);
        assert_eq!(first_author_refs[0]["child_drawer"], "book");
        assert_eq!(first_author_refs[0]["child_id"], "book_001");
        assert_eq!(first_author_refs[0]["child_pointer"], "@book:book_001");
        assert_eq!(first_author_refs[0]["field_name"], "author_id");
        assert_eq!(first_author_refs[0]["explicit"], true);

        upsert_one(
            &engine,
            "book",
            json!({
                "_id": "book_001",
                "title": "Reverse Index Book",
                "author_id": "@entity:entity_b"
            }),
        )
        .expect("book update should upsert");

        assert!(reverse_relationship_entries(&database, "@entity:entity_a").is_empty());
        let second_author_refs = reverse_relationship_entries(&database, "@entity:entity_b");
        assert_eq!(second_author_refs.len(), 1);
        assert_eq!(second_author_refs[0]["child_pointer"], "@book:book_001");
    }

    let reopened = WardrobeEngine::open(&database_directory).expect("engine should reopen");
    assert_eq!(
        reverse_relationship_entries(&database, "@entity:entity_b").len(),
        1
    );
    assert_eq!(
        reopened
            .delete(
                OperationFilter::pointer("@book:book_001"),
                None::<OperationOptions>
            )
            .expect("book delete should succeed"),
        1
    );
    assert!(reverse_relationship_entries(&database, "@entity:entity_b").is_empty());
}

#[test]
fn us_129_set_null_delete_rule_updates_reverse_relationship_index() {
    let database = TempDatabase::new("us_129_reverse_index_set_null");
    fs::create_dir_all(&database.path).expect("temp dir should create");
    write_drawer_metadata(
        &database,
        "entity",
        json!({
            "format_version": 1,
            "primary_key": "_id",
            "record_count": 0,
            "unique_constraints": [],
            "relationship_constraints": {
                "authored_books": {
                    "type": "1:M",
                    "target_drawer": "book",
                    "mapped_by": "author_id",
                    "reverse": true
                }
            },
            "delete_rules": {
                "authored_books": {
                    "action": "SetNull"
                }
            },
            "cascade_delete_rules": {}
        }),
    );
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    upsert_one(
        &engine,
        "entity",
        json!({"_id": "entity_parent", "display_name": "Parent"}),
    )
    .expect("entity should upsert");
    upsert_one(
        &engine,
        "book",
        json!({
            "_id": "book_child",
            "title": "SetNull Child",
            "author_id": "@entity:entity_parent"
        }),
    )
    .expect("book should upsert");
    assert_eq!(
        reverse_relationship_entries(&database, "@entity:entity_parent").len(),
        1
    );

    assert_eq!(
        engine
            .delete(
                OperationFilter::pointer("@entity:entity_parent"),
                None::<OperationOptions>
            )
            .expect("set-null parent delete should succeed"),
        1
    );

    assert!(reverse_relationship_entries(&database, "@entity:entity_parent").is_empty());
    let book = read_record(&engine, OperationFilter::pointer("@book:book_child"))
        .expect("book should read")
        .expect("book should remain");
    assert!(book.get("author_id").is_none());
}

#[test]
fn us_129_reverse_relationship_index_survives_compaction() {
    let database = TempDatabase::new("us_129_reverse_index_compaction");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    upsert_one(
        &engine,
        "entity",
        json!({"_id": "entity_compact", "display_name": "Compact Parent"}),
    )
    .expect("entity should upsert");
    upsert_one(
        &engine,
        "book",
        json!({
            "_id": "book_compact",
            "title": "Compaction Child",
            "author_id": "@entity:entity_compact"
        }),
    )
    .expect("book should upsert");
    upsert_one(
        &engine,
        "book",
        json!({
            "_id": "book_compact",
            "title": "Compaction Child Updated",
            "author_id": "@entity:entity_compact"
        }),
    )
    .expect("book update should upsert");

    engine
        .compact(CompactRequest::drawer("book"))
        .expect("book compaction should succeed");

    let compact_refs = reverse_relationship_entries(&database, "@entity:entity_compact");
    assert_eq!(compact_refs.len(), 1);
    assert_eq!(compact_refs[0]["child_pointer"], "@book:book_compact");

    let reopened = WardrobeEngine::open(&database_directory).expect("engine should reopen");
    let book = read_record(&reopened, OperationFilter::pointer("@book:book_compact"))
        .expect("book should read")
        .expect("book should remain");
    assert_eq!(book["author_id"]["display_name"], "Compact Parent");
}

#[test]
fn us_112_delete_by_filter_runs_as_single_transaction_and_updates_metadata() {
    let database = TempDatabase::new("us_112_single_transaction_delete_by_filter");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    upsert_batch(
        &engine,
        "gem",
        vec![
            json!({"_id": "fire_a", "element": "Fire"}),
            json!({"_id": "fire_b", "element": "Fire"}),
            json!({"_id": "water", "element": "Water"}),
            json!({"_id": "earth", "element": "Earth"}),
        ],
    )
    .expect("records should seed");

    let wal_record_count_before_delete = wal_records(&database).len();
    let deleted = engine
        .delete(
            OperationFilter::query_in("gem", json!({"element": "Fire"})),
            OperationOptions::new().multi(true),
        )
        .expect("delete-by-filter should succeed");

    assert_eq!(deleted, 2);
    assert_eq!(
        engine
            .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
            .expect("remaining count should succeed"),
        2
    );

    let metadata_contents =
        fs::read_to_string(database.path.join("gem_meta.drw")).expect("metadata should read");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_contents).expect("metadata should parse");
    assert_eq!(metadata["record_count"], 2);

    let new_wal_records = wal_records(&database)
        .into_iter()
        .skip(wal_record_count_before_delete)
        .collect::<Vec<_>>();
    let begin_records = new_wal_records
        .iter()
        .filter(|record| record["event"] == "begin")
        .collect::<Vec<_>>();
    let commit_records = new_wal_records
        .iter()
        .filter(|record| record["event"] == "commit")
        .collect::<Vec<_>>();

    assert_eq!(begin_records.len(), 1);
    assert_eq!(commit_records.len(), 1);
    assert_eq!(begin_records[0]["operation"]["type"], "delete_by_filter");
    assert_eq!(begin_records[0]["operation"]["drawer_name"], "gem");
    assert_eq!(
        engine
            .delete(
                OperationFilter::query_in("gem", json!({"element": "Void"})),
                OperationOptions::new().multi(true)
            )
            .expect("no-match delete should succeed"),
        0
    );
}

#[test]
fn us_112_delete_by_filter_uses_materialized_index_and_invalidates_it_once() {
    let database = TempDatabase::new("us_112_indexed_delete_by_filter");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    upsert_batch(
        &engine,
        "gem",
        vec![
            json!({"_id": "fire_a", "element": "Fire"}),
            json!({"_id": "fire_b", "element": "Fire"}),
            json!({"_id": "water", "element": "Water"}),
        ],
    )
    .expect("records should seed");
    engine
        .execute_command(Command::Alter(AlterRequest::schema_rule(
            "gem",
            "add",
            "index",
            "element",
            json!({"kind": "index"}),
        )))
        .expect("index should be declared");

    assert_eq!(
        engine
            .count(
                OperationFilter::query_in("gem", json!({"element": "Fire"})),
                None::<OperationOptions>
            )
            .expect("first indexed count should materialize index"),
        2
    );

    let metadata_before: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(database.path.join("gem_meta.drw"))
            .expect("metadata should read before delete"),
    )
    .expect("metadata should parse before delete");
    let generation_before = metadata_before["secondary_index_generation"]
        .as_u64()
        .expect("generation should be present");
    assert_eq!(
        metadata_before["materialized_secondary_indexes"]["element"].as_u64(),
        Some(generation_before)
    );

    assert_eq!(
        engine
            .delete(
                OperationFilter::query_in("gem", json!({"element": "Fire"})),
                OperationOptions::new().multi(true)
            )
            .expect("indexed delete-by-filter should succeed"),
        2
    );

    let metadata_after: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(database.path.join("gem_meta.drw"))
            .expect("metadata should read after delete"),
    )
    .expect("metadata should parse after delete");
    assert_eq!(
        metadata_after["secondary_index_generation"].as_u64(),
        Some(generation_before + 1)
    );
    assert!(
        !metadata_after["materialized_secondary_indexes"]
            .as_object()
            .expect("materialized indexes should be an object")
            .contains_key("element")
    );
    assert_eq!(
        engine
            .count(
                OperationFilter::query_in("gem", json!({"element": "Fire"})),
                None::<OperationOptions>
            )
            .expect("post-delete count should succeed"),
        0
    );
}

#[test]
fn us_112_delete_by_filter_replays_from_transaction_wal() {
    let database = TempDatabase::new("us_112_delete_by_filter_wal_replay");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
        upsert_batch(
            &engine,
            "gem",
            vec![
                json!({"_id": "fire", "element": "Fire"}),
                json!({"_id": "water", "element": "Water"}),
            ],
        )
        .expect("records should seed");
    }

    write_wal_record(
        &database,
        json!({
            "event": "begin",
            "tx_id": "manual-delete-by-filter",
            "operation": {
                "type": "delete_by_filter",
                "drawer_name": "gem",
                "filter": {
                    "element": "Fire"
                }
            }
        }),
    );
    write_wal_record(
        &database,
        json!({
            "event": "commit",
            "tx_id": "manual-delete-by-filter"
        }),
    );

    let reopened = WardrobeEngine::open(&database_directory).expect("engine should reopen");
    assert_eq!(
        reopened
            .count(
                OperationFilter::query_in("gem", json!({"element": "Fire"})),
                None::<OperationOptions>
            )
            .expect("fire count should succeed"),
        0
    );
    assert_eq!(
        reopened
            .count(
                OperationFilter::query_in("gem", json!({"element": "Water"})),
                None::<OperationOptions>
            )
            .expect("water count should succeed"),
        1
    );
}

#[test]
fn us_112_delete_by_filter_preserves_cascade_rules() {
    let database = TempDatabase::new("us_112_delete_by_filter_cascade");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");
        engine
            .upsert(
                json!({
                    "_id": "@character:lnk_us_112_cascade_owner",
                    "name": "Cascade Target",
                    "weapons": [
                        {
                            "_id": "@weapon:lnk_us_112_cascade_weapon",
                            "name": "Cascade Weapon"
                        }
                    ]
                }),
                OperationFilter::drawer("character"),
                None::<OperationOptions>,
            )
            .expect("character graph should upsert");
    }

    write_cascade_delete_rules(&database, "character", &["weapons"]);
    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reinitialize");

    assert_eq!(
        restarted_engine
            .delete(
                OperationFilter::query_in("character", json!({"name": "Cascade Target"})),
                OperationOptions::new().multi(true)
            )
            .expect("cascade delete-by-filter should succeed"),
        1
    );
    assert!(
        read_record(
            &restarted_engine,
            OperationFilter::pointer("@character:lnk_us_112_cascade_owner")
        )
        .expect("character lookup should succeed")
        .is_none()
    );
    assert!(
        read_record(
            &restarted_engine,
            OperationFilter::pointer("@weapon:lnk_us_112_cascade_weapon")
        )
        .expect("weapon lookup should succeed")
        .is_none()
    );
}

#[test]
fn us_112_delete_by_filter_preserves_restrict_rules() {
    let database = TempDatabase::new("us_112_delete_by_filter_restrict");
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
                "critical_weapons": {
                    "type": "1:M",
                    "target_drawer": "weapon",
                    "mapped_by": "character"
                }
            },
            "delete_rules": {
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
            json!({
                "_id": "@character:lnk_us_112_restrict_parent",
                "name": "Restrict Target"
            }),
            OperationFilter::drawer("character"),
            None::<OperationOptions>,
        )
        .expect("character should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_112_restrict_child",
                "name": "Restrict Child",
                "character": { "_id": "@character:lnk_us_112_restrict_parent" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("weapon should upsert");

    let error = engine
        .delete(
            OperationFilter::query_in("character", json!({"name": "Restrict Target"})),
            OperationOptions::new().multi(true),
        )
        .expect_err("restrict rule should block delete-by-filter");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("Delete restricted"));
    assert_eq!(
        engine
            .count(
                OperationFilter::drawer("character"),
                None::<OperationOptions>
            )
            .expect("character count should succeed"),
        1
    );
    assert_eq!(
        engine
            .count(OperationFilter::drawer("weapon"), None::<OperationOptions>)
            .expect("weapon count should succeed"),
        1
    );
}

#[test]
fn us_112_delete_by_filter_preserves_set_null_rules() {
    let database = TempDatabase::new("us_112_delete_by_filter_set_null");
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
            json!({
                "_id": "@character:lnk_us_112_set_null_parent",
                "name": "SetNull Target"
            }),
            OperationFilter::drawer("character"),
            None::<OperationOptions>,
        )
        .expect("character should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_us_112_set_null_child",
                "name": "SetNull Child",
                "character": { "_id": "@character:lnk_us_112_set_null_parent" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("weapon should upsert");

    assert_eq!(
        engine
            .delete(
                OperationFilter::query_in("character", json!({"name": "SetNull Target"})),
                OperationOptions::new().multi(true)
            )
            .expect("set-null delete-by-filter should succeed"),
        1
    );

    let weapon = read_record(
        &engine,
        OperationFilter::pointer("@weapon:lnk_us_112_set_null_child"),
    )
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
                json!({
                    "_id": format!("@gem:lnk_us_040_reader_{i}"),
                    "element": if i % 2 == 0 { "Fire" } else { "Water" },
                    "potency": i
                }),
                OperationFilter::drawer("gem"),
                None::<OperationOptions>,
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
                    .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
                    .expect("count should succeed concurrently");
                assert_eq!(total, 12);

                let filtered = read_records(
                    &engine,
                    OperationFilter::query_in("gem", json!({ "element": "Fire" })),
                )
                .expect("filter should succeed concurrently");
                assert_eq!(filtered.len(), 6);

                let found = read_record(
                    &engine,
                    OperationFilter::pointer(format!(
                        "@gem:lnk_us_040_reader_{}",
                        thread_index % 4
                    )),
                )
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
                json!({
                    "_id": format!("@gem:lnk_us_040_delete_{i}"),
                    "element": "Old",
                    "potency": i
                }),
                OperationFilter::drawer("gem"),
                None::<OperationOptions>,
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
                        .delete(
                            OperationFilter::pointer(format!(
                                "@gem:lnk_us_040_delete_{thread_index}"
                            )),
                            None::<OperationOptions>,
                        )
                        .expect("delete should succeed concurrently");
                    assert_eq!(deleted, 1);
                } else {
                    engine
                        .upsert(
                            json!({
                                "_id": format!("@gem:lnk_us_040_insert_{thread_index}"),
                                "element": "New",
                                "potency": thread_index
                            }),
                            OperationFilter::drawer("gem"),
                            None::<OperationOptions>,
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
        .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
        .expect("final count should succeed");
    assert_eq!(total, 10);

    let new_records = read_records(
        &engine,
        OperationFilter::query_in("gem", json!({ "element": "New" })),
    )
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
            json!({
                "_id": "@gem:lnk_us_041_logged",
                "element": "Logged",
                "potency": 41
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
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
fn us_074_transaction_coordinator_commits_wal_and_hardens_drawer_state() {
    let database = TempDatabase::new("us_074_transaction_coordinator_commit");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let pointer = engine
        .upsert(
            json!({"_id": "coordinated_fire", "element": "Fire"}),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("coordinated upsert should succeed");

    assert_eq!(pointer, vec!["@gem:coordinated_fire".to_string()]);
    let records = wal_records(&database);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["event"], "begin");
    assert_eq!(records[1]["event"], "commit");

    let metadata_contents = fs::read_to_string(database.path.join("gem_meta.drw"))
        .expect("metadata should be readable after commit");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_contents).expect("metadata should parse after commit");
    assert_eq!(metadata["record_count"], 1);

    let data_records = drawer_records_from_disk(&database.path.join("gem.drw"));
    assert_eq!(data_records.len(), 1);
    assert_eq!(data_records[0]["_id"], "coordinated_fire");

    let index_records = drawer_records_from_disk(&database.path.join("gem_index.drw"));
    assert!(
        index_records
            .iter()
            .any(|record| record["f"] == "_" && record["k"] == "coordinated_fire")
    );
}

#[test]
fn us_103_open_ignores_uncommitted_upsert_transaction_from_wal() {
    let database = TempDatabase::new("us_103_ignore_uncommitted_upsert");
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
    let found = read_record(
        &engine,
        OperationFilter::pointer("@gem:lnk_us_041_replayed"),
    )
    .expect("lookup should succeed");

    assert!(found.is_none());

    let records = wal_records(&database);
    assert_eq!(records.len(), 1);
}

#[test]
fn us_103_open_replays_committed_upsert_transaction_from_wal() {
    let database = TempDatabase::new("us_103_replay_committed_upsert");
    write_wal_record(
        &database,
        json!({
            "event": "begin",
            "tx_id": "manual-upsert",
            "operation": {
                "type": "upsert",
                "drawer_name": "gem",
                "payload": {
                    "_id": "@gem:lnk_us_103_replayed",
                    "element": "Replay",
                    "potency": 4100
                }
            }
        }),
    );
    write_wal_record(
        &database,
        json!({
            "event": "commit",
            "tx_id": "manual-upsert"
        }),
    );

    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should recover");
    let found = read_record(
        &engine,
        OperationFilter::pointer("@gem:lnk_us_103_replayed"),
    )
    .expect("lookup should succeed")
    .expect("replayed record should exist");

    assert_eq!(found["element"], "Replay");
    assert_eq!(found["potency"], 4100);
    assert!(wal_records(&database).is_empty());
}

#[test]
fn us_103_open_replays_committed_cascading_delete_transaction_from_wal() {
    let database = TempDatabase::new("us_103_replay_cascade_delete");
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
                json!({
                    "_id": "@character:lnk_us_041_cascade_parent",
                    "name": "Wal Cascade Parent"
                }),
                OperationFilter::drawer("character"),
                None::<OperationOptions>,
            )
            .expect("character should upsert");
        engine
            .upsert(
                json!({
                    "_id": "@weapon:lnk_us_041_cascade_child",
                    "name": "Wal Cascade Child",
                    "character": { "_id": "@character:lnk_us_041_cascade_parent" }
                }),
                OperationFilter::drawer("weapon"),
                None::<OperationOptions>,
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
    write_wal_record(
        &database,
        json!({
            "event": "commit",
            "tx_id": "manual-cascade-delete"
        }),
    );

    let recovered_engine =
        WardrobeEngine::open(&database_directory).expect("engine should recover delete");

    assert_eq!(
        recovered_engine
            .count(
                OperationFilter::drawer("character"),
                None::<OperationOptions>
            )
            .expect("character count should succeed"),
        0
    );
    assert_eq!(
        recovered_engine
            .count(OperationFilter::drawer("weapon"), None::<OperationOptions>)
            .expect("weapon count should succeed"),
        0
    );

    assert!(wal_records(&database).is_empty());
}

#[test]
fn us_041_failed_mutations_append_abort_and_do_not_replay_on_open() {
    let database = TempDatabase::new("us_041_abort_failed_mutation");
    let database_directory = database.path.to_string_lossy().into_owned();

    {
        let engine = WardrobeEngine::open(&database_directory).expect("engine should open");
        let error = engine
            .upsert(
                json!(["not", "an", "object"]),
                OperationFilter::drawer("gem"),
                None::<OperationOptions>,
            )
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
            .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
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
            json!({
                "_id": "@weapon:lnk_schema_invalid",
                "name": "Schema Blade",
                "damage": "heavy"
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
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
            .count(OperationFilter::drawer("weapon"), None::<OperationOptions>)
            .expect("count should succeed"),
        0
    );

    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_schema_valid",
                "name": "Schema Blade",
                "damage": 42
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("valid schema record should write");
    assert_eq!(
        engine
            .count(OperationFilter::drawer("weapon"), None::<OperationOptions>)
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
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("weapon should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_vacuum_delete",
                "name": "Deleted Blade"
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("second weapon should upsert");
    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_vacuum_keep",
                "name": "Compact Blade",
                "gem": { "_id": "@gem:lnk_vacuum_gem" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("weapon should update");
    engine
        .delete(
            OperationFilter::pointer("@weapon:lnk_vacuum_delete"),
            None::<OperationOptions>,
        )
        .expect("weapon should delete");

    let data_path = database.path.join("weapon.drw");
    let before_len = fs::metadata(&data_path)
        .expect("weapon data metadata should read")
        .len();
    assert!(drawer_tombstone_count(&data_path) >= 1);

    let report = engine
        .compact(CompactRequest::drawer("weapon"))
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

    let weapons = read_records(&engine, OperationFilter::drawer("weapon"))
        .expect("weapons should read after vacuum");
    assert_eq!(weapons.len(), 1);
    assert_eq!(weapons[0]["name"], "Compact Blade");
    assert_eq!(weapons[0]["gem"]["element"], "Light");

    let restarted_engine =
        WardrobeEngine::open(&database_directory).expect("engine should reopen after vacuum");
    let weapon = read_record(
        &restarted_engine,
        OperationFilter::pointer("@weapon:lnk_vacuum_keep"),
    )
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
            upsert_command(
                json!({
                    "_id": "@gem:lnk_routed_keep",
                    "element": "Long Element Value",
                    "potency": 1
                }),
                OperationFilter::drawer("gem"),
            ),
        )
        .expect("routed gem should upsert");
    engine
        .execute(
            coordinate.clone(),
            upsert_command(
                json!({
                    "_id": "@gem:lnk_routed_keep",
                    "element": "Air",
                    "potency": 2
                }),
                OperationFilter::drawer("gem"),
            ),
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
            Command::Compact(CompactRequest::drawer("gem")),
        )
        .expect("routed vacuum should succeed");

    let CommandResult::Compact(report) = result else {
        panic!("expected vacuum report");
    };

    assert_eq!(report.records_rewritten, 1);
    assert!(report.bytes_reclaimed > 0);
    assert!(!database.path.join("gem.drw").exists());
    assert!(drawer_tombstone_count(&scoped_data_path) == 0);

    let result = engine
        .execute(coordinate, read_command(OperationFilter::drawer("gem")))
        .expect("routed find should succeed");
    let CommandResult::Read(ReadResult::Records(records)) = result else {
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
    let error = read_records(&engine, OperationFilter::drawer("gem"))
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
        .compact(CompactRequest::drawer_with_mode(
            "weapon",
            CompactMode::Migrate,
        ))
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
            json!({
                "_id": "@gem:lnk_lru_gem",
                "element": "Light",
                "potency": 9001
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("gem should upsert");
    assert_eq!(
        status_cached_drawer_count(&engine).expect("cache count should read"),
        1
    );

    engine
        .upsert(
            json!({
                "_id": "@weapon:lnk_lru_weapon",
                "name": "Cache Blade",
                "gem": { "_id": "@gem:lnk_lru_gem" }
            }),
            OperationFilter::drawer("weapon"),
            None::<OperationOptions>,
        )
        .expect("weapon should upsert");
    assert_eq!(
        status_cached_drawer_count(&engine).expect("cache count should read"),
        1
    );

    let gem = read_record(&engine, OperationFilter::pointer("@gem:lnk_lru_gem"))
        .expect("evicted gem drawer should reload")
        .expect("gem should exist");
    assert_eq!(gem["element"], "Light");
    assert_eq!(
        status_cached_drawer_count(&engine).expect("cache count should read"),
        1
    );

    let weapons = read_records(&engine, OperationFilter::drawer("weapon"))
        .expect("evicted weapon drawer should reload");
    assert_eq!(weapons.len(), 1);
    assert_eq!(weapons[0]["name"], "Cache Blade");
    assert_eq!(weapons[0]["gem"]["potency"], 9001);
    assert_eq!(
        status_cached_drawer_count(&engine).expect("cache count should read"),
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
    let _ = engine.upsert(
        json!({"name":"hero"}),
        OperationFilter::drawer("character"),
        None::<OperationOptions>,
    )?;
    let wal_path = database.path.join(WAL_FILE_NAME);
    assert!(wal_path.exists());
    let metadata = fs::metadata(&wal_path)?;
    assert!(metadata.len() > 0);
    Ok(())
}

#[test]
fn us_127_open_with_config_applies_cache_and_wal_settings_without_global_logging() {
    shutdown_application_logging();
    let database = TempDatabase::new("us_127_open_with_config");
    let mut config = WardrobeConfig::default();
    config.data.directory = database.path.clone();
    config.cache.max_cached_drawers = Some(2);
    config.wal.checkpoint_size_bytes = 2048;
    config.wal.checkpoint_ops = 3;
    config.wal.durability = wardrobe_core::DurabilityPolicy::Grouped {
        commit_window_ms: 9,
        max_batch_size: 11,
    };
    config.logging.level = wardrobe_core::ApplicationLogLevel::Info;

    let engine = WardrobeEngine::open_with_config(config).expect("engine should open with config");

    assert_eq!(engine.configured_max_cached_drawers(), Some(2));
    assert_eq!(engine.configured_wal_thresholds(), (2048, 3));
    assert_eq!(
        engine.configured_durability_policy(),
        wardrobe_core::DurabilityPolicy::Grouped {
            commit_window_ms: 9,
            max_batch_size: 11
        }
    );
    assert!(!application_logging_is_configured());
}

#[test]
fn us_127_engine_builder_opens_with_explicit_config() {
    let database = TempDatabase::new("us_127_engine_builder");
    let engine = WardrobeEngine::builder()
        .directory(database.path.clone())
        .max_cached_drawers(4)
        .wal_checkpoint_thresholds(4096, 5)
        .open()
        .expect("builder should open engine");

    assert_eq!(engine.configured_max_cached_drawers(), Some(4));
    assert_eq!(engine.configured_wal_thresholds(), (4096, 5));
}

#[test]
fn us_060_ops_threshold_triggers_checkpoint_and_truncates_wal() -> std::io::Result<()> {
    let database = TempDatabase::new("us_060_ops_threshold_checkpoint");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine =
        WardrobeEngine::open_with_wal_checkpoint_thresholds(&database_directory, 1_048_576, 2)
            .expect("engine opens with WAL thresholds");

    let pointer = engine.upsert(
        json!({"_id": "fire", "element": "Fire"}),
        OperationFilter::drawer("gem"),
        None::<OperationOptions>,
    )?;
    assert_eq!(pointer, vec!["@gem:fire".to_string()]);

    let wal_path = database.path.join(WAL_FILE_NAME);
    let wal_meta_path = database.path.join(".wal.meta");
    assert!(wal_path.exists());
    assert!(wal_meta_path.exists());
    assert_eq!(fs::metadata(&wal_path)?.len(), 0);

    drop(engine);
    let reopened = WardrobeEngine::open(&database_directory).expect("engine reopens");
    let record = read_record(&reopened, OperationFilter::pointer("@gem:fire"))
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

    engine.upsert(
        json!({"_id": "water", "element": "Water"}),
        OperationFilter::drawer("gem"),
        None::<OperationOptions>,
    )?;

    let wal_path = database.path.join(WAL_FILE_NAME);
    let wal_meta_path = database.path.join(".wal.meta");
    assert!(wal_path.exists());
    assert!(wal_meta_path.exists());
    assert_eq!(fs::metadata(&wal_path)?.len(), 0);

    drop(engine);
    let reopened = WardrobeEngine::open(&database_directory).expect("engine reopens");
    let record = read_record(&reopened, OperationFilter::pointer("@gem:water"))
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

    let database_inventory = create_inventory(
        engine
            .create(CreateRequest::database("managed_db"))
            .expect("database should be created"),
    );
    assert_eq!(database_inventory.name, "managed_db");
    assert!(database.path.join("managed_db").exists());

    let missing_parent = engine
        .create(CreateRequest::schema("missing_db", "core"))
        .expect_err("schema creation should require a registered database");
    assert_eq!(missing_parent.kind(), std::io::ErrorKind::NotFound);

    let schema_inventory = create_inventory(
        engine
            .create(CreateRequest::schema("managed_db", "core"))
            .expect("schema should be created"),
    );
    assert_eq!(schema_inventory.name, "core");
    assert!(database.path.join("managed_db").join("core").exists());

    let drawer_inventory = create_inventory(
        engine
            .create(CreateRequest::drawer("managed_db", "core", "gem"))
            .expect("drawer should be created"),
    );
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
        .execute_command(Command::Create(CreateRequest::drawer(
            "managed_db",
            "core",
            "weapon",
        )))
        .expect("define drawer command should route through engine boundary");
    assert!(matches!(
        command_result,
        CommandResult::Create(CreateResult::StorageInventory(inventory)) if inventory.name == "weapon"
    ));

    let reopened = WardrobeEngine::open(&storage_pool).expect("engine should reopen");
    let databases = status_databases(&reopened).expect("catalog databases should load");
    assert_eq!(
        databases
            .iter()
            .map(|inventory| inventory.name.as_str())
            .collect::<Vec<_>>(),
        vec!["managed_db"]
    );
    assert_eq!(
        status_schemas(&reopened, "managed_db").expect("catalog schemas should load"),
        vec!["core".to_string()]
    );
    assert_eq!(
        status_drawers(&reopened, "managed_db", "core")
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
        status_tenants(&engine).expect("tenants should load"),
        vec!["tenant_a".to_string()]
    );

    let result = engine
        .execute_in_scope(
            StorageScope::tenant("tenant_a", "production", "core"),
            upsert_command(
                json!({
                    "_id": "tenant_fire",
                    "element": "Fire"
                }),
                OperationFilter::drawer("gem"),
            ),
        )
        .expect("tenant scoped upsert should route");
    assert!(matches!(
        result,
        CommandResult::Upsert(UpsertResult::Pointers(pointers))
            if pointers == vec!["@gem:tenant_fire".to_string()]
    ));

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
            command: Box::new(read_command(OperationFilter::drawer("gem"))),
        })
        .expect("tenant command should route");
    assert!(matches!(
        records,
        CommandResult::Read(ReadResult::Records(records))
            if records.len() == 1 && records[0]["_id"] == "tenant_fire"
    ));

    let missing_tenant = engine
        .execute_for_tenant(
            "tenant_b",
            "production",
            "core",
            read_command(OperationFilter::drawer("gem")),
        )
        .expect_err("missing tenant should fail");
    assert_eq!(missing_tenant.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn us_066_binary_wal_logs_mutating_commands() {
    let database = TempDatabase::new("us_066_binary_wal");
    let storage_pool = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&storage_pool).expect("engine should initialize");

    let empty_verification = status_wal(&engine, None).expect("empty wal should verify");
    assert_eq!(empty_verification.entry_count, 0);
    assert_eq!(empty_verification.last_sequence, None);

    let result = engine
        .execute_command(upsert_command(
            json!({
                "_id": "wal_fire",
                "element": "Fire"
            }),
            OperationFilter::drawer("gem"),
        ))
        .expect("upsert command should succeed");
    assert!(matches!(
        result,
        CommandResult::Upsert(UpsertResult::Pointers(pointers))
            if pointers == vec!["@gem:wal_fire".to_string()]
    ));

    let root_wal = database.path.join(WAL_FILE_NAME);
    assert!(root_wal.exists());

    let verification = status_wal(&engine, None).expect("wal should verify");
    assert_eq!(verification.entry_count, 3);
    assert_eq!(verification.last_sequence, Some(3));

    let command_result = engine
        .execute_command(Command::Status(StatusRequest::wal(None::<String>)))
        .expect("wal verification command should succeed");
    assert_eq!(
        command_result,
        CommandResult::Status(StatusResult::Wal(verification))
    );

    engine
        .execute(
            StorageCoordinate::new("tenant_wal", "production", "core"),
            upsert_command(
                json!({
                    "_id": "tenant_wal_fire",
                    "element": "Routed Fire"
                }),
                OperationFilter::drawer("gem"),
            ),
        )
        .expect("coordinate upsert should succeed");

    let routed_wal = database
        .path
        .join("tenant_wal")
        .join("production")
        .join("core")
        .join(WAL_FILE_NAME);
    assert!(routed_wal.exists());

    let routed_verification =
        status_wal(&engine, Some("tenant_wal/production/core")).expect("routed wal should verify");
    assert_eq!(routed_verification.entry_count, 3);
    assert_eq!(routed_verification.last_sequence, Some(3));
}

#[test]
fn us_101_bulk_upsert_returns_ordered_pointers() {
    let database = TempDatabase::new("us_101_bulk_upsert_ordered_pointers");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    let result = engine
        .execute_command(upsert_command(
            json!([
                json!({"_id": "bulk_fire", "element": "Fire"}),
                json!({"_id": "bulk_water", "element": "Water"}),
            ]),
            OperationFilter::drawer("gem"),
        ))
        .expect("bulk upsert should succeed");

    assert_eq!(
        result,
        CommandResult::Upsert(UpsertResult::Pointers(vec![
            "@gem:bulk_fire".to_string(),
            "@gem:bulk_water".to_string()
        ]))
    );
    assert_eq!(
        engine
            .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
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

    upsert_batch(
        &engine,
        "entity",
        vec![
            json!({"_id": "entity_00000000", "display_name": "Author"}),
            json!({"_id": "entity_00000001", "display_name": "Editor"}),
        ],
    )
    .expect("entity batch should upsert");

    let pointers = upsert_batch(
        &engine,
        "book",
        vec![json!({
            "_id": "book_00000000",
            "title": "Bulk Relationship Book",
            "author_id": "entity_00000000",
            "editor_id": "entity_00000001"
        })],
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

    let error = upsert_batch(
        &engine,
        "gem",
        vec![
            json!({"_id": "bulk_fire", "element": "Fire"}),
            json!({"_id": "bulk_broken"}),
        ],
    )
    .expect_err("invalid atomic batch should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        engine
            .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
            .expect("count should succeed"),
        0
    );

    drop(engine);
    let reopened = WardrobeEngine::open(&database_directory).expect("engine should reopen");
    assert_eq!(
        reopened
            .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
            .expect("count should succeed"),
        0
    );
}

#[test]
fn us_102_engine_transactions_flush_dirty_metadata_on_commit() {
    let database = TempDatabase::new("us_102_engine_metadata_commit_flush");
    let database_directory = database.path.to_string_lossy().into_owned();
    let engine = WardrobeEngine::open(&database_directory).expect("engine should initialize");

    upsert_batch(
        &engine,
        "gem",
        vec![
            json!({"_id": "fire", "element": "Fire"}),
            json!({"_id": "water", "element": "Water"}),
        ],
    )
    .expect("bulk upsert should commit");

    let metadata_contents = fs::read_to_string(database.path.join("gem_meta.drw"))
        .expect("metadata should read after bulk upsert");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_contents).expect("metadata should parse after bulk upsert");
    assert_eq!(metadata["record_count"], 2);

    engine
        .upsert(
            json!({"_id": "fire", "element": "Flame"}),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("update should commit");
    let metadata_contents = fs::read_to_string(database.path.join("gem_meta.drw"))
        .expect("metadata should read after update");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_contents).expect("metadata should parse after update");
    assert_eq!(metadata["record_count"], 2);

    assert_eq!(
        engine
            .delete(
                OperationFilter::pointer("@gem:water"),
                None::<OperationOptions>
            )
            .expect("delete should commit"),
        1
    );
    let metadata_contents = fs::read_to_string(database.path.join("gem_meta.drw"))
        .expect("metadata should read after delete");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_contents).expect("metadata should parse after delete");
    assert_eq!(metadata["record_count"], 1);
}
