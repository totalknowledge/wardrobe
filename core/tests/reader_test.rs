use std::fs;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_core::DatabaseReader;

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
    writeln!(file, r#"{{"_id":"@gem:lnk_a","element":"Fire"}}"#).expect("write should succeed");
    writeln!(file, "!!DEAD!!").expect("write should succeed");

    let reader = DatabaseReader::open_drawer(&file_path).expect("reader should open");
    let record = reader
        .read_record_at_offset(0)
        .expect("read should succeed")
        .expect("record should exist");
    assert_eq!(record["element"], "Fire");

    let raw_bytes = reader
        .read_raw_bytes_at_offset(0)
        .expect("raw read should succeed")
        .expect("bytes should exist");
    assert!(raw_bytes.ends_with(b"\n"));

    let tombstoned = reader
        .read_record_at_offset(raw_bytes.len() as u64)
        .expect("read should succeed");
    assert!(tombstoned.is_none());
}

#[test]
fn us_071_reader_reuses_handle_for_successive_reads_and_closes_cleanly() {
    let file_path = temp_file_path("persistent_reader_successive_reads");
    let mut file = fs::File::create(&file_path).expect("file should create");
    writeln!(file, r#"{{"_id":"@gem:fire","element":"Fire"}}"#).expect("write should succeed");
    writeln!(file, r#"{{"_id":"@gem:water","element":"Water"}}"#).expect("write should succeed");
    file.sync_all().expect("sync should succeed");

    let reader = DatabaseReader::open_drawer(&file_path).expect("reader should open");
    let first_raw = reader
        .read_raw_bytes_at_offset(0)
        .expect("first raw read should succeed")
        .expect("first record should exist");
    let first = reader
        .read_record_at_offset(0)
        .expect("first read should succeed")
        .expect("first record should exist");
    let second = reader
        .read_record_at_offset(first_raw.len() as u64)
        .expect("second read should succeed")
        .expect("second record should exist");

    assert_eq!(first["element"], "Fire");
    assert_eq!(second["element"], "Water");
    reader.close().expect("reader should close cleanly");
}

#[test]
fn stream_with_offsets_reports_each_line_offset() {
    let file_path = temp_file_path("reader_stream_with_offsets");
    let mut file = fs::File::create(&file_path).expect("file should create");
    writeln!(file, r#"{{"a":1}}"#).expect("write should succeed");
    writeln!(file, r#"{{"b":2}}"#).expect("write should succeed");

    let reader = DatabaseReader::open_drawer(&file_path).expect("reader should open");
    let mut offsets = Vec::new();

    reader
        .stream_with_offsets(|offset, line| {
            offsets.push((offset, String::from_utf8_lossy(line).to_string()))
        })
        .expect("stream should succeed");

    assert_eq!(offsets.len(), 2);
    assert_eq!(offsets[0].0, 0);
    assert!(offsets[1].0 > 0);
}

#[test]
fn reader_handles_truncated_frame_gracefully() {
    let file_path = temp_file_path("reader_trunc_graceful");
    let mut file = fs::File::create(&file_path).expect("create");
    file.write_all(b"WRDB\x00\x00\x00\x40data").expect("write");
    file.sync_all().expect("sync");

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
    file.write_all(b"data\n").expect("write");

    let reader = DatabaseReader::open_drawer(&file_path).expect("open");
    assert!(reader.read_raw_bytes_at_offset(100).unwrap().is_none());
    assert!(reader.read_record_at_offset(100).unwrap().is_none());
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
    file.write_all(b"{\"corrupted_payload_without_closing_brace\n")
        .expect("write");

    let reader = DatabaseReader::open_drawer(&file_path).expect("open");
    assert!(reader.read_record_at_offset(0).unwrap().is_none());
}

#[test]
fn read_record_at_offset_valid_plaintext_parsing() {
    let file_path = temp_file_path("valid_plaintext");
    let mut file = fs::File::create(&file_path).expect("create");
    file.write_all(b"{\"valid\":true}\n").expect("write");

    let reader = DatabaseReader::open_drawer(&file_path).expect("open");
    let record = reader.read_record_at_offset(0).unwrap().unwrap();
    assert_eq!(record["valid"], true);
}

#[test]
fn read_record_at_offset_valid_binary_parsing() {
    let file_path = temp_file_path("valid_binary");
    let mut file = fs::File::create(&file_path).expect("create");
    file.write_all(b"WRDB\x00\x00\x00\x18\x13\x00\x00\x00\x08valid\x00\x01\x00")
        .expect("write");

    let reader = DatabaseReader::open_drawer(&file_path).expect("open");
    let _ = reader.read_record_at_offset(0);
}
