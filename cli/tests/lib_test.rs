use serde_json::json;
use std::fs;
use std::net::TcpStream;
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_cli::{
    CliConfig, print_json, pub_normalize_record_ids, run_cli_logic, run_command, shell_split,
};
use wardrobe_core::{Command, CommandResult, StorageInventory, WardrobeClient, WardrobeEngine};

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
        assert_eq!(
            command,
            Command::ManageUser {
                action: expected_action.clone(),
                payload: payload.clone()
            }
        );
        let payload = serde_json::to_vec(&CommandResult::Admin(serde_json::json!({"ok": true})))
            .expect("ser");
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
        "--connection".to_string(),
        "custom_dir".to_string(),
        "--pretty".to_string(),
        "records".to_string(),
        "character".to_string(),
    ];
    let config = CliConfig::from_args(args).unwrap();
    assert_eq!(config.connection, "custom_dir");
    assert!(config.pretty);
    assert_eq!(config.command_parts, vec!["records", "character"]);
}

#[test]
fn test_cli_config_alternate_flags() {
    let args = vec![
        "--data-dir".to_string(),
        "alt_dir".to_string(),
        "drawers".to_string(),
    ];
    let config = CliConfig::from_args(args).unwrap();
    assert_eq!(config.connection, "alt_dir");
    assert_eq!(config.command_parts, vec!["drawers"]);
}

#[test]
fn test_cli_config_missing_target_payload() {
    let args = vec!["--target".to_string()];
    let result = CliConfig::from_args(args);
    assert!(result.is_err());
}

