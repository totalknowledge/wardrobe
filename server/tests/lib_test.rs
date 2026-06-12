use std::io;
use std::io::Cursor;
use std::path::PathBuf;
use std::thread;
use wardrobe_core::{ProtocolFrame, ProtocolOpcode};
use wardrobe_server::{ServerConfig, print_help};

#[test]
fn server_config_from_args_defaults() {
    let cfg = ServerConfig::from_args(Vec::<String>::new()).expect("should parse defaults");
    assert_eq!(cfg.data_dir, "./wardrobe");
    assert_eq!(cfg.tcp_bind.unwrap().starts_with("127.0.0.1"), true);
    assert!(!cfg.check_only);
}

#[test]
fn server_config_explicit_valid_flags() {
    let args = vec![
        "--data-dir".to_string(),
        "./alt_dir".to_string(),
        "--tcp-bind".to_string(),
        "127.0.0.1:9999".to_string(),
        "--max-connections".to_string(),
        "42".to_string(),
        "--check".to_string(),
    ];
    let cfg = ServerConfig::from_args(args).unwrap();
    assert_eq!(cfg.data_dir, "./alt_dir");
    assert_eq!(cfg.tcp_bind, Some("127.0.0.1:9999".to_string()));
    assert_eq!(cfg.max_connections, Some(42));
    assert!(cfg.check_only);
}

#[test]
fn server_config_no_tcp_flag() {
    let args = vec![
        "--no-tcp".to_string(),
        "--unix-socket".to_string(),
        "test.sock".to_string(),
    ];
    let cfg = ServerConfig::from_args(args).unwrap();
    assert!(cfg.tcp_bind.is_none());
    assert_eq!(cfg.unix_socket, Some(PathBuf::from("test.sock")));
}

#[test]
fn server_config_missing_payload_args() {
    assert!(ServerConfig::from_args(vec!["--data-dir".to_string()]).is_err());
    assert!(ServerConfig::from_args(vec!["--tcp-bind".to_string()]).is_err());
    assert!(ServerConfig::from_args(vec!["--unix-socket".to_string()]).is_err());
    assert!(ServerConfig::from_args(vec!["--max-connections".to_string()]).is_err());
}

#[test]
fn server_config_invalid_max_connections_zero() {
    let args = vec!["--max-connections".to_string(), "0".to_string()];
    let res = ServerConfig::from_args(args);
    assert!(res.is_err());
    assert_eq!(res.err().unwrap().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn join_listener_handles_ok_result() {
    let handle = thread::spawn(|| -> io::Result<()> { Ok(()) });
    let res = handle.join().map_err(|_| io::Error::other("panicked"));
    assert!(res.is_ok());
}

#[test]
fn write_error_frame_writes_error_opcode_and_message() {
    let mut buf: Vec<u8> = Vec::new();
    ProtocolFrame::new(ProtocolOpcode::Error, b"boom".to_vec())
        .write_to_stream(&mut buf)
        .expect("write should succeed");
    let mut cursor = Cursor::new(buf);
    let frame = ProtocolFrame::read_from_stream(&mut cursor).expect("frame should parse");
    assert_eq!(frame.opcode, ProtocolOpcode::Error);
    assert!(String::from_utf8_lossy(&frame.payload).contains("boom"));
}

#[test]
fn server_config_from_args_unknown_arg_errors() {
    let args = vec!["--no-such-arg".to_string()];
    let res = ServerConfig::from_args(args);
    assert!(res.is_err());
    assert_eq!(res.err().unwrap().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn server_config_parse_max_connections_invalid_value() {
    let args = vec!["--max-connections".to_string(), "not-a-number".to_string()];
    let res = ServerConfig::from_args(args);
    assert!(res.is_err());
    assert_eq!(res.err().unwrap().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn server_config_unix_socket_requires_path() {
    let args = vec!["--unix-socket".to_string()];
    let res = ServerConfig::from_args(args);
    assert!(res.is_err());
    assert_eq!(res.err().unwrap().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn test_print_help_execution() {
    print_help();
}
