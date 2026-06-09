use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_core::{DatabaseWriter, PlainTextJsonFormat, StorageFormat};

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
        PlainTextJsonFormat::serialize_record(&serde_json::json!({"a": 1})).expect("serialize");

    let first_offset = writer
        .append_record(&first_payload, 8)
        .expect("append should succeed");
    assert_eq!(first_offset, 0);

    let replacement_payload =
        PlainTextJsonFormat::serialize_record(&serde_json::json!({"a": 2})).expect("serialize");
    writer
        .overwrite_at_offset(first_offset, &replacement_payload, 8)
        .expect("overwrite should succeed");

    writer
        .write_tombstone_at_offset(first_offset, 16)
        .expect("tombstone should succeed");

    let contents = fs::read_to_string(&file_path).expect("file should be readable");
    assert!(contents.starts_with("!!DEAD!!"));
}

#[test]
fn append_aligned_index_writes_data_and_reports_length() {
    let file_path = temp_file_path("writer_append_aligned_index");
    let writer = DatabaseWriter::open_drawer(&file_path).expect("writer should open");

    let len = writer.current_length().expect("length should be readable");
    assert_eq!(len, 0);

    let mut writer = writer;
    let index_payload =
        PlainTextJsonFormat::serialize_record(&serde_json::json!({"f": "_id", "k": "@x", "o": 0}))
            .expect("serialize");
    let offset = writer
        .append_aligned_index(&index_payload, 16)
        .expect("append should succeed");
    assert_eq!(offset, 0);

    let contents = fs::read_to_string(&file_path).expect("file should be readable");
    assert!(contents.contains(r#""f":"_id""#));
}

#[test]
fn append_record_pads_to_exact_alignment_with_large_padding_span() {
    let file_path = temp_file_path("writer_append_exact_alignment");
    let mut writer = DatabaseWriter::open_drawer(&file_path).expect("writer should open");
    let payload =
        PlainTextJsonFormat::serialize_record(&serde_json::json!({"a": 1})).expect("serialize");

    writer
        .append_record(&payload, 1024)
        .expect("append should succeed");

    let bytes = fs::read(&file_path).expect("file should be readable");
    assert_eq!(bytes.len(), 1024);
    assert_eq!(&bytes[..payload.len()], payload.as_slice());
    assert!(bytes[payload.len()..1023].iter().all(|byte| *byte == b' '));
    assert_eq!(bytes[1023], b'\n');

    let _ = fs::remove_file(file_path);
}

#[test]
fn tombstone_padding_preserves_exact_alignment_with_large_padding_span() {
    let file_path = temp_file_path("writer_tombstone_exact_alignment");
    let mut writer = DatabaseWriter::open_drawer(&file_path).expect("writer should open");
    let payload =
        PlainTextJsonFormat::serialize_record(&serde_json::json!({"a": 1})).expect("serialize");

    let offset = writer
        .append_record(&payload, 1024)
        .expect("append should succeed");
    writer
        .write_tombstone_at_offset(offset, 1024)
        .expect("tombstone should succeed");

    let bytes = fs::read(&file_path).expect("file should be readable");
    let tombstone = PlainTextJsonFormat::tombstone_marker();
    assert_eq!(bytes.len(), 1024);
    assert_eq!(&bytes[..tombstone.len()], tombstone);
    assert!(
        bytes[tombstone.len()..1023]
            .iter()
            .all(|byte| *byte == b' ')
    );
    assert_eq!(bytes[1023], b'\n');

    let _ = fs::remove_file(file_path);
}
