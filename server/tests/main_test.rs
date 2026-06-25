use serde_json::json;
use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wardrobe_core::{
    Command, CommandResult, ProtocolFrame, ProtocolOpcode, StorageCoordinate, WardrobeClient,
    WardrobeEngine,
};
use wardrobe_server::{ServerConfig, serve_tcp_listener};

fn temp_storage_directory(test_name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("wardrobe_server_{test_name}_{nanos}"))
}

fn spawn_test_tcp_server(
    listener: TcpListener,
    engine: Arc<WardrobeEngine>,
    connection_pool_limit: Option<usize>,
) -> JoinHandle<std::io::Result<()>> {
    listener
        .set_nonblocking(true)
        .expect("test listener should switch to nonblocking mode");
    thread::spawn(move || serve_tcp_listener(listener, engine, connection_pool_limit))
}

fn write_command(stream: &mut TcpStream, command: &Command) {
    let payload = serde_json::to_vec(command).expect("command should serialize");
    ProtocolFrame::new(ProtocolOpcode::Command, payload)
        .write_to_stream(stream)
        .expect("command frame should write");
}

fn read_result(stream: &mut TcpStream) -> CommandResult {
    let response = ProtocolFrame::read_from_stream(stream).expect("result frame should read");
    assert_eq!(response.opcode, ProtocolOpcode::Result);
    serde_json::from_slice(&response.payload).expect("result should deserialize")
}

#[test]
fn daemon_check_initializes_storage_directory_and_exits() {
    let storage_directory = temp_storage_directory("check_initializes_storage");

    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_wardrobe-server"))
        .arg("--data-dir")
        .arg(&storage_directory)
        .arg("--check")
        .output()
        .expect("server binary should run");

    assert!(output.status.success());
    assert!(storage_directory.is_dir());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Wardrobe daemon initialized"));
    assert!(stdout.contains("Wardrobe daemon check completed"));

    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn daemon_check_rejects_missing_data_dir_argument() {
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_wardrobe-server"))
        .arg("--data-dir")
        .output()
        .expect("server binary should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--data-dir requires a directory path"));
}

