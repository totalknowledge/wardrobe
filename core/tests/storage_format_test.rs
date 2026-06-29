use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_core::{BsonBinaryFormat, DatabaseReader, DatabaseWriter, StorageFormat};

fn temp_file_path(test_name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir()
        .join(format!("wardrobe_{test_name}_{nanos}"))
        .with_extension("drw")
}

#[test]
fn bson_binary_format_round_trips_records() {
    let record = json!({
        "_id": "@gem:lnk_a",
        "element": "Fire",
        "potency": 8800
    });

    let serialized = BsonBinaryFormat::serialize_record(&record).expect("record should serialize");
    let deserialized =
        BsonBinaryFormat::deserialize_record(&serialized).expect("record should deserialize");

    assert_eq!(deserialized, Some(record));
    assert!(BsonBinaryFormat::is_binary_frame(&serialized));
}

#[test]
fn us_073_bson_binary_format_rejects_legacy_text_records() {
    let padded_record = BsonBinaryFormat::deserialize_record(br#"{"_id":"@gem:lnk_a"}       "#);
    assert!(padded_record.is_err());

    let legacy_tombstone = BsonBinaryFormat::deserialize_record(b"!!DEAD!!       \n");
    assert!(legacy_tombstone.is_err());
    assert!(!BsonBinaryFormat::is_tombstone(b"!!DEAD!!       "));

    let empty = BsonBinaryFormat::deserialize_record(b"     \n");
    assert!(empty.is_err());

    let invalid = BsonBinaryFormat::deserialize_record(b"not json");
    assert!(invalid.is_err());
}

#[test]
fn us_073_bson_binary_tombstone_frame_decodes_as_empty_record() {
    let tombstone =
        BsonBinaryFormat::tombstone_frame(BsonBinaryFormat::frame_header_len()).expect("tombstone");
    let decoded =
        BsonBinaryFormat::deserialize_record(&tombstone).expect("tombstone should decode cleanly");
    assert!(decoded.is_none());
    assert!(BsonBinaryFormat::is_tombstone(&tombstone));
}

#[test]
fn us_045_bson_storage_format_preserves_reader_writer_flow() {
    let file_path = temp_file_path("us_045_reader_writer_format_flow");
    let mut writer = DatabaseWriter::open_drawer(&file_path).expect("writer should open");
    let record = json!({"_id": "@gem:lnk_a", "element": "Fire"});
    let payload = BsonBinaryFormat::serialize_record(&record).expect("record should serialize");

    let offset = writer
        .append_record(&payload, 8)
        .expect("append should succeed");
    assert_eq!(offset, 0);

    let reader = DatabaseReader::open_drawer(&file_path).expect("reader should open");
    let decoded = reader
        .read_record_at_offset(offset, None)
        .expect("read should succeed");
    assert_eq!(decoded, Some(record));

    let raw_contents = fs::read(&file_path).expect("file should be readable");
    assert!(raw_contents.starts_with(b"WRDB"));
}

#[test]
fn us_045_bson_binary_layout_uses_big_endian_slot_and_payload_lengths() {
    let record = json!({"_id": "@gem:lnk_a", "power": 10});
    let payload = BsonBinaryFormat::serialize_record(&record).expect("serialize");
    let framed = BsonBinaryFormat::with_slot_size(&payload, 64).expect("slot rewrite");

    let payload_len = u32::from_be_bytes(framed[8..12].try_into().expect("payload length bytes"));
    let slot_len = u32::from_be_bytes(framed[12..16].try_into().expect("slot length bytes"));

    assert_eq!(
        payload_len as usize + BsonBinaryFormat::frame_header_len(),
        payload.len()
    );
    assert_eq!(slot_len, 64);
}

#[test]
fn us_045_bson_binary_rejects_invalid_slot_mutations() {
    let record = json!({"_id": "@gem:lnk_a"});
    let payload = BsonBinaryFormat::serialize_record(&record).expect("serialize");

    let too_small = BsonBinaryFormat::with_slot_size(&payload, payload.len() - 1);
    assert!(too_small.is_err());

    let invalid_header = BsonBinaryFormat::with_slot_size(b"{}", 32);
    assert!(invalid_header.is_err());
}

#[test]
fn us_134_native_record_round_trips_supported_value_types() {
    let field_name_map = BTreeMap::from([
        ("_".to_string(), "_id".to_string()),
        ("a".to_string(), "name".to_string()),
        ("b".to_string(), "missing".to_string()),
        ("c".to_string(), "enabled".to_string()),
        ("d".to_string(), "disabled".to_string()),
        ("e".to_string(), "signed".to_string()),
        ("f".to_string(), "unsigned".to_string()),
        ("g".to_string(), "ratio".to_string()),
        ("h".to_string(), "nothing".to_string()),
        ("i".to_string(), "tags".to_string()),
        ("j".to_string(), "nested".to_string()),
    ]);
    let stored_record = json!({
        "_": "record-1",
        "a": "Bob",
        "c": true,
        "d": false,
        "e": -42,
        "f": 42_u64,
        "g": 1.5,
        "h": null,
        "i": ["alpha", 2, true],
        "j": {"inner": "value"}
    });

    let encoded = BsonBinaryFormat::serialize_native_record(&stored_record, &field_name_map)
        .expect("native record should serialize");
    assert_eq!(encoded[4], 2);

    let decoded = BsonBinaryFormat::deserialize_record_with_map(&encoded, Some(&field_name_map))
        .expect("native record should deserialize")
        .expect("native record should be live");

    assert_eq!(decoded, stored_record);
    assert!(decoded.get("b").is_none(), "missing fields stay absent");
}

#[test]
fn us_134_native_record_presence_bitmap_uses_lsb_field_order() {
    let field_name_map = BTreeMap::from([
        ("_".to_string(), "_id".to_string()),
        ("a".to_string(), "name".to_string()),
        ("b".to_string(), "age".to_string()),
        ("c".to_string(), "weight".to_string()),
    ]);
    let stored_record = json!({
        "_": "record-1",
        "b": 56
    });

    let encoded = BsonBinaryFormat::serialize_native_record(&stored_record, &field_name_map)
        .expect("native record should serialize");

    assert_eq!(
        encoded[16], 4,
        "field_count_at_write should be encoded as a single-byte varint"
    );
    assert_eq!(
        encoded[17], 0b0000_0101,
        "presence bitmap should mark field ordinals 0 and 2"
    );
}

#[test]
fn us_134_native_record_decodes_after_field_map_growth() {
    let original_field_name_map = BTreeMap::from([
        ("_".to_string(), "_id".to_string()),
        ("a".to_string(), "name".to_string()),
        ("b".to_string(), "age".to_string()),
    ]);
    let grown_field_name_map = BTreeMap::from([
        ("_".to_string(), "_id".to_string()),
        ("a".to_string(), "name".to_string()),
        ("b".to_string(), "age".to_string()),
        ("A".to_string(), "new_after_lowercase_tokens".to_string()),
    ]);
    let stored_record = json!({
        "_": "record-1",
        "a": "Bob",
        "b": 56
    });

    let encoded =
        BsonBinaryFormat::serialize_native_record(&stored_record, &original_field_name_map)
            .expect("native record should serialize");
    let decoded =
        BsonBinaryFormat::deserialize_record_with_map(&encoded, Some(&grown_field_name_map))
            .expect("native record should deserialize with grown map")
            .expect("native record should be live");

    assert_eq!(decoded, stored_record);
}

#[test]
fn us_134_native_record_requires_field_map_and_bson_still_reads_with_map() {
    let field_name_map = BTreeMap::from([("_".to_string(), "_id".to_string())]);
    let native =
        BsonBinaryFormat::serialize_native_record(&json!({"_": "record-1"}), &field_name_map)
            .expect("native record should serialize");
    let err = BsonBinaryFormat::deserialize_record_with_map(&native, None)
        .expect_err("native records need a field map");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

    let bson_record = json!({"_": "record-1"});
    let bson = BsonBinaryFormat::serialize_record(&bson_record).expect("bson should serialize");
    let decoded = BsonBinaryFormat::deserialize_record_with_map(&bson, Some(&field_name_map))
        .expect("bson should deserialize with ignored map");
    assert_eq!(decoded, Some(bson_record));
}
