use serde_json::json;
use std::fs;
use std::net::TcpStream;
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_cli::{
    CliConfig, print_json, pub_normalize_record_ids, run_cli_logic, run_command, shell_split,
};
use wardrobe_core::{
    Command, CommandResult, CreateRequest, CreateResult, InspectResult, OperationFilter,
    OperationOptions, PermissionRequest, StatusRequest, StatusResult, StorageInventory,
    WardrobeClient, WardrobeEngine,
};

fn status_databases(client: &WardrobeClient) -> Vec<StorageInventory> {
    match client
        .status(StatusRequest::databases())
        .expect("status databases")
    {
        StatusResult::Databases(databases) => databases,
        other => panic!("expected databases, got {other:?}"),
    }
}

fn status_schemas(client: &WardrobeClient, database_name: &str) -> Vec<String> {
    match client
        .status(StatusRequest::schemas(database_name))
        .expect("status schemas")
    {
        StatusResult::Schemas(schemas) => schemas,
        other => panic!("expected schemas, got {other:?}"),
    }
}

fn status_drawers(
    client: &WardrobeClient,
    database_name: &str,
    schema_name: &str,
) -> Vec<StorageInventory> {
    match client
        .status(StatusRequest::drawers(database_name, schema_name))
        .expect("status drawers")
    {
        StatusResult::Drawers(drawers) => drawers,
        other => panic!("expected drawers, got {other:?}"),
    }
}

fn temp_storage_directory(test_name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("wardrobe_cli_lib_{test_name}_{nanos}"))
}

fn spawn_protocol_server<F>(handler: F) -> String
where
    F: Fn(TcpStream) + Send + 'static,
{
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            handler(stream);
        }
    });
    format!("wardrobe://{}", addr)
}

fn assert_manage_user_command(args: &[&str], action: &str, payload: serde_json::Value) {
    let expected_action = action.to_string();
    let target = spawn_protocol_server(move |mut stream| {
        let request =
            wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
                .expect("read");
        let command: Command = serde_json::from_slice(&request.payload).expect("command");
        let expected_command = match expected_action.as_str() {
            "add_user" => Command::Create(CreateRequest::user(payload.clone())),
            "grant_permission" => {
                let username = payload["username"].as_str().expect("username");
                let permission_scope = payload["permission_scope"]
                    .as_str()
                    .expect("permission scope");
                let request = payload
                    .get("scope")
                    .and_then(|scope| {
                        Some(PermissionRequest::with_scope(
                            username,
                            permission_scope,
                            scope.get("path")?.as_str()?,
                            scope.get("rights")?.as_str()?,
                        ))
                    })
                    .unwrap_or_else(|| PermissionRequest::new(username, permission_scope));
                Command::Grant(request)
            }
            "revoke_permission" => {
                let username = payload["username"].as_str().expect("username");
                let permission_scope = payload["permission_scope"]
                    .as_str()
                    .expect("permission scope");
                let request = payload
                    .get("scope")
                    .and_then(|scope| {
                        Some(PermissionRequest::with_scope(
                            username,
                            permission_scope,
                            scope.get("path")?.as_str()?,
                            scope.get("rights")?.as_str()?,
                        ))
                    })
                    .unwrap_or_else(|| PermissionRequest::new(username, permission_scope));
                Command::Revoke(request)
            }
            other => panic!("unsupported manage-user action fixture {other}"),
        };
        assert_eq!(command, expected_command);
        let result = match expected_action.as_str() {
            "add_user" => CommandResult::Create(CreateResult::Admin(json!({"ok": true}))),
            "grant_permission" => CommandResult::Grant(json!({"ok": true})),
            "revoke_permission" => CommandResult::Revoke(json!({"ok": true})),
            other => panic!("unsupported manage-user result fixture {other}"),
        };
        let payload = serde_json::to_vec(&result).expect("ser");
        wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
            wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
            payload,
        )
        .write_to_stream(&mut stream)
        .expect("write");
    });

    let client = WardrobeClient::open(&target).unwrap();
    let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    assert!(run_command(&client, &args, false).is_ok());
}

#[test]
fn test_cli_config_clean_parsing() {
    let args = vec![
        "custom_dir".to_string(),
        "--pretty".to_string(),
        "read".to_string(),
        "character".to_string(),
    ];
    let config = CliConfig::from_args(args).unwrap();
    assert_eq!(config.connection, "custom_dir");
    assert!(config.pretty);
    assert_eq!(config.command_parts, vec!["read", "character"]);
    assert_eq!(
        config.logging.level,
        wardrobe_core::ApplicationLogLevel::Off
    );
    assert_eq!(
        config.logging.destination,
        wardrobe_core::ApplicationLogDestination::Stderr
    );
}

#[test]
fn test_cli_config_accepts_positional_connection_target() {
    let args = vec![
        "./my_data".to_string(),
        "read".to_string(),
        "database/drawer".to_string(),
    ];
    let config = CliConfig::from_args(args).unwrap();
    assert_eq!(config.connection, "./my_data");
    assert_eq!(config.command_parts, vec!["read", "database/drawer"]);
}

#[test]
fn test_cli_config_preserves_legacy_default_connection_commands() {
    let args = vec!["read".to_string(), "database/drawer".to_string()];
    let config = CliConfig::from_args(args).unwrap();
    assert_eq!(config.connection, "read");
    assert_eq!(config.command_parts, vec!["database/drawer"]);
}

#[test]
fn test_cli_config_alternate_flags() {
    let args = vec![
        "alt_dir".to_string(),
        "status".to_string(),
        "drawer-names".to_string(),
    ];
    let config = CliConfig::from_args(args).unwrap();
    assert_eq!(config.connection, "alt_dir");
    assert_eq!(config.command_parts, vec!["status", "drawer-names"]);
}

