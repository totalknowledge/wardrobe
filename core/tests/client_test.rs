mod common;

use common::TempDatabase;
use serde_json::json;
use std::net::TcpListener;
use std::path::Path;
use std::thread::{self, JoinHandle};
use wardrobe_core::{
    Command, CommandResult, CompactMode, CompactRequest, ConnectionTarget, CreateRequest,
    CreateResult, DriverKind, OperationFilter, OperationOptions, OrderDirection, PermissionRequest,
    ProtocolFrame, ProtocolOpcode, QueryModifiers, ReadResult, StatusRequest, StatusResult,
    StorageInventory, StorageLocator, VacuumReport, WardrobeClient,
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

struct ReadTarget {
    filter: OperationFilter,
    options: OperationOptions,
}

impl From<OperationFilter> for ReadTarget {
    fn from(filter: OperationFilter) -> Self {
        Self {
            filter,
            options: OperationOptions::default(),
        }
    }
}

impl From<(OperationFilter, OperationOptions)> for ReadTarget {
    fn from((filter, options): (OperationFilter, OperationOptions)) -> Self {
        Self { filter, options }
    }
}

fn read_records<T>(client: &WardrobeClient, target: T) -> std::io::Result<Vec<serde_json::Value>>
where
    T: Into<ReadTarget>,
{
    let target = target.into();
    match client.read(target.filter, target.options)? {
        ReadResult::Records(records) => Ok(records),
        other => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("expected records, got {other:?}"),
        )),
    }
}

fn read_record<T>(client: &WardrobeClient, target: T) -> std::io::Result<Option<serde_json::Value>>
where
    T: Into<ReadTarget>,
{
    let target = target.into();
    match client.read(target.filter, target.options)? {
        ReadResult::Record(record) => Ok(record),
        other => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("expected record, got {other:?}"),
        )),
    }
}

fn status_tenants(client: &WardrobeClient) -> std::io::Result<Vec<String>> {
    match client.status(StatusRequest::tenants())? {
        StatusResult::Tenants(tenants) => Ok(tenants),
        other => Err(unexpected_status(other)),
    }
}

fn status_databases(client: &WardrobeClient) -> std::io::Result<Vec<StorageInventory>> {
    match client.status(StatusRequest::databases())? {
        StatusResult::Databases(databases) => Ok(databases),
        other => Err(unexpected_status(other)),
    }
}

fn status_schemas(client: &WardrobeClient, database_name: &str) -> std::io::Result<Vec<String>> {
    match client.status(StatusRequest::schemas(database_name))? {
        StatusResult::Schemas(schemas) => Ok(schemas),
        other => Err(unexpected_status(other)),
    }
}

fn status_drawers(
    client: &WardrobeClient,
    database_name: &str,
    schema_name: &str,
) -> std::io::Result<Vec<StorageInventory>> {
    match client.status(StatusRequest::drawers(database_name, schema_name))? {
        StatusResult::Drawers(drawers) => Ok(drawers),
        other => Err(unexpected_status(other)),
    }
}

fn create_inventory(result: CreateResult) -> StorageInventory {
    match result {
        CreateResult::StorageInventory(inventory) => inventory,
        other => panic!("expected storage inventory, got {other:?}"),
    }
}

