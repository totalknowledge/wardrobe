mod common;

use common::TempDatabase;
use serde_json::json;
use std::net::TcpListener;
use std::path::Path;
use std::thread::{self, JoinHandle};
use wardrobe_core::{
    Command, CommandResult, DriverKind, OrderDirection, ProtocolFrame, ProtocolOpcode,
    QueryModifiers, StorageInventory, VacuumReport, WardrobeClient,
};

#[cfg(unix)]
use std::os::unix::net::UnixListener;

#[cfg(not(unix))]
use std::io::ErrorKind;

fn spawn_tcp_protocol_server(script: Vec<(Command, CommandResult)>) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("tcp listener should bind");
    let address = listener
        .local_addr()
        .expect("tcp listener address should read")
        .to_string();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("tcp client should connect");
        run_protocol_script(&mut stream, script);
    });

    (format!("wardrobe://{address}"), handle)
}

fn run_protocol_script<S>(stream: &mut S, script: Vec<(Command, CommandResult)>)
where
    S: std::io::Read + std::io::Write,
{
    for (expected_command, result) in script {
        let request = ProtocolFrame::read_from_stream(stream).expect("request frame should decode");
        assert_eq!(request.opcode, ProtocolOpcode::Command);
        let command: Command =
            serde_json::from_slice(&request.payload).expect("command should deserialize");
        assert_eq!(command, expected_command);

        let payload = serde_json::to_vec(&result).expect("result should serialize");
        ProtocolFrame::new(ProtocolOpcode::Result, payload)
            .write_to_stream(stream)
            .expect("result frame should write");
    }
}

#[test]
fn client_direct_disk_path_delegates_to_embedded_engine() {
    let database = TempDatabase::new("client_direct_path_embedded");
    let connection = database.path.to_string_lossy().into_owned();
    let client = WardrobeClient::open(&connection).expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::Embedded);
    assert!(client.requires_embedded_engine());
    assert!(!client.uses_socket_transport());

    let pointer = client
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_client_fire",
                "element": "Fire"
            }),
        )
        .expect("embedded upsert should delegate to engine");
    assert_eq!(pointer, "@gem:client_fire");

    let records = client.find_all("gem").expect("records should read");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["element"], "Fire");
}

#[test]
fn client_local_uri_delegates_to_embedded_engine() {
    let database = TempDatabase::new("client_local_uri_embedded");
    let connection = format!("wardrobe://local/{}", database.path.display());
    let client = WardrobeClient::open(&connection).expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::Embedded);
    assert!(client.requires_embedded_engine());
    assert!(!client.uses_socket_transport());
}

#[test]
fn client_file_uri_delegates_to_embedded_engine() {
    let database = TempDatabase::new("client_file_uri_embedded");
    let connection = format!("wardrobe+file://{}", database.path.display());
    let client = WardrobeClient::open(&connection).expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::Embedded);
    assert!(client.requires_embedded_engine());
    assert!(!client.uses_socket_transport());

    client
        .upsert(
            "gem",
            json!({
                "_id": "client_file_uri_gem",
                "element": "Water"
            }),
        )
        .expect("embedded file-uri upsert should write locally");

    assert!(database.path.join("gem.drw").is_file());
}

#[test]
fn client_network_driver_does_not_initialize_local_storage() {
    let accidental_path = Path::new("localhost:24842");
    assert!(!accidental_path.exists());
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::FindAll {
            drawer_name: "gem".to_string(),
        },
        CommandResult::Records(Vec::new()),
    )]);

    let client = WardrobeClient::open(connection).expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::Network);
    assert!(!accidental_path.exists());
    assert!(
        client
            .find_all("gem")
            .expect("find all should round trip")
            .is_empty()
    );
    handle.join().expect("protocol server should finish");
}

