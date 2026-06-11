use std::io::{Cursor, ErrorKind};
use wardrobe_core::{PROTOCOL_MAGIC, ProtocolFrame, ProtocolOpcode};

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
