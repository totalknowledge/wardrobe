#![deny(unsafe_code)]

mod config;
mod engine;
mod report;
mod targets;
mod utils;

pub use config::{BenchmarkConfig, LibraryProfile, ParseOutcome, print_help};
pub use engine::{PhaseName, PhaseRecorder, ProgressReporter, TargetSpec, run_benchmark};
pub use report::{BenchmarkReport, PhaseMetrics, TargetReport};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use crate::engine::*;
    use crate::targets::mongodb::*;
    use crate::targets::mysql::*;
    use crate::targets::neo4j::*;
    use crate::targets::sqlite::*;
    use crate::targets::wardrobe_embedded::*;
    use crate::targets::wardrobe_remote::*;
    use crate::targets::*;
    use crate::utils::*;
    use ::mongodb::bson::Bson;
    use neo4rs::BoltType;
    use rusqlite::Connection;
    use serde_json::json;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::env;
    use std::fs;
    use std::io::{self, Error, ErrorKind};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;
    use ::wardrobe_embedded::{
        Command, CommandResult, CreateRequest, DurabilityPolicy, OperationFilter, ReadResult,
        SecurityConfig, SecurityMode, StatusRequest, StorageDiagnosis, StorageInventory,
        WardrobeEngine, initialize_managed_pki, issue_managed_client_certificate,
    };

    #[derive(Debug, Clone)]
    struct MockRunnerState {
        commands: Vec<Command>,
        responses: VecDeque<CommandResult>,
    }

    struct MockWardrobeRunner {
        state: Rc<RefCell<MockRunnerState>>,
    }

    impl WardrobeCommandRunner for MockWardrobeRunner {
        fn execute(&mut self, command: Command) -> io::Result<CommandResult> {
            let mut state = self.state.borrow_mut();
            state.commands.push(command);
            state.responses.pop_front().ok_or_else(|| {
                Error::new(
                    ErrorKind::UnexpectedEof,
                    "mock Wardrobe runner ran out of responses",
                )
            })
        }
    }

    fn scoped_command(command: &Command) -> &Command {
        if let Command::ExecuteInScope { command, .. } = command {
            command.as_ref()
        } else {
            panic!("expected ExecuteInScope command")
        }
    }

    fn filter_contains_query(filter: &OperationFilter) -> bool {
        match filter {
            OperationFilter::Query(_) => true,
            OperationFilter::Many(filters) => filters.iter().any(filter_contains_query),
            _ => false,
        }
    }

    fn filter_contains_pointer(filter: &OperationFilter) -> bool {
        match filter {
            OperationFilter::Pointer(_) => true,
            OperationFilter::Many(filters) => filters.iter().any(filter_contains_pointer),
            _ => false,
        }
    }

    fn tiny_profile() -> LibraryProfile {
        LibraryProfile {
            entity_records: 3,
            book_records: 6,
            chunk_size: 2,
            traversal_queries: 2,
            point_lookups: 3,
            range_lookups: 3,
            delete_by_id_operations: 2,
            purge_buckets: 2,
        }
    }

    fn bolt_field<'a>(row: &'a BoltType, key: &str) -> &'a BoltType {
        let BoltType::Map(map) = row else {
            panic!("row should be encoded as a Bolt map");
        };
        map.value
            .get(&neo4rs::BoltString::from(key))
            .unwrap_or_else(|| panic!("row should contain field '{key}'"))
    }

    fn assert_bolt_string(row: &BoltType, key: &str, expected: &str) {
        let BoltType::String(value) = bolt_field(row, key) else {
            panic!("field '{key}' should be a Bolt string");
        };
        assert_eq!(value.value, expected);
    }

    fn assert_bolt_integer(row: &BoltType, key: &str, expected: i64) {
        let BoltType::Integer(value) = bolt_field(row, key) else {
            panic!("field '{key}' should be a Bolt integer");
        };
        assert_eq!(value.value, expected);
    }

    fn test_namespace() -> WardrobeNamespace {
        WardrobeNamespace {
            database: "wardrobe_benchmark_test".to_string(),
            schema: "library".to_string(),
            generated: false,
        }
    }

    struct FakeTarget {
        name: String,
        calls: Vec<PhaseName>,
        fail_phase: Option<PhaseName>,
    }

    impl FakeTarget {
        fn new() -> Self {
            Self {
                name: "Fake Target".to_string(),
                calls: Vec::new(),
                fail_phase: None,
            }
        }

        fn maybe_fail(&self, phase: PhaseName) -> io::Result<u64> {
            if self.fail_phase == Some(phase) {
                Err(Error::other(format!("failed at {}", phase.label())))
            } else {
                Ok(1)
            }
        }
    }

    impl BenchmarkTarget for FakeTarget {
        fn name(&self) -> &str {
            &self.name
        }

        fn provision_schema(
            &mut self,
            _profile: &LibraryProfile,
            _progress: &ProgressReporter,
        ) -> io::Result<()> {
            Ok(())
        }

        fn massive_ingestion(
            &mut self,
            _profile: &LibraryProfile,
            _recorder: &mut PhaseRecorder,
            _progress: &ProgressReporter,
        ) -> io::Result<u64> {
            self.calls.push(PhaseName::MassiveIngestion);
            self.maybe_fail(PhaseName::MassiveIngestion)
        }

        fn index_mutation(
            &mut self,
            _profile: &LibraryProfile,
            _recorder: &mut PhaseRecorder,
            _progress: &ProgressReporter,
        ) -> io::Result<u64> {
            self.calls.push(PhaseName::IndexMutation);
            self.maybe_fail(PhaseName::IndexMutation)
        }

        fn point_lookup(
            &mut self,
            _profile: &LibraryProfile,
            _recorder: &mut PhaseRecorder,
            _progress: &ProgressReporter,
        ) -> io::Result<u64> {
            self.calls.push(PhaseName::PointLookup);
            self.maybe_fail(PhaseName::PointLookup)
        }

        fn range_lookup(
            &mut self,
            _profile: &LibraryProfile,
            _recorder: &mut PhaseRecorder,
            _progress: &ProgressReporter,
        ) -> io::Result<u64> {
            self.calls.push(PhaseName::RangeLookup);
            self.maybe_fail(PhaseName::RangeLookup)
        }

        fn complex_traversal(
            &mut self,
            _profile: &LibraryProfile,
            _recorder: &mut PhaseRecorder,
            _progress: &ProgressReporter,
        ) -> io::Result<u64> {
            self.calls.push(PhaseName::ComplexTraversal);
            self.maybe_fail(PhaseName::ComplexTraversal)
        }

        fn delete_by_id(
            &mut self,
            _profile: &LibraryProfile,
            _recorder: &mut PhaseRecorder,
            _progress: &ProgressReporter,
        ) -> io::Result<u64> {
            self.calls.push(PhaseName::DeleteById);
            self.maybe_fail(PhaseName::DeleteById)
        }

        fn targeted_purge(
            &mut self,
            _profile: &LibraryProfile,
            _recorder: &mut PhaseRecorder,
            _progress: &ProgressReporter,
        ) -> io::Result<u64> {
            self.calls.push(PhaseName::TargetedPurge);
            self.maybe_fail(PhaseName::TargetedPurge)
        }

        fn compaction(
            &mut self,
            _profile: &LibraryProfile,
            _recorder: &mut PhaseRecorder,
            _progress: &ProgressReporter,
        ) -> io::Result<u64> {
            self.calls.push(PhaseName::Compaction);
            self.maybe_fail(PhaseName::Compaction)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
            Ok(123)
        }
    }

    #[test]
    fn parses_all_targets() {
        let ParseOutcome::Run(config) =
            BenchmarkConfig::from_args(["--targets".to_string(), "all".to_string()])
                .expect("config should parse")
        else {
            panic!("expected run config");
        };
        assert_eq!(config.targets, TargetSpec::all());
    }

    #[test]
    fn benchmark_config_parses_full_option_matrix() {
        let ParseOutcome::Run(config) = BenchmarkConfig::from_args([
            "--targets".to_string(),
            "wardrobe-embedded,wardrobe-remote,sqlite,redb,rocksdb,mongodb,mysql,postgres,neo4j,surrealdb".to_string(),
            "--work-dir".to_string(),
            "target/custom-bench".to_string(),
            "--output".to_string(),
            "target/custom-bench/report.md".to_string(),
            "--quiet".to_string(),
            "--entities".to_string(),
            "7".to_string(),
            "--books".to_string(),
            "11".to_string(),
            "--chunk-size".to_string(),
            "3".to_string(),
            "--traversal-queries".to_string(),
            "5".to_string(),
            "--point-lookups".to_string(),
            "13".to_string(),
            "--range-lookups".to_string(),
            "17".to_string(),
            "--delete-by-id".to_string(),
            "7".to_string(),
            "--purge-buckets".to_string(),
            "4".to_string(),
            "--wardrobe-embedded-path".to_string(),
            "target/custom-bench/embedded".to_string(),
            "--wardrobe-remote-uri".to_string(),
            "wardrobe://127.0.0.1:24842".to_string(),
            "--wardrobe-client-profile".to_string(),
            "target/wardrobe-security/benchmark-client/profile.toml".to_string(),
            "--wardrobe-durability".to_string(),
            "grouped".to_string(),
            "--wardrobe-group-commit-window-ms".to_string(),
            "11".to_string(),
            "--wardrobe-group-commit-max-batch".to_string(),
            "17".to_string(),
            "--wardrobe-database".to_string(),
            "bench_db".to_string(),
            "--wardrobe-schema".to_string(),
            "bench_schema".to_string(),
            "--sqlite-db".to_string(),
            "target/custom-bench/sqlite.db".to_string(),
            "--mongo-uri".to_string(),
            "mongodb://127.0.0.1:27018".to_string(),
            "--mongo-database".to_string(),
            "mongo_suite".to_string(),
            "--mysql-host".to_string(),
            "mysql.local".to_string(),
            "--mysql-port".to_string(),
            "4406".to_string(),
            "--mysql-database".to_string(),
            "mysql_suite".to_string(),
            "--mysql-user".to_string(),
            "benchmark_user".to_string(),
            "--mysql-password-env".to_string(),
            "BENCH_PASSWORD".to_string(),
            "--postgres-host".to_string(),
            "postgres.local".to_string(),
            "--postgres-port".to_string(),
            "5433".to_string(),
            "--postgres-database".to_string(),
            "postgres_suite".to_string(),
            "--postgres-user".to_string(),
            "postgres_user".to_string(),
            "--postgres-password-env".to_string(),
            "POSTGRES_PASSWORD".to_string(),
            "--neo4j-uri".to_string(),
            "127.0.0.1:8687".to_string(),
            "--neo4j-database".to_string(),
            "neo_suite".to_string(),
            "--neo4j-user".to_string(),
            "neo_user".to_string(),
            "--neo4j-password-env".to_string(),
            "NEO_PASSWORD".to_string(),
        ])
        .expect("full benchmark options should parse") else {
            panic!("expected run config");
        };

        assert_eq!(config.targets, TargetSpec::all());
        assert_eq!(config.work_dir, PathBuf::from("target/custom-bench"));
        assert_eq!(
            config.output_path,
            Some(PathBuf::from("target/custom-bench/report.md"))
        );
        assert!(!config.progress_enabled);
        assert_eq!(config.profile.entity_records, 7);
        assert_eq!(config.profile.book_records, 11);
        assert_eq!(config.profile.chunk_size, 3);
        assert_eq!(config.profile.traversal_queries, 5);
        assert_eq!(config.profile.point_lookups, 13);
        assert_eq!(config.profile.range_lookups, 17);
        assert_eq!(config.profile.delete_by_id_operations, 7);
        assert_eq!(config.profile.purge_buckets, 4);
        assert_eq!(
            config.wardrobe_embedded_path,
            Some(PathBuf::from("target/custom-bench/embedded"))
        );
        assert_eq!(
            config.wardrobe_remote_uri,
            Some("wardrobe://127.0.0.1:24842".to_string())
        );
        assert_eq!(
            config.wardrobe_client_profile,
            Some(PathBuf::from(
                "target/wardrobe-security/benchmark-client/profile.toml"
            ))
        );
        assert_eq!(
            config.wardrobe_durability_policy,
            DurabilityPolicy::Grouped {
                commit_window_ms: 11,
                max_batch_size: 17
            }
        );
        assert_eq!(config.wardrobe_database, Some("bench_db".to_string()));
        assert_eq!(config.wardrobe_schema, Some("bench_schema".to_string()));
        assert_eq!(
            config.sqlite_db,
            Some(PathBuf::from("target/custom-bench/sqlite.db"))
        );
        assert_eq!(config.mongo_uri, "mongodb://127.0.0.1:27018");
        assert_eq!(config.mongo_database, "mongo_suite");
        assert_eq!(config.mysql_host, "mysql.local");
        assert_eq!(config.mysql_port, 4406);
        assert_eq!(config.mysql_database, "mysql_suite");
        assert_eq!(config.mysql_user, Some("benchmark_user".to_string()));
        assert_eq!(
            config.mysql_password_env,
            Some("BENCH_PASSWORD".to_string())
        );
        assert_eq!(config.postgres_host, "postgres.local");
        assert_eq!(config.postgres_port, 5433);
        assert_eq!(config.postgres_database, "postgres_suite");
        assert_eq!(config.postgres_user, Some("postgres_user".to_string()));
        assert_eq!(
            config.postgres_password_env,
            Some("POSTGRES_PASSWORD".to_string())
        );
        assert_eq!(config.neo4j_uri, "127.0.0.1:8687");
        assert_eq!(config.neo4j_database, "neo_suite");
        assert_eq!(config.neo4j_user, "neo_user");
        assert_eq!(config.neo4j_password_env, "NEO_PASSWORD");
    }

    #[test]
    fn benchmark_config_supports_no_mysql_password_override() {
        let ParseOutcome::Run(config) = BenchmarkConfig::from_args([
            "--targets".to_string(),
            "mysql".to_string(),
            "--mysql-no-password".to_string(),
        ])
        .expect("mysql no-password flag should parse") else {
            panic!("expected run config");
        };

        assert_eq!(config.targets, vec![TargetSpec::MySql]);
        assert_eq!(config.mysql_password_env, None);
    }

    #[test]
    fn credentials_parser_reads_neo4j_fallback_file_keys() {
        let credentials = parse_credentials(
            r#"
WARDROBE_BENCH_NEO4J_USER=neo4j
WARDROBE_BENCH_NEO4J_PASSWORD=wardrobe_benchmark
IGNORED=value
"#,
            DEFAULT_NEO4J_USER_ENV,
            DEFAULT_NEO4J_PASSWORD_ENV,
        );

        assert_eq!(credentials.user, Some("neo4j".to_string()));
        assert_eq!(credentials.password, Some("wardrobe_benchmark".to_string()));
    }

    #[test]
    fn neo4j_checkpoint_unavailable_predicate_matches_procedure_gap() {
        let missing_checkpoint = Error::other(
            "Neo4j error `Neo.ClientError.Procedure.ProcedureNotFound`: There is no procedure with the name `db.checkpoint` registered",
        );
        let auth_error =
            Error::other("Neo4j error `Neo.ClientError.Security.Unauthorized`: access denied");

        assert!(neo4j_checkpoint_is_unavailable(&missing_checkpoint));
        assert!(!neo4j_checkpoint_is_unavailable(&auth_error));
    }

    #[test]
    fn neo4j_ingestion_queries_use_bulk_unwind_rows() {
        assert!(NEO4J_BULK_ENTITY_UPSERT_QUERY.contains("UNWIND $rows AS row"));
        assert!(NEO4J_BULK_BOOK_UPSERT_QUERY.contains("UNWIND $rows AS row"));
        assert!(NEO4J_BULK_BOOK_UPSERT_QUERY.contains("MATCH (author:BenchNode:Entity"));
        assert!(NEO4J_BULK_BOOK_UPSERT_QUERY.contains("MERGE (b)-[:EDITED_BY]->(editor)"));
        assert!(!NEO4J_BULK_ENTITY_UPSERT_QUERY.contains("$id"));
        assert!(!NEO4J_BULK_BOOK_UPSERT_QUERY.contains("$id"));
    }

    #[test]
    fn neo4j_batch_rows_encode_chunk_payloads_as_one_bolt_list() {
        let profile = tiny_profile();

        let entity_rows = neo4j_entity_rows(&profile, 0, 2);
        let BoltType::List(entity_rows) = entity_rows else {
            panic!("entity rows should be encoded as a Bolt list");
        };
        assert_eq!(entity_rows.value.len(), 2);
        assert_bolt_string(&entity_rows.value[0], "id", "entity_00000000");
        assert_bolt_string(
            &entity_rows.value[0],
            "display_name",
            "Library Entity 00000000",
        );
        assert_bolt_integer(&entity_rows.value[0], "cohort", 0);

        let book_rows = neo4j_book_rows(&profile, 0, 2);
        let BoltType::List(book_rows) = book_rows else {
            panic!("book rows should be encoded as a Bolt list");
        };
        assert_eq!(book_rows.value.len(), 2);
        assert_bolt_string(&book_rows.value[0], "id", "book_00000000");
        assert_bolt_string(&book_rows.value[0], "author_id", "entity_00000000");
        assert_bolt_string(&book_rows.value[0], "editor_id", "entity_00000000");
        assert_bolt_integer(&book_rows.value[0], "quantity", 1);
        assert_bolt_integer(&book_rows.value[1], "purge_bucket", 1);
    }

    #[test]
    fn neo4j_storage_metric_query_reports_store_size_bytes_not_node_count() {
        assert!(NEO4J_STORE_SIZE_BYTES_QUERY.contains("dbms.queryJmx"));
        assert!(NEO4J_STORE_SIZE_BYTES_QUERY.contains("TotalStoreSize.value"));
        assert!(NEO4J_STORE_SIZE_BYTES_QUERY.contains("storage_bytes"));
        assert!(!NEO4J_STORE_SIZE_BYTES_QUERY.contains("count(n)"));
    }

    #[test]
    fn parse_targets_supports_aliases_and_case_insensitive_values() {
        let targets = parse_targets("embedded,REMOTE,redb,mongo,mariadb,neo")
            .expect("target aliases should parse");
        assert_eq!(
            targets,
            vec![
                TargetSpec::WardrobeEmbedded,
                TargetSpec::WardrobeRemote,
                TargetSpec::Redb,
                TargetSpec::MongoDb,
                TargetSpec::MySql,
                TargetSpec::Neo4j,
            ]
        );
    }

    #[test]
    fn parse_targets_rejects_empty_entries() {
        let error =
            parse_targets("wardrobe-embedded,,sqlite").expect_err("empty target entry should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("empty target name"));
    }

    #[test]
    fn benchmark_config_help_flag_returns_help_outcome() {
        let outcome =
            BenchmarkConfig::from_args(["--help".to_string()]).expect("help should parse");
        assert_eq!(outcome, ParseOutcome::Help);
    }

    #[test]
    fn benchmark_config_requires_flag_values() {
        let error = BenchmarkConfig::from_args(["--targets".to_string()])
            .expect_err("missing --targets value should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(
            error
                .to_string()
                .contains("--targets requires a following value")
        );
    }

    #[test]
    fn benchmark_config_rejects_invalid_mysql_port() {
        let error =
            BenchmarkConfig::from_args(["--mysql-port".to_string(), "not-a-port".to_string()])
                .expect_err("invalid mysql port should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("Invalid --mysql-port value"));
    }

    #[test]
    fn benchmark_config_rejects_invalid_wardrobe_namespace_overrides() {
        let error =
            BenchmarkConfig::from_args(["--wardrobe-database".to_string(), "bad/name".to_string()])
                .expect_err("invalid wardrobe database should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("--wardrobe-database"));
    }

    #[test]
    fn wardrobe_namespace_defaults_to_run_isolated_database() {
        let config = BenchmarkConfig::default();
        let namespace =
            WardrobeNamespace::from_config(&config, "run-123").expect("namespace should build");
        assert_eq!(namespace.database, "wardrobe_benchmark_run_123");
        assert_eq!(namespace.schema, DEFAULT_WARDROBE_SCHEMA_NAME);
        assert!(namespace.generated);
    }

    #[test]
    fn external_wardrobe_remote_requires_a_client_profile() {
        let parse_error = BenchmarkConfig::from_args([
            "--targets".to_string(),
            "wardrobe-remote".to_string(),
            "--wardrobe-remote-uri".to_string(),
            "wardrobe://127.0.0.1:24842".to_string(),
        ])
        .expect_err("remote config without a client profile should fail");
        assert_eq!(parse_error.kind(), ErrorKind::InvalidInput);
        assert!(
            parse_error
                .to_string()
                .contains("--wardrobe-client-profile")
        );

        let namespace = WardrobeNamespace::from_config(&BenchmarkConfig::default(), "run-123")
            .expect("namespace should build");
        let error = WardrobeTarget::remote_uri("wardrobe://127.0.0.1:24842", None, namespace)
            .err()
            .expect("remote server without a client profile should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("--wardrobe-client-profile"));
    }

    #[test]
    fn tcp_wardrobe_runner_connects_with_managed_client_profile() {
        let root = env::temp_dir().join(format!(
            "wardrobe_benchmark_tls_runner_{}",
            unix_timestamp_micros()
        ));
        let data_dir = root.join("data");
        let security_dir = root.join("security");
        initialize_managed_pki(
            &security_dir,
            &["localhost".to_string()],
            &["127.0.0.1".parse().expect("IP should parse")],
        )
        .expect("managed PKI should initialize");
        let certificate = issue_managed_client_certificate(
            &security_dir,
            "wardrobe:service:benchmark",
            "test",
            None,
            "localhost",
        )
        .expect("client certificate should issue");
        let engine =
            WardrobeEngine::open(data_dir.to_string_lossy().as_ref()).expect("engine should open");
        engine
            .create(CreateRequest::user(json!({
                "username": "benchmark",
                "role": "administrator",
                "permissions": ["*"],
                "certificate_identities": ["wardrobe:service:benchmark"],
            })))
            .expect("benchmark identity should register");
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let address = listener.local_addr().expect("listener should have address");
        listener
            .set_nonblocking(true)
            .expect("listener should be nonblocking");
        let security = SecurityConfig {
            mode: SecurityMode::Managed,
            security_dir,
            ..SecurityConfig::default()
        };
        let server_thread = std::thread::spawn(move || {
            wardrobe_server::serve_tls_tcp_listener(listener, Arc::new(engine), Some(1), security)
                .expect("TLS listener should serve");
        });

        let runner =
            TcpWardrobeRunner::connect(&format!("wardrobe://{address}"), &certificate.profile)
                .expect("runner should connect");

        assert!(
            runner
                .stream
                .sock
                .nodelay()
                .expect("runner stream should report nodelay")
        );

        drop(runner);
        server_thread.join().expect("server thread should exit");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn benchmark_config_rejects_zero_counts() {
        let error = BenchmarkConfig::from_args(["--entities".to_string(), "0".to_string()])
            .expect_err("zero entities should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("--entities"));
    }

    #[test]
    fn chunk_ranges_splits_exact_and_remainder_segments() {
        assert_eq!(chunk_ranges(6, 3), vec![(0, 3), (3, 6)]);
        assert_eq!(chunk_ranges(7, 3), vec![(0, 3), (3, 6), (6, 7)]);
        assert!(chunk_ranges(0, 3).is_empty());
    }

    #[test]
    fn expected_purge_count_matches_bucket_distribution_formula() {
        let profile = LibraryProfile {
            entity_records: 10,
            book_records: 11,
            chunk_size: 2,
            traversal_queries: 3,
            point_lookups: 4,
            range_lookups: 4,
            delete_by_id_operations: 2,
            purge_buckets: 4,
        };

        assert_eq!(profile.expected_purge_count(), 3);
        assert_eq!(profile.delete_by_id_book_ids().len(), 2);
        assert_eq!(profile.expected_book_records_after_mutating_phases(), 6);
    }

    #[test]
    fn library_profile_generates_joinable_book_payloads() {
        let profile = tiny_profile();

        let entity = profile.entity_payload(2);
        assert_eq!(entity["_id"], "entity_00000002");
        assert_eq!(entity["entity_id"], "entity_00000002");
        assert_eq!(entity["role"], "author");
        assert_eq!(entity["cohort"], 2);

        let book = profile.book_payload(1);
        assert_eq!(book["_id"], "book_00000001");
        assert_eq!(book["author_id"], "entity_00000001");
        assert_eq!(book["editor_id"], "entity_00000000");
        assert_eq!(book["purge_bucket"], 1);
        assert_eq!(profile.traversal_entity_id(4), "entity_00000001");
    }

    #[test]
    fn sql_generators_escape_values_and_use_materialized_joins() {
        let profile = tiny_profile();

        assert_eq!(sql_string("O'Hare"), "'O''Hare'");
        assert_eq!(mysql_identifier("bad`name"), "bad``name");

        let sqlite_entities = sqlite_entity_insert(&profile, 0, 2);
        assert!(sqlite_entities.contains("BEGIN IMMEDIATE;"));
        assert!(sqlite_entities.contains("INSERT OR REPLACE INTO entities"));
        assert!(sqlite_entities.contains("'entity_00000000'"));

        let sqlite_books = sqlite_book_insert(&profile, 0, 1);
        assert!(sqlite_books.contains("INSERT OR REPLACE INTO books"));
        assert!(sqlite_books.contains("'book_00000000'"));
        assert!(sqlite_books.contains("'entity_00000000'"));

        let mysql_entities = mysql_entity_insert(&profile, 0, 2);
        assert!(mysql_entities.contains("ON DUPLICATE KEY UPDATE display_name"));

        let mysql_books = mysql_book_insert(&profile, 0, 1);
        assert!(mysql_books.contains("ON DUPLICATE KEY UPDATE isbn"));

        let mysql_query = mysql_materialized_book_query(&sql_string("entity_00000000"));
        assert!(mysql_query.contains("JOIN entities author ON author.id = b.author_id"));
        assert!(mysql_query.contains("JOIN entities editor ON editor.id = b.editor_id"));
        assert!(mysql_query.contains("WHERE b.author_id = 'entity_00000000'"));
    }

    #[test]
    fn mongo_documents_and_pipeline_preserve_library_shape() {
        let profile = tiny_profile();

        let entities = mongo_documents(&profile, ENTITY_DRAWER, 0, 2).expect("entities convert");
        assert_eq!(entities.len(), 2);
        assert_eq!(
            entities[0].get_str("_id").expect("entity id"),
            "entity_00000000"
        );
        assert_eq!(
            entities[1].get_str("display_name").expect("entity display"),
            "Library Entity 00000001"
        );

        let books = mongo_documents(&profile, BOOK_DRAWER, 0, 1).expect("books convert");
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].get_str("_id").expect("book id"), "book_00000000");
        assert_eq!(
            books[0].get_str("author_id").expect("book author"),
            "entity_00000000"
        );

        let pipeline = mongo_materialized_book_pipeline("entity_00000000");
        assert_eq!(pipeline.len(), 6);
        assert_eq!(
            pipeline[0]
                .get_document("$match")
                .expect("match stage")
                .get_str("author_id")
                .expect("author match"),
            "entity_00000000"
        );
        assert!(pipeline[1].contains_key("$lookup"));
        assert!(pipeline[5].contains_key("$project"));
    }

    #[test]
    fn sqlite_materialized_query_hydrates_author_and_editor_records() {
        let connection = Connection::open_in_memory().expect("sqlite memory open");
        connection
            .execute_batch(
                r#"
CREATE TABLE entities (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    role TEXT NOT NULL,
    cohort INTEGER NOT NULL
);
CREATE TABLE books (
    id TEXT PRIMARY KEY,
    isbn TEXT NOT NULL,
    title TEXT NOT NULL,
    author_id TEXT NOT NULL,
    editor_id TEXT NOT NULL,
    branch TEXT NOT NULL,
    quantity INTEGER NOT NULL,
    purge_bucket INTEGER NOT NULL
);
INSERT INTO entities (id, display_name, role, cohort) VALUES
('entity_00000000', 'Author Zero', 'author', 7),
('entity_00000001', 'Editor One', 'editor', 9);
INSERT INTO books (id, isbn, title, author_id, editor_id, branch, quantity, purge_bucket)
VALUES ('book_00000000', 'isbn-0', 'SQLite Join Book', 'entity_00000000', 'entity_00000000', 'central', 3, 1);
"#,
            )
            .expect("schema and fixtures should load");

        let mut statement = connection
            .prepare(SQLITE_MATERIALIZED_BOOK_QUERY)
            .expect("materialized query should prepare");
        let rows = statement
            .query_map(["entity_00000000"], sqlite_materialized_book_value)
            .expect("materialized query should run");
        let records = rows
            .map(|row| row.expect("row should materialize"))
            .collect::<Vec<_>>();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["_id"], "book_00000000");
        assert_eq!(records[0]["author"]["display_name"], "Author Zero");
        assert_eq!(records[0]["editor"]["role"], "author");
    }

    #[test]
    fn bson_storage_size_numbers_convert_safely() {
        assert_eq!(bson_number_to_u64(Some(&Bson::Int32(42))), Some(42));
        assert_eq!(bson_number_to_u64(Some(&Bson::Int64(42))), Some(42));
        assert_eq!(bson_number_to_u64(Some(&Bson::Double(42.9))), Some(42));
        assert_eq!(bson_number_to_u64(Some(&Bson::Int32(-1))), None);
        assert_eq!(bson_number_to_u64(Some(&Bson::Int64(-1))), None);
        assert_eq!(bson_number_to_u64(Some(&Bson::Double(-1.0))), None);
        assert_eq!(bson_number_to_u64(Some(&Bson::Double(f64::INFINITY))), None);
        assert_eq!(
            bson_number_to_u64(Some(&Bson::String("42".to_string()))),
            None
        );
        assert_eq!(bson_number_to_u64(None), None);
    }

    #[test]
    fn filesystem_helpers_count_nested_files_and_handle_missing_paths() {
        let root = env::temp_dir().join(format!(
            "wardrobe_benchmark_fs_helpers_{}",
            unix_timestamp_micros()
        ));
        let nested = root.join("nested");
        fs::create_dir_all(&nested).expect("nested dir should create");
        fs::write(root.join("root.bin"), [1_u8, 2, 3]).expect("root file should write");
        fs::write(nested.join("child.bin"), [4_u8, 5]).expect("child file should write");

        assert_eq!(directory_size(&root).expect("directory size"), 5);
        assert_eq!(
            file_size_or_zero(root.join("missing.bin")).expect("missing size"),
            0
        );
        assert_eq!(
            sqlite_sidecar(Path::new("library.sqlite"), "-wal"),
            PathBuf::from("library.sqlite-wal")
        );
        sync_file_if_exists(&root.join("missing.bin")).expect("missing sync should be ok");
        fsync_tree(&root).expect("fsync tree should complete");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn common_benchmark_runner_and_reporting_paths_are_exercised() {
        print_help();

        assert_eq!(
            TargetSpec::WardrobeEmbedded.label(),
            "Wardrobe (Embedded Flat-File Mode)"
        );
        assert_eq!(
            TargetSpec::WardrobeRemote.label(),
            "Wardrobe (Remote TCP Server Mode)"
        );
        assert_eq!(TargetSpec::Sqlite.label(), "SQLite (Local WAL File Mode)");
        assert_eq!(
            TargetSpec::Redb.label(),
            "redb (Pure Rust Embedded Key-Value Mode)"
        );
        assert_eq!(
            TargetSpec::MongoDb.label(),
            "MongoDB (Document Store Base Comparison)"
        );
        assert_eq!(
            TargetSpec::MySql.label(),
            "MySQL / MariaDB (Relational Pointer Base Comparison)"
        );
        assert_eq!(
            TargetSpec::Neo4j.label(),
            "Neo4j (Graph Database Base Comparison)"
        );

        assert_eq!(PhaseName::MassiveIngestion.label(), "Massive Ingestion");
        assert_eq!(PhaseName::IndexMutation.label(), "Index Mutation");
        assert_eq!(PhaseName::PointLookup.label(), "Point Lookup");
        assert_eq!(PhaseName::RangeLookup.label(), "Range Lookup");
        assert_eq!(PhaseName::ComplexTraversal.label(), "Complex Traversal");
        assert_eq!(PhaseName::DeleteById.label(), "Delete by ID");
        assert_eq!(PhaseName::TargetedPurge.label(), "Targeted Purge");
        assert_eq!(PhaseName::Compaction.label(), "Compaction");

        let progress = ProgressReporter::new(true);
        progress.log("coverage progress log");
        report_record_progress(&progress, "records", 0, 0);
        report_record_progress(&progress, "records", 1, 2);
        report_record_progress(&progress, "records", 2, 2);

        let mut target = FakeTarget::new();
        let report = run_target(&mut target, &tiny_profile(), &ProgressReporter::new(false))
            .expect("fake target should run");
        assert_eq!(report.name, "Fake Target");
        assert_eq!(report.phases.len(), 8);
        assert_eq!(report.storage_bytes, 123);
        assert_eq!(
            target.calls,
            vec![
                PhaseName::MassiveIngestion,
                PhaseName::PointLookup,
                PhaseName::ComplexTraversal,
                PhaseName::IndexMutation,
                PhaseName::DeleteById,
                PhaseName::TargetedPurge,
                PhaseName::Compaction,
                PhaseName::RangeLookup,
            ]
        );

        let mut failing_target = FakeTarget::new();
        failing_target.fail_phase = Some(PhaseName::ComplexTraversal);
        let error = run_target(
            &mut failing_target,
            &tiny_profile(),
            &ProgressReporter::new(false),
        )
        .expect_err("fake target should fail");
        assert!(error.to_string().contains("Complex Traversal"));

        let unavailable = unavailable_target_report("Unavailable DB", "connection refused".into());
        assert_eq!(unavailable.name, "Unavailable DB");
        assert_eq!(
            unavailable.unavailable_reason.as_deref(),
            Some("connection refused")
        );

        let report = BenchmarkReport {
            profile: tiny_profile(),
            run_dir: PathBuf::from("target/fake"),
            targets: vec![unavailable],
        };
        let markdown = report.to_markdown();
        assert!(markdown.contains("Unavailable DB"));
        assert!(markdown.contains("Storage Diagnostics"));
        assert!(markdown.contains("Unavailable: connection refused"));
    }

    #[test]
    fn parsing_and_small_helpers_cover_error_and_default_branches() {
        assert_eq!(parse_positive_u64("--window", "17").unwrap(), 17);
        assert!(parse_positive_u64("--window", "bad").is_err());
        assert!(parse_positive_u64("--window", "0").is_err());
        assert!(parse_positive_usize("--size", "bad").is_err());

        assert_eq!(
            parse_wardrobe_durability_policy("strict").unwrap(),
            DurabilityPolicy::Strict
        );
        assert_eq!(
            parse_wardrobe_durability_policy("grouped").unwrap(),
            DurabilityPolicy::Grouped {
                commit_window_ms: DEFAULT_WARDROBE_GROUP_COMMIT_WINDOW_MS,
                max_batch_size: DEFAULT_WARDROBE_GROUP_COMMIT_MAX_BATCH
            }
        );
        assert!(parse_wardrobe_durability_policy("eventual").is_err());
        assert_eq!(
            default_grouped_durability_policy(),
            DurabilityPolicy::Grouped {
                commit_window_ms: DEFAULT_WARDROBE_GROUP_COMMIT_WINDOW_MS,
                max_batch_size: DEFAULT_WARDROBE_GROUP_COMMIT_MAX_BATCH
            }
        );
        assert_eq!(
            update_group_commit_window(DurabilityPolicy::Strict, 22),
            DurabilityPolicy::Grouped {
                commit_window_ms: 22,
                max_batch_size: DEFAULT_WARDROBE_GROUP_COMMIT_MAX_BATCH
            }
        );
        assert_eq!(
            update_group_commit_max_batch(DurabilityPolicy::Strict, 33),
            DurabilityPolicy::Grouped {
                commit_window_ms: DEFAULT_WARDROBE_GROUP_COMMIT_WINDOW_MS,
                max_batch_size: 33
            }
        );

        assert_eq!(identifier_fragment("///"), "run");
        assert_eq!(identifier_fragment("run 123!"), "run_123");
        assert_eq!(optional_count(Some(7)), "7");
        assert_eq!(optional_count(None), "unavailable");
        assert_eq!(to_io_error("boom").to_string(), "boom");
        assert!(read_default_mysql_credentials().is_ok());
        assert!(read_default_neo4j_credentials().is_ok());
        assert_eq!(
            read_credentials_file(
                "target/wardrobe-benchmark/missing-credentials.env",
                "USER",
                "PASSWORD",
            )
            .expect("missing credentials file should be ok"),
            ServiceCredentials::default()
        );
        assert_eq!(
            parse_credentials("USER=alice\nPASSWORD=secret\n", "USER", "PASSWORD"),
            ServiceCredentials {
                user: Some("alice".to_string()),
                password: Some("secret".to_string())
            }
        );
        assert_eq!(weighted_percentile(&[], 0.95), 0.0);

        let mut recorder = PhaseRecorder::new(PhaseName::Compaction);
        assert!(recorder.measure(0, || Ok(())).is_err());
        assert!(
            recorder
                .measure(1, || Err::<(), _>(Error::other("nope")))
                .is_err()
        );
        let metrics = recorder.finish();
        assert_eq!(metrics.operations, 0);
        assert_eq!(metrics.ops_per_second, 0.0);
        assert_eq!(metrics.mean_micros, 0.0);
    }

    #[test]
    fn filesystem_sync_helpers_cover_file_and_missing_tree_paths() {
        let root = env::temp_dir().join(format!(
            "wardrobe_benchmark_sync_helpers_{}",
            unix_timestamp_micros()
        ));
        fs::create_dir_all(&root).expect("root should create");
        let file = root.join("data.bin");
        fs::write(&file, b"data").expect("file should write");

        fsync_tree(&file).expect("file fsync should work");
        fsync_tree(&root.join("missing")).expect("missing fsync tree should be ok");
        assert_eq!(directory_size(root.join("missing")).unwrap(), 0);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn wardrobe_expect_helpers_reject_wrong_command_results() {
        assert_eq!(
            expect_inventory(CommandResult::Count(0))
                .expect_err("inventory mismatch")
                .kind(),
            ErrorKind::InvalidData
        );
        assert_eq!(
            expect_pointers(CommandResult::Delete(::wardrobe_embedded::DeleteResult {
                deleted: 0,
            }))
            .expect_err("pointers mismatch")
            .kind(),
            ErrorKind::InvalidData
        );
        assert_eq!(
            expect_records(CommandResult::Count(0))
                .expect_err("records mismatch")
                .kind(),
            ErrorKind::InvalidData
        );
        assert_eq!(
            expect_count(CommandResult::Read(ReadResult::Records(Vec::new())))
                .expect_err("count mismatch")
                .kind(),
            ErrorKind::InvalidData
        );
        assert_eq!(
            expect_vacuumed(CommandResult::Count(0))
                .expect_err("vacuum mismatch")
                .kind(),
            ErrorKind::InvalidData
        );
        assert_eq!(
            expect_admin(CommandResult::Count(0))
                .expect_err("admin mismatch")
                .kind(),
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn weighted_phase_metrics_report_percentiles() {
        let mut recorder = PhaseRecorder::new(PhaseName::MassiveIngestion);
        recorder.samples.push(LatencySample {
            operations: 9,
            elapsed: Duration::from_micros(90),
        });
        recorder.samples.push(LatencySample {
            operations: 1,
            elapsed: Duration::from_micros(100),
        });

        let metrics = recorder.finish();

        assert_eq!(metrics.operations, 10);
        assert_eq!(metrics.total_micros, 190);
        assert_eq!(metrics.mean_micros, 19.0);
        assert_eq!(metrics.p95_micros, 100.0);
        assert_eq!(metrics.p99_micros, 100.0);
    }

    #[test]
    fn markdown_report_contains_required_columns() {
        let report = BenchmarkReport {
            profile: LibraryProfile {
                entity_records: 1,
                book_records: 1,
                chunk_size: 1,
                traversal_queries: 1,
                point_lookups: 1,
                range_lookups: 1,
                delete_by_id_operations: 1,
                purge_buckets: 1,
            },
            run_dir: PathBuf::from("target/test"),
            targets: vec![TargetReport {
                name: "Wardrobe (Embedded Flat-File Mode)".to_string(),
                storage_bytes: 42,
                storage_diagnostics: Vec::new(),
                unavailable_reason: None,
                phases: vec![PhaseMetrics {
                    phase: PhaseName::Compaction,
                    operations: 1,
                    total_micros: 10,
                    ops_per_second: 100_000.0,
                    mean_micros: 10.0,
                    p95_micros: 10.0,
                    p99_micros: 10.0,
                }],
            }],
        };

        let markdown = report.to_markdown();

        assert!(markdown.contains("| Target | Phase | Operations | Total us | OPS | Mean us | p95 us | p99 us | Storage bytes |"));
        assert!(markdown.contains("Wardrobe (Embedded Flat-File Mode)"));
        assert!(markdown.contains("Compaction"));
    }

    #[test]
    fn benchmark_continues_when_target_is_unavailable() {
        let work_dir = env::temp_dir().join(format!(
            "wardrobe_benchmark_unavailable_target_{}",
            unix_timestamp_micros()
        ));
        let config = BenchmarkConfig {
            targets: vec![TargetSpec::WardrobeEmbedded, TargetSpec::Neo4j],
            profile: tiny_profile(),
            work_dir: work_dir.clone(),
            ..BenchmarkConfig::default()
        };

        let report =
            run_benchmark_with_builder(config, |spec, config, run_dir, namespace, progress| {
                match spec {
                    TargetSpec::WardrobeEmbedded => {
                        build_target(spec, config, run_dir, namespace, progress)
                    }
                    TargetSpec::Neo4j => Err(Error::new(
                        ErrorKind::ConnectionRefused,
                        "neo4j connection refused",
                    )),
                    other => Err(Error::other(format!(
                        "unexpected target in test: {other:?}"
                    ))),
                }
            })
            .expect("benchmark should continue even when a target is unavailable");

        assert_eq!(report.targets.len(), 2);
        assert_eq!(report.targets[0].name, "Wardrobe (Embedded Flat-File Mode)");
        assert_eq!(report.targets[0].unavailable_reason, None);
        assert_eq!(
            report.targets[1].name,
            "Neo4j (Graph Database Base Comparison)"
        );
        assert!(report.targets[1].unavailable_reason.is_some());
        assert!(report.targets[1].phases.is_empty());

        let markdown = report.to_markdown();
        assert!(markdown.contains("Neo4j (Graph Database Base Comparison)"));
        assert!(markdown.contains("Unavailable"));

        let _ = fs::remove_dir_all(work_dir);
    }

    #[test]
    fn tiny_wardrobe_embedded_benchmark_runs() {
        let work_dir = env::temp_dir().join(format!(
            "wardrobe_benchmark_test_{}",
            unix_timestamp_micros()
        ));
        let config = BenchmarkConfig {
            targets: vec![TargetSpec::WardrobeEmbedded],
            profile: LibraryProfile {
                entity_records: 3,
                book_records: 6,
                chunk_size: 2,
                traversal_queries: 2,
                point_lookups: 3,
                range_lookups: 3,
                delete_by_id_operations: 2,
                purge_buckets: 2,
            },
            work_dir: work_dir.clone(),
            ..BenchmarkConfig::default()
        };

        let report = run_benchmark(config).expect("tiny benchmark should run");

        assert_eq!(report.targets.len(), 1);
        assert_eq!(report.targets[0].phases.len(), 8);
        assert!(report.targets[0].storage_bytes > 0);

        let _ = fs::remove_dir_all(work_dir);
    }

    #[test]
    fn tiny_sqlite_benchmark_uses_file_backed_database() {
        let work_dir = env::temp_dir().join(format!(
            "wardrobe_benchmark_sqlite_test_{}",
            unix_timestamp_micros()
        ));
        let config = BenchmarkConfig {
            targets: vec![TargetSpec::Sqlite],
            profile: LibraryProfile {
                entity_records: 3,
                book_records: 6,
                chunk_size: 2,
                traversal_queries: 2,
                point_lookups: 3,
                range_lookups: 3,
                delete_by_id_operations: 2,
                purge_buckets: 2,
            },
            work_dir: work_dir.clone(),
            ..BenchmarkConfig::default()
        };

        let report = run_benchmark(config).expect("tiny SQLite benchmark should run");
        let sqlite_path = report.run_dir.join("sqlite").join("library.sqlite");

        assert_eq!(report.targets.len(), 1);
        assert_eq!(report.targets[0].name, "SQLite (Local WAL File Mode)");
        assert!(sqlite_path.is_file());
        assert!(report.targets[0].storage_bytes > 0);

        let _ = fs::remove_dir_all(work_dir);
    }

    #[test]
    fn tiny_redb_benchmark_uses_file_backed_database() {
        let work_dir = env::temp_dir().join(format!(
            "wardrobe_benchmark_redb_test_{}",
            unix_timestamp_micros()
        ));
        let config = BenchmarkConfig {
            targets: vec![TargetSpec::Redb],
            profile: tiny_profile(),
            work_dir: work_dir.clone(),
            ..BenchmarkConfig::default()
        };

        let report = run_benchmark(config).expect("tiny redb benchmark should run");
        let redb_path = report.run_dir.join("redb").join("library.redb");

        assert_eq!(report.targets.len(), 1);
        assert_eq!(
            report.targets[0].name,
            "redb (Pure Rust Embedded Key-Value Mode)"
        );
        assert_eq!(report.targets[0].phases.len(), 8);
        assert!(redb_path.is_file());
        assert!(report.targets[0].storage_bytes > 0);
        assert!(
            report.targets[0]
                .storage_diagnostics
                .iter()
                .any(|line| line.contains("allocated pages"))
        );

        let _ = fs::remove_dir_all(work_dir);
    }

    #[test]
    fn tiny_rocksdb_benchmark_uses_persistent_directory() {
        let work_dir = env::temp_dir().join(format!(
            "wardrobe_benchmark_rocksdb_test_{}",
            unix_timestamp_micros()
        ));
        let config = BenchmarkConfig {
            targets: vec![TargetSpec::RocksDb],
            profile: tiny_profile(),
            work_dir: work_dir.clone(),
            ..BenchmarkConfig::default()
        };

        let report = run_benchmark(config).expect("tiny rocksdb benchmark should run");
        let rocksdb_dir = report.run_dir.join("rocksdb").join("library.rocksdb");

        assert_eq!(report.targets.len(), 1);
        assert_eq!(
            report.targets[0].name,
            "RocksDB (Embedded Key-Value Mode)"
        );
        assert_eq!(report.targets[0].phases.len(), 8);
        assert!(rocksdb_dir.is_dir());
        assert!(report.targets[0].storage_bytes > 0);
        assert!(
            report.targets[0]
                .storage_diagnostics
                .iter()
                .any(|line| line.contains("RocksDB stats"))
        );

        let _ = fs::remove_dir_all(work_dir);
    }

    #[test]
    fn tiny_wardrobe_remote_benchmark_runs() {
        let work_dir = env::temp_dir().join(format!(
            "wardrobe_benchmark_remote_test_{}",
            unix_timestamp_micros()
        ));
        let config = BenchmarkConfig {
            targets: vec![TargetSpec::WardrobeRemote],
            profile: LibraryProfile {
                entity_records: 3,
                book_records: 6,
                chunk_size: 2,
                traversal_queries: 2,
                point_lookups: 3,
                range_lookups: 3,
                delete_by_id_operations: 2,
                purge_buckets: 2,
            },
            work_dir: work_dir.clone(),
            ..BenchmarkConfig::default()
        };

        let report = run_benchmark(config).expect("tiny remote benchmark should run");

        assert_eq!(report.targets.len(), 1);
        assert_eq!(report.targets[0].phases.len(), 8);
        assert!(report.targets[0].storage_bytes > 0);

        let _ = fs::remove_dir_all(work_dir);
    }

    #[test]
    fn tiny_wardrobe_embedded_and_remote_report_equivalent_scoped_storage() {
        let work_dir = env::temp_dir().join(format!(
            "wardrobe_benchmark_parity_test_{}",
            unix_timestamp_micros()
        ));
        let config = BenchmarkConfig {
            targets: vec![TargetSpec::WardrobeEmbedded, TargetSpec::WardrobeRemote],
            profile: LibraryProfile {
                entity_records: 3,
                book_records: 6,
                chunk_size: 2,
                traversal_queries: 2,
                point_lookups: 3,
                range_lookups: 3,
                delete_by_id_operations: 2,
                purge_buckets: 2,
            },
            work_dir: work_dir.clone(),
            ..BenchmarkConfig::default()
        };

        let report = run_benchmark(config).expect("tiny parity benchmark should run");

        assert_eq!(report.targets.len(), 2);
        let embedded = &report.targets[0];
        let remote = &report.targets[1];
        assert_eq!(embedded.name, "Wardrobe (Embedded Flat-File Mode)");
        assert_eq!(remote.name, "Wardrobe (Remote TCP Server Mode)");
        assert!(
            embedded.storage_bytes.abs_diff(remote.storage_bytes) <= 16,
            "embedded scoped bytes {} should match remote scoped bytes {}",
            embedded.storage_bytes,
            remote.storage_bytes
        );
        assert!(
            embedded
                .storage_diagnostics
                .iter()
                .any(|line| line.contains("Record parity expectation"))
        );
        assert!(
            remote
                .storage_diagnostics
                .iter()
                .any(|line| line.contains("Record parity expectation"))
        );

        let _ = fs::remove_dir_all(work_dir);
    }

    #[test]
    fn wardrobe_remote_storage_uses_scoped_drawer_inventory_and_keeps_root_diagnostics() {
        let state = Rc::new(RefCell::new(MockRunnerState {
            commands: Vec::new(),
            responses: VecDeque::from(vec![
                CommandResult::Status(json!([
                    StorageInventory {
                        name: ENTITY_DRAWER.to_string(),
                        record_count: 3,
                        disk_size_bytes: 300,
                        register_file_count: 3,
                    },
                    StorageInventory {
                        name: BOOK_DRAWER.to_string(),
                        record_count: 5,
                        disk_size_bytes: 700,
                        register_file_count: 3,
                    },
                ])),
                CommandResult::Status(json!(StorageDiagnosis {
                    storage_directory: "/data/wardrobe".to_string(),
                    storage_bytes: 12_345,
                    data_bytes: 8_000,
                    index_bytes: 2_000,
                    metadata_bytes: 500,
                    logical_wal_bytes: 1_000,
                    transaction_wal_bytes: 700,
                    other_bytes: 145,
                    drawer_count: 4,
                    status: "ok".to_string(),
                    drawers: vec![
                        "old_run/library/entity".to_string(),
                        "old_run/library/book".to_string(),
                        "wardrobe_benchmark_test/library/entity".to_string(),
                        "wardrobe_benchmark_test/library/book".to_string(),
                    ],
                })),
                CommandResult::Status(json!(::wardrobe_embedded::WalVerification {
                    path: "/data/wardrobe/.wal".to_string(),
                    entry_count: 4,
                    last_sequence: Some(4),
                })),
                CommandResult::Status(json!(::wardrobe_embedded::WalVerification {
                    path: "/data/wardrobe/wardrobe/.wal".to_string(),
                    entry_count: 3,
                    last_sequence: Some(3),
                })),
            ]),
        }));

        let mut target = WardrobeTarget {
            name: "Wardrobe (Remote TCP Server Mode)".to_string(),
            runner: Some(Box::new(MockWardrobeRunner {
                state: Rc::clone(&state),
            })),
            storage_root: None,
            server_handle: None,
            namespace: test_namespace(),
            profile: Some(tiny_profile()),
            last_storage_snapshot: None,
            pre_compaction_storage_snapshot: None,
        };

        let storage_bytes = target
            .storage_footprint_bytes()
            .expect("remote storage bytes should be reported");
        let diagnostics = target
            .storage_diagnostics()
            .expect("remote diagnostics should render");

        assert_eq!(storage_bytes, 1_000);
        assert!(diagnostics.iter().any(|line| line.contains("12345")));
        assert!(diagnostics.iter().any(|line| line.contains("logical WAL")));
        assert!(diagnostics.iter().any(|line| {
            line.contains("Root-wide drawer discovery sees 4 drawers")
                && line.contains("2 belong to benchmark scope wardrobe_benchmark_test/library")
                && line.contains("2 are outside it")
        }));
        assert!(
            diagnostics
                .iter()
                .any(|line| line.contains("old_run/library/entity"))
        );
        assert!(matches!(
            state.borrow().commands[0],
            Command::Status(StatusRequest::Drawers { .. })
        ));
        assert!(matches!(
            state.borrow().commands[1],
            Command::Status(StatusRequest::Storage)
        ));
    }

    #[test]
    fn wardrobe_traversal_does_not_issue_find_by_id_materialization_calls() {
        let state = Rc::new(RefCell::new(MockRunnerState {
            commands: Vec::new(),
            responses: VecDeque::from(vec![
                CommandResult::Count(1),
                CommandResult::Read(ReadResult::Records(vec![json!({
                    "_id": "book_00000000",
                    "author_id": "entity_00000000",
                    "editor_id": "entity_00000000",
                })])),
                CommandResult::Read(ReadResult::Records(vec![json!({
                    "_id": "book_00000001",
                    "author_id": "entity_00000000",
                    "editor_id": "entity_00000000",
                })])),
            ]),
        }));

        let mut target = WardrobeTarget {
            name: "Wardrobe (Remote TCP Server Mode)".to_string(),
            runner: Some(Box::new(MockWardrobeRunner {
                state: Rc::clone(&state),
            })),
            storage_root: None,
            server_handle: None,
            namespace: test_namespace(),
            profile: None,
            last_storage_snapshot: None,
            pre_compaction_storage_snapshot: None,
        };

        let profile = LibraryProfile {
            entity_records: 1,
            book_records: 2,
            chunk_size: 1,
            traversal_queries: 2,
            point_lookups: 2,
            range_lookups: 2,
            delete_by_id_operations: 1,
            purge_buckets: 1,
        };
        let progress = ProgressReporter::new(false);
        let mut recorder = PhaseRecorder::new(PhaseName::ComplexTraversal);

        let operations = target
            .complex_traversal(&profile, &mut recorder, &progress)
            .expect("traversal should complete");

        assert_eq!(operations, 2);

        let commands = &state.borrow().commands;
        let find_by_filter_calls = commands
            .iter()
            .filter(|command| {
                matches!(
                    scoped_command(command),
                    Command::Read { filter, .. } if filter_contains_query(filter)
                )
            })
            .count();
        let find_by_id_calls = commands
            .iter()
            .filter(|command| {
                matches!(
                    scoped_command(command),
                    Command::Read { filter, .. } if filter_contains_pointer(filter)
                )
            })
            .count();

        assert_eq!(find_by_filter_calls, 2);
        assert_eq!(find_by_id_calls, 0);
    }

    #[test]
    fn wardrobe_point_lookup_uses_pointer_reads() {
        let profile = tiny_profile();
        let lookup_ids = profile.point_lookup_book_ids();
        let state = Rc::new(RefCell::new(MockRunnerState {
            commands: Vec::new(),
            responses: VecDeque::from(
                lookup_ids
                    .iter()
                    .map(|id| {
                        CommandResult::Read(ReadResult::Record(Some(json!({
                            "book_id": id,
                        }))))
                    })
                    .collect::<Vec<_>>(),
            ),
        }));

        let mut target = WardrobeTarget {
            name: "Wardrobe (Remote TCP Server Mode)".to_string(),
            runner: Some(Box::new(MockWardrobeRunner {
                state: Rc::clone(&state),
            })),
            storage_root: None,
            server_handle: None,
            namespace: test_namespace(),
            profile: None,
            last_storage_snapshot: None,
            pre_compaction_storage_snapshot: None,
        };
        let progress = ProgressReporter::new(false);
        let mut recorder = PhaseRecorder::new(PhaseName::PointLookup);

        let operations = target
            .point_lookup(&profile, &mut recorder, &progress)
            .expect("point lookup should complete");

        assert_eq!(operations, lookup_ids.len() as u64);
        let pointer_reads = state
            .borrow()
            .commands
            .iter()
            .filter(|command| {
                matches!(
                    scoped_command(command),
                    Command::Read { filter, .. } if filter_contains_pointer(filter)
                )
            })
            .count();
        assert_eq!(pointer_reads, lookup_ids.len());
    }

    #[test]
    fn wardrobe_delete_by_id_uses_pointer_deletes_and_verifies_missing_records() {
        let profile = tiny_profile();
        let delete_ids = profile.delete_by_id_book_ids();
        let mut responses = VecDeque::new();
        for _ in &delete_ids {
            responses.push_back(CommandResult::Delete(::wardrobe_embedded::DeleteResult {
                deleted: 1,
            }));
            responses.push_back(CommandResult::Read(ReadResult::Record(None)));
        }
        let state = Rc::new(RefCell::new(MockRunnerState {
            commands: Vec::new(),
            responses,
        }));

        let mut target = WardrobeTarget {
            name: "Wardrobe (Remote TCP Server Mode)".to_string(),
            runner: Some(Box::new(MockWardrobeRunner {
                state: Rc::clone(&state),
            })),
            storage_root: None,
            server_handle: None,
            namespace: test_namespace(),
            profile: None,
            last_storage_snapshot: None,
            pre_compaction_storage_snapshot: None,
        };
        let progress = ProgressReporter::new(false);
        let mut recorder = PhaseRecorder::new(PhaseName::DeleteById);

        let operations = target
            .delete_by_id(&profile, &mut recorder, &progress)
            .expect("delete by id should complete");

        assert_eq!(operations, delete_ids.len() as u64);
        let commands = &state.borrow().commands;
        let pointer_deletes = commands
            .iter()
            .filter(|command| {
                matches!(
                    scoped_command(command),
                    Command::Delete { filter, .. } if filter_contains_pointer(filter)
                )
            })
            .count();
        let pointer_reads = commands
            .iter()
            .filter(|command| {
                matches!(
                    scoped_command(command),
                    Command::Read { filter, .. } if filter_contains_pointer(filter)
                )
            })
            .count();
        assert_eq!(pointer_deletes, delete_ids.len());
        assert_eq!(pointer_reads, delete_ids.len());
    }

    #[test]
    fn wardrobe_purge_uses_single_delete_by_filter_command() {
        let state = Rc::new(RefCell::new(MockRunnerState {
            commands: Vec::new(),
            responses: VecDeque::from(vec![CommandResult::Delete(::wardrobe_embedded::DeleteResult {
                deleted: 2,
            })]),
        }));

        let mut target = WardrobeTarget {
            name: "Wardrobe (Remote TCP Server Mode)".to_string(),
            runner: Some(Box::new(MockWardrobeRunner {
                state: Rc::clone(&state),
            })),
            storage_root: None,
            server_handle: None,
            namespace: test_namespace(),
            profile: None,
            last_storage_snapshot: None,
            pre_compaction_storage_snapshot: None,
        };

        let profile = LibraryProfile {
            entity_records: 2,
            book_records: 4,
            chunk_size: 1,
            traversal_queries: 1,
            point_lookups: 2,
            range_lookups: 2,
            delete_by_id_operations: 1,
            purge_buckets: 2,
        };
        let progress = ProgressReporter::new(false);
        let mut recorder = PhaseRecorder::new(PhaseName::TargetedPurge);

        let operations = target
            .targeted_purge(&profile, &mut recorder, &progress)
            .expect("purge should complete");

        assert_eq!(operations, 2);

        let commands = &state.borrow().commands;
        let delete_by_filter_calls = commands
            .iter()
            .filter(|command| {
                matches!(
                    scoped_command(command),
                    Command::Delete { filter, .. } if filter_contains_query(filter)
                )
            })
            .count();
        let per_record_delete_calls = commands
            .iter()
            .filter(|command| {
                matches!(
                    scoped_command(command),
                    Command::Delete { filter, .. } if filter_contains_pointer(filter)
                )
            })
            .count();

        assert_eq!(delete_by_filter_calls, 1);
        assert_eq!(per_record_delete_calls, 0);
    }
}