#[test]
fn test_shell_split_whitespace_handling() {
    let parts = shell_split("   diagnose    gem   payload_data   ");
    assert_eq!(parts, vec!["diagnose", "gem", "payload_data"]);
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
            .upsert("gem", json!({ "_id": "@gem:lnk_fire", "element": "Fire" }))
            .expect("insert");
    }

    let config = CliConfig {
        connection: storage_directory.to_string_lossy().to_string(),
        pretty: false,
        command_parts: vec!["drawers".to_string()],
    };
    assert!(run_cli_logic(config).is_ok());

    let config_diag = CliConfig {
        connection: storage_directory.to_string_lossy().to_string(),
        pretty: false,
        command_parts: vec!["diagnose".to_string()],
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
            .upsert("gem", json!({ "_id": "@gem:lnk_fire", "element": "Fire" }))
            .expect("insert");
    }

    let config_inspect = CliConfig {
        connection: storage_directory.to_string_lossy().to_string(),
        pretty: true,
        command_parts: vec!["inspect".to_string(), "gem".to_string()],
    };
    assert!(run_cli_logic(config_inspect).is_ok());

    let config_records = CliConfig {
        connection: storage_directory.to_string_lossy().to_string(),
        pretty: false,
        command_parts: vec!["records".to_string(), "gem".to_string()],
    };
    assert!(run_cli_logic(config_records).is_ok());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_network_show_commands_via_library() {
    let target = spawn_protocol_server(|mut stream| {
        let _req = wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
            .expect("read");
        let payload = serde_json::to_vec(&CommandResult::Databases(vec![StorageInventory {
            name: "net_db".to_string(),
            record_count: 1,
            disk_size_bytes: 512,
            register_file_count: 1,
        }]))
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
        command_parts: vec!["show-databases".to_string()],
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

    let res_records = run_command(&client, &["records".to_string()], false);
    assert!(res_records.is_err());

    let res_count = run_command(&client, &["count".to_string()], false);
    assert!(res_count.is_err());

    let res_upsert = run_command(&client, &["upsert".to_string()], false);
    assert!(res_upsert.is_err());

    let res_delete = run_command(&client, &["delete-by-id".to_string()], false);
    assert!(res_delete.is_err());

    let res_schemas = run_command(&client, &["show-schemas".to_string()], false);
    assert!(res_schemas.is_err());

    let res_drawers = run_command(&client, &["show-drawers".to_string()], false);
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

    let delete_args = vec!["delete-by-id".to_string(), "@gem:lnk_topaz".to_string()];
    assert!(run_command(&client, &delete_args, false).is_ok());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_embedded_administrative_setup_commands_execution_paths() {
    let storage_directory = temp_storage_directory("admin_setup");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    let define_database = vec![
        "define".to_string(),
        "database".to_string(),
        "admin_db".to_string(),
    ];
    assert!(run_command(&client, &define_database, false).is_ok());

    let define_schema = vec![
        "define".to_string(),
        "schema".to_string(),
        "admin_db".to_string(),
        "public".to_string(),
    ];
    assert!(run_command(&client, &define_schema, false).is_ok());

    let define_drawer = vec![
        "define".to_string(),
        "drawer".to_string(),
        "admin_db".to_string(),
        "public".to_string(),
        "gem".to_string(),
    ];
    assert!(run_command(&client, &define_drawer, true).is_ok());

    let show_databases = vec!["show".to_string(), "databases".to_string()];
    assert!(run_command(&client, &show_databases, false).is_ok());

    let list_schemas = vec![
        "list".to_string(),
        "schemas".to_string(),
        "admin_db".to_string(),
    ];
    assert!(run_command(&client, &list_schemas, false).is_ok());

    let ls_drawers = vec![
        "ls".to_string(),
        "drawers".to_string(),
        "admin_db".to_string(),
        "public".to_string(),
    ];
    assert!(run_command(&client, &ls_drawers, false).is_ok());

    let databases = client.show_databases().expect("databases");
    assert!(databases.iter().any(|db| db.name == "admin_db"));
    assert!(
        client
            .show_schemas("admin_db")
            .expect("schemas")
            .contains(&"public".to_string())
    );
    assert!(
        client
            .show_drawers("admin_db", "public")
            .expect("drawers")
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

    let show_wardrobes = vec!["show".to_string(), "wardrobes".to_string()];
    assert!(run_command(&client, &show_wardrobes, false).is_ok());

    let show_bays = vec!["show".to_string(), "bays".to_string(), "armory".to_string()];
    assert!(run_command(&client, &show_bays, false).is_ok());

    let show_drawers = vec![
        "show".to_string(),
        "drawers".to_string(),
        "armory/public".to_string(),
    ];
    assert!(run_command(&client, &show_drawers, false).is_ok());

    let check_wardrobe = vec!["check".to_string(), "armory".to_string()];
    assert!(run_command(&client, &check_wardrobe, false).is_ok());

    let check_drawer = vec!["check".to_string(), "armory/public/gem".to_string()];
    assert!(run_command(&client, &check_drawer, false).is_ok());

    let upsert = vec![
        "upsert".to_string(),
        "armory/public/gem".to_string(),
        "{\"_id\":\"ruby\",\"power\":42}".to_string(),
    ];
    assert!(run_command(&client, &upsert, false).is_ok());

    let clean_drawer = vec!["clean".to_string(), "armory/public/gem".to_string()];
    assert!(run_command(&client, &clean_drawer, false).is_ok());

    let clean_bay = vec!["clean".to_string(), "armory/public".to_string()];
    assert!(run_command(&client, &clean_bay, false).is_ok());

    let clean_wardrobe = vec!["clean".to_string(), "armory".to_string()];
    assert!(run_command(&client, &clean_wardrobe, false).is_ok());

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
            &["records".to_string(), "armory/public/gem".to_string()],
            false
        )
        .is_ok()
    );
    assert!(
        run_command(
            &client,
            &[
                "records".to_string(),
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
                "find".to_string(),
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
            .count("armory/public/gem", Some(json!({"element": "fire"})), None)
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
    assert_eq!(client.count("armory/public/gem", None, None).unwrap(), 0);

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
                "records".to_string(),
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
        vec!["add", "index", "armory/public/user", "tool.type"],
        vec![
            "add",
            "key",
            "armory/public/user",
            "profile_id",
            "secondary",
        ],
        vec!["add", "constraint", "armory/public/user", "email", "unique"],
        vec!["add", "constraint", "armory/public/user", "age", "non-null"],
        vec![
            "add",
            "relationship",
            "armory/public/user",
            "tool_id",
            "armory/public/tool",
        ],
        vec!["add", "cascade-delete", "armory/public/user", "tool_id"],
        vec![
            "add",
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
        vec!["remove", "index", "armory/public/user", "tool.type"],
        vec![
            "remove",
            "constraint",
            "armory/public/user",
            "email",
            "unique",
        ],
        vec![
            "remove",
            "constraint",
            "armory/public/user",
            "age",
            "non-null",
        ],
        vec!["remove", "relationship", "armory/public/user", "tool_id"],
        vec!["remove", "cascade-delete", "armory/public/user", "tool_id"],
        vec!["remove", "trigger", "armory/public/user", "on_upsert"],
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
        "add".to_string(),
        "relationship".to_string(),
        "armory/public/user".to_string(),
        "tool_id".to_string(),
    ];
    assert!(run_command(&client, &invalid_relationship, false).is_err());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_schema_creation_rejects_missing_parent_database() {
    let storage_directory = temp_storage_directory("missing_parent");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    let args = vec![
        "create-schema".to_string(),
        "missing_db".to_string(),
        "public".to_string(),
    ];
    assert!(run_command(&client, &args, false).is_err());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_data_command_aliases_execution_paths() {
    let storage_directory = temp_storage_directory("data_aliases");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    let insert_args = vec![
        "insert".to_string(),
        "gem".to_string(),
        "{\"_id\":\"@gem:lnk_amethyst\",\"power\":88}".to_string(),
    ];
    assert!(run_command(&client, &insert_args, false).is_ok());

    let find_args = vec![
        "find".to_string(),
        "gem".to_string(),
        "{\"power\":88}".to_string(),
    ];
    assert!(run_command(&client, &find_args, false).is_ok());

    let create_alias_args = vec![
        "create".to_string(),
        "gem".to_string(),
        "{\"_id\":\"@gem:lnk_sapphire\",\"power\":99}".to_string(),
    ];
    assert!(run_command(&client, &create_alias_args, false).is_ok());

    let remove_args = vec![
        "remove".to_string(),
        "gem".to_string(),
        "{\"_id\":\"@gem:amethyst\"}".to_string(),
    ];
    assert!(run_command(&client, &remove_args, false).is_ok());

    let _ = fs::remove_dir_all(storage_directory);
}

#[test]
fn test_network_metadata_commands_routing() {
    let target = spawn_protocol_server(|mut stream| {
        let _req = wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
            .expect("read");
        let payload =
            serde_json::to_vec(&CommandResult::Schemas(vec!["public".to_string()])).expect("ser");
        wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
            wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
            payload,
        )
        .write_to_stream(&mut stream)
        .expect("write");
    });

    let client = WardrobeClient::open(&target).unwrap();

    let schema_args = vec!["show-schemas".to_string(), "main_db".to_string()];
    assert!(run_command(&client, &schema_args, true).is_ok());

    let target_drawers = spawn_protocol_server(|mut stream| {
        let _req = wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
            .expect("read");
        let payload = serde_json::to_vec(&CommandResult::Drawers(vec![StorageInventory {
            name: "weapon".to_string(),
            record_count: 4,
            disk_size_bytes: 4096,
            register_file_count: 2,
        }]))
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
        "show-drawers".to_string(),
        "main_db".to_string(),
        "public".to_string(),
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
            Command::DefineDatabase {
                database_name: "admin_db".to_string()
            }
        );
        let payload = serde_json::to_vec(&CommandResult::StorageInventory(StorageInventory {
            name: "admin_db".to_string(),
            record_count: 0,
            disk_size_bytes: 0,
            register_file_count: 1,
        }))
        .expect("ser");
        wardrobe_core::wrdb_lib::protocol::ProtocolFrame::new(
            wardrobe_core::wrdb_lib::protocol::ProtocolOpcode::Result,
            payload,
        )
        .write_to_stream(&mut stream)
        .expect("write");
    });

    let client = WardrobeClient::open(&target).unwrap();
    let create_db_args = vec!["create-db".to_string(), "admin_db".to_string()];
    assert!(run_command(&client, &create_db_args, false).is_ok());

    let target_manage = spawn_protocol_server(|mut stream| {
        let request =
            wardrobe_core::wrdb_lib::protocol::ProtocolFrame::read_from_stream(&mut stream)
                .expect("read");
        let command: Command = serde_json::from_slice(&request.payload).expect("command");
        assert_eq!(
            command,
            Command::ManageUser {
                action: "grant".to_string(),
                payload: serde_json::json!({"user": "alice"})
            }
        );
        let payload = serde_json::to_vec(&CommandResult::Admin(serde_json::json!({"ok": true})))
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
        "manage".to_string(),
        "user".to_string(),
        "grant".to_string(),
        "{\"user\":\"alice\"}".to_string(),
    ];
    assert!(run_command(&client_manage, &manage_args, false).is_ok());
}

#[test]
fn test_rbac_aliases_parse_nested_user_payloads() {
    assert_manage_user_command(
        &["auth", "user", "grant", "{\"user\":\"alice\"}"],
        "grant",
        json!({"user": "alice"}),
    );

    assert_manage_user_command(
        &["rbac", "user", "revoke", "{\"user\":\"bob\"}"],
        "revoke",
        json!({"user": "bob"}),
    );

    assert_manage_user_command(
        &["auth", "grant", "{\"user\":\"carol\"}"],
        "grant",
        json!({"user": "carol"}),
    );
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
fn test_unsupported_network_commands_emit_warnings() {
    let target = spawn_protocol_server(|_| {});
    let client = WardrobeClient::open(&target).unwrap();

    assert!(run_command(&client, &["drawers".to_string()], false).is_ok());
    assert!(run_command(&client, &["diagnose".to_string()], false).is_ok());
    assert!(run_command(&client, &["inspect".to_string(), "gem".to_string()], false).is_ok());
}

#[test]
fn test_empty_and_whitespace_command_handling() {
    let storage_directory = temp_storage_directory("empty_commands");
    let client = WardrobeClient::open(&storage_directory.to_string_lossy()).unwrap();

    assert!(run_command(&client, &[], false).is_ok());

    let _ = fs::remove_dir_all(storage_directory);
}
