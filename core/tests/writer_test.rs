use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_core::{BsonBinaryFormat, DatabaseReader, DatabaseWriter, Recycler, StorageFormat};

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
fn append_overwrite_and_tombstone_work() {
    let file_path = temp_file_path("writer_append_overwrite_tombstone");
    let mut writer = DatabaseWriter::open_drawer(&file_path).expect("writer should open");
    let first_payload =
        BsonBinaryFormat::serialize_record(&serde_json::json!({"a": 1})).expect("serialize");
    let slot_size = Recycler::new().calculate_aligned_size(first_payload.len());

    let first_offset = writer
        .append_record(&first_payload, 8)
        .expect("append should succeed");
    assert_eq!(first_offset, 0);

    let replacement_payload =
        BsonBinaryFormat::serialize_record(&serde_json::json!({"a": 2})).expect("serialize");
    writer
        .overwrite_at_offset(first_offset, &replacement_payload, 8)
        .expect("overwrite should succeed");

    writer
        .write_tombstone_at_offset(first_offset, slot_size)
        .expect("tombstone should succeed");

    let reader = DatabaseReader::open_drawer(&file_path).expect("reader should open");
    let decoded = reader
        .read_record_at_offset(first_offset, None)
        .expect("read should succeed");
    assert!(decoded.is_none());
}

#[test]
fn append_aligned_index_writes_data_and_reports_length() {
    let file_path = temp_file_path("writer_append_aligned_index");
    let writer = DatabaseWriter::open_drawer(&file_path).expect("writer should open");

    let len = writer.current_length().expect("length should be readable");
    assert_eq!(len, 0);

    let mut writer = writer;
    let index_payload =
        BsonBinaryFormat::serialize_record(&serde_json::json!({"f": "_id", "k": "@x", "o": 0}))
            .expect("serialize");
    let offset = writer
        .append_aligned_index(&index_payload, 16)
        .expect("append should succeed");
    assert_eq!(offset, 0);

    let contents = fs::read(&file_path).expect("file should be readable");
    let decoded = BsonBinaryFormat::deserialize_record(&contents)
        .expect("decode should succeed")
        .expect("record should exist");
    assert_eq!(decoded["f"], "_id");
}

#[test]
fn us_073_writer_rejects_legacy_text_payloads() {
    let file_path = temp_file_path("writer_rejects_legacy_text");
    let mut writer = DatabaseWriter::open_drawer(&file_path).expect("writer should open");

    let error = writer
        .append_record(br#"{"legacy":true}"#, 64)
        .expect_err("legacy text payload should be rejected");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        writer.current_length().expect("length should be readable"),
        0
    );

    let _ = fs::remove_file(file_path);
}

#[test]
fn append_record_pads_to_exact_alignment_with_large_padding_span() {
    let file_path = temp_file_path("writer_append_exact_alignment");
    let mut writer = DatabaseWriter::open_drawer(&file_path).expect("writer should open");
    let payload =
        BsonBinaryFormat::serialize_record(&serde_json::json!({"a": 1})).expect("serialize");

    writer
        .append_record(&payload, 1024)
        .expect("append should succeed");

    let bytes = fs::read(&file_path).expect("file should be readable");
    assert_eq!(bytes.len(), 1024);
    let slot_len =
        u32::from_be_bytes(bytes[12..16].try_into().expect("slot length bytes")) as usize;
    assert_eq!(slot_len, 1024);
    let decoded = BsonBinaryFormat::deserialize_record(&bytes)
        .expect("decode should succeed")
        .expect("record should exist");
    assert_eq!(decoded["a"], 1);
    assert!(bytes[payload.len()..1024].iter().all(|byte| *byte == 0));

    let _ = fs::remove_file(file_path);
}

#[test]
fn tombstone_padding_preserves_exact_alignment_with_large_padding_span() {
    let file_path = temp_file_path("writer_tombstone_exact_alignment");
    let mut writer = DatabaseWriter::open_drawer(&file_path).expect("writer should open");
    let payload =
        BsonBinaryFormat::serialize_record(&serde_json::json!({"a": 1})).expect("serialize");

    let offset = writer
        .append_record(&payload, 1024)
        .expect("append should succeed");
    writer
        .write_tombstone_at_offset(offset, 1024)
        .expect("tombstone should succeed");

    let bytes = fs::read(&file_path).expect("file should be readable");
    assert_eq!(bytes.len(), 1024);
    assert!(BsonBinaryFormat::is_tombstone(&bytes));
    assert!(bytes[16..1024].iter().all(|byte| *byte == 0));

    let _ = fs::remove_file(file_path);
}

#[test]
fn open_drawer_creates_file_without_exposing_sync_lifecycle() -> std::io::Result<()> {
    let path = temp_file_path("writer_open_creates_file");
    let _ = fs::remove_file(&path);
    let _writer = DatabaseWriter::open_drawer(&path)?;
    assert!(path.exists());
    let _ = fs::remove_file(&path);
    Ok(())
}