#[test]
fn client_network_driver_sends_commands_and_unpacks_results() {
    let report = VacuumReport {
        records_rewritten: 1,
        data_bytes_before: 100,
        data_bytes_after: 64,
        index_bytes_before: 50,
        index_bytes_after: 24,
        bytes_reclaimed: 62,
    };
    let modifiers = Some(QueryModifiers {
        order_by: Some("element".to_string()),
        order_direction: Some(OrderDirection::Ascending),
        limit: Some(10),
        offset: Some(0),
    });
    let (connection, handle) = spawn_tcp_protocol_server(vec![
        (
            Command::Upsert {
                drawer_name: "gem".to_string(),
                payload: json!({"_id": "network_fire", "element": "Fire"}),
            },
            CommandResult::Pointer("@gem:network_fire".to_string()),
        ),
        (
            Command::FindAll {
                drawer_name: "gem".to_string(),
            },
            CommandResult::Records(vec![json!({"element": "Fire"})]),
        ),
        (
            Command::FindByFilter {
                drawer_name: "gem".to_string(),
                filter: json!({"element": "F%"}),
                modifiers: modifiers.clone(),
            },
            CommandResult::Records(vec![json!({"element": "Fire"})]),
        ),
        (
            Command::Count {
                drawer_name: "gem".to_string(),
                filter: Some(json!({"element": "F%"})),
                modifiers: None,
            },
            CommandResult::Count(1),
        ),
        (
            Command::FindById {
                pointer: "@gem:network_fire".to_string(),
            },
            CommandResult::Record(Some(json!({"element": "Fire"}))),
        ),
        (
            Command::Delete {
                pointer: "@gem:network_fire".to_string(),
            },
            CommandResult::Deleted(true),
        ),
        (
            Command::Delete {
                pointer: "@gem:explicit_delete".to_string(),
            },
            CommandResult::Deleted(true),
        ),
        (
            Command::Vacuum {
                drawer_name: "gem".to_string(),
            },
            CommandResult::Vacuumed(report.clone()),
        ),
        (
            Command::Migrate {
                drawer_name: "gem".to_string(),
            },
            CommandResult::Migrated(report.clone()),
        ),
        (
            Command::ShowTenants,
            CommandResult::Tenants(vec!["tenant_alpha".to_string()]),
        ),
        (
            Command::ShowDatabases,
            CommandResult::Databases(vec![StorageInventory {
                name: "main_db".to_string(),
                record_count: 3,
                disk_size_bytes: 4096,
                register_file_count: 7,
            }]),
        ),
        (
            Command::ShowSchemas {
                database_name: "main_db".to_string(),
            },
            CommandResult::Schemas(vec!["tenant_schema".to_string()]),
        ),
    ]);

    let client = WardrobeClient::open(connection).expect("client should open");
    assert_eq!(client.driver_kind(), DriverKind::Network);
    assert!(!client.requires_embedded_engine());
    assert!(client.uses_socket_transport());

    assert_eq!(
        client
            .upsert("gem", json!({"_id": "network_fire", "element": "Fire"}))
            .expect("upsert should round trip"),
        "@gem:network_fire"
    );
    assert_eq!(
        client.find_all("gem").expect("find all should round trip"),
        vec![json!({"element": "Fire"})]
    );
    assert_eq!(
        client
            .find_by_filter("gem", json!({"element": "F%"}), modifiers)
            .expect("filter should round trip"),
        vec![json!({"element": "Fire"})]
    );
    assert_eq!(
        client
            .count("gem", Some(json!({"element": "F%"})), None)
            .expect("count should round trip"),
        1
    );
    assert_eq!(
        client
            .find_by_id("@gem:network_fire")
            .expect("find by id should round trip"),
        Some(json!({"element": "Fire"}))
    );
    assert!(
        client
            .delete_by_id("@gem:network_fire")
            .expect("delete by id should round trip")
    );
    assert!(
        client
            .delete(("gem", "lnk_explicit_delete"))
            .expect("explicit delete should round trip")
    );
    assert_eq!(
        client
            .vacuum_drawer("gem")
            .expect("vacuum should round trip"),
        report
    );
    assert_eq!(
        client
            .migrate_drawer("gem")
            .expect("migration should round trip"),
        report
    );
    assert_eq!(
        client.show_tenants().expect("tenants should round trip"),
        vec!["tenant_alpha".to_string()]
    );
    assert_eq!(
        client
            .show_databases()
            .expect("databases should round trip"),
        vec![StorageInventory {
            name: "main_db".to_string(),
            record_count: 3,
            disk_size_bytes: 4096,
            register_file_count: 7,
        }]
    );
    assert_eq!(
        client
            .show_schemas("main_db")
            .expect("schemas should round trip"),
        vec!["tenant_schema".to_string()]
    );

    handle.join().expect("protocol server should finish");
}

#[cfg(not(unix))]
#[test]
fn client_unix_socket_driver_reports_unsupported_on_non_unix() {
    match WardrobeClient::open("wardrobe://unix/tmp/wardrobe.sock") {
        Ok(_) => panic!("unix sockets should be unsupported on this platform"),
        Err(error) => assert_eq!(error.kind(), ErrorKind::Unsupported),
    }
}

#[cfg(unix)]
#[test]
fn client_unix_socket_driver_sends_command_frames() {
    let database = TempDatabase::new("client_unix_socket_protocol");
    std::fs::create_dir_all(&database.path).expect("test temp directory should be created");
    let socket_path = database.path.join("wardrobe.sock");
    let listener = UnixListener::bind(&socket_path).expect("unix listener should bind");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("unix client should connect");
        run_protocol_script(
            &mut stream,
            vec![(
                Command::Count {
                    drawer_name: "gem".to_string(),
                    filter: None,
                    modifiers: None,
                },
                CommandResult::Count(7),
            )],
        );
    });

    let client = WardrobeClient::open(format!("wardrobe+unix://{}", socket_path.display()))
        .expect("client should open");

    assert_eq!(client.driver_kind(), DriverKind::UnixSocket);
    assert!(!client.requires_embedded_engine());
    assert!(client.uses_socket_transport());
    assert_eq!(
        client
            .count("gem", None, None)
            .expect("count should round trip"),
        7
    );
    handle.join().expect("protocol server should finish");
}
