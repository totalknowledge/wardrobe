mod common;

use common::TempDatabase;
use serde_json::json;
use std::net::TcpListener;
use std::path::Path;
use std::thread::{self, JoinHandle};
use wardrobe_core::{
    Command, CommandResult, ConnectionTarget, DriverKind, OrderDirection, ProtocolFrame,
    ProtocolOpcode, QueryModifiers, StorageInventory, StorageLocator, VacuumReport, WardrobeClient,
};

#[cfg(unix)]
use std::os::unix::net::UnixListener;

#[cfg(not(unix))]
use std::io::ErrorKind;

fn spawn_tcp_protocol_server(script: Vec<(Command, CommandResult)>) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind failed");
    let address = listener.local_addr().expect("address failed").to_string();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept failed");
        run_protocol_script(&mut stream, script);
    });

    (format!("wardrobe://{address}"), handle)
}

fn run_protocol_script<S>(stream: &mut S, script: Vec<(Command, CommandResult)>)
where
    S: std::io::Read + std::io::Write,
{
    for (expected_command, result) in script {
        let request = ProtocolFrame::read_from_stream(stream).expect("decode failed");
        assert_eq!(request.opcode, ProtocolOpcode::Command);
        let command: Command =
            serde_json::from_slice(&request.payload).expect("deserialize failed");
        assert_eq!(command, expected_command);

        let payload = serde_json::to_vec(&result).expect("serialize failed");
        ProtocolFrame::new(ProtocolOpcode::Result, payload)
            .write_to_stream(stream)
            .expect("write failed");
    }
}

fn spawn_tcp_protocol_server_with_opcode(
    opcode: ProtocolOpcode,
    payload: Vec<u8>,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind failed");
    let address = listener.local_addr().expect("address failed").to_string();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept failed");
        let _ = ProtocolFrame::read_from_stream(&mut stream).expect("read failed");
        ProtocolFrame::new(opcode, payload)
            .write_to_stream(&mut stream)
            .expect("write failed");
    });
    (format!("wardrobe://{address}"), handle)
}

#[test]
fn client_direct_disk_path_delegates_to_embedded_engine() {
    let database = TempDatabase::new("client_direct_path_embedded");
    let connection = database.path.to_string_lossy().into_owned();
    let client = WardrobeClient::open(&connection).expect("open failed");

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
        .expect("upsert failed");
    assert_eq!(pointer, "@gem:client_fire");

    let records = client.find_all("gem").expect("find failed");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["element"], "Fire");
}

#[test]
fn client_embedded_driver_exhaustive_execution() {
    let database = TempDatabase::new("client_embedded_exhaustive");
    let connection = database.path.to_string_lossy().into_owned();
    let client = WardrobeClient::open(&connection).expect("open failed");

    let target = client.connection_target();
    assert!(matches!(target, ConnectionTarget::EmbeddedPath(_)));

    let _ = client.upsert("gem", json!({"_id": "test_id", "element": "Earth"}));
    let _ = client.upsert("gem", json!({"_id": "other_id", "element": "Air"}));

    let filter_results = client
        .find_by_filter("gem", json!({}), None)
        .expect("filter failed");
    assert!(filter_results.len() >= 1);

    let count_value = client.count("gem", None, None).expect("count failed");
    assert!(count_value >= 1);

    let single_record = client
        .find_by_id("@gem:test_id")
        .expect("find_by_id failed");
    assert!(single_record.is_some());

    let vacuum_res = client.vacuum_drawer("gem");
    assert!(vacuum_res.is_ok());

    let migrate_res = client.migrate_drawer("gem");
    assert!(migrate_res.is_ok());

    let tenants = client.show_tenants();
    assert!(tenants.is_ok());

    let databases = client.show_databases();
    assert!(databases.is_ok());

    let schemas = client.show_schemas("main");
    assert!(schemas.is_ok());

    let drawers = client.show_drawers("main", "default");
    assert!(drawers.is_ok());

    let delete_by_id_res = client.delete_by_id("@gem:test_id").expect("delete failed");
    assert!(delete_by_id_res);

    let inline_locator = StorageLocator::Inline("@gem:other_id".to_string());
    let delete_inline_res = client.delete(inline_locator).expect("delete failed");
    assert!(delete_inline_res);

    let explicit_locator = StorageLocator::Explicit {
        drawer: "gem".to_string(),
        id: "missing_id".to_string(),
    };
    let delete_explicit_res = client.delete(explicit_locator).expect("delete failed");
    assert!(!delete_explicit_res);
}

