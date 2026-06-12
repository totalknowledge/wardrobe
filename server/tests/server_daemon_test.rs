use serde_json::json;
use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_core::{
    Command, CommandResult, ProtocolFrame, ProtocolOpcode, StorageCoordinate, WardrobeClient,
    WardrobeEngine,
};
use wardrobe_server::serve_tcp_listener;

fn temp_storage_directory(test_name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("wardrobe_server_{test_name}_{nanos}"))
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
        thread::spawn(move || serve_tcp_listener(listener, engine, Some(1)))
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
fn tcp_daemon_handles_multiple_clients_concurrently() {
    let storage_directory = temp_storage_directory("tcp_handles_multiple_clients");
    let engine =
        Arc::new(WardrobeEngine::open(storage_directory.to_string_lossy().as_ref()).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener.local_addr().expect("listener address should read");
    let server = {
        let engine = Arc::clone(&engine);
        thread::spawn(move || serve_tcp_listener(listener, engine, Some(2)))
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
        thread::spawn(move || serve_tcp_listener(listener, engine, Some(1)))
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
fn malformed_frame_drops_only_that_client_channel() {
    let storage_directory = temp_storage_directory("malformed_frame_isolated");
    let engine =
        Arc::new(WardrobeEngine::open(storage_directory.to_string_lossy().as_ref()).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener.local_addr().expect("listener address should read");
    let server = {
        let engine = Arc::clone(&engine);
        thread::spawn(move || serve_tcp_listener(listener, engine, Some(2)))
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