fn unexpected_status(result: StatusResult) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("unexpected status result {result:?}"),
    )
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
            json!({
                "_id": "@gem:lnk_client_fire",
                "element": "Fire"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
        )
        .expect("upsert failed");
    assert_eq!(pointer, vec!["@gem:client_fire".to_string()]);

    let records = read_records(&client, OperationFilter::drawer("gem")).expect("find failed");
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

    let _ = client.upsert(
        json!({"_id": "test_id", "element": "Earth"}),
        OperationFilter::drawer("gem"),
        None::<OperationOptions>,
    );
    let _ = client.upsert(
        json!({"_id": "other_id", "element": "Air"}),
        OperationFilter::drawer("gem"),
        None::<OperationOptions>,
    );

    let filter_results =
        read_records(&client, OperationFilter::query_in("gem", json!({}))).expect("filter failed");
    assert!(filter_results.len() >= 1);

    let count_value = client
        .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
        .expect("count failed");
    assert!(count_value >= 1);

    let single_record =
        read_record(&client, OperationFilter::pointer("@gem:test_id")).expect("find_by_id failed");
    assert!(single_record.is_some());

    let vacuum_res = client.compact(CompactRequest::drawer("gem"));
    assert!(vacuum_res.is_ok());

    let migrate_res = client.compact(CompactRequest::drawer_with_mode(
        "gem",
        CompactMode::Migrate,
    ));
    assert!(migrate_res.is_ok());

    let tenants = status_tenants(&client);
    assert!(tenants.is_ok());

    let databases = status_databases(&client);
    assert!(databases.is_ok());

    let schemas = status_schemas(&client, "main");
    assert!(schemas.is_ok());

    let drawers = status_drawers(&client, "main", "default");
    assert!(drawers.is_ok());

    let delete_by_id_res = client
        .delete(
            OperationFilter::pointer("@gem:test_id"),
            None::<OperationOptions>,
        )
        .expect("delete failed");
    assert_eq!(delete_by_id_res, 1);

    let inline_locator = StorageLocator::Inline("@gem:other_id".to_string());
    let delete_inline_res = client
        .delete(inline_locator, None::<OperationOptions>)
        .expect("delete failed");
    assert_eq!(delete_inline_res, 1);

    let explicit_locator = StorageLocator::Explicit {
        drawer: "gem".to_string(),
        id: "missing_id".to_string(),
    };
    let delete_explicit_res = client
        .delete(explicit_locator, None::<OperationOptions>)
        .expect("delete failed");
    assert_eq!(delete_explicit_res, 0);

    let deleted_by_filter = client
        .delete(
            OperationFilter::query_in("gem", json!({ "element": "Unknown" })),
            OperationOptions::new().multi(true),
        )
        .expect("delete_by_filter failed");
    assert_eq!(deleted_by_filter, 0);
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
            json!({
                "_id": "client_file_uri_gem",
                "element": "Water"
            }),
            OperationFilter::drawer("gem"),
            None::<OperationOptions>,
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
    assert!(
        read_records(&client, OperationFilter::drawer("gem"))
            .expect("find failed")
            .is_empty()
    );
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
    let modifiers = QueryModifiers {
        order_by: Some("element".to_string()),
        order_direction: Some(OrderDirection::Ascending),
        limit: Some(10),
        offset: Some(0),
    };
    let (connection, handle) = spawn_tcp_protocol_server(vec![
        (
            Command::Upsert {
                drawer_name: "gem".to_string(),
                payload: json!({"_id": "network_fire", "element": "Fire"}),
            },
            CommandResult::Pointer("@gem:network_fire".to_string()),
        ),
        (
            Command::BulkUpsert {
                drawer_name: "gem".to_string(),
                records: vec![
                    json!({"_id": "network_water", "element": "Water"}),
                    json!({"_id": "network_earth", "element": "Earth"}),
                ],
                atomic: true,
            },
            CommandResult::Pointers(vec![
                "@gem:network_water".to_string(),
                "@gem:network_earth".to_string(),
            ]),
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
                modifiers: None,
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
            Command::DeleteByFilter {
                drawer_name: "gem".to_string(),
                filter: json!({"element": "Water"}),
            },
            CommandResult::Count(1),
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
            .upsert(
                json!({"_id": "network_fire", "element": "Fire"}),
                OperationFilter::drawer("gem"),
                None::<OperationOptions>
            )
            .expect("upsert failed"),
        vec!["@gem:network_fire".to_string()]
    );
    assert_eq!(
        client
            .upsert(
                json!([
                    json!({"_id": "network_water", "element": "Water"}),
                    json!({"_id": "network_earth", "element": "Earth"}),
                ]),
                OperationFilter::drawer("gem"),
                None::<OperationOptions>
            )
            .expect("bulk upsert failed"),
        vec![
            "@gem:network_water".to_string(),
            "@gem:network_earth".to_string()
        ]
    );
    assert_eq!(
        read_records(&client, OperationFilter::drawer("gem")).expect("find failed"),
        vec![json!({"element": "Fire"})]
    );
    assert_eq!(
        read_records(
            &client,
            (
                OperationFilter::query_in("gem", json!({"element": "F%"})),
                OperationOptions::from(modifiers.clone()),
            )
        )
        .expect("filter failed"),
        vec![json!({"element": "Fire"})]
    );
    assert_eq!(
        client
            .count(
                OperationFilter::query_in("gem", json!({"element": "F%"})),
                None::<OperationOptions>
            )
            .expect("count failed"),
        1
    );
    assert_eq!(
        read_record(&client, OperationFilter::pointer("@gem:network_fire")).expect("find failed"),
        Some(json!({"element": "Fire"}))
    );
    assert_eq!(
        client
            .delete(
                OperationFilter::pointer("@gem:network_fire"),
                None::<OperationOptions>
            )
            .expect("delete failed"),
        1
    );
    assert_eq!(
        client
            .delete(("gem", "lnk_explicit_delete"), None::<OperationOptions>)
            .expect("delete failed"),
        1
    );
    assert_eq!(
        client
            .delete(
                OperationFilter::query_in("gem", json!({"element": "Water"})),
                OperationOptions::new().multi(true)
            )
            .expect("delete_by_filter failed"),
        1
    );
    assert_eq!(
        client
            .compact(CompactRequest::drawer("gem"))
            .expect("vacuum failed"),
        report
    );
    assert_eq!(
        client
            .compact(CompactRequest::drawer_with_mode(
                "gem",
                CompactMode::Migrate
            ))
            .expect("migrate failed"),
        report
    );
    assert_eq!(
        status_tenants(&client).expect("tenants failed"),
        vec!["tenant_alpha".to_string()]
    );
    assert_eq!(
        status_databases(&client).expect("databases failed"),
        vec![StorageInventory {
            name: "main_db".to_string(),
            record_count: 3,
            disk_size_bytes: 4096,
            register_file_count: 7,
        }]
    );
    assert_eq!(
        status_schemas(&client, "main_db").expect("schemas failed"),
        vec!["tenant_schema".to_string()]
    );
    assert_eq!(
        status_drawers(&client, "main_db", "tenant_schema").expect("drawers failed"),
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
    assert_eq!(
        client
            .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
            .expect("count failed"),
        7
    );
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
    match client.count(OperationFilter::drawer("gem"), None::<OperationOptions>) {
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
    match status_databases(&client) {
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
    match status_drawers(&client, "db", "schema") {
        Ok(_) => panic!("expected failure"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidData),
    }
    handle.join().expect("join failed");
}

#[test]
fn client_admin_setup_commands_route_over_network() {
    let database_inventory = StorageInventory {
        name: "admin_db".to_string(),
        record_count: 0,
        disk_size_bytes: 0,
        register_file_count: 1,
    };
    let schema_inventory = StorageInventory {
        name: "public".to_string(),
        record_count: 0,
        disk_size_bytes: 0,
        register_file_count: 1,
    };
    let drawer_inventory = StorageInventory {
        name: "gem".to_string(),
        record_count: 0,
        disk_size_bytes: 0,
        register_file_count: 1,
    };

    let (connection, handle) = spawn_tcp_protocol_server(vec![
        (
            Command::DefineDatabase {
                database_name: "admin_db".to_string(),
            },
            CommandResult::StorageInventory(database_inventory.clone()),
        ),
        (
            Command::DefineSchema {
                database_name: "admin_db".to_string(),
                schema_name: "public".to_string(),
            },
            CommandResult::StorageInventory(schema_inventory.clone()),
        ),
        (
            Command::DefineDrawer {
                database_name: "admin_db".to_string(),
                schema_name: "public".to_string(),
                drawer_name: "gem".to_string(),
            },
            CommandResult::StorageInventory(drawer_inventory.clone()),
        ),
        (
            Command::DefineTenantRoute {
                tenant_id: "tenant_a".to_string(),
                database_name: "admin_db".to_string(),
                location: "tenant_a/admin_db/public".to_string(),
            },
            CommandResult::StorageInventory(database_inventory.clone()),
        ),
        (
            Command::ManageUser {
                action: "grant_permission".to_string(),
                payload: json!({"username": "alice", "permission_scope": "global:rud"}),
            },
            CommandResult::Admin(json!({"ok": true})),
        ),
    ]);

    let client = WardrobeClient::open(connection).expect("open failed");

    assert_eq!(
        create_inventory(
            client
                .create(CreateRequest::database("admin_db"))
                .expect("create database")
        ),
        database_inventory
    );
    assert_eq!(
        create_inventory(
            client
                .create(CreateRequest::schema("admin_db", "public"))
                .expect("create schema")
        ),
        schema_inventory
    );
    assert_eq!(
        create_inventory(
            client
                .create(CreateRequest::drawer("admin_db", "public", "gem"))
                .expect("create drawer")
        ),
        drawer_inventory
    );
    assert_eq!(
        create_inventory(
            client
                .create(CreateRequest::tenant_route(
                    "tenant_a",
                    "admin_db",
                    "tenant_a/admin_db/public"
                ))
                .expect("register route")
        ),
        database_inventory
    );
    assert_eq!(
        client
            .grant(PermissionRequest::new("alice", "global:rud"))
            .expect("manage user"),
        json!({"ok": true})
    );

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
    match client.upsert(
        json!({"_id": "x"}),
        OperationFilter::drawer("gem"),
        None::<OperationOptions>,
    ) {
        Ok(_) => panic!("expected failure"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidData),
    }
    handle.join().expect("join failed");
}

#[test]
fn client_unexpected_result_on_bulk_upsert_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::BulkUpsert {
            drawer_name: "gem".to_string(),
            records: vec![json!({"_id": "x"})],
            atomic: true,
        },
        CommandResult::Records(vec![json!({"_id": "x"})]),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    let result = client.upsert(
        json!([json!({"_id": "x"})]),
        OperationFilter::drawer("gem"),
        None::<OperationOptions>,
    );

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
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
    match read_records(&client, OperationFilter::drawer("gem")) {
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
    let res = read_records(&client, OperationFilter::drawer("gem"));
    assert!(res.is_err());
    handle.join().expect("join failed");
}

#[test]
fn client_handles_malformed_json_response_as_invaliddata() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind failed");
    let addr = listener.local_addr().expect("address failed").to_string();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept failed");
        let _ = ProtocolFrame::read_from_stream(&mut stream).expect("read failed");
        let payload = b"not-json".to_vec();
        ProtocolFrame::new(ProtocolOpcode::Result, payload)
            .write_to_stream(&mut stream)
            .expect("write failed");
    });

    let client = WardrobeClient::open(format!("wardrobe://{}", addr)).expect("open failed");
    match read_records(&client, OperationFilter::drawer("gem")) {
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
    match read_records(&client, OperationFilter::drawer("gem")) {
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
    match read_records(&client, OperationFilter::drawer("gem")) {
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

    let tenants = status_tenants(&client).expect("status failed");
    assert_eq!(tenants, vec!["tenant_beta".to_string()]);

    let dbs = status_databases(&client).expect("status failed");
    assert!(dbs.is_empty());

    let schemas = status_schemas(&client, "db").expect("status failed");
    assert!(schemas.is_empty());

    let drawers = status_drawers(&client, "db", "schema").expect("status failed");
    assert!(drawers.is_empty());

    handle.join().expect("join failed");
}

#[test]
fn client_unexpected_result_on_find_by_filter_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::FindByFilter {
            drawer_name: "gem".to_string(),
            filter: json!({"element": "Fire"}),
            modifiers: None,
        },
        CommandResult::Count(0),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    let result = read_records(
        &client,
        OperationFilter::query_in("gem", json!({"element": "Fire"})),
    );
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
    let result = read_record(&client, OperationFilter::pointer("@gem:target_identifier"));
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
    let result = client.delete(
        OperationFilter::pointer("@gem:target_identifier"),
        None::<OperationOptions>,
    );
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    handle.join().expect("join failed");
}

#[test]
fn client_unexpected_result_on_delete_by_filter_returns_invaliddata() {
    let (connection, handle) = spawn_tcp_protocol_server(vec![(
        Command::DeleteByFilter {
            drawer_name: "gem".to_string(),
            filter: json!({"element": "Fire"}),
        },
        CommandResult::Deleted(true),
    )]);

    let client = WardrobeClient::open(connection).expect("open failed");
    let result = client.delete(
        OperationFilter::query_in("gem", json!({"element": "Fire"})),
        OperationOptions::new().multi(true),
    );

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
    let result = client.compact(CompactRequest::drawer("gem"));
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
    let result = client.compact(CompactRequest::drawer_with_mode(
        "gem",
        CompactMode::Migrate,
    ));
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
    let result = status_schemas(&client, "main_database");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    handle.join().expect("join failed");
}
