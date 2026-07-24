use std::io::{Cursor, ErrorKind, Write};
use serde_json::json;
use wardrobe_core::{
    Command, CommandResult, OperationFilter, OperationOptions, PaginatedReadResult,
    PaginationMetadata, PROTOCOL_MAGIC, ProtocolFrame, ProtocolOpcode, ReadResult,
};

#[derive(Default)]
struct RecordingWriter {
    writes: Vec<Vec<u8>>,
    flushes: usize,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writes.push(buf.to_vec());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn protocol_frame_writes_stable_binary_header_and_payload() {
    let frame = ProtocolFrame::new(ProtocolOpcode::Command, b"find-all:gem".to_vec());
    let mut buffer = Vec::new();

    frame
        .write_to_stream(&mut buffer)
        .expect("frame should encode");

    assert_eq!(&buffer[0..2], &PROTOCOL_MAGIC);
    assert_eq!(buffer[2], 0x01);
    assert_eq!(&buffer[3..7], &(12u32).to_be_bytes());
    assert_eq!(&buffer[7..], b"find-all:gem");
}

#[test]
fn protocol_frame_unflushed_writer_splits_header_and_payload_without_flush() {
    let frame = ProtocolFrame::new(ProtocolOpcode::Command, b"read:gem".to_vec());
    let mut writer = RecordingWriter::default();

    frame
        .write_to_stream_unflushed(&mut writer)
        .expect("frame should encode");

    assert_eq!(writer.flushes, 0);
    assert_eq!(writer.writes.len(), 2);
    assert_eq!(&writer.writes[0][0..2], &PROTOCOL_MAGIC);
    assert_eq!(writer.writes[0][2], 0x01);
    assert_eq!(&writer.writes[0][3..7], &(8u32).to_be_bytes());
    assert_eq!(writer.writes[1], b"read:gem");
}

#[test]
fn protocol_frame_generic_writer_still_flushes_after_payload() {
    let frame = ProtocolFrame::new(ProtocolOpcode::Result, b"ok".to_vec());
    let mut writer = RecordingWriter::default();

    frame
        .write_to_stream(&mut writer)
        .expect("frame should encode");

    assert_eq!(writer.flushes, 1);
    assert_eq!(writer.writes.len(), 2);
    assert_eq!(writer.writes[1], b"ok");
}

#[test]
fn protocol_frame_reads_encoded_payload_from_stream() {
    let expected = ProtocolFrame::new(ProtocolOpcode::Result, br#"{"ok":true}"#.to_vec());
    let mut buffer = Vec::new();
    expected
        .write_to_stream(&mut buffer)
        .expect("frame should encode");

    let mut stream = Cursor::new(buffer);
    let decoded = ProtocolFrame::read_from_stream(&mut stream).expect("frame should decode");

    assert_eq!(decoded, expected);
}

#[test]
fn protocol_frame_rejects_invalid_magic_bytes() {
    let mut stream = Cursor::new(vec![0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
    let error = ProtocolFrame::read_from_stream(&mut stream).expect_err("bad magic should reject");

    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn protocol_frame_rejects_invalid_opcode() {
    let mut stream = Cursor::new(vec![0x57, 0x44, 0xff, 0x00, 0x00, 0x00, 0x00]);
    let error = ProtocolFrame::read_from_stream(&mut stream).expect_err("bad opcode should reject");

    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn protocol_frame_reports_truncated_payload() {
    let mut stream = Cursor::new(vec![0x57, 0x44, 0x03, 0x00, 0x00, 0x00, 0x04, b'o']);
    let error =
        ProtocolFrame::read_from_stream(&mut stream).expect_err("short payload should reject");

    assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
}

#[test]
fn cursor_page_options_and_metadata_round_trip_through_protocol_payloads() {
    let command = Command::Read {
        filter: OperationFilter::drawer("book"),
        options: OperationOptions::new()
            .order_by("rank")
            .page(2)
            .page_size(25)
            .cursor("cursor-token"),
    };
    let command_json = serde_json::to_vec(&command).expect("command should serialize");
    let decoded_command: Command = serde_json::from_slice(&command_json).expect("command should deserialize");
    assert_eq!(decoded_command, command);

    let result = CommandResult::Read(ReadResult::Page(PaginatedReadResult {
        records: vec![json!({"_id": "book-25", "rank": 25})],
        pagination: PaginationMetadata {
            next_cursor: Some("cursor-next".to_string()),
            has_more: true,
            page: Some(2),
            page_size: 25,
        },
    }));
    let result_json = serde_json::to_vec(&result).expect("result should serialize");
    let decoded_result: CommandResult = serde_json::from_slice(&result_json).expect("result should deserialize");
    assert_eq!(decoded_result, result);
}