#[test]
fn test_cli_config_logging_flags_are_parsed_and_not_commands() {
    let args = vec![
        "custom_dir".to_string(),
        "--log-level".to_string(),
        "debug".to_string(),
        "--log-format".to_string(),
        "json".to_string(),
        "--log-destination".to_string(),
        "file".to_string(),
        "--log-file".to_string(),
        "logs/wardrobe-cli.log".to_string(),
        "read".to_string(),
        "gem".to_string(),
    ];
    let config = CliConfig::from_args(args).unwrap();
    assert_eq!(config.connection, "custom_dir");

    assert_eq!(
        config.logging.level,
        wardrobe_core::ApplicationLogLevel::Debug
    );
    assert_eq!(
        config.logging.format,
        wardrobe_core::ApplicationLogFormat::Json
    );
    assert_eq!(
        config.logging.destination,
        wardrobe_core::ApplicationLogDestination::File
    );
    assert_eq!(
        config.logging.file,
        Some(std::path::PathBuf::from("logs/wardrobe-cli.log"))
    );
    assert_eq!(config.command_parts, vec!["read", "gem"]);
}

#[test]
fn test_cli_config_rejects_invalid_logging_flags() {
    assert!(
        CliConfig::from_args(vec![
            "target".to_string(),
            "--log-level".to_string(),
            "verbose".to_string()
        ])
        .is_err()
    );
    assert!(
        CliConfig::from_args(vec![
            "target".to_string(),
            "--log-format".to_string(),
            "xml".to_string()
        ])
        .is_err()
    );
    assert!(
        CliConfig::from_args(vec![
            "target".to_string(),
            "--log-destination".to_string(),
            "syslog".to_string()
        ])
        .is_err()
    );
    assert!(
        CliConfig::from_args(vec![
            "target".to_string(),
            "--log-level".to_string(),
            "info".to_string(),
            "--log-destination".to_string(),
            "file".to_string(),
        ])
        .is_err()
    );
}