#[test]
fn client_local_uri_delegates_to_embedded_engine() {
    let database = TempDatabase::new("client_local_uri_embedded");
    let connection = format!("wardrobe://local/{}", database.path.display());
    let client = WardrobeClient::open(&connection).expect("open failed");

    assert_eq!(client.driver_kind(), DriverKind::Embedded);
    assert!(client.requires_embedded_engine());
    assert!(!client.uses_socket_transport());
}

#[test]
fn client_file_uri_delegates_to_embedded_engine() {
    let database = TempDatabase::new("client_file_uri_embedded");
    let connection = format!("wardrobe+file://{}", database.path.display());
    let client = WardrobeClient::open(&connection).expect("open failed");

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
        .expect("upsert failed");

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

    let client = WardrobeClient::open(connection).expect("open failed");

    assert_eq!(client.driver_kind(), DriverKind::Network);
    assert!(!accidental_path.exists());
    assert!(client.find_all("gem").expect("find failed").is_empty());
    handle.join().expect("join failed");
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
        (
            Command::ShowDrawers {
                database_name: "main_db".to_string(),
                schema_name: "tenant_schema".to_string(),
            },
            CommandResult::Drawers(vec![StorageInventory {
                name: "gem".to_string(),
                record_count: 2,
                disk_size_bytes: 2048,
                register_file_count: 3,
            }]),
        ),
    ]);

    let client = WardrobeClient::open(connection).expect("open failed");
    assert_eq!(client.driver_kind(), DriverKind::Network);
    assert!(!client.requires_embedded_engine());
    assert!(client.uses_socket_transport());

    assert_eq!(
        client
            .upsert("gem", json!({"_id": "network_fire", "element": "Fire"}))
            .expect("upsert failed"),
        "@gem:network_fire"
    );
    assert_eq!(
        client.find_all("gem").expect("find failed"),
        vec![json!({"element": "Fire"})]
    );
    assert_eq!(
        client
            .find_by_filter("gem", json!({"element": "F%"}), modifiers)
            .expect("filter failed"),
        vec![json!({"element": "Fire"})]
    );
    assert_eq!(
        client
            .count("gem", Some(json!({"element": "F%"})), None)
            .expect("count failed"),
        1
    );
    assert_eq!(
        client.find_by_id("@gem:network_fire").expect("find failed"),
        Some(json!({"element": "Fire"}))
    );
    assert!(
        client
            .delete_by_id("@gem:network_fire")
            .expect("delete failed")
    );
    assert!(
        client
            .delete(("gem", "lnk_explicit_delete"))
            .expect("delete failed")
    );
    assert_eq!(client.vacuum_drawer("gem").expect("vacuum failed"), report);
    assert_eq!(
        client.migrate_drawer("gem").expect("migrate failed"),
        report
    );
    assert_eq!(
        client.show_tenants().expect("tenants failed"),
        vec!["tenant_alpha".to_string()]
    );
    assert_eq!(
        client.show_databases().expect("databases failed"),
        vec![StorageInventory {
            name: "main_db".to_string(),
            record_count: 3,
            disk_size_bytes: 4096,
            register_file_count: 7,
        }]
    );
    assert_eq!(
        client.show_schemas("main_db").expect("schemas failed"),
        vec!["tenant_schema".to_string()]
    );
    assert_eq!(
        client
            .show_drawers("main_db", "tenant_schema")
            .expect("drawers failed"),
        vec![StorageInventory {
            name: "gem".to_string(),
            record_count: 2,
            disk_size_bytes: 2048,
            register_file_count: 3,
        }]
    );

    handle.join().expect("join failed");
}

