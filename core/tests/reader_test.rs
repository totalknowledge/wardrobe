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
