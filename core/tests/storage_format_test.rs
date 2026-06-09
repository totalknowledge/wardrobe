use serde_json::json;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_core::{DatabaseReader, DatabaseWriter, PlainTextJsonFormat, StorageFormat};

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
fn plaintext_json_format_round_trips_records() {
    let record = json!({
        "_id": "@gem:lnk_a",
        "element": "Fire",
        "potency": 8800
    });

    let serialized =
        PlainTextJsonFormat::serialize_record(&record).expect("record should serialize");
    let deserialized =
        PlainTextJsonFormat::deserialize_record(&serialized).expect("record should deserialize");

    assert_eq!(deserialized, Some(record));
    assert!(!serialized.ends_with(b"\n"));
}

#[test]
fn plaintext_json_format_handles_padding_tombstones_empty_and_invalid_records() {
    let padded_record = PlainTextJsonFormat::deserialize_record(br#"{"_id":"@gem:lnk_a"}       "#)
        .expect("padded record should parse");
    assert_eq!(padded_record, Some(json!({"_id": "@gem:lnk_a"})));

    let tombstone = PlainTextJsonFormat::deserialize_record(b"!!DEAD!!       \n")
        .expect("tombstone should decode cleanly");
    assert!(tombstone.is_none());
    assert!(PlainTextJsonFormat::is_tombstone(b"!!DEAD!!       "));

    let empty =
        PlainTextJsonFormat::deserialize_record(b"     \n").expect("empty should decode cleanly");
    assert!(empty.is_none());

    let invalid = PlainTextJsonFormat::deserialize_record(b"not json")
        .expect("invalid text should decode cleanly");
    assert!(invalid.is_none());
}

#[test]
fn us_016_plaintext_storage_format_preserves_reader_writer_flow() {
    let file_path = temp_file_path("us_016_reader_writer_format_flow");
    let mut writer = DatabaseWriter::open_drawer(&file_path).expect("writer should open");
    let record = json!({"_id": "@gem:lnk_a", "element": "Fire"});
    let payload = PlainTextJsonFormat::serialize_record(&record).expect("record should serialize");

    let offset = writer
        .append_record(&payload, 8)
        .expect("append should succeed");
    assert_eq!(offset, 0);

    let mut reader = DatabaseReader::open_drawer(&file_path).expect("reader should open");
    let decoded = reader
        .read_record_at_offset(offset)
        .expect("read should succeed");
    assert_eq!(decoded, Some(record));

    let raw_contents = fs::read(&file_path).expect("file should be readable");
    assert!(raw_contents.ends_with(b"\n"));
    assert!(raw_contents.starts_with(b"{"));
}