#[cfg(not(unix))]
#[test]
fn client_unix_socket_driver_reports_unsupported_on_non_unix() {
    match WardrobeClient::open("wardrobe://unix/tmp/wardrobe.sock") {
        Ok(_) => panic!("expected failure"),
        Err(error) => assert_eq!(error.kind(), ErrorKind::Unsupported),
    }
}

#[test]
fn opening_empty_connection_returns_error() {
    match WardrobeClient::open("") {
        Ok(_) => panic!("expected failure"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput),
    }
}

#[cfg(unix)]
#[test]
fn client_unix_socket_driver_sends_command_frames() {
    let database = TempDatabase::new("client_unix_socket_protocol");
    std::fs::create_dir_all(&database.path).expect("dir failed");
    let socket_path = database.path.join("wardrobe.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind failed");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept failed");
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
        .expect("open failed");

    assert_eq!(client.driver_kind(), DriverKind::UnixSocket);
    assert!(!client.requires_embedded_engine());
    assert!(client.uses_socket_transport());
    assert_eq!(client.count("gem", None, None).expect("count failed"), 7);
    handle.join().expect("join failed");
}

#[test]
fn client_unexpected_result_paths_return_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::Count {
            drawer_name: "gem".to_string(),
            filter: None,
            modifiers: None,
        },
        CommandResult::Pointer("@gem:wrong".to_string()),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    match client.count("gem", None, None) {
        Ok(_) => panic!("expected failure"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidData),
    }
    handle.join().expect("join failed");
}

#[test]
fn opening_unsupported_scheme_returns_error() {
    let res = WardrobeClient::open("unsupported://host:1234");
    assert!(res.is_err());
}

#[test]
fn client_show_databases_unexpected_result_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::ShowDatabases,
        CommandResult::Pointer("@gem:bad".to_string()),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    match client.show_databases() {
        Ok(_) => panic!("expected failure"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidData),
    }
    handle.join().expect("join failed");
}

#[test]
fn client_show_drawers_unexpected_result_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::ShowDrawers {
            database_name: "db".to_string(),
            schema_name: "schema".to_string(),
        },
        CommandResult::Pointer("@gem:bad".to_string()),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    match client.show_drawers("db", "schema") {
        Ok(_) => panic!("expected failure"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidData),
    }
    handle.join().expect("join failed");
}

#[test]
fn client_unexpected_result_on_upsert_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::Upsert {
            drawer_name: "gem".to_string(),
            payload: json!({"_id": "x"}),
        },
        CommandResult::Records(vec![json!({"element": "Fire"})]),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    match client.upsert("gem", json!({"_id": "x"})) {
        Ok(_) => panic!("expected failure"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidData),
    }
    handle.join().expect("join failed");
}

#[test]
fn client_unexpected_result_on_find_all_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::FindAll {
            drawer_name: "gem".to_string(),
        },
        CommandResult::Count(5),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    match client.find_all("gem") {
        Ok(_) => panic!("expected failure"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidData),
    }
    handle.join().expect("join failed");
}

#[test]
fn client_handles_server_closing_connection_gracefully() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind failed");
    let addr = listener.local_addr().expect("address failed").to_string();
    let handle = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            drop(stream);
        }
    });

    let connection = format!("wardrobe://{}", addr);
    let client = WardrobeClient::open(connection).expect("open failed");
    let res = client.find_all("gem");
    assert!(res.is_err());
    handle.join().expect("join failed");
}

#[test]
fn client_handles_malformed_json_response_as_invaliddata() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind failed");
    let addr = listener.local_addr().expect("address failed").to_string();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept failed");
        let payload = b"not-json".to_vec();
        ProtocolFrame::new(ProtocolOpcode::Result, payload)
            .write_to_stream(&mut stream)
            .expect("write failed");
    });

    let client = WardrobeClient::open(format!("wardrobe://{}", addr)).expect("open failed");
    match client.find_all("gem") {
        Ok(_) => panic!("expected failure"),
        Err(e) => {
            let k = e.kind();
            assert!(
                k == std::io::ErrorKind::InvalidData || k == std::io::ErrorKind::ConnectionReset
            );
        }
    }
    handle.join().expect("join failed");
}

