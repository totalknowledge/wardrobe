use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_core::{BsonBinaryFormat, DatabaseReader, StorageFormat};

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
fn read_record_and_raw_bytes_at_offset_handle_live_and_dead_lines() {
    let file_path = temp_file_path("reader_record_raw_bytes");
    let mut file = fs::File::create(&file_path).expect("file should create");
    let record_payload =
        BsonBinaryFormat::serialize_record(&json!({"_id":"@gem:lnk_a","element":"Fire"}))
            .expect("record should serialize");
    let mut tombstone_payload =
        BsonBinaryFormat::tombstone_frame(BsonBinaryFormat::frame_header_len() * 2)
            .expect("tombstone should serialize");
    tombstone_payload.resize(BsonBinaryFormat::frame_header_len() * 2, 0);
    file.write_all(&record_payload)
        .expect("write should succeed");
    file.write_all(&tombstone_payload)
        .expect("write should succeed");

    let reader = DatabaseReader::open_drawer(&file_path).expect("reader should open");
    let record = reader
        .read_record_at_offset(0, None)
        .expect("read should succeed")
        .expect("record should exist");
    assert_eq!(record["element"], "Fire");

    let raw_bytes = reader
        .read_raw_bytes_at_offset(0)
        .expect("raw read should succeed")
        .expect("bytes should exist");
    assert!(raw_bytes.starts_with(b"WRDB"));

    let tombstoned = reader
        .read_record_at_offset(raw_bytes.len() as u64, None)
        .expect("read should succeed");
    assert!(tombstoned.is_none());
}

#[test]
fn us_071_reader_reuses_handle_for_successive_reads_and_closes_cleanly() {
    let file_path = temp_file_path("persistent_reader_successive_reads");
    let mut file = fs::File::create(&file_path).expect("file should create");
    let first_payload =
        BsonBinaryFormat::serialize_record(&json!({"_id":"@gem:fire","element":"Fire"}))
            .expect("first record should serialize");
    let second_payload =
        BsonBinaryFormat::serialize_record(&json!({"_id":"@gem:water","element":"Water"}))
            .expect("second record should serialize");
    file.write_all(&first_payload)
        .expect("write should succeed");
    file.write_all(&second_payload)
        .expect("write should succeed");
    file.flush().expect("sync should succeed");

    let reader = DatabaseReader::open_drawer(&file_path).expect("reader should open");
    let first_raw = reader
        .read_raw_bytes_at_offset(0)
        .expect("first raw read should succeed")
        .expect("first record should exist");
    let first = reader
        .read_record_at_offset(0, None)
        .expect("first read should succeed")
        .expect("first record should exist");
    let second = reader
        .read_record_at_offset(first_raw.len() as u64, None)
        .expect("second read should succeed")
        .expect("second record should exist");

    assert_eq!(first["element"], "Fire");
    assert_eq!(second["element"], "Water");
    reader.close().expect("reader should close cleanly");
}

#[test]
fn read_records_at_offsets_batches_ordered_live_reads() {
    let file_path = temp_file_path("reader_batch_offsets");
    let mut file = fs::File::create(&file_path).expect("file should create");
    let first_payload =
        BsonBinaryFormat::serialize_record(&json!({"_id":"@gem:fire","element":"Fire"}))
            .expect("first record should serialize");
    let mut tombstone_payload =
        BsonBinaryFormat::tombstone_frame(BsonBinaryFormat::frame_header_len() * 2)
            .expect("tombstone should serialize");
    tombstone_payload.resize(BsonBinaryFormat::frame_header_len() * 2, 0);
    let second_payload =
        BsonBinaryFormat::serialize_record(&json!({"_id":"@gem:water","element":"Water"}))
            .expect("second record should serialize");
    let tombstone_offset = first_payload.len() as u64;
    let second_offset = tombstone_offset + tombstone_payload.len() as u64;

    file.write_all(&first_payload)
        .expect("first write should succeed");
    file.write_all(&tombstone_payload)
        .expect("tombstone write should succeed");
    file.write_all(&second_payload)
        .expect("second write should succeed");
    file.flush().expect("sync should succeed");

    let reader = DatabaseReader::open_drawer(&file_path).expect("reader should open");
    let records = reader
        .read_records_at_offsets(vec![0, tombstone_offset, second_offset], None)
        .expect("batch read should succeed");

    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["element"], "Fire");
    assert_eq!(records[1]["element"], "Water");
}

#[test]
fn us_134_reader_passes_field_map_to_native_record_decoder() {
    let file_path = temp_file_path("reader_native_records_with_field_map");
    let field_name_map = BTreeMap::from([
        ("_".to_string(), "_id".to_string()),
        ("a".to_string(), "name".to_string()),
    ]);
    let first_payload = BsonBinaryFormat::serialize_native_record(
        &json!({"_": "one", "a": "First"}),
        &field_name_map,
    )
    .expect("first native record should serialize");
    let second_payload = BsonBinaryFormat::serialize_native_record(
        &json!({"_": "two", "a": "Second"}),
        &field_name_map,
    )
    .expect("second native record should serialize");
    let second_offset = first_payload.len() as u64;

    let mut file = fs::File::create(&file_path).expect("file should create");
    file.write_all(&first_payload)
        .expect("first write should succeed");
    file.write_all(&second_payload)
        .expect("second write should succeed");
    file.flush().expect("sync should succeed");

    let reader = DatabaseReader::open_drawer(&file_path).expect("reader should open");
    let first = reader
        .read_record_at_offset(0, Some(&field_name_map))
        .expect("native read should succeed")
        .expect("native record should exist");
    assert_eq!(first["a"], "First");

    let records = reader
        .read_records_at_offsets(vec![0, second_offset], Some(&field_name_map))
        .expect("native batch read should succeed");
    assert_eq!(records.len(), 2);
    assert_eq!(records[1]["a"], "Second");
}

