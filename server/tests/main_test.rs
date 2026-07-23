use serde_json::json;
use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wardrobe_core::{
    AlterRequest, Command, CommandResult, CompactRequest, CreateRequest, CreateResult,
    DurabilityPolicy, InspectResult, OperationFilter, OperationOptions, PermissionRequest,
    ProtocolFrame, ProtocolOpcode, ReadResult, StatusRequest, StorageCoordinate, WardrobeClient,
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

fn read_records(client: &WardrobeClient, filter: OperationFilter) -> Vec<serde_json::Value> {
    match client
        .read(filter, None::<OperationOptions>)
        .expect("read should route")
    {
        ReadResult::Records(records) => records,
        other => panic!("expected records, got {other:?}"),
    }
}

fn engine_read_records(engine: &WardrobeEngine, filter: OperationFilter) -> Vec<serde_json::Value> {
    match engine
        .read(filter, None::<OperationOptions>)
        .expect("read should succeed")
    {
        ReadResult::Records(records) => records,
        other => panic!("expected records, got {other:?}"),
    }
}

fn status_storage(client: &WardrobeClient) -> wardrobe_core::StorageDiagnosis {
    client
        .status(StatusRequest::storage())
        .expect("status storage")
}

fn status_check(client: &WardrobeClient, path: &str) -> wardrobe_core::CheckReport {
    client
        .status(StatusRequest::path(path))
        .expect("status path")
}

fn status_drawer_names(client: &WardrobeClient) -> Vec<String> {
    client
        .status(StatusRequest::drawer_names())
        .expect("status drawer names")
}

fn admin_result(result: CreateResult) -> serde_json::Value {
    match result {
        CreateResult::Admin(payload) => payload,
        other => panic!("expected admin result, got {other:?}"),
    }
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
fn daemon_check_file_logging_writes_application_log_without_terminal() {
    let storage_directory = temp_storage_directory("check_file_logging");
    let log_path = storage_directory.join("logs").join("wardrobe.log");

    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_wardrobe-server"))
        .arg("--data-dir")
        .arg(&storage_directory)
        .arg("--check")
        .arg("--log-level")
        .arg("info")
        .arg("--log-format")
        .arg("json")
        .arg("--log-destination")
        .arg("file")
        .arg("--log-file")
        .arg(&log_path)
        .output()
        .expect("server binary should run");

    assert!(output.status.success());
    let contents = std::fs::read_to_string(&log_path).expect("log file should be readable");
    assert!(contents.contains("\"target\":\"wardrobe_server\""));
    assert!(contents.contains("\"message\":\"config_loaded\""));
    assert!(contents.contains("\"message\":\"startup_complete\""));

    let _ = std::fs::remove_dir_all(storage_directory);
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
            json!({
                "_id": "server_fire",
                "element": "Fire"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("upsert should route through server");
    assert_eq!(pointer, vec!["@gem:server_fire".to_string()]);

    assert_eq!(
        client
            .count(
                OperationFilter::query_in("gem", json!({"element": "Fire"})),
                None::<OperationOptions>
            )
            .expect("count should route through server"),
        1
    );
    let plant_type_id = "fab3d886c9094b61bd6cbd1806daac0e";
    client
        .create(CreateRequest::database("nispuk"))
        .expect("remote database should create");
    client
        .create(CreateRequest::schema("nispuk", "default"))
        .expect("remote schema should create");
    client
        .create(CreateRequest::drawer("nispuk", "default", "plant_types"))
        .expect("remote plant types drawer should create");
    client
        .create(CreateRequest::drawer("nispuk", "default", "plants"))
        .expect("remote plants drawer should create");
    client
        .alter(AlterRequest::schema_rule(
            "nispuk/default/plants",
            "add",
            "relationship",
            "plantType",
            json!({
                "type": "M:1",
                "target_drawer": "nispuk/default/plant_types"
            }),
        ))
        .expect("remote plant type relationship should create");
    client
        .upsert(
            json!({
                "_id": plant_type_id,
                "name": "Aloha Mix",
                "scientificName": "Tropacolum majus",
                "category": "flower",
                "variety": "Hummingbird Nasturtium"
            }),
            OperationFilter::drawer("nispuk/default/plant_types"),
            None::<OperationOptions>,
        )
        .expect("remote plant type should upsert");
    client
        .upsert(
            json!({
                "_id": "c21b0f6f-b6cb-4a34-a72b-c39568a7e0c5",
                "plantType": {
                    "_id": plant_type_id
                },
                "bed": "1",
                "quantity": 1
            }),
            OperationFilter::drawer("nispuk/default/plants"),
            None::<OperationOptions>,
        )
        .expect("remote plant should upsert");

    let plants = read_records(&client, OperationFilter::drawer("nispuk/default/plants"));
    assert_eq!(plants.len(), 1);
    assert_eq!(plants[0]["plantType"]["name"], "Aloha Mix");
    assert_eq!(
        plants[0]["plantType"]["_id"],
        "fab3d886c9094b61bd6cbd1806daac0e"
    );

    drop(client);
    server
        .join()
        .expect("server thread should finish")
        .expect("server should finish cleanly");

    let records = engine_read_records(&engine, OperationFilter::drawer("gem"));
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
            payload: json!({ "_id": "first", "element": "Fire" }),
            filter: OperationFilter::drawer("gem"),
            options: OperationOptions::default(),
        },
    );
    assert!(matches!(read_result(&mut first), CommandResult::Upsert(_)));

    first
        .shutdown(Shutdown::Both)
        .expect("first stream should close");

    write_command(
        &mut second,
        &Command::Upsert {
            payload: json!({ "_id": "second", "element": "Water" }),
            filter: OperationFilter::drawer("gem"),
            options: OperationOptions::default(),
        },
    );
    assert!(matches!(read_result(&mut second), CommandResult::Upsert(_)));
    second
        .shutdown(Shutdown::Both)
        .expect("second stream should close");

    server
        .join()
        .expect("server thread should finish")
        .expect("server should finish cleanly");

    assert_eq!(
        engine
            .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
            .expect("gem count"),
        2
    );
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
            .upsert(
                json!({ "_id": "client_one", "element": "Air" }),
                OperationFilter::drawer("gem"),
                None::<OperationOptions>,
            )
            .expect("first client should upsert");
    });
    let second = thread::spawn(move || {
        let client =
            WardrobeClient::open(format!("wardrobe://{address}")).expect("client should connect");
        client
            .upsert(
                json!({ "_id": "client_two", "name": "Blade" }),
                OperationFilter::drawer("weapon"),
                None::<OperationOptions>,
            )
            .expect("second client should upsert");
    });

    first.join().expect("first client should finish");
    second.join().expect("second client should finish");
    server
        .join()
        .expect("server thread should finish")
        .expect("server should finish cleanly");

    assert_eq!(
        engine
            .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
            .expect("gem count"),
        1
    );
    assert_eq!(
        engine
            .count(OperationFilter::drawer("weapon"), None::<OperationOptions>)
            .expect("weapon count"),
        1
    );
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
            payload: json!({
                "_id": "scoped_fire",
                "element": "Fire"
            }),
            filter: OperationFilter::drawer("gem"),
            options: OperationOptions::default(),
        }),
    };
    let vacuum = Command::Execute {
        coordinate,
        command: Box::new(Command::Compact(CompactRequest::drawer("gem"))),
    };

    write_command(&mut stream, &upsert);
    assert!(matches!(read_result(&mut stream), CommandResult::Upsert(_)));
    write_command(&mut stream, &vacuum);
    assert!(matches!(
        read_result(&mut stream),
        CommandResult::Compact(_)
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
        .create(CreateRequest::database("armory"))
        .expect("database should be created remotely");
    client
        .create(CreateRequest::schema("armory", "public"))
        .expect("schema should be created remotely");
    client
        .create(CreateRequest::drawer("armory", "public", "gem"))
        .expect("drawer should be created remotely");

    client
        .upsert(
            json!({
                "_id": "server_matrix",
                "element": "Light"
            }),
            OperationFilter::drawer("armory/public/gem"),
            None::<OperationOptions>,
        )
        .expect("upsert should route remotely");
    assert_eq!(
        client
            .count(
                OperationFilter::query_in("armory/public/gem", json!({"element": "Light"})),
                None::<OperationOptions>
            )
            .expect("count should route remotely"),
        1
    );
    assert_eq!(
        read_records(&client, OperationFilter::drawer("armory/public/gem")).len(),
        1
    );

    let inspection = client
        .inspect(
            OperationFilter::drawer("armory/public/gem"),
            None::<OperationOptions>,
        )
        .expect("inspect should route remotely");
    let InspectResult::Drawer(inspection) = inspection else {
        panic!("expected drawer inspection result");
    };
    assert_eq!(inspection.path, "armory/public/gem");
    assert_eq!(inspection.record_count, 1);

    let check = status_check(&client, "armory/public/gem");
    assert_eq!(check.kind, "drawer");
    assert!(
        check
            .entries
            .iter()
            .any(|entry| entry.label == "data" && entry.exists)
    );

    let vacuum = client
        .compact(CompactRequest::drawer("armory/public/gem"))
        .expect("clean/vacuum should route remotely");
    assert!(vacuum.data_bytes_after <= vacuum.data_bytes_before);

    let archive = client
        .backup("armory/public/gem")
        .expect("backup should route remotely");
    assert_eq!(archive.scope, "drawer");
    assert!(!archive.files.is_empty());

    let restore = client
        .restore("armory/public/gem_copy", archive)
        .expect("restore should route remotely");
    assert_eq!(restore.destination_path, "armory/public/gem_copy");
    assert_eq!(
        client
            .count(
                OperationFilter::drawer("armory/public/gem_copy"),
                None::<OperationOptions>
            )
            .expect("restored drawer should be queryable remotely"),
        1
    );

    let diagnosis = status_storage(&client);
    assert!(diagnosis.drawer_count >= 2);
    let drawer_names = status_drawer_names(&client);
    assert!(drawer_names.iter().any(|name| name == "armory/public/gem"));

    let add_user = client
        .create(CreateRequest::user(
            json!({"username": "dev_admin", "role": "operator"}),
        ))
        .expect("add user should route remotely");
    assert_eq!(admin_result(add_user)["ok"], true);
    let grant = client
        .grant(PermissionRequest::new("dev_admin", "armory/public:rud"))
        .expect("grant permission should route remotely");
    assert_eq!(grant["permission_scope"], "armory/public:rud");
    let revoke = client
        .revoke(PermissionRequest::new("dev_admin", "armory/public:d"))
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
            json!({ "_id": "after_bad_frame", "element": "Water" }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("healthy client should still succeed");
    drop(client);

    server
        .join()
        .expect("server thread should finish")
        .expect("server should finish cleanly");
    assert_eq!(
        engine
            .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
            .expect("gem count"),
        1
    );
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
        max_cached_drawers: None,
        wal_checkpoint_size_bytes: 1024 * 1024,
        wal_checkpoint_ops: 1000,
        durability_policy: DurabilityPolicy::Strict,
        profile_commands: false,
        logging: wardrobe_core::ApplicationLoggingConfig::default(),
        security: wardrobe_core::SecurityConfig::default(),
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
        max_cached_drawers: None,
        wal_checkpoint_size_bytes: 1024 * 1024,
        wal_checkpoint_ops: 1000,
        durability_policy: DurabilityPolicy::Strict,
        profile_commands: false,
        logging: wardrobe_core::ApplicationLoggingConfig::default(),
        security: wardrobe_core::SecurityConfig::default(),
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
        max_cached_drawers: None,
        wal_checkpoint_size_bytes: 1024 * 1024,
        wal_checkpoint_ops: 1000,
        durability_policy: DurabilityPolicy::Strict,
        profile_commands: false,
        logging: wardrobe_core::ApplicationLoggingConfig::default(),
        security: wardrobe_core::SecurityConfig::default(),
    };
    assert!(wardrobe_server::run(cfg).is_err());
}