#[test]
fn test_cli_logging_defaults_to_stderr_without_corrupting_stdout() {
    let storage_directory = temp_storage_directory("logging_stderr_stdout_clean");
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_wardrobe"))
        .arg(&storage_directory)
        .arg("--log-level")
        .arg("info")
        .arg("status")
        .arg("drawer-names")
        .output()
        .expect("wardrobe binary should run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stdout.trim(), "[]");
    assert!(stderr.contains("cli_start"));
    assert!(stderr.contains("command_start"));
    assert!(!stdout.contains("command_start"));

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_cli_config_missing_target_payload() {
    let args = Vec::new();
    let result = CliConfig::from_args(args);
    assert!(result.is_err());
}

#[test]
fn test_shell_split_whitespace_handling() {
    let parts = shell_split("   status    path   gem   ");
    assert_eq!(parts, vec!["status", "path", "gem"]);
}

#[test]
fn test_normalize_record_ids_link_stripping() {
    let mut records = vec![
        json!({ "_id": "@gem:lnk_ruby", "val": 1 }),
        json!({ "_id": "@weapon:steel_blade", "val": 2 }),
        json!({ "no_id": true }),
    ];
    pub_normalize_record_ids(&mut records);
    assert_eq!(records[0]["_id"], "ruby");
    assert_eq!(records[1]["_id"], "steel_blade");
}

#[test]
fn test_embedded_drawers_and_diagnose_execution() {
    let storage_directory = temp_storage_directory("embedded_logic");
    {
        let engine =
            WardrobeEngine::open(&storage_directory.to_string_lossy()).expect("engine open");
        engine
            .upsert(
                json!({ "_id": "@gem:lnk_fire", "element": "Fire" }),
                OperationFilter::drawer("gem"),
                None::<OperationOptions>,
            )
            .expect("insert");
    }

    let config = CliConfig {
        connection: storage_directory.to_string_lossy().to_string(),
        pretty: false,
        command_parts: vec!["status".to_string(), "drawer-names".to_string()],
        logging: wardrobe_core::ApplicationLoggingConfig::default(),
    };
    assert!(run_cli_logic(config).is_ok());

    let config_diag = CliConfig {
        connection: storage_directory.to_string_lossy().to_string(),
        pretty: false,
        command_parts: vec!["status".to_string(), "storage".to_string()],
        logging: wardrobe_core::ApplicationLoggingConfig::default(),
    };
    assert!(run_cli_logic(config_diag).is_ok());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_embedded_inspect_and_records_execution() {
    let storage_directory = temp_storage_directory("inspect_logic");
    {
        let engine =
            WardrobeEngine::open(&storage_directory.to_string_lossy()).expect("engine open");
        engine
            .upsert(
                json!({ "_id": "@gem:lnk_fire", "element": "Fire" }),
                OperationFilter::drawer("gem"),
                None::<OperationOptions>,
            )
            .expect("insert");
    }

    let config_inspect = CliConfig {
        connection: storage_directory.to_string_lossy().to_string(),
        pretty: true,
        command_parts: vec!["inspect".to_string(), "gem".to_string()],
        logging: wardrobe_core::ApplicationLoggingConfig::default(),
    };
    assert!(run_cli_logic(config_inspect).is_ok());

    let config_records = CliConfig {
        connection: storage_directory.to_string_lossy().to_string(),
        pretty: false,
        command_parts: vec!["read".to_string(), "gem".to_string()],
        logging: wardrobe_core::ApplicationLoggingConfig::default(),
    };
    assert!(run_cli_logic(config_records).is_ok());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_network_show_commands_via_library() {
    let target = spawn_protocol_server(|mut stream| {
        let _req = wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
            .expect("read");
        let payload = serde_json::to_vec(&CommandResult::Status(StatusResult::Databases(vec![
            StorageInventory {
                name: "net_db".to_string(),
                record_count: 1,
                disk_size_bytes: 512,
                register_file_count: 1,
            },
        ])))
        .expect("ser");
        wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
            wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
            payload,
        )
        .write_to_stream(&mut stream)
        .expect("write");
    });

    let config = CliConfig {
        connection: target,
        pretty: false,
        command_parts: vec!["status".to_string(), "wardrobes".to_string()],
        logging: wardrobe_core::ApplicationLoggingConfig::default(),
    };
    assert!(run_cli_logic(config).is_ok());
}

#[test]
fn test_command_routing_guards_and_failures() {
    let storage_directory = temp_storage_directory("routing_guards");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    assert!(run_command(&client, &[], false).is_ok());

    let res_inspect = run_command(&client, &["inspect".to_string()], false);
    assert!(res_inspect.is_err());

    let res_read = run_command(&client, &["read".to_string()], false);
    assert!(res_read.is_ok());

    let res_count = run_command(&client, &["count".to_string()], false);
    assert!(res_count.is_err());

    let res_backup = run_command(&client, &["backup".to_string()], false);
    assert!(res_backup.is_err());

    let res_restore = run_command(&client, &["restore".to_string()], false);
    assert!(res_restore.is_err());

    let res_create_user = run_command(&client, &["create".to_string(), "user".to_string()], false);
    assert!(res_create_user.is_err());

    let res_grant = run_command(&client, &["grant".to_string()], false);
    assert!(res_grant.is_err());

    let res_revoke = run_command(
        &client,
        &["revoke".to_string(), "permission".to_string()],
        false,
    );
    assert!(res_revoke.is_err());

    let res_upsert = run_command(&client, &["upsert".to_string()], false);
    assert!(res_upsert.is_err());

    let res_delete = run_command(&client, &["delete".to_string()], false);
    assert!(res_delete.is_err());

    let res_schemas = run_command(&client, &["status".to_string(), "bays".to_string()], false);
    assert!(res_schemas.is_err());

    let res_drawers = run_command(
        &client,
        &["status".to_string(), "drawers".to_string()],
        false,
    );
    assert!(res_drawers.is_err());

    let res_unknown = run_command(&client, &["invalid-cmd-target".to_string()], false);
    assert!(res_unknown.is_err());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_json_rendering_variants() {
    let val = json!({ "active": true });
    assert!(print_json(&val, false).is_ok());
    assert!(print_json(&val, true).is_ok());
}

#[test]
fn test_embedded_write_commands_execution_paths() {
    let storage_directory = temp_storage_directory("write_paths");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    let payload = "{\"_id\":\"@gem:lnk_topaz\",\"power\":150}";
    let upsert_args = vec!["upsert".to_string(), "gem".to_string(), payload.to_string()];
    assert!(run_command(&client, &upsert_args, false).is_ok());

    let bad_payload_args = vec![
        "upsert".to_string(),
        "gem".to_string(),
        "{invalid-json".to_string(),
    ];
    assert!(run_command(&client, &bad_payload_args, false).is_err());

    let delete_args = vec!["delete".to_string(), "@gem:lnk_topaz".to_string()];
    assert!(run_command(&client, &delete_args, false).is_ok());

    for payload in [
        "{\"_id\":\"@gem:lnk_delete_all_one\",\"power\":1}",
        "{\"_id\":\"@gem:lnk_delete_all_two\",\"power\":2}",
    ] {
        let upsert_args = vec!["upsert".to_string(), "gem".to_string(), payload.to_string()];
        assert!(run_command(&client, &upsert_args, false).is_ok());
    }
    let delete_all_args = vec!["delete".to_string(), "gem".to_string(), "{}".to_string()];
    assert!(run_command(&client, &delete_all_args, false).is_ok());
    assert_eq!(
        client
            .count(OperationFilter::drawer("gem"), None::<OperationOptions>)
            .unwrap(),
        0
    );

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_embedded_administrative_setup_commands_execution_paths() {
    let storage_directory = temp_storage_directory("admin_setup");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    let define_database = vec![
        "create".to_string(),
        "wardrobe".to_string(),
        "admin_db".to_string(),
    ];
    assert!(run_command(&client, &define_database, false).is_ok());

    let define_schema = vec![
        "create".to_string(),
        "bay".to_string(),
        "admin_db/public".to_string(),
    ];
    assert!(run_command(&client, &define_schema, false).is_ok());

    let define_drawer = vec![
        "create".to_string(),
        "drawer".to_string(),
        "admin_db/public/gem".to_string(),
    ];
    assert!(run_command(&client, &define_drawer, true).is_ok());

    let show_databases = vec!["status".to_string(), "wardrobes".to_string()];
    assert!(run_command(&client, &show_databases, false).is_ok());

    let list_schemas = vec![
        "status".to_string(),
        "bays".to_string(),
        "admin_db".to_string(),
    ];
    assert!(run_command(&client, &list_schemas, false).is_ok());

    let ls_drawers = vec![
        "status".to_string(),
        "drawers".to_string(),
        "admin_db/public".to_string(),
    ];
    assert!(run_command(&client, &ls_drawers, false).is_ok());

    let databases = status_databases(&client);
    assert!(databases.iter().any(|db| db.name == "admin_db"));
    assert!(status_schemas(&client, "admin_db").contains(&"public".to_string()));
    assert!(
        status_drawers(&client, "admin_db", "public")
            .iter()
            .any(|drawer| drawer.name == "gem")
    );

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_structural_lifecycle_commands_execution_paths() {
    let storage_directory = temp_storage_directory("structural_lifecycle");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    let create_wardrobe = vec![
        "create".to_string(),
        "wardrobe".to_string(),
        "armory".to_string(),
    ];
    assert!(run_command(&client, &create_wardrobe, false).is_ok());

    let create_bay = vec![
        "create".to_string(),
        "bay".to_string(),
        "armory/public".to_string(),
    ];
    assert!(run_command(&client, &create_bay, false).is_ok());

    let create_drawer = vec![
        "create".to_string(),
        "drawer".to_string(),
        "armory/public/gem".to_string(),
    ];
    assert!(run_command(&client, &create_drawer, false).is_ok());

    let show_wardrobes = vec!["status".to_string(), "wardrobes".to_string()];
    assert!(run_command(&client, &show_wardrobes, false).is_ok());

    let show_bays = vec![
        "status".to_string(),
        "bays".to_string(),
        "armory".to_string(),
    ];
    assert!(run_command(&client, &show_bays, false).is_ok());

    let show_drawers = vec![
        "status".to_string(),
        "drawers".to_string(),
        "armory/public".to_string(),
    ];
    assert!(run_command(&client, &show_drawers, false).is_ok());

    assert!(run_command(&client, &["read".to_string()], false).is_ok());
    assert!(run_command(&client, &["read".to_string(), "armory".to_string()], false).is_ok());
    assert!(
        run_command(
            &client,
            &["read".to_string(), "armory/public".to_string()],
            false,
        )
        .is_ok()
    );

    let check_wardrobe = vec![
        "status".to_string(),
        "path".to_string(),
        "armory".to_string(),
    ];
    assert!(run_command(&client, &check_wardrobe, false).is_ok());

    let check_drawer = vec![
        "status".to_string(),
        "path".to_string(),
        "armory/public/gem".to_string(),
    ];
    assert!(run_command(&client, &check_drawer, false).is_ok());

    let upsert = vec![
        "upsert".to_string(),
        "armory/public/gem".to_string(),
        "{\"_id\":\"ruby\",\"power\":42}".to_string(),
    ];
    assert!(run_command(&client, &upsert, false).is_ok());

    let clean_drawer = vec!["compact".to_string(), "armory/public/gem".to_string()];
    assert!(run_command(&client, &clean_drawer, false).is_ok());

    let clean_bay = vec!["compact".to_string(), "armory/public".to_string()];
    assert!(run_command(&client, &clean_bay, false).is_ok());

    let clean_wardrobe = vec!["compact".to_string(), "armory".to_string()];
    assert!(run_command(&client, &clean_wardrobe, false).is_ok());

    let drop_drawer = vec![
        "drop".to_string(),
        "drawer".to_string(),
        "armory/public/gem".to_string(),
    ];
    assert!(run_command(&client, &drop_drawer, false).is_ok());
    assert!(
        !status_drawers(&client, "armory", "public")
            .iter()
            .any(|drawer| drawer.name == "gem")
    );

    let drop_bay = vec![
        "drop".to_string(),
        "bay".to_string(),
        "armory/public".to_string(),
    ];
    assert!(run_command(&client, &drop_bay, false).is_ok());
    assert!(!status_schemas(&client, "armory").contains(&"public".to_string()));

    let drop_wardrobe = vec![
        "drop".to_string(),
        "wardrobe".to_string(),
        "armory".to_string(),
    ];
    assert!(run_command(&client, &drop_wardrobe, false).is_ok());
    assert!(
        !status_databases(&client)
            .iter()
            .any(|db| db.name == "armory")
    );

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_document_query_and_inspection_commands_match_help_paths() {
    let storage_directory = temp_storage_directory("document_query_paths");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    assert!(
        run_command(
            &client,
            &[
                "create".to_string(),
                "wardrobe".to_string(),
                "armory".to_string()
            ],
            false
        )
        .is_ok()
    );
    assert!(
        run_command(
            &client,
            &[
                "create".to_string(),
                "bay".to_string(),
                "armory/public".to_string()
            ],
            false
        )
        .is_ok()
    );
    assert!(
        run_command(
            &client,
            &[
                "create".to_string(),
                "drawer".to_string(),
                "armory/public/gem".to_string()
            ],
            false
        )
        .is_ok()
    );

    let ruby = vec![
        "upsert".to_string(),
        "armory/public/gem".to_string(),
        "{\"_id\":\"ruby\",\"power\":42,\"element\":\"fire\"}".to_string(),
    ];
    assert!(run_command(&client, &ruby, false).is_ok());

    let sapphire = vec![
        "upsert".to_string(),
        "armory/public/gem".to_string(),
        "{\"_id\":\"sapphire\",\"power\":7,\"element\":\"water\"}".to_string(),
    ];
    assert!(run_command(&client, &sapphire, false).is_ok());

    assert!(
        run_command(
            &client,
            &["read".to_string(), "armory/public/gem".to_string()],
            false
        )
        .is_ok()
    );
    assert!(
        run_command(
            &client,
            &[
                "read".to_string(),
                "armory/public/gem".to_string(),
                "{\"power\":42}".to_string()
            ],
            false
        )
        .is_ok()
    );
    assert!(
        run_command(
            &client,
            &[
                "read".to_string(),
                "armory/public/gem".to_string(),
                "{\"element\":\"water\"}".to_string()
            ],
            false
        )
        .is_ok()
    );
    assert!(
        run_command(
            &client,
            &["count".to_string(), "armory/public/gem".to_string()],
            false
        )
        .is_ok()
    );
    assert!(
        run_command(
            &client,
            &[
                "count".to_string(),
                "armory/public/gem".to_string(),
                "{\"power\":42}".to_string()
            ],
            false
        )
        .is_ok()
    );
    assert!(
        run_command(
            &client,
            &["inspect".to_string(), "armory/public/gem".to_string()],
            true
        )
        .is_ok()
    );

    assert!(
        run_command(
            &client,
            &[
                "delete".to_string(),
                "armory/public/gem".to_string(),
                "ruby".to_string()
            ],
            false
        )
        .is_ok()
    );
    assert_eq!(
        client
            .count(
                OperationFilter::query_in("armory/public/gem", json!({"element": "fire"})),
                None::<OperationOptions>
            )
            .unwrap(),
        0
    );

    assert!(
        run_command(
            &client,
            &[
                "delete".to_string(),
                "armory/public/gem".to_string(),
                "{\"power\":7}".to_string()
            ],
            false
        )
        .is_ok()
    );
    assert_eq!(
        client
            .count(
                OperationFilter::drawer("armory/public/gem"),
                None::<OperationOptions>
            )
            .unwrap(),
        0
    );

    assert!(
        run_command(
            &client,
            &[
                "count".to_string(),
                "armory/public/gem".to_string(),
                "{invalid-json".to_string()
            ],
            false
        )
        .is_err()
    );
    assert!(
        run_command(
            &client,
            &[
                "read".to_string(),
                "armory/public/gem".to_string(),
                "{invalid-json".to_string()
            ],
            false
        )
        .is_err()
    );

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_schema_and_relationship_management_commands_execution_paths() {
    let storage_directory = temp_storage_directory("schema_relationship_management");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    for args in [
        vec!["create", "wardrobe", "armory"],
        vec!["create", "bay", "armory/public"],
        vec!["create", "drawer", "armory/public/user"],
        vec!["create", "drawer", "armory/public/tool"],
    ] {
        let args = args.into_iter().map(ToOwned::to_owned).collect::<Vec<_>>();
        assert!(run_command(&client, &args, false).is_ok());
    }

    for args in [
        vec!["alter", "index", "armory/public/user", "tool.type"],
        vec![
            "alter",
            "key",
            "armory/public/user",
            "profile_id",
            "secondary",
        ],
        vec![
            "alter",
            "constraint",
            "armory/public/user",
            "email",
            "unique",
        ],
        vec![
            "alter",
            "constraint",
            "armory/public/user",
            "age",
            "non-null",
        ],
        vec![
            "alter",
            "relationship",
            "armory/public/user",
            "tool_id",
            "armory/public/tool",
        ],
        vec!["alter", "cascade-delete", "armory/public/user", "tool_id"],
        vec![
            "alter",
            "trigger",
            "armory/public/user",
            "on_upsert",
            "./scripts/sync_profile.sh",
        ],
    ] {
        let args = args.into_iter().map(ToOwned::to_owned).collect::<Vec<_>>();
        assert!(run_command(&client, &args, true).is_ok());
    }

    let metadata_path = storage_directory
        .join("armory")
        .join("public")
        .join("user_meta.drw");
    let metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&metadata_path).unwrap()).unwrap();
    assert_eq!(
        metadata["relationship_constraints"]["tool_id"]["target_drawer"],
        "armory/public/tool"
    );
    assert_eq!(
        metadata["relationship_constraints"]["tool_id"]["type"],
        "M:1"
    );
    assert_eq!(metadata["cascade_delete_rules"]["tool_id"], true);
    assert_eq!(metadata["delete_rules"]["tool_id"]["action"], "Cascade");
    assert!(
        metadata["unique_constraints"]
            .as_array()
            .unwrap()
            .contains(&json!("email"))
    );
    assert!(
        metadata["unique_constraints"]
            .as_array()
            .unwrap()
            .contains(&json!("profile_id"))
    );
    assert!(
        metadata["schema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("age"))
    );
    assert!(metadata["schema"]["x-wardrobe-cli"]["indexes"]["tool.type"].is_object());
    assert_eq!(
        metadata["schema"]["x-wardrobe-cli"]["triggers"]["on_upsert"]["command"],
        "./scripts/sync_profile.sh"
    );

    for args in [
        vec!["drop", "index", "armory/public/user", "tool.type"],
        vec![
            "drop",
            "constraint",
            "armory/public/user",
            "email",
            "unique",
        ],
        vec![
            "drop",
            "constraint",
            "armory/public/user",
            "age",
            "non-null",
        ],
        vec!["drop", "relationship", "armory/public/user", "tool_id"],
        vec!["drop", "cascade-delete", "armory/public/user", "tool_id"],
        vec!["drop", "trigger", "armory/public/user", "on_upsert"],
    ] {
        let args = args.into_iter().map(ToOwned::to_owned).collect::<Vec<_>>();
        assert!(run_command(&client, &args, false).is_ok());
    }

    let metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&metadata_path).unwrap()).unwrap();
    assert!(metadata["relationship_constraints"]["tool_id"].is_null());
    assert!(metadata["cascade_delete_rules"]["tool_id"].is_null());
    assert!(metadata["delete_rules"]["tool_id"].is_null());
    assert!(
        !metadata["unique_constraints"]
            .as_array()
            .unwrap()
            .contains(&json!("email"))
    );
    assert!(
        !metadata["schema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("age"))
    );
    assert!(metadata["schema"]["x-wardrobe-cli"]["indexes"]["tool.type"].is_null());
    assert!(metadata["schema"]["x-wardrobe-cli"]["triggers"]["on_upsert"].is_null());

    let invalid_relationship = vec![
        "alter".to_string(),
        "relationship".to_string(),
        "armory/public/user".to_string(),
        "tool_id".to_string(),
    ];
    assert!(run_command(&client, &invalid_relationship, false).is_err());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_backup_and_restore_commands_execution_paths() {
    let storage_directory = temp_storage_directory("backup_restore_workflows");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    for args in [
        vec!["create", "wardrobe", "armory"],
        vec!["create", "bay", "armory/public"],
        vec!["create", "drawer", "armory/public/gem"],
    ] {
        let args = args.into_iter().map(ToOwned::to_owned).collect::<Vec<_>>();
        assert!(run_command(&client, &args, false).is_ok());
    }

    let upsert = vec![
        "upsert".to_string(),
        "armory/public/gem".to_string(),
        "{\"_id\":\"ruby\",\"power\":42}".to_string(),
    ];
    assert!(run_command(&client, &upsert, false).is_ok());

    let drawer_archive = temp_storage_directory("drawer_backup").with_extension("wrb");
    let drawer_backup = vec![
        "backup".to_string(),
        "armory/public/gem".to_string(),
        drawer_archive.to_string_lossy().to_string(),
    ];
    assert!(run_command(&client, &drawer_backup, true).is_ok());
    assert!(drawer_archive.is_file());

    let drawer_restore = vec![
        "restore".to_string(),
        "armory/public/gem_copy".to_string(),
        drawer_archive.to_string_lossy().to_string(),
    ];
    assert!(run_command(&client, &drawer_restore, false).is_ok());
    assert_eq!(
        client
            .count(
                OperationFilter::drawer("armory/public/gem_copy"),
                None::<OperationOptions>
            )
            .unwrap(),
        1
    );

    let bay_archive = temp_storage_directory("bay_backup").with_extension("wrb");
    let bay_backup = vec![
        "backup".to_string(),
        "armory/public".to_string(),
        bay_archive.to_string_lossy().to_string(),
    ];
    assert!(run_command(&client, &bay_backup, false).is_ok());

    let bay_restore = vec![
        "restore".to_string(),
        "armory_copy/public".to_string(),
        bay_archive.to_string_lossy().to_string(),
    ];
    assert!(run_command(&client, &bay_restore, false).is_ok());
    assert_eq!(
        client
            .count(
                OperationFilter::drawer("armory_copy/public/gem"),
                None::<OperationOptions>
            )
            .unwrap(),
        1
    );

    let wardrobe_archive = temp_storage_directory("wardrobe_backup").with_extension("wrb");
    let wardrobe_backup = vec![
        "backup".to_string(),
        "armory".to_string(),
        wardrobe_archive.to_string_lossy().to_string(),
    ];
    assert!(run_command(&client, &wardrobe_backup, false).is_ok());

    let wardrobe_restore = vec![
        "restore".to_string(),
        "vault".to_string(),
        wardrobe_archive.to_string_lossy().to_string(),
    ];
    assert!(run_command(&client, &wardrobe_restore, false).is_ok());
    assert_eq!(
        client
            .count(
                OperationFilter::drawer("vault/public/gem"),
                None::<OperationOptions>
            )
            .unwrap(),
        1
    );

    let invalid_archive = temp_storage_directory("invalid_backup").with_extension("wrb");
    fs::write(&invalid_archive, "not a wardrobe archive").unwrap();
    let invalid_restore = vec![
        "restore".to_string(),
        "broken/public".to_string(),
        invalid_archive.to_string_lossy().to_string(),
    ];
    assert!(run_command(&client, &invalid_restore, false).is_err());

    let mismatched_scope_restore = vec![
        "restore".to_string(),
        "armory/mismatch".to_string(),
        drawer_archive.to_string_lossy().to_string(),
    ];
    assert!(run_command(&client, &mismatched_scope_restore, false).is_err());

    let _ = fs::remove_file(drawer_archive);
    let _ = fs::remove_file(bay_archive);
    let _ = fs::remove_file(wardrobe_archive);
    let _ = fs::remove_file(invalid_archive);
    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_schema_creation_rejects_missing_parent_database() {
    let storage_directory = temp_storage_directory("missing_parent");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    let args = vec![
        "create".to_string(),
        "bay".to_string(),
        "missing_db/public".to_string(),
    ];
    assert!(run_command(&client, &args, false).is_err());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_removed_data_command_aliases_are_rejected() {
    let storage_directory = temp_storage_directory("data_aliases");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    let insert_args = vec![
        "insert".to_string(),
        "gem".to_string(),
        "{\"_id\":\"@gem:lnk_amethyst\",\"power\":88}".to_string(),
    ];
    assert!(run_command(&client, &insert_args, false).is_err());

    let find_args = vec![
        "find".to_string(),
        "gem".to_string(),
        "{\"power\":88}".to_string(),
    ];
    assert!(run_command(&client, &find_args, false).is_err());

    let create_alias_args = vec![
        "create".to_string(),
        "gem".to_string(),
        "{\"_id\":\"@gem:lnk_sapphire\",\"power\":99}".to_string(),
    ];
    assert!(run_command(&client, &create_alias_args, false).is_err());

    let remove_args = vec![
        "remove".to_string(),
        "gem".to_string(),
        "{\"_id\":\"@gem:amethyst\"}".to_string(),
    ];
    assert!(run_command(&client, &remove_args, false).is_err());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_network_metadata_commands_routing() {
    let target = spawn_protocol_server(|mut stream| {
        let _req = wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
            .expect("read");
        let payload = serde_json::to_vec(&CommandResult::Status(StatusResult::Schemas(vec![
            "public".to_string(),
        ])))
        .expect("ser");
        wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
            wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
            payload,
        )
        .write_to_stream(&mut stream)
        .expect("write");
    });

    let client = WardrobeClient::open(&target).unwrap();

    let schema_args = vec![
        "status".to_string(),
        "bays".to_string(),
        "main_db".to_string(),
    ];
    assert!(run_command(&client, &schema_args, true).is_ok());

    let target_drawers = spawn_protocol_server(|mut stream| {
        let _req = wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
            .expect("read");
        let payload = serde_json::to_vec(&CommandResult::Status(StatusResult::Drawers(vec![
            StorageInventory {
                name: "weapon".to_string(),
                record_count: 4,
                disk_size_bytes: 4096,
                register_file_count: 2,
            },
        ])))
        .expect("ser");
        wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
            wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
            payload,
        )
        .write_to_stream(&mut stream)
        .expect("write");
    });

    let client_drawers = WardrobeClient::open(&target_drawers).unwrap();
    let drawer_args = vec![
        "status".to_string(),
        "drawers".to_string(),
        "main_db/public".to_string(),
    ];
    assert!(run_command(&client_drawers, &drawer_args, false).is_ok());
}

#[test]
fn test_network_administrative_commands_routing() {
    let target = spawn_protocol_server(|mut stream| {
        let request =
            wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
                .expect("read");
        let command: Command = serde_json::from_slice(&request.payload).expect("command");
        assert_eq!(
            command,
            Command::Create(CreateRequest::database("admin_db"))
        );
        let payload = serde_json::to_vec(&CommandResult::Create(CreateResult::StorageInventory(
            StorageInventory {
                name: "admin_db".to_string(),
                record_count: 0,
                disk_size_bytes: 0,
                register_file_count: 1,
            },
        )))
        .expect("ser");
        wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
            wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
            payload,
        )
        .write_to_stream(&mut stream)
        .expect("write");
    });

    let client = WardrobeClient::open(&target).unwrap();
    let create_db_args = vec![
        "create".to_string(),
        "wardrobe".to_string(),
        "admin_db".to_string(),
    ];
    assert!(run_command(&client, &create_db_args, false).is_ok());

    let target_manage = spawn_protocol_server(|mut stream| {
        let request =
            wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
                .expect("read");
        let command: Command = serde_json::from_slice(&request.payload).expect("command");
        assert_eq!(
            command,
            Command::Grant(PermissionRequest::with_scope(
                "alice",
                "armory/public:rud",
                "armory/public",
                "rud"
            ))
        );
        let payload = serde_json::to_vec(&CommandResult::Grant(serde_json::json!({"ok": true})))
            .expect("ser");
        wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
            wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
            payload,
        )
        .write_to_stream(&mut stream)
        .expect("write");
    });

    let client_manage = WardrobeClient::open(&target_manage).unwrap();
    let manage_args = vec![
        "grant".to_string(),
        "permission".to_string(),
        "alice".to_string(),
        "armory/public:rud".to_string(),
    ];
    assert!(run_command(&client_manage, &manage_args, false).is_ok());
}

#[test]
fn test_documented_user_admin_permission_commands_routing() {
    assert_manage_user_command(
        &[
            "create",
            "user",
            "{\"username\":\"dev_admin\",\"role\":\"operator\"}",
        ],
        "add_user",
        json!({"username": "dev_admin", "role": "operator"}),
    );

    assert_manage_user_command(
        &["grant", "permission", "dev_admin", "my_wardrobe/my_bay:rud"],
        "grant_permission",
        json!({
            "username": "dev_admin",
            "permission_scope": "my_wardrobe/my_bay:rud",
            "scope": {
                "path": "my_wardrobe/my_bay",
                "rights": "rud"
            }
        }),
    );

    assert_manage_user_command(
        &[
            "revoke",
            "permission",
            "dev_admin",
            "my_wardrobe/my_bay/user:D",
        ],
        "revoke_permission",
        json!({
            "username": "dev_admin",
            "permission_scope": "my_wardrobe/my_bay/user:d",
            "scope": {
                "path": "my_wardrobe/my_bay/user",
                "rights": "d"
            }
        }),
    );
}

#[test]
fn test_removed_admin_aliases_are_rejected() {
    let storage_directory = temp_storage_directory("removed_admin_aliases");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    for args in [
        vec![
            "auth".to_string(),
            "user".to_string(),
            "grant".to_string(),
            "{\"username\":\"alice\"}".to_string(),
        ],
        vec![
            "rbac".to_string(),
            "user".to_string(),
            "revoke".to_string(),
            "{\"username\":\"bob\"}".to_string(),
        ],
        vec![
            "manage".to_string(),
            "user".to_string(),
            "grant".to_string(),
            "{\"username\":\"carol\"}".to_string(),
        ],
    ] {
        assert!(run_command(&client, &args, false).is_err());
    }

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_embedded_manage_user_is_rejected() {
    let storage_directory = temp_storage_directory("embedded_manage_user");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    let args = vec![
        "manage".to_string(),
        "user".to_string(),
        "grant".to_string(),
        "{\"user\":\"alice\"}".to_string(),
    ];
    assert!(run_command(&client, &args, false).is_err());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_embedded_user_admin_permission_commands_update_local_ledger() {
    let storage_directory = temp_storage_directory("embedded_user_admin_ledger");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    let create_user = vec![
        "create".to_string(),
        "user".to_string(),
        "{\"username\":\"dev_admin\",\"role\":\"operator\"}".to_string(),
    ];
    assert!(run_command(&client, &create_user, false).is_ok());

    let grant_permission = vec![
        "grant".to_string(),
        "permission".to_string(),
        "dev_admin".to_string(),
        "my_wardrobe/my_bay:rud".to_string(),
    ];
    assert!(run_command(&client, &grant_permission, false).is_ok());

    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(storage_directory.join("_wardrobe_access_control.json"))
            .expect("local access-control ledger should exist"),
    )
    .expect("local access-control ledger should parse");
    assert_eq!(registry["users"]["dev_admin"]["username"], "dev_admin");
    assert_eq!(
        registry["users"]["dev_admin"]["permissions"],
        json!(["my_wardrobe/my_bay:rud"])
    );

    let revoke_permission = vec![
        "revoke".to_string(),
        "permission".to_string(),
        "dev_admin".to_string(),
        "my_wardrobe/my_bay:rud".to_string(),
    ];
    assert!(run_command(&client, &revoke_permission, false).is_ok());

    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(storage_directory.join("_wardrobe_access_control.json"))
            .expect("local access-control ledger should exist"),
    )
    .expect("local access-control ledger should parse");
    assert_eq!(registry["users"]["dev_admin"]["permissions"], json!([]));

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_documented_user_admin_validation_errors() {
    let storage_directory = temp_storage_directory("documented_user_admin_validation");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    let missing_username = vec![
        "create".to_string(),
        "user".to_string(),
        "{\"role\":\"operator\"}".to_string(),
    ];
    let err = run_command(&client, &missing_username, false).expect_err("username guard");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

    let invalid_scope_right = vec![
        "grant".to_string(),
        "permission".to_string(),
        "dev_admin".to_string(),
        "my_wardrobe/my_bay:x".to_string(),
    ];
    let err = run_command(&client, &invalid_scope_right, false).expect_err("right guard");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

    let invalid_scope_path = vec![
        "revoke".to_string(),
        "permission".to_string(),
        "dev_admin".to_string(),
        "my_wardrobe/my_bay/user/extra:r".to_string(),
    ];
    let err = run_command(&client, &invalid_scope_path, false).expect_err("path guard");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

    let valid_embedded_scope = vec![
        "grant".to_string(),
        "permission".to_string(),
        "dev_admin".to_string(),
        "my_wardrobe:r".to_string(),
    ];
    assert!(run_command(&client, &valid_embedded_scope, false).is_ok());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_network_diagnostic_and_recovery_commands_routing() {
    let target_drawers = spawn_protocol_server(|mut stream| {
        let request =
            wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
                .expect("read");
        let command: Command = serde_json::from_slice(&request.payload).expect("command");
        assert_eq!(command, Command::Status(StatusRequest::drawer_names()));
        let payload = serde_json::to_vec(&CommandResult::Status(StatusResult::DrawerNames(vec![
            "armory/public/gem".to_string(),
        ])))
        .expect("ser");
        wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
            wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
            payload,
        )
        .write_to_stream(&mut stream)
        .expect("write");
    });
    let client_drawers = WardrobeClient::open(&target_drawers).unwrap();
    assert!(
        run_command(
            &client_drawers,
            &["status".to_string(), "drawer-names".to_string()],
            false,
        )
        .is_ok()
    );

    let target_diagnose = spawn_protocol_server(|mut stream| {
        let request =
            wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
                .expect("read");
        let command: Command = serde_json::from_slice(&request.payload).expect("command");
        assert_eq!(command, Command::Status(StatusRequest::storage()));
        let payload = serde_json::to_vec(&CommandResult::Status(StatusResult::Storage(
            wardrobe_core::StorageDiagnosis {
                storage_directory: "/srv/wardrobe".to_string(),
                storage_bytes: 4096,
                data_bytes: 2048,
                index_bytes: 1024,
                metadata_bytes: 256,
                logical_wal_bytes: 512,
                transaction_wal_bytes: 128,
                other_bytes: 128,
                drawer_count: 1,
                status: "ok".to_string(),
                drawers: vec!["armory/public/gem".to_string()],
            },
        )))
        .expect("ser");
        wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
            wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
            payload,
        )
        .write_to_stream(&mut stream)
        .expect("write");
    });
    let client_diagnose = WardrobeClient::open(&target_diagnose).unwrap();
    assert!(
        run_command(
            &client_diagnose,
            &["status".to_string(), "storage".to_string()],
            false,
        )
        .is_ok()
    );

    let target_inspect = spawn_protocol_server(|mut stream| {
        let request =
            wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
                .expect("read");
        let command: Command = serde_json::from_slice(&request.payload).expect("command");
        assert_eq!(
            command,
            Command::Inspect {
                filter: OperationFilter::drawer("armory/public/gem"),
                options: OperationOptions::default(),
            }
        );
        let payload = serde_json::to_vec(&CommandResult::Inspect(InspectResult::Drawer(
            wardrobe_core::DrawerInspectionMetrics {
                path: "armory/public/gem".to_string(),
                data_bytes: 10,
                index_bytes: 5,
                meta_bytes: 3,
                total_bytes: 18,
                record_count: 1,
                register_file_count: 3,
                tombstone_fragmentation_percent: None,
            },
        )))
        .expect("ser");
        wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
            wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
            payload,
        )
        .write_to_stream(&mut stream)
        .expect("write");
    });
    let client_inspect = WardrobeClient::open(&target_inspect).unwrap();
    assert!(
        run_command(
            &client_inspect,
            &["inspect".to_string(), "armory/public/gem".to_string()],
            false,
        )
        .is_ok()
    );

    let target_check = spawn_protocol_server(|mut stream| {
        let request =
            wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
                .expect("read");
        let command: Command = serde_json::from_slice(&request.payload).expect("command");
        assert_eq!(
            command,
            Command::Status(StatusRequest::path("armory/public/gem"))
        );
        let payload = serde_json::to_vec(&CommandResult::Status(StatusResult::Check(
            wardrobe_core::CheckReport {
                path: "armory/public/gem".to_string(),
                kind: "drawer".to_string(),
                entries: vec![wardrobe_core::CheckEntry {
                    label: "data".to_string(),
                    path: "/srv/wardrobe/armory/public/gem.drw".to_string(),
                    exists: true,
                    bytes: Some(10),
                }],
            },
        )))
        .expect("ser");
        wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
            wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
            payload,
        )
        .write_to_stream(&mut stream)
        .expect("write");
    });
    let client_check = WardrobeClient::open(&target_check).unwrap();
    assert!(
        run_command(
            &client_check,
            &[
                "status".to_string(),
                "path".to_string(),
                "armory/public/gem".to_string()
            ],
            false,
        )
        .is_ok()
    );

    let archive = wardrobe_core::BackupArchive {
        format: "wardrobe-cli-backup-v1".to_string(),
        source_path: "armory/public/gem".to_string(),
        scope: "drawer".to_string(),
        files: vec![wardrobe_core::BackupArchiveFile {
            path: "gem.drw".to_string(),
            bytes_hex: "00".to_string(),
        }],
    };
    let backup_path = temp_storage_directory("network_backup").with_extension("wrb");
    let target_backup = {
        let archive = archive.clone();
        spawn_protocol_server(move |mut stream| {
            let request =
                wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
                    .expect("read");
            let command: Command = serde_json::from_slice(&request.payload).expect("command");
            assert_eq!(
                command,
                Command::Backup {
                    source_path: "armory/public/gem".to_string()
                }
            );
            let payload = serde_json::to_vec(&CommandResult::Backup(archive.clone())).expect("ser");
            wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
                wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
                payload,
            )
            .write_to_stream(&mut stream)
            .expect("write");
        })
    };
    let client_backup = WardrobeClient::open(&target_backup).unwrap();
    assert!(
        run_command(
            &client_backup,
            &[
                "backup".to_string(),
                "armory/public/gem".to_string(),
                backup_path.to_string_lossy().to_string(),
            ],
            false,
        )
        .is_ok()
    );
    assert!(backup_path.is_file());

    let target_restore = {
        let archive = archive.clone();
        spawn_protocol_server(move |mut stream| {
            let request =
                wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
                    .expect("read");
            let command: Command = serde_json::from_slice(&request.payload).expect("command");
            assert_eq!(
                command,
                Command::Restore {
                    destination_path: "armory/public/gem_copy".to_string(),
                    archive: archive.clone(),
                }
            );
            let payload =
                serde_json::to_vec(&CommandResult::Restore(wardrobe_core::RestoreReport {
                    destination_path: "armory/public/gem_copy".to_string(),
                    scope: "drawer".to_string(),
                    file_count: 1,
                    byte_count: 1,
                }))
                .expect("ser");
            wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
                wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
                payload,
            )
            .write_to_stream(&mut stream)
            .expect("write");
        })
    };
    let client_restore = WardrobeClient::open(&target_restore).unwrap();
    assert!(
        run_command(
            &client_restore,
            &[
                "restore".to_string(),
                "armory/public/gem_copy".to_string(),
                backup_path.to_string_lossy().to_string(),
            ],
            false,
        )
        .is_ok()
    );

    let _ = fs::remove_file(backup_path);
}

#[test]
fn test_empty_and_whitespace_command_handling() {
    let storage_directory = temp_storage_directory("empty_commands");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    assert!(run_command(&client, &[], false).is_ok());

    let _ = fs::remove_dir_all(storage_directory);
}