#[test]
fn client_handles_server_explicit_protocol_error_opcode() {
    let (connection, handle) = spawn_tcp_protocol_server_with_opcode(
        ProtocolOpcode::Error,
        b"Engine panic: disk write failure".to_vec(),
    );

    let client = WardrobeClient::open(connection).expect("open failed");
    match client.find_all("gem") {
        Ok(_) => panic!("expected failure"),
        Err(e) => {
            assert_eq!(e.kind(), std::io::ErrorKind::Other);
            assert!(e.to_string().contains("Engine panic"));
        }
    }
    handle.join().expect("join failed");
}

#[test]
fn client_handles_server_misbehaving_with_command_opcode() {
    let (connection, handle) =
        spawn_tcp_protocol_server_with_opcode(ProtocolOpcode::Command, b"{}".to_vec());

    let client = WardrobeClient::open(connection).expect("open failed");
    match client.find_all("gem") {
        Ok(_) => panic!("expected failure"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidData),
    }
    handle.join().expect("join failed");
}

#[test]
fn client_alias_methods_execute_successfully() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![
        (
            Command::ShowTenants,
            CommandResult::Tenants(vec!["tenant_beta".to_string()]),
        ),
        (Command::ShowDatabases, CommandResult::Databases(Vec::new())),
        (
            Command::ShowSchemas {
                database_name: "db".to_string(),
            },
            CommandResult::Schemas(Vec::new()),
        ),
        (
            Command::ShowDrawers {
                database_name: "db".to_string(),
                schema_name: "schema".to_string(),
            },
            CommandResult::Drawers(Vec::new()),
        ),
    ]);

    let client = WardrobeClient::open(connection).expect("open failed");

    let tenants = client.list_tenants().expect("alias failed");
    assert_eq!(tenants, vec!["tenant_beta".to_string()]);

    let dbs = client.list_databases().expect("alias failed");
    assert!(dbs.is_empty());

    let schemas = client.list_schemas("db").expect("alias failed");
    assert!(schemas.is_empty());

    let drawers = client.list_drawers("db", "schema").expect("alias failed");
    assert!(drawers.is_empty());

    handle.join().expect("join failed");
}

#[test]
fn client_unexpected_result_on_find_by_filter_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::FindByFilter {
            drawer_name: "gem".to_string(),
            filter: json!({}),
            modifiers: None,
        },
        CommandResult::Count(0),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    let result = client.find_by_filter("gem", json!({}), None);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    handle.join().expect("join failed");
}

#[test]
fn client_unexpected_result_on_find_by_id_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::FindById {
            pointer: "@gem:target_identifier".to_string(),
        },
        CommandResult::Count(0),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    let result = client.find_by_id("@gem:target_identifier");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    handle.join().expect("join failed");
}

#[test]
fn client_unexpected_result_on_delete_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::Delete {
            pointer: "@gem:target_identifier".to_string(),
        },
        CommandResult::Count(0),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    let result = client.delete_by_id("@gem:target_identifier");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    handle.join().expect("join failed");
}

#[test]
fn client_unexpected_result_on_vacuum_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::Vacuum {
            drawer_name: "gem".to_string(),
        },
        CommandResult::Count(0),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    let result = client.vacuum_drawer("gem");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    handle.join().expect("join failed");
}

#[test]
fn client_unexpected_result_on_migrate_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::Migrate {
            drawer_name: "gem".to_string(),
        },
        CommandResult::Count(0),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    let result = client.migrate_drawer("gem");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    handle.join().expect("join failed");
}

#[test]
fn client_unexpected_result_on_show_schemas_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::ShowSchemas {
            database_name: "main_database".to_string(),
        },
        CommandResult::Count(0),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    let result = client.show_schemas("main_database");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    handle.join().expect("join failed");
}