#[test]
fn tcp_daemon_routes_client_commands_to_shared_engine() {
    let storage_directory = temp_storage_directory("tcp_routes_client_commands");
    let engine =
        Arc::new(WardrobeEngine::open(storage_directory.to_string_lossy().as_ref()).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener.local_addr().expect("listener address should read");
    let server = {
        let engine = Arc::clone(&engine);
        spawn_test_tcp_server(listener, engine, Some(1))
    };

    let client =
        WardrobeClient::open(format!("wardrobe://{address}")).expect("client should connect");
    let pointer = client
        .upsert(
            "gem",
            json!({
                "_id": "server_fire",
                "element": "Fire"
            }),
        )
        .expect("upsert should route through server");
    assert_eq!(pointer, "@gem:server_fire");

    assert_eq!(
        client
            .count("gem", Some(json!({"element": "Fire"})), None)
            .expect("count should route through server"),
        1
    );

    drop(client);
    server
        .join()
        .expect("server thread should finish")
        .expect("server should finish cleanly");

    let records = engine.find_all("gem").expect("records should read");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["element"], "Fire");
    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn tcp_connection_pool_limit_does_not_terminate_listener() {
    let storage_directory = temp_storage_directory("tcp_pool_limit_keeps_listener_alive");
    let engine =
        Arc::new(WardrobeEngine::open(storage_directory.to_string_lossy().as_ref()).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener.local_addr().expect("listener address should read");
    let server = {
        let engine = Arc::clone(&engine);
        spawn_test_tcp_server(listener, engine, Some(1))
    };

    let mut first = TcpStream::connect(address).expect("first client should connect");
    let mut second = TcpStream::connect(address).expect("second client should queue");
    second
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("second client read timeout should be configured");

    write_command(
        &mut first,
        &Command::Upsert {
            drawer_name: "gem".to_string(),
            payload: json!({ "_id": "first", "element": "Fire" }),
        },
    );
    assert!(matches!(read_result(&mut first), CommandResult::Pointer(_)));

    first
        .shutdown(Shutdown::Both)
        .expect("first stream should close");

    write_command(
        &mut second,
        &Command::Upsert {
            drawer_name: "gem".to_string(),
            payload: json!({ "_id": "second", "element": "Water" }),
        },
    );
    assert!(matches!(
        read_result(&mut second),
        CommandResult::Pointer(_)
    ));
    second
        .shutdown(Shutdown::Both)
        .expect("second stream should close");

    server
        .join()
        .expect("server thread should finish")
        .expect("server should finish cleanly");

    assert_eq!(engine.count("gem", None, None).expect("gem count"), 2);
    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn tcp_daemon_handles_multiple_clients_concurrently() {
    let storage_directory = temp_storage_directory("tcp_handles_multiple_clients");
    let engine =
        Arc::new(WardrobeEngine::open(storage_directory.to_string_lossy().as_ref()).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener.local_addr().expect("listener address should read");
    let server = {
        let engine = Arc::clone(&engine);
        spawn_test_tcp_server(listener, engine, Some(2))
    };

    let first = thread::spawn(move || {
        let client =
            WardrobeClient::open(format!("wardrobe://{address}")).expect("client should connect");
        client
            .upsert("gem", json!({ "_id": "client_one", "element": "Air" }))
            .expect("first client should upsert");
    });
    let second = thread::spawn(move || {
        let client =
            WardrobeClient::open(format!("wardrobe://{address}")).expect("client should connect");
        client
            .upsert("weapon", json!({ "_id": "client_two", "name": "Blade" }))
            .expect("second client should upsert");
    });

    first.join().expect("first client should finish");
    second.join().expect("second client should finish");
    server
        .join()
        .expect("server thread should finish")
        .expect("server should finish cleanly");

    assert_eq!(engine.count("gem", None, None).expect("gem count"), 1);
    assert_eq!(engine.count("weapon", None, None).expect("weapon count"), 1);
    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn tcp_daemon_routes_scoped_commands_and_maintenance_frames() {
    let storage_directory = temp_storage_directory("tcp_routes_scoped_commands");
    let engine =
        Arc::new(WardrobeEngine::open(storage_directory.to_string_lossy().as_ref()).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener.local_addr().expect("listener address should read");
    let server = {
        let engine = Arc::clone(&engine);
        spawn_test_tcp_server(listener, engine, Some(1))
    };

    let coordinate = StorageCoordinate::new("tenant_a", "prod", "core");
    let mut stream = TcpStream::connect(address).expect("client should connect");
    let upsert = Command::Execute {
        coordinate: coordinate.clone(),
        command: Box::new(Command::Upsert {
            drawer_name: "gem".to_string(),
            payload: json!({
                "_id": "scoped_fire",
                "element": "Fire"
            }),
        }),
    };
    let vacuum = Command::Execute {
        coordinate,
        command: Box::new(Command::Vacuum {
            drawer_name: "gem".to_string(),
        }),
    };

    write_command(&mut stream, &upsert);
    assert!(matches!(
        read_result(&mut stream),
        CommandResult::Pointer(_)
    ));
    write_command(&mut stream, &vacuum);
    assert!(matches!(
        read_result(&mut stream),
        CommandResult::Vacuumed(_)
    ));
    stream
        .shutdown(Shutdown::Both)
        .expect("client stream should close");

    server
        .join()
        .expect("server thread should finish")
        .expect("server should finish cleanly");

    assert!(
        storage_directory
            .join("tenant_a")
            .join("prod")
            .join("core")
            .join("gem.drw")
            .is_file()
    );
    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn tcp_daemon_routes_full_cli_command_matrix() {
    let storage_directory = temp_storage_directory("tcp_routes_full_cli_command_matrix");
    let engine =
        Arc::new(WardrobeEngine::open(storage_directory.to_string_lossy().as_ref()).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener.local_addr().expect("listener address should read");
    let server = {
        let engine = Arc::clone(&engine);
        spawn_test_tcp_server(listener, engine, Some(1))
    };

    let client =
        WardrobeClient::open(format!("wardrobe://{address}")).expect("client should connect");
    client
        .create_database("armory")
        .expect("database should be created remotely");
    client
        .create_schema("armory", "public")
        .expect("schema should be created remotely");
    client
        .create_drawer("armory", "public", "gem")
        .expect("drawer should be created remotely");

    client
        .upsert(
            "armory/public/gem",
            json!({
                "_id": "server_matrix",
                "element": "Light"
            }),
        )
        .expect("upsert should route remotely");
    assert_eq!(
        client
            .count("armory/public/gem", Some(json!({"element": "Light"})), None)
            .expect("count should route remotely"),
        1
    );
    assert_eq!(
        client
            .find_all("armory/public/gem")
            .expect("records should route remotely")
            .len(),
        1
    );

    let inspection = client
        .inspect_drawer("armory/public/gem")
        .expect("inspect should route remotely");
    assert_eq!(inspection.path, "armory/public/gem");
    assert_eq!(inspection.record_count, 1);

    let check = client
        .check_path("armory/public/gem")
        .expect("check should route remotely");
    assert_eq!(check.kind, "drawer");
    assert!(
        check
            .entries
            .iter()
            .any(|entry| entry.label == "data" && entry.exists)
    );

    let vacuum = client
        .vacuum_drawer("armory/public/gem")
        .expect("clean/vacuum should route remotely");
    assert!(vacuum.data_bytes_after <= vacuum.data_bytes_before);

    let archive = client
        .backup_archive("armory/public/gem")
        .expect("backup should route remotely");
    assert_eq!(archive.scope, "drawer");
    assert!(!archive.files.is_empty());

    let restore = client
        .restore_archive("armory/public/gem_copy", archive)
        .expect("restore should route remotely");
    assert_eq!(restore.destination_path, "armory/public/gem_copy");
    assert_eq!(
        client
            .count("armory/public/gem_copy", None, None)
            .expect("restored drawer should be queryable remotely"),
        1
    );

    let diagnosis = client
        .diagnose_storage()
        .expect("diagnose should route remotely");
    assert!(diagnosis.drawer_count >= 2);
    let drawer_names = client
        .list_drawer_names()
        .expect("drawers should route remotely");
    assert!(drawer_names.iter().any(|name| name == "armory/public/gem"));

    let add_user = client
        .manage_user(
            "add_user",
            json!({"username": "dev_admin", "role": "operator"}),
        )
        .expect("add user should route remotely");
    assert_eq!(add_user["ok"], true);
    let grant = client
        .manage_user(
            "grant_permission",
            json!({"username": "dev_admin", "permission_scope": "armory/public:rud"}),
        )
        .expect("grant permission should route remotely");
    assert_eq!(grant["permission_scope"], "armory/public:rud");
    let revoke = client
        .manage_user(
            "revoke_permission",
            json!({"username": "dev_admin", "permission_scope": "armory/public:d"}),
        )
        .expect("revoke permission should route remotely");
    assert_eq!(revoke["ok"], true);

    drop(client);
    server
        .join()
        .expect("server thread should finish")
        .expect("server should finish cleanly");

    assert!(
        storage_directory
            .join("_wardrobe_access_control.json")
            .is_file()
    );
    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn malformed_frame_drops_only_that_client_channel() {
    let storage_directory = temp_storage_directory("malformed_frame_isolated");
    let engine =
        Arc::new(WardrobeEngine::open(storage_directory.to_string_lossy().as_ref()).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener.local_addr().expect("listener address should read");
    let server = {
        let engine = Arc::clone(&engine);
        spawn_test_tcp_server(listener, engine, Some(2))
    };

    {
        let mut bad_client = TcpStream::connect(address).expect("bad client should connect");
        bad_client
            .write_all(&[0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0x00])
            .expect("bad frame should write");
    }

    let client = WardrobeClient::open(format!("wardrobe://{address}"))
        .expect("healthy client should connect");
    client
        .upsert(
            "gem",
            json!({ "_id": "after_bad_frame", "element": "Water" }),
        )
        .expect("healthy client should still succeed");
    drop(client);

    server
        .join()
        .expect("server thread should finish")
        .expect("server should finish cleanly");
    assert_eq!(engine.count("gem", None, None).expect("gem count"), 1);
    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn tcp_bind_failure_is_reported() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().expect("local addr").port();
    let args = vec!["--tcp-bind".to_string(), format!("127.0.0.1:{}", port)];
    let cfg = ServerConfig::from_args(args).expect("cfg parse");
    let res = wardrobe_server::run(cfg);
    assert!(res.is_err());
}

#[test]
fn handle_protocol_stream_tolerates_unexpected_eof_and_returns_ok() {
    let storage_directory = temp_storage_directory("handle_unexpected_eof");
    let engine =
        Arc::new(WardrobeEngine::open(storage_directory.to_string_lossy().as_ref()).unwrap());
    let partial = vec![0x57u8, 0x52, 0x44];
    let mut cursor = std::io::Cursor::new(partial);
    let res = wardrobe_server::handle_protocol_stream(engine, &mut cursor);
    assert!(res.is_ok());
    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn run_returns_error_when_no_listeners_enabled() {
    let cfg = ServerConfig {
        data_dir: "./wardrobe".to_string(),
        check_only: false,
        tcp_bind: None,
        unix_socket: None,
        connection_pool_limit: None,
        profile_commands: false,
    };
    let res = wardrobe_server::run(cfg);
    assert!(res.is_err());
}

#[test]
fn handle_protocol_stream_writes_error_frame_for_non_command_opcode() {
    let storage_directory = temp_storage_directory("handle_non_command");
    let engine =
        Arc::new(WardrobeEngine::open(storage_directory.to_string_lossy().as_ref()).unwrap());

    let payload = serde_json::to_vec(&CommandResult::Count(1)).expect("ser");
    let mut data = Vec::new();
    ProtocolFrame::new(ProtocolOpcode::Result, payload)
        .write_to_stream(&mut data)
        .expect("write");
    let mut cursor = std::io::Cursor::new(data);

    let res = wardrobe_server::handle_protocol_stream(engine, &mut cursor);
    assert!(res.is_ok());
    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn handle_protocol_stream_deserialization_failure_path() {
    let storage_directory = temp_storage_directory("deserialization_failure");
    let engine =
        Arc::new(WardrobeEngine::open(storage_directory.to_string_lossy().as_ref()).unwrap());

    let mut data = Vec::new();
    ProtocolFrame::new(ProtocolOpcode::Command, b"invalid-json-structure".to_vec())
        .write_to_stream(&mut data)
        .unwrap();
    let mut cursor = std::io::Cursor::new(data);

    assert!(wardrobe_server::handle_protocol_stream(engine, &mut cursor).is_ok());
    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn handle_protocol_stream_io_error_kinds() {
    let storage_directory = temp_storage_directory("io_errors");
    let engine =
        Arc::new(WardrobeEngine::open(storage_directory.to_string_lossy().as_ref()).unwrap());

    let mut data = Vec::new();
    ProtocolFrame::new(ProtocolOpcode::Command, b"".to_vec())
        .write_to_stream(&mut data)
        .unwrap();
    data.truncate(data.len() - 2);
    let mut cursor = std::io::Cursor::new(data);

    assert!(wardrobe_server::handle_protocol_stream(engine, &mut cursor).is_ok());
    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn run_execution_with_check_only_flag() {
    let storage_directory = temp_storage_directory("run_check_only");
    let cfg = ServerConfig {
        data_dir: storage_directory.to_string_lossy().to_string(),
        check_only: true,
        tcp_bind: None,
        unix_socket: None,
        connection_pool_limit: None,
        profile_commands: false,
    };
    assert!(wardrobe_server::run(cfg).is_ok());
    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
#[cfg(not(unix))]
fn run_execution_unsupported_unix_platform_guard() {
    let cfg = ServerConfig {
        data_dir: "./wardrobe".to_string(),
        check_only: false,
        tcp_bind: None,
        unix_socket: Some(std::path::PathBuf::from("unsupported.sock")),
        connection_pool_limit: None,
        profile_commands: false,
    };
    assert!(wardrobe_server::run(cfg).is_err());
}