#[test]
fn stream_with_offsets_reports_each_line_offset() {
    let file_path = temp_file_path("reader_stream_with_offsets");
    let mut file = fs::File::create(&file_path).expect("file should create");
    let first_payload =
        BsonBinaryFormat::serialize_record(&json!({"a":1})).expect("first should serialize");
    let second_payload =
        BsonBinaryFormat::serialize_record(&json!({"b":2})).expect("second should serialize");
    file.write_all(&first_payload)
        .expect("write should succeed");
    file.write_all(&second_payload)
        .expect("write should succeed");

    let reader = DatabaseReader::open_drawer(&file_path).expect("reader should open");
    let mut offsets = Vec::new();

    reader
        .stream_with_offsets(|offset, line| offsets.push((offset, line.to_vec())))
        .expect("stream should succeed");

    assert_eq!(offsets.len(), 2);
    assert_eq!(offsets[0].0, 0);
    assert_eq!(offsets[1].0, first_payload.len() as u64);
    assert!(offsets[0].1.starts_with(b"WRDB"));
    assert!(offsets[1].1.starts_with(b"WRDB"));
}

#[test]
fn reader_handles_truncated_frame_gracefully() {
    let file_path = temp_file_path("reader_trunc_graceful");
    let mut file = fs::File::create(&file_path).expect("create");
    file.write_all(b"WRDB\x00\x00\x00\x40data").expect("write");
    file.flush().expect("sync");

    if let Ok(reader) = DatabaseReader::open_drawer(&file_path) {
        assert!(reader.read_raw_bytes_at_offset(0).is_err());
    }
}

#[test]
fn reader_open_drawer_missing_file_errors() {
    let missing_path = temp_file_path("missing_file");
    assert!(DatabaseReader::open_drawer(&missing_path).is_err());
}

#[test]
fn reader_offset_out_of_bounds_returns_none() {
    let file_path = temp_file_path("bounds_test");
    let mut file = fs::File::create(&file_path).expect("create");
    let payload = BsonBinaryFormat::serialize_record(&json!({"valid":true}))
        .expect("record should serialize");
    file.write_all(&payload).expect("write");

    let reader = DatabaseReader::open_drawer(&file_path).expect("open");
    assert!(reader.read_raw_bytes_at_offset(100).unwrap().is_none());
    assert!(reader.read_record_at_offset(100, None).unwrap().is_none());
}

#[test]
fn reader_empty_file_streams_nothing() {
    let file_path = temp_file_path("empty_stream");
    fs::File::create(&file_path).expect("create");

    let reader = DatabaseReader::open_drawer(&file_path).expect("open");
    let mut execution_count = 0;
    let res = reader.stream_with_offsets(|_, _| execution_count += 1);
    assert!(res.is_ok());
    assert_eq!(execution_count, 0);
}

#[test]
fn stream_with_offsets_binary_overflow_error() {
    let file_path = temp_file_path("binary_overflow");
    let mut file = fs::File::create(&file_path).expect("create");
    file.write_all(b"WRDB\x00\x00\x00\x7f").expect("write");

    let reader = DatabaseReader::open_drawer(&file_path).expect("open");
    assert!(reader.stream_with_offsets(|_, _| {}).is_err());
}

#[test]
fn read_record_at_offset_deserialization_failure() {
    let file_path = temp_file_path("deserialization_fail");
    let mut file = fs::File::create(&file_path).expect("create");
    file.write_all(b"WRDB\x01\x00\x00\x00\x00\x00\x00\x04\x00\x00\x00\x14nope")
        .expect("write");

    let reader = DatabaseReader::open_drawer(&file_path).expect("open");
    assert!(reader.read_record_at_offset(0, None).is_err());
}

#[test]
fn us_072_read_record_at_offset_rejects_legacy_plaintext() {
    let file_path = temp_file_path("legacy_plaintext_rejected");
    let mut file = fs::File::create(&file_path).expect("create");
    file.write_all(b"{\"valid\":true}\n").expect("write");

    let reader = DatabaseReader::open_drawer(&file_path).expect("open");
    let error = reader
        .read_record_at_offset(0, None)
        .expect_err("legacy plaintext should be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn us_072_stream_with_offsets_rejects_legacy_plaintext() {
    let file_path = temp_file_path("legacy_plaintext_stream_rejected");
    let mut file = fs::File::create(&file_path).expect("create");
    file.write_all(b"{\"valid\":true}\n").expect("write");

    let reader = DatabaseReader::open_drawer(&file_path).expect("open");
    let error = reader
        .stream_with_offsets(|_, _| {})
        .expect_err("legacy plaintext should be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn read_record_at_offset_valid_binary_parsing() {
    let file_path = temp_file_path("valid_binary");
    let mut file = fs::File::create(&file_path).expect("create");
    let payload = BsonBinaryFormat::serialize_record(&json!({"valid":true}))
        .expect("record should serialize");
    file.write_all(&payload).expect("write");

    let reader = DatabaseReader::open_drawer(&file_path).expect("open");
    let record = reader
        .read_record_at_offset(0, None)
        .expect("read should succeed")
        .expect("record should exist");
    assert_eq!(record["valid"], true);
}
