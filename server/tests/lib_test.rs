use std::fs;
use std::io;
use std::io::Cursor;
use std::path::PathBuf;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_embedded::{
    ApplicationLogDestination, ApplicationLogFormat, ApplicationLogLevel, DurabilityPolicy,
    ProtocolFrame, ProtocolOpcode,
};
use wardrobe_server::{ServerConfig, print_help};

fn temp_config_file(test_name: &str, contents: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("wardrobe_server_{test_name}_{nanos}.toml"));
    fs::write(&path, contents).expect("config fixture should write");
    path
}

#[test]
fn server_config_from_args_defaults() {
    let cfg = ServerConfig::from_args(Vec::<String>::new()).expect("should parse defaults");
    assert_eq!(cfg.data_dir, "./wardrobe");
    assert_eq!(cfg.tcp_bind.unwrap().starts_with("127.0.0.1"), true);
    assert!(!cfg.check_only);
    assert_eq!(cfg.logging.level, ApplicationLogLevel::Off);
    assert_eq!(cfg.logging.destination, ApplicationLogDestination::Stderr);
}

#[test]
fn server_config_explicit_valid_flags() {
    let args = vec![
        "--data-dir".to_string(),
        "./alt_dir".to_string(),
        "--tcp-bind".to_string(),
        "127.0.0.1:9999".to_string(),
        "--connection-pool-limit".to_string(),
        "42".to_string(),
        "--check".to_string(),
    ];
    let cfg = ServerConfig::from_args(args).unwrap();
    assert_eq!(cfg.data_dir, "./alt_dir");
    assert_eq!(cfg.tcp_bind, Some("127.0.0.1:9999".to_string()));
    assert_eq!(cfg.connection_pool_limit, Some(42));
    assert!(cfg.check_only);
}

#[test]
fn server_config_application_logging_flags() {
    let args = vec![
        "--log-level".to_string(),
        "info".to_string(),
        "--log-format".to_string(),
        "json".to_string(),
        "--log-destination".to_string(),
        "file".to_string(),
        "--log-file".to_string(),
        "logs/wardrobe.log".to_string(),
    ];
    let cfg = ServerConfig::from_args(args).unwrap();

    assert_eq!(cfg.logging.level, ApplicationLogLevel::Info);
    assert_eq!(cfg.logging.format, ApplicationLogFormat::Json);
    assert_eq!(cfg.logging.destination, ApplicationLogDestination::File);
    assert_eq!(cfg.logging.file, Some(PathBuf::from("logs/wardrobe.log")));
}

#[test]
fn server_config_loads_first_positional_toml_file() {
    let path = temp_config_file(
        "positional",
        r#"
        [data]
        directory = "./from_config"

        [network]
        tcp_bind = "127.0.0.1:3333"

        [cache]
        max_cached_drawers = 7

        [wal]
        durability = "grouped"
        group_commit_window_ms = 12
        group_commit_max_batch = 34
        checkpoint_size_bytes = 4096
        checkpoint_ops = 5

        [logging]
        level = "warn"
        format = "json"
        destination = "stderr"
        "#,
    );

    let cfg = ServerConfig::from_args(vec![path.to_string_lossy().to_string()])
        .expect("positional config should parse");
    let _ = fs::remove_file(path);

    assert_eq!(cfg.data_dir, "./from_config");
    assert_eq!(cfg.tcp_bind, Some("127.0.0.1:3333".to_string()));
    assert_eq!(cfg.max_cached_drawers, Some(7));
    assert_eq!(cfg.wal_checkpoint_size_bytes, 4096);
    assert_eq!(cfg.wal_checkpoint_ops, 5);
    assert_eq!(cfg.logging.level, ApplicationLogLevel::Warn);
    assert_eq!(cfg.logging.format, ApplicationLogFormat::Json);
    assert_eq!(
        cfg.durability_policy,
        DurabilityPolicy::Grouped {
            commit_window_ms: 12,
            max_batch_size: 34
        }
    );
}

#[test]
fn server_config_flag_loads_file_and_cli_overrides_win() {
    let path = temp_config_file(
        "override",
        r#"
        [data]
        directory = "./from_config"

        [network]
        tcp_bind = "127.0.0.1:3333"

        [cache]
        max_cached_drawers = 7

        [wal]
        checkpoint_size_bytes = 4096
        checkpoint_ops = 5

        [logging]
        level = "warn"
        "#,
    );

    let cfg = ServerConfig::from_args(vec![
        "--config".to_string(),
        path.to_string_lossy().to_string(),
        "--data-dir".to_string(),
        "./from_cli".to_string(),
        "--tcp-bind".to_string(),
        "127.0.0.1:4444".to_string(),
        "--max-cached-drawers".to_string(),
        "9".to_string(),
        "--wal-checkpoint-ops".to_string(),
        "8".to_string(),
        "--log-level".to_string(),
        "error".to_string(),
    ])
    .expect("config and overrides should parse");
    let _ = fs::remove_file(path);

    assert_eq!(cfg.data_dir, "./from_cli");
    assert_eq!(cfg.tcp_bind, Some("127.0.0.1:4444".to_string()));
    assert_eq!(cfg.max_cached_drawers, Some(9));
    assert_eq!(cfg.wal_checkpoint_size_bytes, 4096);
    assert_eq!(cfg.wal_checkpoint_ops, 8);
    assert_eq!(cfg.logging.level, ApplicationLogLevel::Error);
}

#[test]
fn server_config_rejects_file_with_no_enabled_listener() {
    let path = temp_config_file(
        "no_listener",
        r#"
        [network]
        tcp_enabled = false
        unix_socket_enabled = false
        "#,
    );

    let result = ServerConfig::from_args(vec![path.to_string_lossy().to_string()]);
    let _ = fs::remove_file(path);

    assert!(result.is_err());
    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidInput);
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
    assert!(ServerConfig::from_args(vec!["--connection-pool-limit".to_string()]).is_err());
    assert!(ServerConfig::from_args(vec!["--log-level".to_string()]).is_err());
    assert!(ServerConfig::from_args(vec!["--log-format".to_string()]).is_err());
    assert!(ServerConfig::from_args(vec!["--log-destination".to_string()]).is_err());
    assert!(ServerConfig::from_args(vec!["--log-file".to_string()]).is_err());
}

#[test]
fn server_config_invalid_connection_pool_limit_zero() {
    let args = vec!["--connection-pool-limit".to_string(), "0".to_string()];
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
fn server_config_rejects_removed_max_connections_flag() {
    let args = vec!["--max-connections".to_string(), "7".to_string()];
    let res = ServerConfig::from_args(args);
    assert!(res.is_err());
    assert_eq!(res.err().unwrap().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn server_config_parse_connection_pool_limit_invalid_value() {
    let args = vec![
        "--connection-pool-limit".to_string(),
        "not-a-number".to_string(),
    ];
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
