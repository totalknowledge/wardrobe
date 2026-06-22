use serde_json::json;
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
        .read_record_at_offset(offset)
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
