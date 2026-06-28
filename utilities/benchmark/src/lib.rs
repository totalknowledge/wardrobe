#![deny(unsafe_code)]

use mongodb::IndexModel;
use mongodb::bson::{Bson, Document, doc};
use mongodb::sync::{Client as MongoClient, Collection};
use mysql::prelude::Queryable;
use mysql::{OptsBuilder, Pool, PooledConn, Row};
use neo4rs::{BoltList, BoltType, Graph, query};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Error, ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;
use wardrobe_core::{
    AlterRequest, Command, CommandResult, CompactRequest, ConnectionTarget, CreateRequest,
    DurabilityPolicy, OperationFilter, OperationOptions, ProtocolFrame, ProtocolOpcode, ReadResult,
    StatusRequest, StatusResult, StorageDiagnosis, StorageInventory, StorageScope, UpsertResult,
    WardrobeEngine,
};

const DEFAULT_WARDROBE_DATABASE_PREFIX: &str = "wardrobe_benchmark";
const DEFAULT_WARDROBE_SCHEMA_NAME: &str = "library";
const ENTITY_DRAWER: &str = "entity";
const BOOK_DRAWER: &str = "book";
const DEFAULT_WORK_DIR: &str = "target/wardrobe-benchmark";
const DEFAULT_ENTITY_RECORDS: usize = 10_000;
const DEFAULT_BOOK_RECORDS: usize = 50_000;
const DEFAULT_CHUNK_SIZE: usize = 500;
const DEFAULT_TRAVERSAL_QUERIES: usize = 100;
const DEFAULT_PURGE_BUCKETS: usize = 10;
const DEFAULT_MYSQL_USER: &str = "wardrobe_benchmark";
const DEFAULT_MYSQL_PASSWORD: &str = "wardrobe_benchmark";
const DEFAULT_MYSQL_USER_ENV: &str = "WARDROBE_BENCH_MYSQL_USER";
const DEFAULT_MYSQL_PASSWORD_ENV: &str = "WARDROBE_BENCH_MYSQL_PASSWORD";
const DEFAULT_MYSQL_CREDENTIALS_FILE: &str = "target/wardrobe-benchmark/mysql-credentials.env";
const DEFAULT_NEO4J_USER: &str = "neo4j";
const DEFAULT_NEO4J_PASSWORD: &str = "wardrobe_benchmark";
const DEFAULT_NEO4J_USER_ENV: &str = "WARDROBE_BENCH_NEO4J_USER";
const DEFAULT_NEO4J_PASSWORD_ENV: &str = "WARDROBE_BENCH_NEO4J_PASSWORD";
const DEFAULT_NEO4J_CREDENTIALS_FILE: &str = "target/wardrobe-benchmark/neo4j-credentials.env";
const DEFAULT_WARDROBE_GROUP_COMMIT_WINDOW_MS: u64 = 5;
const DEFAULT_WARDROBE_GROUP_COMMIT_MAX_BATCH: usize = 128;
const SQLITE_MATERIALIZED_BOOK_QUERY: &str = r#"
SELECT
    b.id,
    b.isbn,
    b.title,
    b.author_id,
    b.editor_id,
    b.branch,
    b.quantity,
    b.purge_bucket,
    author.id,
    author.display_name,
    author.role,
    author.cohort,
    editor.id,
    editor.display_name,
    editor.role,
    editor.cohort
FROM books b
JOIN entities author ON author.id = b.author_id
JOIN entities editor ON editor.id = b.editor_id
WHERE b.author_id = ?1 AND b.editor_id = ?1;
"#;
const NEO4J_BULK_ENTITY_UPSERT_QUERY: &str = r#"
UNWIND $rows AS row
MERGE (e:BenchNode:Entity {bench_marker: $marker, id: row.id})
SET e.display_name = row.display_name,
    e.role = row.role,
    e.cohort = row.cohort
"#;
const NEO4J_BULK_BOOK_UPSERT_QUERY: &str = r#"
UNWIND $rows AS row
MERGE (b:BenchNode:Book {bench_marker: $marker, id: row.id})
SET b.isbn = row.isbn,
    b.title = row.title,
    b.author_id = row.author_id,
    b.editor_id = row.editor_id,
    b.branch = row.branch,
    b.quantity = row.quantity,
    b.purge_bucket = row.purge_bucket
WITH b, row, $marker AS marker
MATCH (author:BenchNode:Entity {bench_marker: marker, id: row.author_id})
MATCH (editor:BenchNode:Entity {bench_marker: marker, id: row.editor_id})
MERGE (b)-[:AUTHORED_BY]->(author)
MERGE (b)-[:EDITED_BY]->(editor)
"#;
const NEO4J_STORE_SIZE_BYTES_QUERY: &str = r#"
CALL dbms.queryJmx("org.neo4j:*") YIELD name, attributes
WHERE name CONTAINS "Store file sizes"
  AND (name CONTAINS ("database=" + $database) OR name CONTAINS "instance=kernel")
RETURN coalesce(max(toInteger(attributes.TotalStoreSize.value)), 0) AS storage_bytes
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    Run(BenchmarkConfig),
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkConfig {
    pub targets: Vec<TargetSpec>,
    pub profile: LibraryProfile,
    pub work_dir: PathBuf,
    pub output_path: Option<PathBuf>,
    pub progress_enabled: bool,
    pub wardrobe_embedded_path: Option<PathBuf>,
    pub wardrobe_remote_uri: Option<String>,
    pub wardrobe_durability_policy: DurabilityPolicy,
    pub wardrobe_database: Option<String>,
    pub wardrobe_schema: Option<String>,
    pub sqlite_db: Option<PathBuf>,
    pub mongo_uri: String,
    pub mongo_database: String,
    pub mysql_host: String,
    pub mysql_port: u16,
    pub mysql_database: String,
    pub mysql_user: Option<String>,
    pub mysql_password_env: Option<String>,
    pub neo4j_uri: String,
    pub neo4j_database: String,
    pub neo4j_user: String,
    pub neo4j_password_env: String,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            targets: vec![TargetSpec::WardrobeEmbedded],
            profile: LibraryProfile::default(),
            work_dir: PathBuf::from(DEFAULT_WORK_DIR),
            output_path: None,
            progress_enabled: true,
            wardrobe_embedded_path: None,
            wardrobe_remote_uri: None,
            wardrobe_durability_policy: DurabilityPolicy::Strict,
            wardrobe_database: None,
            wardrobe_schema: None,
            sqlite_db: None,
            mongo_uri: "mongodb://127.0.0.1:27017".to_string(),
            mongo_database: "wardrobe_benchmark".to_string(),
            mysql_host: "127.0.0.1".to_string(),
            mysql_port: 3306,
            mysql_database: "wardrobe_benchmark".to_string(),
            mysql_user: None,
            mysql_password_env: Some(DEFAULT_MYSQL_PASSWORD_ENV.to_string()),
            neo4j_uri: "127.0.0.1:7687".to_string(),
            neo4j_database: "neo4j".to_string(),
            neo4j_user: DEFAULT_NEO4J_USER.to_string(),
            neo4j_password_env: DEFAULT_NEO4J_PASSWORD_ENV.to_string(),
        }
    }
}

impl BenchmarkConfig {
    pub fn from_args<I>(args: I) -> io::Result<ParseOutcome>
    where
        I: IntoIterator<Item = String>,
    {
        let mut config = Self::default();
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => return Ok(ParseOutcome::Help),
                "--targets" => config.targets = parse_targets(&required_value(&mut args, &arg)?)?,
                "--work-dir" => config.work_dir = PathBuf::from(required_value(&mut args, &arg)?),
                "--output" => {
                    config.output_path = Some(PathBuf::from(required_value(&mut args, &arg)?));
                }
                "--quiet" | "--no-progress" => config.progress_enabled = false,
                "--entities" => {
                    config.profile.entity_records =
                        parse_positive_usize(&arg, &required_value(&mut args, &arg)?)?;
                }
                "--books" => {
                    config.profile.book_records =
                        parse_positive_usize(&arg, &required_value(&mut args, &arg)?)?;
                }
                "--chunk-size" => {
                    config.profile.chunk_size =
                        parse_positive_usize(&arg, &required_value(&mut args, &arg)?)?;
                }
                "--traversal-queries" => {
                    config.profile.traversal_queries =
                        parse_positive_usize(&arg, &required_value(&mut args, &arg)?)?;
                }
                "--purge-buckets" => {
                    config.profile.purge_buckets =
                        parse_positive_usize(&arg, &required_value(&mut args, &arg)?)?;
                }
                "--wardrobe-embedded-path" => {
                    config.wardrobe_embedded_path =
                        Some(PathBuf::from(required_value(&mut args, &arg)?));
                }
                "--wardrobe-remote-uri" => {
                    config.wardrobe_remote_uri = Some(required_value(&mut args, &arg)?);
                }
                "--wardrobe-durability" => {
                    config.wardrobe_durability_policy =
                        parse_wardrobe_durability_policy(&required_value(&mut args, &arg)?)?;
                }
                "--wardrobe-group-commit-window-ms" => {
                    let commit_window_ms =
                        parse_positive_u64(&arg, &required_value(&mut args, &arg)?)?;
                    config.wardrobe_durability_policy = update_group_commit_window(
                        config.wardrobe_durability_policy.clone(),
                        commit_window_ms,
                    );
                }
                "--wardrobe-group-commit-max-batch" => {
                    let max_batch_size =
                        parse_positive_usize(&arg, &required_value(&mut args, &arg)?)?;
                    config.wardrobe_durability_policy = update_group_commit_max_batch(
                        config.wardrobe_durability_policy.clone(),
                        max_batch_size,
                    );
                }
                "--wardrobe-database" => {
                    config.wardrobe_database = Some(required_value(&mut args, &arg)?);
                }
                "--wardrobe-schema" => {
                    config.wardrobe_schema = Some(required_value(&mut args, &arg)?);
                }
                "--sqlite-db" => {
                    config.sqlite_db = Some(PathBuf::from(required_value(&mut args, &arg)?));
                }
                "--mongo-uri" => config.mongo_uri = required_value(&mut args, &arg)?,
                "--mongo-database" => config.mongo_database = required_value(&mut args, &arg)?,
                "--mysql-host" => config.mysql_host = required_value(&mut args, &arg)?,
                "--mysql-port" => {
                    config.mysql_port =
                        required_value(&mut args, &arg)?
                            .parse::<u16>()
                            .map_err(|error| {
                                Error::new(
                                    ErrorKind::InvalidInput,
                                    format!("Invalid --mysql-port value: {error}"),
                                )
                            })?;
                }
                "--mysql-database" => config.mysql_database = required_value(&mut args, &arg)?,
                "--mysql-user" => config.mysql_user = Some(required_value(&mut args, &arg)?),
                "--mysql-password-env" => {
                    config.mysql_password_env = Some(required_value(&mut args, &arg)?);
                }
                "--mysql-no-password" => config.mysql_password_env = None,
                "--neo4j-uri" => config.neo4j_uri = required_value(&mut args, &arg)?,
                "--neo4j-database" => config.neo4j_database = required_value(&mut args, &arg)?,
                "--neo4j-user" => config.neo4j_user = required_value(&mut args, &arg)?,
                "--neo4j-password-env" => {
                    config.neo4j_password_env = required_value(&mut args, &arg)?;
                }
                unknown => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("Unknown benchmark argument: {unknown}"),
                    ));
                }
            }
        }

        config.profile.validate()?;
        if let Some(database) = &config.wardrobe_database {
            validate_wardrobe_namespace_component("--wardrobe-database", database)?;
        }
        if let Some(schema) = &config.wardrobe_schema {
            validate_wardrobe_namespace_component("--wardrobe-schema", schema)?;
        }
        if config.targets.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "--targets must include at least one target",
            ));
        }

        Ok(ParseOutcome::Run(config))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSpec {
    WardrobeEmbedded,
    WardrobeRemote,
    Sqlite,
    MongoDb,
    MySql,
    Neo4j,
}

impl TargetSpec {
    fn all() -> Vec<Self> {
        vec![
            Self::WardrobeEmbedded,
            Self::WardrobeRemote,
            Self::Sqlite,
            Self::MongoDb,
            Self::MySql,
            Self::Neo4j,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            Self::WardrobeEmbedded => "Wardrobe (Embedded Flat-File Mode)",
            Self::WardrobeRemote => "Wardrobe (Remote TCP Server Mode)",
            Self::Sqlite => "SQLite (Local WAL File Mode)",
            Self::MongoDb => "MongoDB (Document Store Base Comparison)",
            Self::MySql => "MySQL / MariaDB (Relational Pointer Base Comparison)",
            Self::Neo4j => "Neo4j (Graph Database Base Comparison)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WardrobeNamespace {
    database: String,
    schema: String,
    generated: bool,
}

impl WardrobeNamespace {
    fn from_config(config: &BenchmarkConfig, run_id: &str) -> io::Result<Self> {
        let database = config.wardrobe_database.clone().unwrap_or_else(|| {
            format!(
                "{}_{}",
                DEFAULT_WARDROBE_DATABASE_PREFIX,
                identifier_fragment(run_id)
            )
        });
        let schema = config
            .wardrobe_schema
            .clone()
            .unwrap_or_else(|| DEFAULT_WARDROBE_SCHEMA_NAME.to_string());
        validate_wardrobe_namespace_component("--wardrobe-database", &database)?;
        validate_wardrobe_namespace_component("--wardrobe-schema", &schema)?;
        Ok(Self {
            generated: config.wardrobe_database.is_none() && config.wardrobe_schema.is_none(),
            database,
            schema,
        })
    }

    fn label(&self) -> String {
        format!("{}/{}", self.database, self.schema)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryProfile {
    pub entity_records: usize,
    pub book_records: usize,
    pub chunk_size: usize,
    pub traversal_queries: usize,
    pub purge_buckets: usize,
}

impl Default for LibraryProfile {
    fn default() -> Self {
        Self {
            entity_records: DEFAULT_ENTITY_RECORDS,
            book_records: DEFAULT_BOOK_RECORDS,
            chunk_size: DEFAULT_CHUNK_SIZE,
            traversal_queries: DEFAULT_TRAVERSAL_QUERIES,
            purge_buckets: DEFAULT_PURGE_BUCKETS,
        }
    }
}

impl LibraryProfile {
    fn validate(&self) -> io::Result<()> {
        for (name, value) in [
            ("entities", self.entity_records),
            ("books", self.book_records),
            ("chunk-size", self.chunk_size),
            ("traversal-queries", self.traversal_queries),
            ("purge-buckets", self.purge_buckets),
        ] {
            if value == 0 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("--{name} must be greater than zero"),
                ));
            }
        }
        Ok(())
    }

    fn entity_id(&self, index: usize) -> String {
        format!("entity_{index:08}")
    }

    fn book_id(&self, index: usize) -> String {
        format!("book_{index:08}")
    }

    fn entity_payload(&self, index: usize) -> Value {
        let entity_id = self.entity_id(index);
        json!({
            "_id": entity_id,
            "entity_id": entity_id,
            "display_name": format!("Library Entity {index:08}"),
            "role": if index % 2 == 0 { "author" } else { "editor" },
            "cohort": index % 97,
        })
    }

    fn book_payload(&self, index: usize) -> Value {
        let book_id = self.book_id(index);
        let author_index = index % self.entity_records;
        let editor_index = if index % 10 == 0 {
            author_index
        } else {
            (index.saturating_mul(37).saturating_add(17)) % self.entity_records
        };
        json!({
            "_id": book_id,
            "book_id": book_id,
            "isbn": format!("isbn-{index:08}"),
            "title": format!("Benchmark Volume {index:08}"),
            "author_id": self.entity_id(author_index),
            "editor_id": self.entity_id(editor_index),
            "branch": match index % 3 {
                0 => "central",
                1 => "north",
                _ => "south",
            },
            "quantity": (index % 23) + 1,
            "purge_bucket": index % self.purge_buckets,
        })
    }

    fn traversal_entity_id(&self, query_index: usize) -> String {
        self.entity_id(query_index % self.entity_records)
    }

    fn expected_purge_count(&self) -> usize {
        if self.book_records == 0 {
            0
        } else {
            ((self.book_records - 1) / self.purge_buckets) + 1
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkReport {
    pub profile: LibraryProfile,
    pub run_dir: PathBuf,
    pub targets: Vec<TargetReport>,
}

impl BenchmarkReport {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Cross-Engine Performance Benchmark\n\n");
        out.push_str(&format!("- Run directory: `{}`\n", self.run_dir.display()));
        out.push_str(&format!(
            "- Profile: {} entity records, {} book records, chunk size {}, {} traversal queries\n\n",
            self.profile.entity_records,
            self.profile.book_records,
            self.profile.chunk_size,
            self.profile.traversal_queries,
        ));
        out.push_str("| Target | Phase | Operations | Total us | OPS | Mean us | p95 us | p99 us | Storage bytes |\n");
        out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
        for target in &self.targets {
            if let Some(reason) = &target.unavailable_reason {
                out.push_str(&format!(
                    "| {} | Unavailable | 0 | 0 | 0.00 | 0.00 | 0.00 | 0.00 | 0 |\n",
                    target.name
                ));
                let _ = reason;
            } else {
                for phase in &target.phases {
                    out.push_str(&format!(
                        "| {} | {} | {} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {} |\n",
                        target.name,
                        phase.phase.label(),
                        phase.operations,
                        phase.total_micros,
                        phase.ops_per_second,
                        phase.mean_micros,
                        phase.p95_micros,
                        phase.p99_micros,
                        target.storage_bytes,
                    ));
                }
            }
        }
        let diagnostic_targets = self
            .targets
            .iter()
            .filter(|target| !target.storage_diagnostics.is_empty())
            .collect::<Vec<_>>();
        if !diagnostic_targets.is_empty() {
            out.push_str("\n## Storage Diagnostics\n\n");
            for target in diagnostic_targets {
                out.push_str(&format!("### {}\n\n", target.name));
                for line in &target.storage_diagnostics {
                    out.push_str(&format!("- {line}\n"));
                }
                out.push('\n');
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetReport {
    pub name: String,
    pub phases: Vec<PhaseMetrics>,
    pub storage_bytes: u64,
    pub storage_diagnostics: Vec<String>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseMetrics {
    pub phase: PhaseName,
    pub operations: u64,
    pub total_micros: u128,
    pub ops_per_second: f64,
    pub mean_micros: f64,
    pub p95_micros: f64,
    pub p99_micros: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseName {
    MassiveIngestion,
    IndexMutation,
    ComplexTraversal,
    TargetedPurge,
    Compaction,
}

impl PhaseName {
    fn label(self) -> &'static str {
        match self {
            Self::MassiveIngestion => "Massive Ingestion",
            Self::IndexMutation => "Index Mutation",
            Self::ComplexTraversal => "Complex Traversal",
            Self::TargetedPurge => "Targeted Purge",
            Self::Compaction => "Compaction",
        }
    }
}

#[derive(Debug, Clone)]
struct LatencySample {
    operations: u64,
    elapsed: Duration,
}

#[derive(Debug)]
pub struct PhaseRecorder {
    phase: PhaseName,
    samples: Vec<LatencySample>,
}

impl PhaseRecorder {
    pub fn new(phase: PhaseName) -> Self {
        Self {
            phase,
            samples: Vec::new(),
        }
    }

    pub fn measure<T>(
        &mut self,
        operations: u64,
        work: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        if operations == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "phase samples must contain at least one operation",
            ));
        }
        let started = Instant::now();
        let result = work();
        let elapsed = started.elapsed();
        if result.is_ok() {
            self.samples.push(LatencySample {
                operations,
                elapsed,
            });
        }
        result
    }

    pub fn finish(self) -> PhaseMetrics {
        let operations = self
            .samples
            .iter()
            .map(|sample| sample.operations)
            .sum::<u64>();
        let total_micros = self
            .samples
            .iter()
            .map(|sample| sample.elapsed.as_micros())
            .sum::<u128>();
        let seconds = total_micros as f64 / 1_000_000.0;
        let ops_per_second = if seconds > 0.0 {
            operations as f64 / seconds
        } else {
            0.0
        };
        let mean_micros = if operations > 0 {
            total_micros as f64 / operations as f64
        } else {
            0.0
        };

        PhaseMetrics {
            phase: self.phase,
            operations,
            total_micros,
            ops_per_second,
            mean_micros,
            p95_micros: weighted_percentile(&self.samples, 0.95),
            p99_micros: weighted_percentile(&self.samples, 0.99),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProgressReporter {
    enabled: bool,
    started: Instant,
}

impl ProgressReporter {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            started: Instant::now(),
        }
    }

    pub fn log(&self, message: impl AsRef<str>) {
        if self.enabled {
            eprintln!(
                "[benchmark +{:>7.2}s] {}",
                self.started.elapsed().as_secs_f64(),
                message.as_ref()
            );
        }
    }
}

pub fn run_benchmark(config: BenchmarkConfig) -> io::Result<BenchmarkReport> {
    run_benchmark_with_builder(config, build_target)
}

fn run_benchmark_with_builder(
    config: BenchmarkConfig,
    mut builder: impl FnMut(
        TargetSpec,
        &BenchmarkConfig,
        &Path,
        &WardrobeNamespace,
        &ProgressReporter,
    ) -> io::Result<Box<dyn BenchmarkTarget>>,
) -> io::Result<BenchmarkReport> {
    let progress = ProgressReporter::new(config.progress_enabled);
    let run_id = format!("run-{}", unix_timestamp_micros());
    let run_dir = config.work_dir.join(&run_id);
    let wardrobe_namespace = WardrobeNamespace::from_config(&config, &run_id)?;
    progress.log(format!(
        "creating benchmark run directory at {}",
        run_dir.display()
    ));
    fs::create_dir_all(&run_dir)?;
    progress.log(format!(
        "profile: {} entities, {} books, chunk size {}, {} traversal queries",
        config.profile.entity_records,
        config.profile.book_records,
        config.profile.chunk_size,
        config.profile.traversal_queries
    ));
    if config.targets.iter().any(|target| {
        matches!(
            target,
            TargetSpec::WardrobeEmbedded | TargetSpec::WardrobeRemote
        )
    }) {
        let namespace_source = if wardrobe_namespace.generated {
            "run-isolated default"
        } else {
            "explicit override"
        };
        progress.log(format!(
            "wardrobe namespace: {} ({namespace_source})",
            wardrobe_namespace.label()
        ));
    }

    let mut report = BenchmarkReport {
        profile: config.profile.clone(),
        run_dir: run_dir.clone(),
        targets: Vec::new(),
    };

    for (index, spec) in config.targets.iter().enumerate() {
        progress.log(format!(
            "preparing target {}/{}: {}",
            index + 1,
            config.targets.len(),
            spec.label()
        ));
        match builder(*spec, &config, &run_dir, &wardrobe_namespace, &progress) {
            Ok(mut target) => {
                let target_name = target.name().to_string();
                match run_target(target.as_mut(), &config.profile, &progress) {
                    Ok(target_report) => {
                        progress.log(format!(
                            "completed target: {} (storage footprint {} bytes)",
                            target_report.name, target_report.storage_bytes
                        ));
                        report.targets.push(target_report);
                    }
                    Err(error) => {
                        progress.log(format!(
                            "target {} unavailable; continuing with remaining targets: {}",
                            target_name, error
                        ));
                        report
                            .targets
                            .push(unavailable_target_report(&target_name, error.to_string()));
                    }
                }
            }
            Err(error) => {
                progress.log(format!(
                    "target {} unavailable during setup; continuing with remaining targets: {}",
                    spec.label(),
                    error
                ));
                report
                    .targets
                    .push(unavailable_target_report(spec.label(), error.to_string()));
            }
        }
    }

    progress.log("benchmark run complete; rendering Markdown report");
    Ok(report)
}

fn unavailable_target_report(name: &str, reason: String) -> TargetReport {
    TargetReport {
        name: name.to_string(),
        phases: Vec::new(),
        storage_bytes: 0,
        storage_diagnostics: vec![format!("Unavailable: {reason}")],
        unavailable_reason: Some(reason),
    }
}

pub fn print_help() {
    println!("wardrobe-benchmark");
    println!(
        "  --targets <csv|all>             Targets: wardrobe-embedded,wardrobe-remote,sqlite,mongodb,mysql,neo4j"
    );
    println!(
        "  --work-dir <path>               Benchmark run directory root, default {DEFAULT_WORK_DIR}"
    );
    println!("  --output <path>                 Write Markdown report to a file as well as stdout");
    println!("  --quiet, --no-progress          Suppress progress messages on stderr");
    println!("  --entities <count>              Entity records, default {DEFAULT_ENTITY_RECORDS}");
    println!("  --books <count>                 Book records, default {DEFAULT_BOOK_RECORDS}");
    println!(
        "  --chunk-size <count>            Batch size for native driver writes, default {DEFAULT_CHUNK_SIZE}"
    );
    println!(
        "  --traversal-queries <count>     Complex traversal query repetitions, default {DEFAULT_TRAVERSAL_QUERIES}"
    );
    println!("  --wardrobe-embedded-path <path> Override embedded Wardrobe storage path");
    println!(
        "  --wardrobe-remote-uri <uri>     Use an existing Wardrobe TCP server instead of auto-spawning one"
    );
    println!("  --wardrobe-durability <mode>    Wardrobe WAL durability: strict or grouped");
    println!(
        "  --wardrobe-group-commit-window-ms <ms>  Grouped Wardrobe WAL commit window, default {DEFAULT_WARDROBE_GROUP_COMMIT_WINDOW_MS}"
    );
    println!(
        "  --wardrobe-group-commit-max-batch <count>  Grouped Wardrobe WAL max batch, default {DEFAULT_WARDROBE_GROUP_COMMIT_MAX_BATCH}"
    );
    println!(
        "  --wardrobe-database <name>      Optional Wardrobe database override; default is run-isolated"
    );
    println!(
        "  --wardrobe-schema <name>        Optional Wardrobe schema override, default {DEFAULT_WARDROBE_SCHEMA_NAME}"
    );
    println!("  --sqlite-db <path>              SQLite WAL database path");
    println!(
        "  --mongo-uri <uri>               MongoDB connection URI, default mongodb://127.0.0.1:27017"
    );
    println!("  --mongo-database <name>         MongoDB database name, default wardrobe_benchmark");
    println!("  --mysql-host <host>             MySQL host, default 127.0.0.1");
    println!("  --mysql-port <port>             MySQL port, default 3306");
    println!("  --mysql-database <name>         MySQL database name, default wardrobe_benchmark");
    println!(
        "  --mysql-user <user>             Optional MySQL username, default wardrobe_benchmark"
    );
    println!(
        "  --mysql-password-env <var>      Env var containing the MySQL password, default WARDROBE_BENCH_MYSQL_PASSWORD"
    );
    println!("  --mysql-no-password             Connect to MySQL without a password");
    println!("  --neo4j-uri <host:port>         Neo4j Bolt endpoint, default 127.0.0.1:7687");
    println!("  --neo4j-database <name>         Neo4j database name, default neo4j");
    println!("  --neo4j-user <user>             Neo4j username, default neo4j");
    println!(
        "  --neo4j-password-env <var>      Env var containing the Neo4j password, default WARDROBE_BENCH_NEO4J_PASSWORD"
    );
}

trait BenchmarkTarget {
    fn name(&self) -> &str;
    fn provision_schema(
        &mut self,
        profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()>;
    fn massive_ingestion(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn index_mutation(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn complex_traversal(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn targeted_purge(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn compaction(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn flush(&mut self) -> io::Result<()>;
    fn storage_footprint_bytes(&mut self) -> io::Result<u64>;
    fn storage_diagnostics(&mut self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

fn run_target(
    target: &mut dyn BenchmarkTarget,
    profile: &LibraryProfile,
    progress: &ProgressReporter,
) -> io::Result<TargetReport> {
    progress.log(format!("{}: provisioning schema", target.name()));
    target.provision_schema(profile, progress)?;
    progress.log(format!("{}: schema ready", target.name()));

    let phases = [
        PhaseName::MassiveIngestion,
        PhaseName::IndexMutation,
        PhaseName::ComplexTraversal,
        PhaseName::TargetedPurge,
        PhaseName::Compaction,
    ];
    let mut phase_metrics = Vec::new();

    for phase in phases {
        progress.log(format!("{}: starting {}", target.name(), phase.label()));
        let mut recorder = PhaseRecorder::new(phase);
        match phase {
            PhaseName::MassiveIngestion => {
                target.massive_ingestion(profile, &mut recorder, progress)?;
            }
            PhaseName::IndexMutation => {
                target.index_mutation(profile, &mut recorder, progress)?;
            }
            PhaseName::ComplexTraversal => {
                target.complex_traversal(profile, &mut recorder, progress)?;
            }
            PhaseName::TargetedPurge => {
                target.targeted_purge(profile, &mut recorder, progress)?;
            }
            PhaseName::Compaction => {
                target.compaction(profile, &mut recorder, progress)?;
            }
        }
        progress.log(format!(
            "{}: flushing after {}",
            target.name(),
            phase.label()
        ));
        target.flush()?;
        let metrics = recorder.finish();
        progress.log(format!(
            "{}: finished {} ({} ops, {} us, {:.2} OPS)",
            target.name(),
            metrics.phase.label(),
            metrics.operations,
            metrics.total_micros,
            metrics.ops_per_second
        ));
        phase_metrics.push(metrics);
    }

    progress.log(format!("{}: measuring storage footprint", target.name()));
    let storage_bytes = target.storage_footprint_bytes()?;
    let storage_diagnostics = target.storage_diagnostics()?;
    Ok(TargetReport {
        name: target.name().to_string(),
        phases: phase_metrics,
        storage_bytes,
        storage_diagnostics,
        unavailable_reason: None,
    })
}

fn build_target(
    spec: TargetSpec,
    config: &BenchmarkConfig,
    run_dir: &Path,
    wardrobe_namespace: &WardrobeNamespace,
    progress: &ProgressReporter,
) -> io::Result<Box<dyn BenchmarkTarget>> {
    match spec {
        TargetSpec::WardrobeEmbedded => {
            let path = config
                .wardrobe_embedded_path
                .clone()
                .unwrap_or_else(|| run_dir.join("wardrobe-embedded"));
            progress.log(format!(
                "{}: opening embedded storage at {}",
                spec.label(),
                path.display()
            ));
            Ok(Box::new(WardrobeTarget::embedded(
                path,
                wardrobe_namespace.clone(),
                config.wardrobe_durability_policy.clone(),
            )?))
        }
        TargetSpec::WardrobeRemote => {
            if let Some(uri) = &config.wardrobe_remote_uri {
                progress.log(format!("{}: connecting to {uri}", spec.label()));
                Ok(Box::new(WardrobeTarget::remote_uri(
                    uri,
                    wardrobe_namespace.clone(),
                )?))
            } else {
                progress.log(format!(
                    "{}: starting in-process TCP server under {}",
                    spec.label(),
                    run_dir.join("wardrobe-remote").display()
                ));
                Ok(Box::new(WardrobeTarget::remote_auto(
                    run_dir.join("wardrobe-remote"),
                    wardrobe_namespace.clone(),
                    config.wardrobe_durability_policy.clone(),
                )?))
            }
        }
        TargetSpec::Sqlite => {
            let db_path = config
                .sqlite_db
                .clone()
                .unwrap_or_else(|| run_dir.join("sqlite").join("library.sqlite"));
            progress.log(format!(
                "{}: opening persistent rusqlite WAL file at {}",
                spec.label(),
                db_path.display()
            ));
            Ok(Box::new(SqliteTarget::new(db_path)?))
        }
        TargetSpec::MongoDb => {
            progress.log(format!(
                "{}: opening persistent MongoDB client for {} / database {}",
                spec.label(),
                config.mongo_uri,
                config.mongo_database
            ));
            Ok(Box::new(MongoTarget::new(
                config.mongo_uri.clone(),
                config.mongo_database.clone(),
            )?))
        }
        TargetSpec::MySql => {
            progress.log(format!(
                "{}: opening persistent MySQL connection for {}:{} / database {}",
                spec.label(),
                config.mysql_host,
                config.mysql_port,
                config.mysql_database
            ));
            Ok(Box::new(MySqlTarget::new(
                config.mysql_host.clone(),
                config.mysql_port,
                config.mysql_database.clone(),
                config.mysql_user.clone(),
                config.mysql_password_env.clone(),
            )?))
        }
        TargetSpec::Neo4j => {
            progress.log(format!(
                "{}: opening Neo4j Bolt connection for {} / database {}",
                spec.label(),
                config.neo4j_uri,
                config.neo4j_database
            ));
            Ok(Box::new(Neo4jTarget::new(
                config.neo4j_uri.clone(),
                config.neo4j_database.clone(),
                config.neo4j_user.clone(),
                config.neo4j_password_env.clone(),
                run_dir
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "run".to_string()),
            )?))
        }
    }
}

struct WardrobeTarget {
    name: String,
    runner: Option<Box<dyn WardrobeCommandRunner>>,
    storage_root: Option<PathBuf>,
    server_handle: Option<JoinHandle<io::Result<()>>>,
    namespace: WardrobeNamespace,
    profile: Option<LibraryProfile>,
    last_storage_snapshot: Option<WardrobeStorageSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
struct WardrobeStorageSnapshot {
    drawers: Vec<StorageInventory>,
    diagnosis: Option<StorageDiagnosis>,
    root_wal_entries: Option<usize>,
    database_wal_entries: Option<usize>,
    local_root_bytes: Option<u64>,
}

impl WardrobeStorageSnapshot {
    fn benchmark_drawer_bytes(&self) -> u64 {
        self.benchmark_drawers()
            .iter()
            .map(|drawer| drawer.disk_size_bytes)
            .sum()
    }

    fn benchmark_drawers(&self) -> Vec<&StorageInventory> {
        self.drawers
            .iter()
            .filter(|drawer| drawer.name == ENTITY_DRAWER || drawer.name == BOOK_DRAWER)
            .collect()
    }

    fn diagnostic_lines(
        &self,
        namespace: &WardrobeNamespace,
        profile: Option<&LibraryProfile>,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let benchmark_drawers = self.benchmark_drawers();
        let drawer_summary = benchmark_drawers
            .iter()
            .map(|drawer| {
                format!(
                    "{}: {} records, {} bytes, {} files",
                    drawer.name,
                    drawer.record_count,
                    drawer.disk_size_bytes,
                    drawer.register_file_count
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        lines.push(format!(
            "Benchmark scope {} reports {} drawer bytes ({drawer_summary})",
            namespace.label(),
            self.benchmark_drawer_bytes()
        ));

        if let Some(profile) = profile {
            let expected_book_records = profile
                .book_records
                .saturating_sub(profile.expected_purge_count());
            let entity_records = self
                .drawers
                .iter()
                .find(|drawer| drawer.name == ENTITY_DRAWER)
                .map(|drawer| drawer.record_count)
                .unwrap_or_default();
            let book_records = self
                .drawers
                .iter()
                .find(|drawer| drawer.name == BOOK_DRAWER)
                .map(|drawer| drawer.record_count)
                .unwrap_or_default();
            lines.push(format!(
                "Record parity expectation after purge: entity {}/{}; book {}/{}",
                entity_records, profile.entity_records, book_records, expected_book_records
            ));
        }

        let extra_drawers = self
            .drawers
            .iter()
            .filter(|drawer| drawer.name != ENTITY_DRAWER && drawer.name != BOOK_DRAWER)
            .map(|drawer| drawer.name.as_str())
            .collect::<Vec<_>>();
        if !extra_drawers.is_empty() {
            lines.push(format!(
                "Additional drawers inside benchmark schema: {}",
                extra_drawers.join(", ")
            ));
        }

        if let Some(diagnosis) = &self.diagnosis {
            let non_benchmark_bytes = diagnosis
                .storage_bytes
                .saturating_sub(self.benchmark_drawer_bytes());
            lines.push(format!(
                "Server root reports {} bytes; non-benchmark/root overhead is {} bytes",
                diagnosis.storage_bytes, non_benchmark_bytes
            ));
            lines.push(format!(
                "Root breakdown: data {} bytes, index {} bytes, metadata {} bytes, logical WAL {} bytes, transaction WAL {} bytes, other {} bytes",
                diagnosis.data_bytes,
                diagnosis.index_bytes,
                diagnosis.metadata_bytes,
                diagnosis.logical_wal_bytes,
                diagnosis.transaction_wal_bytes,
                diagnosis.other_bytes
            ));
            let scoped_root_drawer_count = diagnosis
                .drawers
                .iter()
                .filter(|drawer| diagnosis_drawer_is_in_scope(drawer, namespace))
                .count();
            if diagnosis.drawer_count != scoped_root_drawer_count {
                lines.push(format!(
                    "Root-wide drawer discovery sees {} drawers across the storage root; {} belong to benchmark scope {} and {} are outside it",
                    diagnosis.drawer_count,
                    scoped_root_drawer_count,
                    namespace.label(),
                    diagnosis.drawer_count.saturating_sub(scoped_root_drawer_count)
                ));
                let non_benchmark_drawer_examples = diagnosis
                    .drawers
                    .iter()
                    .filter(|drawer| !diagnosis_drawer_is_in_scope(drawer, namespace))
                    .take(5)
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                if !non_benchmark_drawer_examples.is_empty() {
                    lines.push(format!(
                        "Root-wide non-benchmark drawer examples: {}",
                        non_benchmark_drawer_examples.join(", ")
                    ));
                }
            }
            if scoped_root_drawer_count != self.drawers.len() {
                lines.push(format!(
                    "Benchmark scoped status(drawers) returned {} drawers; root scan found {} matching paths",
                    self.drawers.len(),
                    scoped_root_drawer_count
                ));
            }
        } else if let Some(local_root_bytes) = self.local_root_bytes {
            lines.push(format!(
                "Local storage root reports {} bytes; scoped benchmark drawers report {} bytes",
                local_root_bytes,
                self.benchmark_drawer_bytes()
            ));
        }

        lines.push(format!(
            "Logical WAL entries: root {}, database {}",
            optional_count(self.root_wal_entries),
            optional_count(self.database_wal_entries)
        ));

        lines
    }
}

fn diagnosis_drawer_is_in_scope(drawer: &str, namespace: &WardrobeNamespace) -> bool {
    let prefix = format!("{}/{}/", namespace.database, namespace.schema);
    drawer.starts_with(&prefix)
}

impl WardrobeTarget {
    fn embedded(
        path: PathBuf,
        namespace: WardrobeNamespace,
        durability_policy: DurabilityPolicy,
    ) -> io::Result<Self> {
        fs::create_dir_all(&path)?;
        let engine = WardrobeEngine::open_with_durability_policy(
            path.to_string_lossy().as_ref(),
            durability_policy,
        )?;
        Ok(Self {
            name: "Wardrobe (Embedded Flat-File Mode)".to_string(),
            runner: Some(Box::new(EmbeddedWardrobeRunner {
                engine,
                storage_root: path.clone(),
            })),
            storage_root: Some(path),
            server_handle: None,
            namespace,
            profile: None,
            last_storage_snapshot: None,
        })
    }

    fn remote_auto(
        path: PathBuf,
        namespace: WardrobeNamespace,
        durability_policy: DurabilityPolicy,
    ) -> io::Result<Self> {
        fs::create_dir_all(&path)?;
        let engine = Arc::new(WardrobeEngine::open_with_durability_policy(
            path.to_string_lossy().as_ref(),
            durability_policy,
        )?);
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let handle =
            thread::spawn(move || wardrobe_server::serve_tcp_listener(listener, engine, Some(1)));
        let runner =
            TcpWardrobeRunner::connect(&format!("wardrobe://{address}"), Some(path.clone()))?;
        Ok(Self {
            name: "Wardrobe (Remote TCP Server Mode)".to_string(),
            runner: Some(Box::new(runner)),
            storage_root: Some(path),
            server_handle: Some(handle),
            namespace,
            profile: None,
            last_storage_snapshot: None,
        })
    }

    fn remote_uri(uri: &str, namespace: WardrobeNamespace) -> io::Result<Self> {
        let runner = TcpWardrobeRunner::connect(uri, None)?;
        Ok(Self {
            name: "Wardrobe (Remote TCP Server Mode)".to_string(),
            runner: Some(Box::new(runner)),
            storage_root: None,
            server_handle: None,
            namespace,
            profile: None,
            last_storage_snapshot: None,
        })
    }

    fn execute(&mut self, command: Command) -> io::Result<CommandResult> {
        let runner = self.runner.as_deref_mut().ok_or_else(|| {
            Error::new(
                ErrorKind::BrokenPipe,
                "Wardrobe benchmark runner is no longer available",
            )
        })?;
        runner.execute(command)
    }

    fn execute_scoped(&mut self, command: Command) -> io::Result<CommandResult> {
        self.execute(Command::ExecuteInScope {
            scope: StorageScope::schema(&self.namespace.database, &self.namespace.schema),
            command: Box::new(command),
        })
    }

    fn count_book_relationship_matches(&mut self, entity_reference: &str) -> io::Result<usize> {
        expect_count(self.execute_scoped(Command::Count {
            filter: OperationFilter::query_in(
                BOOK_DRAWER,
                json!({
                    "author_id": entity_reference,
                    "editor_id": entity_reference,
                }),
            ),
            options: OperationOptions::default(),
        })?)
    }

    fn traversal_uses_pointer_relationships(
        &mut self,
        profile: &LibraryProfile,
    ) -> io::Result<bool> {
        let entity_id = profile.traversal_entity_id(0);
        if self.count_book_relationship_matches(&entity_id)? > 0 {
            return Ok(false);
        }
        let entity_pointer = format!("@{ENTITY_DRAWER}:{entity_id}");
        Ok(self.count_book_relationship_matches(&entity_pointer)? > 0)
    }

    fn capture_storage_snapshot(&mut self) -> io::Result<WardrobeStorageSnapshot> {
        let drawers = self.show_benchmark_drawers()?;
        let diagnosis = match self.execute(Command::Status(StatusRequest::storage())) {
            Ok(CommandResult::Status(StatusResult::Storage(diagnosis))) => Some(diagnosis),
            Ok(_) | Err(_) => None,
        };
        let root_wal_entries = self.wal_entry_count(None).ok().flatten();
        let database_name = self.namespace.database.clone();
        let database_wal_entries = self.wal_entry_count(Some(&database_name)).ok().flatten();
        let local_root_bytes = self
            .storage_root
            .as_deref()
            .and_then(|root| directory_size(root).ok());

        Ok(WardrobeStorageSnapshot {
            drawers,
            diagnosis,
            root_wal_entries,
            database_wal_entries,
            local_root_bytes,
        })
    }

    fn show_benchmark_drawers(&mut self) -> io::Result<Vec<StorageInventory>> {
        match self.execute(Command::Status(StatusRequest::drawers(
            self.namespace.database.clone(),
            self.namespace.schema.clone(),
        )))? {
            CommandResult::Status(StatusResult::Drawers(drawers)) => Ok(drawers),
            other => Err(Error::new(
                ErrorKind::InvalidData,
                format!("Expected Wardrobe drawer inventory, got {other:?}"),
            )),
        }
    }

    fn wal_entry_count(&mut self, database_name: Option<&str>) -> io::Result<Option<usize>> {
        match self.execute(Command::Status(StatusRequest::wal(
            database_name.map(str::to_string),
        )))? {
            CommandResult::Status(StatusResult::Wal(report)) => Ok(Some(report.entry_count)),
            _ => Ok(None),
        }
    }
}

impl BenchmarkTarget for WardrobeTarget {
    fn name(&self) -> &str {
        &self.name
    }

    fn provision_schema(
        &mut self,
        profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        self.profile = Some(profile.clone());
        self.last_storage_snapshot = None;
        progress.log(format!(
            "{}: creating database '{}'",
            self.name(),
            self.namespace.database
        ));
        expect_inventory(self.execute(Command::Create(CreateRequest::database(
            self.namespace.database.clone(),
        )))?)?;
        progress.log(format!(
            "{}: creating schema '{}'",
            self.name(),
            self.namespace.schema
        ));
        expect_inventory(self.execute(Command::Create(CreateRequest::schema(
            self.namespace.database.clone(),
            self.namespace.schema.clone(),
        )))?)?;
        for drawer in [ENTITY_DRAWER, BOOK_DRAWER] {
            progress.log(format!("{}: creating drawer '{}'", self.name(), drawer));
            expect_inventory(self.execute(Command::Create(CreateRequest::drawer(
                self.namespace.database.clone(),
                self.namespace.schema.clone(),
                drawer,
            )))?)?;
        }
        for field_name in ["author_id", "editor_id", "purge_bucket"] {
            progress.log(format!(
                "{}: creating book index '{}'",
                self.name(),
                field_name
            ));
            expect_admin(
                self.execute_scoped(Command::Alter(AlterRequest::schema_rule(
                    BOOK_DRAWER,
                    "add",
                    "index",
                    field_name,
                    json!({ "kind": "index" }),
                )))?,
            )?;
        }
        self.flush()
    }

    fn massive_ingestion(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (start, end) in chunk_ranges(profile.entity_records, profile.chunk_size) {
            let records = (start..end)
                .map(|index| profile.entity_payload(index))
                .collect::<Vec<_>>();
            recorder.measure((end - start) as u64, || {
                expect_pointers(self.execute_scoped(Command::Upsert {
                    payload: Value::Array(records),
                    filter: OperationFilter::drawer(ENTITY_DRAWER),
                    options: OperationOptions::new().atomic(true),
                })?)
            })?;
            report_record_progress(
                progress,
                &format!("{}: entities ingested", self.name()),
                end,
                profile.entity_records,
            );
        }
        for (start, end) in chunk_ranges(profile.book_records, profile.chunk_size) {
            let records = (start..end)
                .map(|index| profile.book_payload(index))
                .collect::<Vec<_>>();
            recorder.measure((end - start) as u64, || {
                expect_pointers(self.execute_scoped(Command::Upsert {
                    payload: Value::Array(records),
                    filter: OperationFilter::drawer(BOOK_DRAWER),
                    options: OperationOptions::new().atomic(true),
                })?)
            })?;
            report_record_progress(
                progress,
                &format!("{}: books ingested", self.name()),
                end,
                profile.book_records,
            );
        }
        Ok((profile.entity_records + profile.book_records) as u64)
    }

    fn index_mutation(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (index, (action, kind)) in [("add", "index"), ("remove", "index"), ("add", "index")]
            .into_iter()
            .enumerate()
        {
            progress.log(format!(
                "{}: index mutation step {}/3: {} {} on books.isbn",
                self.name(),
                index + 1,
                action,
                kind
            ));
            recorder.measure(1, || {
                expect_admin(
                    self.execute_scoped(Command::Alter(AlterRequest::schema_rule(
                        BOOK_DRAWER,
                        action,
                        kind,
                        "isbn",
                        json!({ "kind": kind }),
                    )))?,
                )
            })?;
        }
        Ok(3)
    }

    fn complex_traversal(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let use_pointer_relationships = self.traversal_uses_pointer_relationships(profile)?;
        let relationship_filter_mode = if use_pointer_relationships {
            "pointer references"
        } else {
            "plain entity ids"
        };
        progress.log(format!(
            "{}: traversal filters using {}",
            self.name(),
            relationship_filter_mode
        ));
        for query_index in 0..profile.traversal_queries {
            let entity_id = profile.traversal_entity_id(query_index);
            let entity_reference = if use_pointer_relationships {
                format!("@{ENTITY_DRAWER}:{entity_id}")
            } else {
                entity_id
            };
            recorder.measure(1, || {
                expect_records(self.execute_scoped(Command::Read {
                    filter: OperationFilter::query_in(
                        BOOK_DRAWER,
                        json!({
                            "author_id": entity_reference,
                            "editor_id": entity_reference,
                        }),
                    ),
                    options: OperationOptions::default(),
                })?)?;
                Ok(())
            })?;
            report_record_progress(
                progress,
                &format!("{}: traversal queries completed", self.name()),
                query_index + 1,
                profile.traversal_queries,
            );
        }
        Ok(profile.traversal_queries as u64)
    }

    fn targeted_purge(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let operations = profile.expected_purge_count() as u64;
        progress.log(format!(
            "{}: deleting about {} book records where purge_bucket = 0",
            self.name(),
            operations
        ));
        recorder.measure(operations.max(1), || {
            expect_delete(self.execute_scoped(Command::Delete {
                filter: OperationFilter::query_in(BOOK_DRAWER, json!({ "purge_bucket": 0 })),
                options: OperationOptions::default(),
            })?)
            .map(|_| ())
        })?;
        Ok(operations.max(1))
    }

    fn compaction(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        progress.log(format!("{}: vacuuming book drawer", self.name()));
        recorder.measure(1, || {
            expect_vacuumed(
                self.execute_scoped(Command::Compact(CompactRequest::drawer(BOOK_DRAWER)))?,
            )
        })?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(root) = &self.storage_root {
            fsync_tree(root)?;
        } else if let Ok(CommandResult::Status(StatusResult::Storage(diagnosis))) =
            self.execute(Command::Status(StatusRequest::storage()))
        {
            let path = PathBuf::from(diagnosis.storage_directory);
            if path.exists() {
                fsync_tree(&path)?;
            }
        }
        Ok(())
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        let snapshot = self.capture_storage_snapshot()?;
        let storage_bytes = snapshot.benchmark_drawer_bytes();
        self.last_storage_snapshot = Some(snapshot);
        Ok(storage_bytes)
    }

    fn storage_diagnostics(&mut self) -> io::Result<Vec<String>> {
        if self.last_storage_snapshot.is_none() {
            let snapshot = self.capture_storage_snapshot()?;
            self.last_storage_snapshot = Some(snapshot);
        }
        Ok(self
            .last_storage_snapshot
            .as_ref()
            .map(|snapshot| snapshot.diagnostic_lines(&self.namespace, self.profile.as_ref()))
            .unwrap_or_default())
    }
}

impl Drop for WardrobeTarget {
    fn drop(&mut self) {
        self.runner.take();
        if let Some(handle) = self.server_handle.take() {
            let _ = handle.join();
        }
    }
}

trait WardrobeCommandRunner {
    fn execute(&mut self, command: Command) -> io::Result<CommandResult>;
}

struct EmbeddedWardrobeRunner {
    engine: WardrobeEngine,
    storage_root: PathBuf,
}

impl WardrobeCommandRunner for EmbeddedWardrobeRunner {
    fn execute(&mut self, command: Command) -> io::Result<CommandResult> {
        let _ = &self.storage_root;
        self.engine.execute_command(command)
    }
}

struct TcpWardrobeRunner {
    stream: TcpStream,
}

impl TcpWardrobeRunner {
    fn connect(uri: &str, _storage_root: Option<PathBuf>) -> io::Result<Self> {
        let target = ConnectionTarget::parse(uri)?;
        let ConnectionTarget::Network { host, port } = target else {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "--wardrobe-remote-uri must be a wardrobe://host:port TCP URI",
            ));
        };
        let stream = TcpStream::connect((host.as_str(), port))?;
        stream.set_nodelay(true)?;
        Ok(Self { stream })
    }
}

impl WardrobeCommandRunner for TcpWardrobeRunner {
    fn execute(&mut self, command: Command) -> io::Result<CommandResult> {
        let payload = serde_json::to_vec(&command).map_err(|error| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("Failed to serialize Wardrobe benchmark command: {error}"),
            )
        })?;
        ProtocolFrame::new(ProtocolOpcode::Command, payload).write_to_stream(&mut self.stream)?;
        let response = ProtocolFrame::read_from_stream(&mut self.stream)?;
        match response.opcode {
            ProtocolOpcode::Result => serde_json::from_slice(&response.payload).map_err(|error| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Failed to deserialize Wardrobe benchmark result: {error}"),
                )
            }),
            ProtocolOpcode::Error => Err(Error::other(
                String::from_utf8_lossy(&response.payload).into_owned(),
            )),
            ProtocolOpcode::Command => Err(Error::new(
                ErrorKind::InvalidData,
                "Wardrobe benchmark expected a result frame, got a command frame",
            )),
        }
    }
}

struct SqliteTarget {
    connection: Connection,
    path: PathBuf,
}

impl SqliteTarget {
    fn new(path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(&path).map_err(to_io_error)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(to_io_error)?;
        Ok(Self { connection, path })
    }
}

impl BenchmarkTarget for SqliteTarget {
    fn name(&self) -> &str {
        "SQLite (Local WAL File Mode)"
    }

    fn provision_schema(
        &mut self,
        _profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!("{}: executing schema setup SQL", self.name()));
        self.connection
            .execute_batch(
                r#"
PRAGMA journal_mode=WAL;
PRAGMA synchronous=FULL;
DROP TABLE IF EXISTS books;
DROP TABLE IF EXISTS entities;
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
    purge_bucket INTEGER NOT NULL,
    FOREIGN KEY(author_id) REFERENCES entities(id),
    FOREIGN KEY(editor_id) REFERENCES entities(id)
);
"#,
            )
            .map_err(to_io_error)?;
        self.flush()
    }

    fn massive_ingestion(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (start, end) in chunk_ranges(profile.entity_records, profile.chunk_size) {
            let sql = sqlite_entity_insert(profile, start, end);
            recorder.measure((end - start) as u64, || {
                self.connection.execute_batch(&sql).map_err(to_io_error)
            })?;
            report_record_progress(
                progress,
                &format!("{}: entities ingested", self.name()),
                end,
                profile.entity_records,
            );
        }
        for (start, end) in chunk_ranges(profile.book_records, profile.chunk_size) {
            let sql = sqlite_book_insert(profile, start, end);
            recorder.measure((end - start) as u64, || {
                self.connection.execute_batch(&sql).map_err(to_io_error)
            })?;
            report_record_progress(
                progress,
                &format!("{}: books ingested", self.name()),
                end,
                profile.book_records,
            );
        }
        Ok((profile.entity_records + profile.book_records) as u64)
    }

    fn index_mutation(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (index, (label, sql)) in [
            (
                "create idx_books_isbn",
                "CREATE INDEX IF NOT EXISTS idx_books_isbn ON books(isbn);",
            ),
            (
                "drop idx_books_isbn",
                "DROP INDEX IF EXISTS idx_books_isbn;",
            ),
            (
                "recreate idx_books_isbn",
                "CREATE INDEX IF NOT EXISTS idx_books_isbn ON books(isbn);",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            progress.log(format!(
                "{}: index mutation step {}/3: {}",
                self.name(),
                index + 1,
                label
            ));
            recorder.measure(1, || {
                self.connection.execute_batch(sql).map_err(to_io_error)
            })?;
        }
        Ok(3)
    }

    fn complex_traversal(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for query_index in 0..profile.traversal_queries {
            let entity_id = profile.traversal_entity_id(query_index);
            recorder.measure(1, || {
                let mut statement = self
                    .connection
                    .prepare(SQLITE_MATERIALIZED_BOOK_QUERY)
                    .map_err(to_io_error)?;
                let rows = statement
                    .query_map([entity_id.as_str()], sqlite_materialized_book_value)
                    .map_err(to_io_error)?;
                for row in rows {
                    let _record = row.map_err(to_io_error)?;
                }
                Ok(())
            })?;
            report_record_progress(
                progress,
                &format!("{}: traversal queries completed", self.name()),
                query_index + 1,
                profile.traversal_queries,
            );
        }
        Ok(profile.traversal_queries as u64)
    }

    fn targeted_purge(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let operations = profile.expected_purge_count() as u64;
        progress.log(format!(
            "{}: deleting about {} book records where purge_bucket = 0",
            self.name(),
            operations
        ));
        recorder.measure(operations.max(1), || {
            self.connection
                .execute("DELETE FROM books WHERE purge_bucket = 0;", [])
                .map(|_| ())
                .map_err(to_io_error)
        })?;
        Ok(operations.max(1))
    }

    fn compaction(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        progress.log(format!("{}: running VACUUM", self.name()));
        recorder.measure(1, || {
            self.connection
                .execute_batch("VACUUM;")
                .map_err(to_io_error)
        })?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(to_io_error)?;
        sync_file_if_exists(&self.path)?;
        sync_file_if_exists(&sqlite_sidecar(&self.path, "-wal"))?;
        sync_file_if_exists(&sqlite_sidecar(&self.path, "-shm"))?;
        Ok(())
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        Ok(file_size_or_zero(&self.path)?
            + file_size_or_zero(sqlite_sidecar(&self.path, "-wal"))?
            + file_size_or_zero(sqlite_sidecar(&self.path, "-shm"))?)
    }
}

struct MongoTarget {
    client: MongoClient,
    database: String,
}

impl MongoTarget {
    fn new(mongo_uri: String, database: String) -> io::Result<Self> {
        let client = MongoClient::with_uri_str(&mongo_uri).map_err(to_io_error)?;
        client
            .database("admin")
            .run_command(doc! { "ping": 1 }, None)
            .map_err(to_io_error)?;
        Ok(Self { client, database })
    }

    fn entities(&self) -> Collection<Document> {
        self.client
            .database(&self.database)
            .collection::<Document>("entities")
    }

    fn books(&self) -> Collection<Document> {
        self.client
            .database(&self.database)
            .collection::<Document>("books")
    }

    fn insert_documents(&self, drawer: &str, documents: Vec<Document>) -> io::Result<()> {
        if documents.is_empty() {
            return Ok(());
        }
        if drawer == ENTITY_DRAWER {
            self.entities()
                .insert_many(documents, None)
                .map(|_| ())
                .map_err(to_io_error)
        } else {
            self.books()
                .insert_many(documents, None)
                .map(|_| ())
                .map_err(to_io_error)
        }
    }
}

impl BenchmarkTarget for MongoTarget {
    fn name(&self) -> &str {
        "MongoDB (Document Store Base Comparison)"
    }

    fn provision_schema(
        &mut self,
        _profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!(
            "{}: dropping and recreating MongoDB collections in '{}'",
            self.name(),
            self.database
        ));
        let database = self.client.database(&self.database);
        database.drop(None).map_err(to_io_error)?;
        database
            .create_collection("entities", None)
            .map_err(to_io_error)?;
        database
            .create_collection("books", None)
            .map_err(to_io_error)?;
        progress.log(format!(
            "{}: MongoDB collections are ready; requesting disk sync",
            self.name()
        ));
        self.flush()
    }

    fn massive_ingestion(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (start, end) in chunk_ranges(profile.entity_records, profile.chunk_size) {
            let documents = mongo_documents(profile, ENTITY_DRAWER, start, end)?;
            recorder.measure((end - start) as u64, || {
                self.insert_documents(ENTITY_DRAWER, documents)
            })?;
            report_record_progress(
                progress,
                &format!("{}: entities ingested", self.name()),
                end,
                profile.entity_records,
            );
        }
        for (start, end) in chunk_ranges(profile.book_records, profile.chunk_size) {
            let documents = mongo_documents(profile, BOOK_DRAWER, start, end)?;
            recorder.measure((end - start) as u64, || {
                self.insert_documents(BOOK_DRAWER, documents)
            })?;
            report_record_progress(
                progress,
                &format!("{}: books ingested", self.name()),
                end,
                profile.book_records,
            );
        }
        Ok((profile.entity_records + profile.book_records) as u64)
    }

    fn index_mutation(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (index, (label, operation)) in [
            ("create isbn_1", "create"),
            ("drop isbn_1", "drop"),
            ("recreate isbn_1", "create"),
        ]
        .into_iter()
        .enumerate()
        {
            progress.log(format!(
                "{}: index mutation step {}/3: {}",
                self.name(),
                index + 1,
                label
            ));
            recorder.measure(1, || {
                if operation == "drop" {
                    self.books()
                        .drop_index("isbn_1", None)
                        .map(|_| ())
                        .map_err(to_io_error)
                } else {
                    self.books()
                        .create_index(IndexModel::builder().keys(doc! { "isbn": 1 }).build(), None)
                        .map(|_| ())
                        .map_err(to_io_error)
                }
            })?;
        }
        Ok(3)
    }

    fn complex_traversal(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for query_index in 0..profile.traversal_queries {
            let entity_id = profile.traversal_entity_id(query_index);
            recorder.measure(1, || {
                let cursor = self
                    .books()
                    .aggregate(mongo_materialized_book_pipeline(&entity_id), None)
                    .map_err(to_io_error)?;
                for document in cursor {
                    let _record = document.map_err(to_io_error)?;
                }
                Ok(())
            })?;
            report_record_progress(
                progress,
                &format!("{}: traversal queries completed", self.name()),
                query_index + 1,
                profile.traversal_queries,
            );
        }
        Ok(profile.traversal_queries as u64)
    }

    fn targeted_purge(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let operations = profile.expected_purge_count() as u64;
        progress.log(format!(
            "{}: deleting about {} book records where purge_bucket = 0",
            self.name(),
            operations
        ));
        recorder.measure(operations.max(1), || {
            self.books()
                .delete_many(doc! { "purge_bucket": 0_i64 }, None)
                .map(|_| ())
                .map_err(to_io_error)
        })?;
        Ok(operations.max(1))
    }

    fn compaction(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        progress.log(format!(
            "{}: running compact/validate fallback on books",
            self.name()
        ));
        recorder.measure(1, || {
            let database = self.client.database(&self.database);
            database
                .run_command(doc! { "compact": "books", "force": true }, None)
                .or_else(|_| {
                    database.run_command(doc! { "validate": "books", "full": false }, None)
                })
                .map(|_| ())
                .map_err(to_io_error)
        })?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.client
            .database("admin")
            .run_command(doc! { "fsync": 1 }, None)
            .map(|_| ())
            .map_err(to_io_error)
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        let stats = self
            .client
            .database(&self.database)
            .run_command(doc! { "dbStats": 1 }, None)
            .map_err(to_io_error)?;
        Ok(bson_number_to_u64(stats.get("storageSize")).unwrap_or(0))
    }
}

struct MySqlTarget {
    connection: PooledConn,
    database: String,
}

impl MySqlTarget {
    fn new(
        host: String,
        port: u16,
        database: String,
        user: Option<String>,
        password_env: Option<String>,
    ) -> io::Result<Self> {
        let fallback_credentials = read_default_mysql_credentials()?;
        let resolved_user = user
            .or_else(|| env::var(DEFAULT_MYSQL_USER_ENV).ok())
            .or(fallback_credentials.user)
            .unwrap_or_else(|| DEFAULT_MYSQL_USER.to_string());
        let password = match password_env {
            Some(name) => match env::var(&name) {
                Ok(value) => Some(value),
                Err(env::VarError::NotPresent) if name == DEFAULT_MYSQL_PASSWORD_ENV => {
                    fallback_credentials
                        .password
                        .or_else(|| Some(DEFAULT_MYSQL_PASSWORD.to_string()))
                }
                Err(env::VarError::NotPresent) => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("MySQL password environment variable '{name}' is not set"),
                    ));
                }
                Err(env::VarError::NotUnicode(_)) => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("MySQL password environment variable '{name}' is not valid UTF-8"),
                    ));
                }
            },
            None => None,
        };
        let mut builder = OptsBuilder::new().ip_or_hostname(Some(host)).tcp_port(port);
        builder = builder.user(Some(resolved_user));
        if let Some(password) = password {
            builder = builder.pass(Some(password));
        }
        let pool = Pool::new(builder).map_err(to_io_error)?;
        let connection = pool.get_conn().map_err(to_io_error)?;
        Ok(Self {
            connection,
            database,
        })
    }
}

impl BenchmarkTarget for MySqlTarget {
    fn name(&self) -> &str {
        "MySQL / MariaDB (Relational Pointer Base Comparison)"
    }

    fn provision_schema(
        &mut self,
        _profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!(
            "{}: creating database and benchmark tables in '{}'",
            self.name(),
            self.database
        ));
        let database = mysql_identifier(&self.database);
        self.connection
            .query_drop(format!("CREATE DATABASE IF NOT EXISTS `{database}`"))
            .map_err(to_io_error)?;
        self.connection
            .query_drop(format!("USE `{database}`"))
            .map_err(to_io_error)?;
        self.connection
            .query_drop("DROP TABLE IF EXISTS books")
            .map_err(to_io_error)?;
        self.connection
            .query_drop("DROP TABLE IF EXISTS entities")
            .map_err(to_io_error)?;
        self.connection
            .query_drop(
                r#"
CREATE TABLE entities (
    id VARCHAR(64) PRIMARY KEY,
    display_name VARCHAR(255) NOT NULL,
    role VARCHAR(32) NOT NULL,
    cohort BIGINT NOT NULL
) ENGINE=InnoDB
"#,
            )
            .map_err(to_io_error)?;
        self.connection
            .query_drop(
                r#"
CREATE TABLE books (
    id VARCHAR(64) PRIMARY KEY,
    isbn VARCHAR(64) NOT NULL,
    title VARCHAR(255) NOT NULL,
    author_id VARCHAR(64) NOT NULL,
    editor_id VARCHAR(64) NOT NULL,
    branch VARCHAR(32) NOT NULL,
    quantity BIGINT NOT NULL,
    purge_bucket BIGINT NOT NULL,
    CONSTRAINT fk_books_author FOREIGN KEY (author_id) REFERENCES entities(id),
    CONSTRAINT fk_books_editor FOREIGN KEY (editor_id) REFERENCES entities(id)
) ENGINE=InnoDB
"#,
            )
            .map_err(to_io_error)?;
        self.flush()
    }

    fn massive_ingestion(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (start, end) in chunk_ranges(profile.entity_records, profile.chunk_size) {
            let sql = mysql_entity_insert(profile, start, end);
            recorder.measure((end - start) as u64, || {
                self.connection.query_drop(&sql).map_err(to_io_error)
            })?;
            report_record_progress(
                progress,
                &format!("{}: entities ingested", self.name()),
                end,
                profile.entity_records,
            );
        }
        for (start, end) in chunk_ranges(profile.book_records, profile.chunk_size) {
            let sql = mysql_book_insert(profile, start, end);
            recorder.measure((end - start) as u64, || {
                self.connection.query_drop(&sql).map_err(to_io_error)
            })?;
            report_record_progress(
                progress,
                &format!("{}: books ingested", self.name()),
                end,
                profile.book_records,
            );
        }
        Ok((profile.entity_records + profile.book_records) as u64)
    }

    fn index_mutation(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (index, (label, sql)) in [
            (
                "create idx_books_isbn",
                "CREATE INDEX idx_books_isbn ON books(isbn)",
            ),
            ("drop idx_books_isbn", "DROP INDEX idx_books_isbn ON books"),
            (
                "recreate idx_books_isbn",
                "CREATE INDEX idx_books_isbn ON books(isbn)",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            progress.log(format!(
                "{}: index mutation step {}/3: {}",
                self.name(),
                index + 1,
                label
            ));
            recorder.measure(1, || self.connection.query_drop(sql).map_err(to_io_error))?;
        }
        Ok(3)
    }

    fn complex_traversal(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for query_index in 0..profile.traversal_queries {
            let entity_id = sql_string(&profile.traversal_entity_id(query_index));
            recorder.measure(1, || {
                let rows = self
                    .connection
                    .query_iter(mysql_materialized_book_query(&entity_id))
                    .map_err(to_io_error)?;
                for row in rows {
                    let row = row.map_err(to_io_error)?;
                    let _record = mysql_materialized_book_value(&row)?;
                }
                Ok(())
            })?;
            report_record_progress(
                progress,
                &format!("{}: traversal queries completed", self.name()),
                query_index + 1,
                profile.traversal_queries,
            );
        }
        Ok(profile.traversal_queries as u64)
    }

    fn targeted_purge(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let operations = profile.expected_purge_count() as u64;
        progress.log(format!(
            "{}: deleting about {} book records where purge_bucket = 0",
            self.name(),
            operations
        ));
        recorder.measure(operations.max(1), || {
            self.connection
                .query_drop("DELETE FROM books WHERE purge_bucket = 0")
                .map_err(to_io_error)
        })?;
        Ok(operations.max(1))
    }

    fn compaction(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        progress.log(format!("{}: running OPTIMIZE TABLE books", self.name()));
        recorder.measure(1, || {
            self.connection
                .query_drop("OPTIMIZE TABLE books")
                .map_err(to_io_error)
        })?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.connection
            .query_drop("FLUSH TABLES")
            .map_err(to_io_error)
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        let database = sql_string(&self.database);
        self.connection
            .query_first::<u64, _>(format!(
                "SELECT COALESCE(SUM(DATA_LENGTH + INDEX_LENGTH), 0) FROM information_schema.TABLES WHERE TABLE_SCHEMA = {database}"
            ))
            .map(|value| value.unwrap_or(0))
            .map_err(to_io_error)
    }
}

struct Neo4jTarget {
    graph: Graph,
    runtime: Runtime,
    database: String,
    marker: String,
}

impl Neo4jTarget {
    fn new(
        uri: String,
        database: String,
        user: String,
        password_env: String,
        marker: String,
    ) -> io::Result<Self> {
        let fallback_credentials = read_default_neo4j_credentials()?;
        let resolved_user = if user == DEFAULT_NEO4J_USER {
            env::var(DEFAULT_NEO4J_USER_ENV)
                .ok()
                .or_else(|| fallback_credentials.user.clone())
                .unwrap_or(user)
        } else {
            user
        };
        let password = match env::var(&password_env) {
            Ok(value) => value,
            Err(env::VarError::NotPresent) if password_env == DEFAULT_NEO4J_PASSWORD_ENV => {
                fallback_credentials
                    .password
                    .unwrap_or_else(|| DEFAULT_NEO4J_PASSWORD.to_string())
            }
            Err(env::VarError::NotPresent) => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Neo4j password environment variable '{password_env}' is not set"),
                ));
            }
            Err(env::VarError::NotUnicode(_)) => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "Neo4j password environment variable '{password_env}' is not valid UTF-8"
                    ),
                ));
            }
        };
        let runtime = Runtime::new().map_err(to_io_error)?;
        let graph = runtime.block_on(async {
            let config = neo4rs::ConfigBuilder::default()
                .uri(uri)
                .user(resolved_user)
                .password(password)
                .db(database.clone())
                .build()
                .map_err(to_io_error)?;
            Graph::connect(config).await.map_err(to_io_error)
        })?;
        Ok(Self {
            graph,
            runtime,
            database,
            marker,
        })
    }

    fn run_query(&self, q: neo4rs::Query) -> io::Result<()> {
        self.runtime
            .block_on(async { self.graph.run(q).await.map_err(to_io_error) })
    }

    fn count_query(&self, q: neo4rs::Query, field: &str) -> io::Result<u64> {
        self.runtime.block_on(async {
            let mut stream = self.graph.execute(q).await.map_err(to_io_error)?;
            if let Some(row) = stream.next().await.map_err(to_io_error)? {
                let count: i64 = row.get(field).map_err(to_io_error)?;
                return Ok(u64::try_from(count).unwrap_or(0));
            }
            Ok(0)
        })
    }

    fn query_barrier(&self) -> io::Result<()> {
        self.runtime.block_on(async {
            let mut stream = self
                .graph
                .execute(query("RETURN 1 AS ok"))
                .await
                .map_err(to_io_error)?;
            while let Some(_row) = stream.next().await.map_err(to_io_error)? {}
            Ok(())
        })
    }

    fn checkpoint_or_barrier(&self, progress: Option<&ProgressReporter>) -> io::Result<()> {
        match self.run_query(query("CALL db.checkpoint()")) {
            Ok(()) => Ok(()),
            Err(error) if neo4j_checkpoint_is_unavailable(&error) => {
                if let Some(progress) = progress {
                    progress.log(format!(
                        "{}: Neo4j db.checkpoint() unavailable; using query barrier",
                        self.name()
                    ));
                }
                self.query_barrier()
            }
            Err(error) => Err(error),
        }
    }

    fn entity_rows(&self, profile: &LibraryProfile, start: usize, end: usize) -> BoltType {
        neo4j_entity_rows(profile, start, end)
    }

    fn book_rows(&self, profile: &LibraryProfile, start: usize, end: usize) -> BoltType {
        neo4j_book_rows(profile, start, end)
    }
}

impl BenchmarkTarget for Neo4jTarget {
    fn name(&self) -> &str {
        "Neo4j (Graph Database Base Comparison)"
    }

    fn provision_schema(
        &mut self,
        _profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!(
            "{}: clearing benchmark nodes in database '{}'",
            self.name(),
            self.database
        ));
        self.run_query(
            query("MATCH (n:BenchNode {bench_marker: $marker}) DETACH DELETE n")
                .param("marker", self.marker.clone()),
        )?;

        progress.log(format!(
            "{}: creating Neo4j constraints/indexes",
            self.name()
        ));
        self.run_query(query(
            "CREATE CONSTRAINT IF NOT EXISTS FOR (e:Entity) REQUIRE (e.bench_marker, e.id) IS UNIQUE",
        ))?;
        self.run_query(query(
            "CREATE CONSTRAINT IF NOT EXISTS FOR (b:Book) REQUIRE (b.bench_marker, b.id) IS UNIQUE",
        ))?;
        self.run_query(query("CREATE INDEX IF NOT EXISTS FOR (b:Book) ON (b.isbn)"))?;
        self.run_query(query(
            "CREATE INDEX IF NOT EXISTS FOR (b:Book) ON (b.purge_bucket)",
        ))?;
        self.flush()
    }

    fn massive_ingestion(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (start, end) in chunk_ranges(profile.entity_records, profile.chunk_size) {
            let rows = self.entity_rows(profile, start, end);
            recorder.measure((end - start) as u64, || {
                self.run_query(
                    query(NEO4J_BULK_ENTITY_UPSERT_QUERY)
                        .param("marker", self.marker.clone())
                        .param("rows", rows),
                )
            })?;
            report_record_progress(
                progress,
                &format!("{}: entities ingested", self.name()),
                end,
                profile.entity_records,
            );
        }

        for (start, end) in chunk_ranges(profile.book_records, profile.chunk_size) {
            let rows = self.book_rows(profile, start, end);
            recorder.measure((end - start) as u64, || {
                self.run_query(
                    query(NEO4J_BULK_BOOK_UPSERT_QUERY)
                        .param("marker", self.marker.clone())
                        .param("rows", rows),
                )
            })?;
            report_record_progress(
                progress,
                &format!("{}: books ingested", self.name()),
                end,
                profile.book_records,
            );
        }
        Ok((profile.entity_records + profile.book_records) as u64)
    }

    fn index_mutation(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (index, (label, cypher)) in [
            (
                "create bench_isbn_idx",
                "CREATE INDEX bench_isbn_idx IF NOT EXISTS FOR (b:Book) ON (b.isbn)",
            ),
            ("drop bench_isbn_idx", "DROP INDEX bench_isbn_idx IF EXISTS"),
            (
                "recreate bench_isbn_idx",
                "CREATE INDEX bench_isbn_idx IF NOT EXISTS FOR (b:Book) ON (b.isbn)",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            progress.log(format!(
                "{}: index mutation step {}/3: {}",
                self.name(),
                index + 1,
                label
            ));
            recorder.measure(1, || self.run_query(query(cypher)))?;
        }
        Ok(3)
    }

    fn complex_traversal(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for query_index in 0..profile.traversal_queries {
            let entity_id = profile.traversal_entity_id(query_index);
            recorder.measure(1, || {
                self.runtime.block_on(async {
                    let mut stream = self
                        .graph
                        .execute(
                            query(
                                "MATCH (author:BenchNode:Entity {bench_marker: $marker, id: $entity_id})<-[:AUTHORED_BY]-(b:BenchNode:Book)-[:EDITED_BY]->(editor:BenchNode:Entity {bench_marker: $marker, id: $entity_id}) RETURN b.id AS book_id, b.isbn AS isbn, b.title AS title, b.author_id AS author_id, b.editor_id AS editor_id, b.branch AS branch, b.quantity AS quantity, b.purge_bucket AS purge_bucket, author.id AS author_entity_id, author.display_name AS author_display_name, author.role AS author_role, author.cohort AS author_cohort, editor.id AS editor_entity_id, editor.display_name AS editor_display_name, editor.role AS editor_role, editor.cohort AS editor_cohort",
                            )
                            .param("marker", self.marker.clone())
                            .param("entity_id", entity_id),
                        )
                        .await
                        .map_err(to_io_error)?;
                    while let Some(_row) = stream.next().await.map_err(to_io_error)? {}
                    Ok(())
                })
            })?;
            report_record_progress(
                progress,
                &format!("{}: traversal queries completed", self.name()),
                query_index + 1,
                profile.traversal_queries,
            );
        }
        Ok(profile.traversal_queries as u64)
    }

    fn targeted_purge(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let operations = profile.expected_purge_count() as u64;
        progress.log(format!(
            "{}: deleting about {} book records where purge_bucket = 0",
            self.name(),
            operations
        ));
        recorder.measure(operations.max(1), || {
            self.run_query(
                query(
                    "MATCH (b:BenchNode:Book {bench_marker: $marker, purge_bucket: 0}) DETACH DELETE b",
                )
                .param("marker", self.marker.clone()),
            )
        })?;
        Ok(operations.max(1))
    }

    fn compaction(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        progress.log(format!(
            "{}: issuing Neo4j checkpoint or query barrier",
            self.name()
        ));
        recorder.measure(1, || self.checkpoint_or_barrier(Some(progress)))?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.checkpoint_or_barrier(None)
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        self.count_query(
            query(NEO4J_STORE_SIZE_BYTES_QUERY).param("database", self.database.clone()),
            "storage_bytes",
        )
    }

    fn storage_diagnostics(&mut self) -> io::Result<Vec<String>> {
        let node_count = self.count_query(
            query("MATCH (n:BenchNode {bench_marker: $marker}) RETURN count(n) AS node_count")
                .param("marker", self.marker.clone()),
            "node_count",
        )?;
        let relationship_count = self.count_query(
            query(
                "MATCH (:BenchNode {bench_marker: $marker})-[r]->(:BenchNode {bench_marker: $marker}) RETURN count(r) AS relationship_count",
            )
            .param("marker", self.marker.clone()),
            "relationship_count",
        )?;
        Ok(vec![
            format!(
                "Benchmark scope marker '{}' in database '{}' contains {} nodes and {} relationships",
                self.marker, self.database, node_count, relationship_count
            ),
            "Neo4j storage metric reports database store size bytes from JMX; logical node and relationship counts are diagnostic only.".to_string(),
        ])
    }
}

fn parse_targets(raw: &str) -> io::Result<Vec<TargetSpec>> {
    if raw.trim().eq_ignore_ascii_case("all") {
        return Ok(TargetSpec::all());
    }
    raw.split(',')
        .map(|part| match part.trim().to_ascii_lowercase().as_str() {
            "wardrobe-embedded" | "embedded" => Ok(TargetSpec::WardrobeEmbedded),
            "wardrobe-remote" | "remote" | "wardrobe-tcp" => Ok(TargetSpec::WardrobeRemote),
            "sqlite" => Ok(TargetSpec::Sqlite),
            "mongodb" | "mongo" => Ok(TargetSpec::MongoDb),
            "mysql" | "mariadb" => Ok(TargetSpec::MySql),
            "neo4j" | "neo" => Ok(TargetSpec::Neo4j),
            "" => Err(Error::new(
                ErrorKind::InvalidInput,
                "--targets contains an empty target name",
            )),
            other => Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Unsupported benchmark target: {other}"),
            )),
        })
        .collect()
}

fn required_value(args: &mut impl Iterator<Item = String>, flag: &str) -> io::Result<String> {
    args.next().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("{flag} requires a following value"),
        )
    })
}

fn parse_positive_usize(flag: &str, raw: &str) -> io::Result<usize> {
    let parsed = raw.parse::<usize>().map_err(|error| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("Invalid {flag} value '{raw}': {error}"),
        )
    })?;
    if parsed == 0 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{flag} must be greater than zero"),
        ));
    }
    Ok(parsed)
}

fn parse_positive_u64(flag: &str, raw: &str) -> io::Result<u64> {
    let parsed = raw.parse::<u64>().map_err(|error| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("Invalid {flag} value '{raw}': {error}"),
        )
    })?;
    if parsed == 0 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{flag} must be greater than zero"),
        ));
    }
    Ok(parsed)
}

fn parse_wardrobe_durability_policy(raw: &str) -> io::Result<DurabilityPolicy> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "strict" => Ok(DurabilityPolicy::Strict),
        "grouped" | "group" | "group-commit" => Ok(default_grouped_durability_policy()),
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unsupported --wardrobe-durability value: {other}"),
        )),
    }
}

fn default_grouped_durability_policy() -> DurabilityPolicy {
    DurabilityPolicy::Grouped {
        commit_window_ms: DEFAULT_WARDROBE_GROUP_COMMIT_WINDOW_MS,
        max_batch_size: DEFAULT_WARDROBE_GROUP_COMMIT_MAX_BATCH,
    }
}

fn update_group_commit_window(policy: DurabilityPolicy, commit_window_ms: u64) -> DurabilityPolicy {
    match policy {
        DurabilityPolicy::Grouped { max_batch_size, .. } => DurabilityPolicy::Grouped {
            commit_window_ms,
            max_batch_size,
        },
        DurabilityPolicy::Strict => DurabilityPolicy::Grouped {
            commit_window_ms,
            max_batch_size: DEFAULT_WARDROBE_GROUP_COMMIT_MAX_BATCH,
        },
    }
}

fn update_group_commit_max_batch(
    policy: DurabilityPolicy,
    max_batch_size: usize,
) -> DurabilityPolicy {
    match policy {
        DurabilityPolicy::Grouped {
            commit_window_ms, ..
        } => DurabilityPolicy::Grouped {
            commit_window_ms,
            max_batch_size,
        },
        DurabilityPolicy::Strict => DurabilityPolicy::Grouped {
            commit_window_ms: DEFAULT_WARDROBE_GROUP_COMMIT_WINDOW_MS,
            max_batch_size,
        },
    }
}

fn validate_wardrobe_namespace_component(flag: &str, value: &str) -> io::Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed == "."
        || trimmed == ".."
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{flag} must be a non-empty single path segment"),
        ));
    }
    Ok(())
}

fn identifier_fragment(value: &str) -> String {
    let fragment = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    if fragment.is_empty() {
        "run".to_string()
    } else {
        fragment
    }
}

fn report_record_progress(
    progress: &ProgressReporter,
    label: &str,
    completed: usize,
    total: usize,
) {
    if total == 0 {
        return;
    }
    let interval = (total / 20).max(1);
    if completed == total || completed % interval == 0 {
        let percent = completed as f64 * 100.0 / total as f64;
        progress.log(format!("{label}: {completed}/{total} ({percent:.1}%)"));
    }
}

fn to_io_error(error: impl std::fmt::Display) -> io::Error {
    Error::other(error.to_string())
}

fn neo4j_checkpoint_is_unavailable(error: &io::Error) -> bool {
    let message = error.to_string();
    message.contains("ProcedureNotFound") && message.contains("db.checkpoint")
}

fn neo4j_entity_rows(profile: &LibraryProfile, start: usize, end: usize) -> BoltType {
    let rows = (start..end)
        .map(|index| {
            let payload = profile.entity_payload(index);
            neo4j_row([
                ("id", neo4j_string_field(&payload, "_id")),
                ("display_name", neo4j_string_field(&payload, "display_name")),
                ("role", neo4j_string_field(&payload, "role")),
                ("cohort", neo4j_i64_field(&payload, "cohort")),
            ])
        })
        .collect::<Vec<_>>();
    BoltType::List(BoltList::from(rows))
}

fn neo4j_book_rows(profile: &LibraryProfile, start: usize, end: usize) -> BoltType {
    let rows = (start..end)
        .map(|index| {
            let payload = profile.book_payload(index);
            neo4j_row([
                ("id", neo4j_string_field(&payload, "_id")),
                ("isbn", neo4j_string_field(&payload, "isbn")),
                ("title", neo4j_string_field(&payload, "title")),
                ("author_id", neo4j_string_field(&payload, "author_id")),
                ("editor_id", neo4j_string_field(&payload, "editor_id")),
                ("branch", neo4j_string_field(&payload, "branch")),
                ("quantity", neo4j_i64_field(&payload, "quantity")),
                ("purge_bucket", neo4j_i64_field(&payload, "purge_bucket")),
            ])
        })
        .collect::<Vec<_>>();
    BoltType::List(BoltList::from(rows))
}

fn neo4j_row<const N: usize>(fields: [(&str, BoltType); N]) -> BoltType {
    let values = fields
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect::<HashMap<_, _>>();
    BoltType::from(values)
}

fn neo4j_string_field(payload: &Value, field: &str) -> BoltType {
    BoltType::from(payload[field].as_str().unwrap_or_default().to_string())
}

fn neo4j_i64_field(payload: &Value, field: &str) -> BoltType {
    BoltType::from(payload[field].as_u64().unwrap_or_default() as i64)
}

#[derive(Default, Debug, PartialEq, Eq)]
struct ServiceCredentials {
    user: Option<String>,
    password: Option<String>,
}

fn read_default_mysql_credentials() -> io::Result<ServiceCredentials> {
    read_credentials_file(
        DEFAULT_MYSQL_CREDENTIALS_FILE,
        DEFAULT_MYSQL_USER_ENV,
        DEFAULT_MYSQL_PASSWORD_ENV,
    )
}

fn read_default_neo4j_credentials() -> io::Result<ServiceCredentials> {
    read_credentials_file(
        DEFAULT_NEO4J_CREDENTIALS_FILE,
        DEFAULT_NEO4J_USER_ENV,
        DEFAULT_NEO4J_PASSWORD_ENV,
    )
}

fn read_credentials_file(
    path: &str,
    user_env: &str,
    password_env: &str,
) -> io::Result<ServiceCredentials> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(ServiceCredentials::default());
        }
        Err(error) => return Err(error),
    };
    Ok(parse_credentials(&contents, user_env, password_env))
}

fn parse_credentials(contents: &str, user_env: &str, password_env: &str) -> ServiceCredentials {
    let mut credentials = ServiceCredentials::default();
    for line in contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                key if key == user_env => credentials.user = Some(value.trim().to_string()),
                key if key == password_env => credentials.password = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    credentials
}

fn bson_number_to_u64(value: Option<&Bson>) -> Option<u64> {
    match value? {
        Bson::Int32(value) => u64::try_from(*value).ok(),
        Bson::Int64(value) => u64::try_from(*value).ok(),
        Bson::Double(value) if value.is_finite() && *value >= 0.0 => Some(*value as u64),
        _ => None,
    }
}

fn weighted_percentile(samples: &[LatencySample], percentile: f64) -> f64 {
    let total_operations = samples.iter().map(|sample| sample.operations).sum::<u64>();
    if total_operations == 0 {
        return 0.0;
    }

    let mut weighted = samples
        .iter()
        .map(|sample| {
            (
                sample.elapsed.as_micros() as f64 / sample.operations as f64,
                sample.operations,
            )
        })
        .collect::<Vec<_>>();
    weighted.sort_by(|(left, _), (right, _)| {
        left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
    });

    let threshold = ((total_operations as f64) * percentile).ceil() as u64;
    let mut seen = 0_u64;
    for (latency, operations) in weighted {
        seen = seen.saturating_add(operations);
        if seen >= threshold {
            return latency;
        }
    }
    0.0
}

fn expect_inventory(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Create(wardrobe_core::CreateResult::StorageInventory(_)) => Ok(()),
        other => unexpected_wardrobe_result("storage inventory", other),
    }
}

fn expect_pointers(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Upsert(UpsertResult::Pointers(_)) => Ok(()),
        other => unexpected_wardrobe_result("pointers", other),
    }
}

fn expect_records(result: CommandResult) -> io::Result<Vec<Value>> {
    match result {
        CommandResult::Read(ReadResult::Records(records)) => Ok(records),
        other => unexpected_wardrobe_result("records", other),
    }
}

fn expect_count(result: CommandResult) -> io::Result<usize> {
    match result {
        CommandResult::Count(count) => Ok(count),
        other => unexpected_wardrobe_result("count", other),
    }
}

fn expect_delete(result: CommandResult) -> io::Result<usize> {
    match result {
        CommandResult::Delete(result) => Ok(result.deleted),
        other => unexpected_wardrobe_result("delete result", other),
    }
}

fn expect_vacuumed(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Compact(_) => Ok(()),
        other => unexpected_wardrobe_result("vacuum report", other),
    }
}

fn expect_admin(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Create(wardrobe_core::CreateResult::Admin(_))
        | CommandResult::Alter(_)
        | CommandResult::Drop(_)
        | CommandResult::Grant(_)
        | CommandResult::Revoke(_) => Ok(()),
        other => unexpected_wardrobe_result("admin response", other),
    }
}

fn unexpected_wardrobe_result<T>(expected: &str, actual: CommandResult) -> io::Result<T> {
    Err(Error::new(
        ErrorKind::InvalidData,
        format!("Expected Wardrobe {expected}, got {actual:?}"),
    ))
}

fn chunk_ranges(total: usize, chunk_size: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < total {
        let end = start.saturating_add(chunk_size).min(total);
        ranges.push((start, end));
        start = end;
    }
    ranges
}

fn materialized_book_value(
    book_id: String,
    isbn: String,
    title: String,
    author_id: String,
    editor_id: String,
    branch: String,
    quantity: i64,
    purge_bucket: i64,
    author_entity_id: String,
    author_display_name: String,
    author_role: String,
    author_cohort: i64,
    editor_entity_id: String,
    editor_display_name: String,
    editor_role: String,
    editor_cohort: i64,
) -> Value {
    json!({
        "_id": book_id.clone(),
        "book_id": book_id,
        "isbn": isbn,
        "title": title,
        "author_id": author_id,
        "editor_id": editor_id,
        "branch": branch,
        "quantity": quantity,
        "purge_bucket": purge_bucket,
        "author": {
            "_id": author_entity_id.clone(),
            "entity_id": author_entity_id,
            "display_name": author_display_name,
            "role": author_role,
            "cohort": author_cohort,
        },
        "editor": {
            "_id": editor_entity_id.clone(),
            "entity_id": editor_entity_id,
            "display_name": editor_display_name,
            "role": editor_role,
            "cohort": editor_cohort,
        },
    })
}

fn sqlite_materialized_book_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(materialized_book_value(
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
    ))
}

fn mongo_materialized_book_pipeline(entity_id: &str) -> Vec<Document> {
    vec![
        doc! { "$match": { "author_id": entity_id, "editor_id": entity_id } },
        doc! {
            "$lookup": {
                "from": "entities",
                "localField": "author_id",
                "foreignField": "_id",
                "as": "author"
            }
        },
        doc! { "$unwind": "$author" },
        doc! {
            "$lookup": {
                "from": "entities",
                "localField": "editor_id",
                "foreignField": "_id",
                "as": "editor"
            }
        },
        doc! { "$unwind": "$editor" },
        doc! {
            "$project": {
                "_id": 1,
                "book_id": 1,
                "isbn": 1,
                "title": 1,
                "author_id": 1,
                "editor_id": 1,
                "branch": 1,
                "quantity": 1,
                "purge_bucket": 1,
                "author": {
                    "_id": "$author._id",
                    "entity_id": "$author.entity_id",
                    "display_name": "$author.display_name",
                    "role": "$author.role",
                    "cohort": "$author.cohort"
                },
                "editor": {
                    "_id": "$editor._id",
                    "entity_id": "$editor.entity_id",
                    "display_name": "$editor.display_name",
                    "role": "$editor.role",
                    "cohort": "$editor.cohort"
                }
            }
        },
    ]
}

fn mysql_materialized_book_query(entity_id: &str) -> String {
    format!(
        r#"
SELECT
    b.id AS book_id,
    b.isbn AS book_isbn,
    b.title AS book_title,
    b.author_id AS book_author_id,
    b.editor_id AS book_editor_id,
    b.branch AS book_branch,
    b.quantity AS book_quantity,
    b.purge_bucket AS book_purge_bucket,
    author.id AS author_entity_id,
    author.display_name AS author_display_name,
    author.role AS author_role,
    author.cohort AS author_cohort,
    editor.id AS editor_entity_id,
    editor.display_name AS editor_display_name,
    editor.role AS editor_role,
    editor.cohort AS editor_cohort
FROM books b
JOIN entities author ON author.id = b.author_id
JOIN entities editor ON editor.id = b.editor_id
WHERE b.author_id = {entity_id} AND b.editor_id = {entity_id}
"#
    )
}

fn mysql_materialized_book_value(row: &Row) -> io::Result<Value> {
    Ok(materialized_book_value(
        mysql_string(row, "book_id")?,
        mysql_string(row, "book_isbn")?,
        mysql_string(row, "book_title")?,
        mysql_string(row, "book_author_id")?,
        mysql_string(row, "book_editor_id")?,
        mysql_string(row, "book_branch")?,
        mysql_i64(row, "book_quantity")?,
        mysql_i64(row, "book_purge_bucket")?,
        mysql_string(row, "author_entity_id")?,
        mysql_string(row, "author_display_name")?,
        mysql_string(row, "author_role")?,
        mysql_i64(row, "author_cohort")?,
        mysql_string(row, "editor_entity_id")?,
        mysql_string(row, "editor_display_name")?,
        mysql_string(row, "editor_role")?,
        mysql_i64(row, "editor_cohort")?,
    ))
}

fn mysql_string(row: &Row, column: &str) -> io::Result<String> {
    row.get(column).ok_or_else(|| missing_mysql_column(column))
}

fn mysql_i64(row: &Row, column: &str) -> io::Result<i64> {
    row.get(column).ok_or_else(|| missing_mysql_column(column))
}

fn missing_mysql_column(column: &str) -> io::Error {
    Error::new(
        ErrorKind::InvalidData,
        format!("MySQL result row is missing column '{column}'"),
    )
}

fn sqlite_entity_insert(profile: &LibraryProfile, start: usize, end: usize) -> String {
    let values = (start..end)
        .map(|index| {
            let id = profile.entity_id(index);
            format!(
                "({}, {}, {}, {})",
                sql_string(&id),
                sql_string(&format!("Library Entity {index:08}")),
                sql_string(if index % 2 == 0 { "author" } else { "editor" }),
                index % 97,
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "BEGIN IMMEDIATE;\nINSERT OR REPLACE INTO entities (id, display_name, role, cohort) VALUES\n{values};\nCOMMIT;"
    )
}

fn sqlite_book_insert(profile: &LibraryProfile, start: usize, end: usize) -> String {
    let values = (start..end)
        .map(|index| {
            let payload = profile.book_payload(index);
            format!(
                "({}, {}, {}, {}, {}, {}, {}, {})",
                sql_string(payload["_id"].as_str().unwrap_or_default()),
                sql_string(payload["isbn"].as_str().unwrap_or_default()),
                sql_string(payload["title"].as_str().unwrap_or_default()),
                sql_string(payload["author_id"].as_str().unwrap_or_default()),
                sql_string(payload["editor_id"].as_str().unwrap_or_default()),
                sql_string(payload["branch"].as_str().unwrap_or_default()),
                payload["quantity"].as_u64().unwrap_or_default(),
                payload["purge_bucket"].as_u64().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "BEGIN IMMEDIATE;\nINSERT OR REPLACE INTO books (id, isbn, title, author_id, editor_id, branch, quantity, purge_bucket) VALUES\n{values};\nCOMMIT;"
    )
}

fn mongo_documents(
    profile: &LibraryProfile,
    drawer: &str,
    start: usize,
    end: usize,
) -> io::Result<Vec<Document>> {
    (start..end)
        .map(|index| {
            let payload = if drawer == ENTITY_DRAWER {
                profile.entity_payload(index)
            } else {
                profile.book_payload(index)
            };
            mongodb::bson::to_document(&payload).map_err(to_io_error)
        })
        .collect()
}

fn mysql_entity_insert(profile: &LibraryProfile, start: usize, end: usize) -> String {
    let values = (start..end)
        .map(|index| {
            let id = profile.entity_id(index);
            format!(
                "({}, {}, {}, {})",
                sql_string(&id),
                sql_string(&format!("Library Entity {index:08}")),
                sql_string(if index % 2 == 0 { "author" } else { "editor" }),
                index % 97,
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "INSERT INTO entities (id, display_name, role, cohort) VALUES\n{values}\nON DUPLICATE KEY UPDATE display_name = VALUES(display_name), role = VALUES(role), cohort = VALUES(cohort)"
    )
}

fn mysql_book_insert(profile: &LibraryProfile, start: usize, end: usize) -> String {
    let values = (start..end)
        .map(|index| {
            let payload = profile.book_payload(index);
            format!(
                "({}, {}, {}, {}, {}, {}, {}, {})",
                sql_string(payload["_id"].as_str().unwrap_or_default()),
                sql_string(payload["isbn"].as_str().unwrap_or_default()),
                sql_string(payload["title"].as_str().unwrap_or_default()),
                sql_string(payload["author_id"].as_str().unwrap_or_default()),
                sql_string(payload["editor_id"].as_str().unwrap_or_default()),
                sql_string(payload["branch"].as_str().unwrap_or_default()),
                payload["quantity"].as_u64().unwrap_or_default(),
                payload["purge_bucket"].as_u64().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "INSERT INTO books (id, isbn, title, author_id, editor_id, branch, quantity, purge_bucket) VALUES\n{values}\nON DUPLICATE KEY UPDATE isbn = VALUES(isbn), title = VALUES(title), author_id = VALUES(author_id), editor_id = VALUES(editor_id), branch = VALUES(branch), quantity = VALUES(quantity), purge_bucket = VALUES(purge_bucket)"
    )
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn mysql_identifier(value: &str) -> String {
    value.replace('`', "``")
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", path.display(), suffix))
}

fn file_size_or_zero(path: impl AsRef<Path>) -> io::Result<u64> {
    match fs::metadata(path.as_ref()) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

fn sync_file_if_exists(path: &Path) -> io::Result<()> {
    let mut file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .or_else(|_| File::open(path))
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    match file.flush() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::PermissionDenied => Ok(()),
        Err(error) => Err(error),
    }
}

fn fsync_tree(path: &Path) -> io::Result<()> {
    if path.is_file() {
        sync_file_if_exists(path)?;
        return Ok(());
    }
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        if child.is_dir() {
            fsync_tree(&child)?;
        } else {
            sync_file_if_exists(&child)?;
        }
    }
    Ok(())
}

fn directory_size(path: impl AsRef<Path>) -> io::Result<u64> {
    let path = path.as_ref();
    if path.is_file() {
        return file_size_or_zero(path);
    }
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        if child.is_dir() {
            total = total.saturating_add(directory_size(child)?);
        } else {
            total = total.saturating_add(file_size_or_zero(child)?);
        }
    }
    Ok(total)
}

fn optional_count(value: Option<usize>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unavailable".to_string())
}

fn unix_timestamp_micros() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

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

        fn complex_traversal(
            &mut self,
            _profile: &LibraryProfile,
            _recorder: &mut PhaseRecorder,
            _progress: &ProgressReporter,
        ) -> io::Result<u64> {
            self.calls.push(PhaseName::ComplexTraversal);
            self.maybe_fail(PhaseName::ComplexTraversal)
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
            "wardrobe-embedded,wardrobe-remote,sqlite,mongodb,mysql,neo4j".to_string(),
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
            "--purge-buckets".to_string(),
            "4".to_string(),
            "--wardrobe-embedded-path".to_string(),
            "target/custom-bench/embedded".to_string(),
            "--wardrobe-remote-uri".to_string(),
            "wardrobe://127.0.0.1:24842".to_string(),
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
        let targets = parse_targets("embedded,REMOTE,mongo,mariadb,neo")
            .expect("target aliases should parse");
        assert_eq!(
            targets,
            vec![
                TargetSpec::WardrobeEmbedded,
                TargetSpec::WardrobeRemote,
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
    fn tcp_wardrobe_runner_connect_disables_nagle() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let address = listener.local_addr().expect("listener should have address");
        let accept_thread = std::thread::spawn(move || {
            let _ = listener.accept().expect("listener should accept client");
        });

        let runner = TcpWardrobeRunner::connect(&format!("wardrobe://{address}"), None)
            .expect("runner should connect");

        assert!(
            runner
                .stream
                .nodelay()
                .expect("runner stream should report nodelay")
        );

        drop(runner);
        accept_thread.join().expect("accept thread should exit");
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
            purge_buckets: 4,
        };

        assert_eq!(profile.expected_purge_count(), 3);
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
        assert_eq!(PhaseName::ComplexTraversal.label(), "Complex Traversal");
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
        assert_eq!(report.phases.len(), 5);
        assert_eq!(report.storage_bytes, 123);
        assert_eq!(
            target.calls,
            vec![
                PhaseName::MassiveIngestion,
                PhaseName::IndexMutation,
                PhaseName::ComplexTraversal,
                PhaseName::TargetedPurge,
                PhaseName::Compaction,
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
            expect_pointers(CommandResult::Delete(wardrobe_core::DeleteResult {
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
                purge_buckets: 2,
            },
            work_dir: work_dir.clone(),
            ..BenchmarkConfig::default()
        };

        let report = run_benchmark(config).expect("tiny benchmark should run");

        assert_eq!(report.targets.len(), 1);
        assert_eq!(report.targets[0].phases.len(), 5);
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
                purge_buckets: 2,
            },
            work_dir: work_dir.clone(),
            ..BenchmarkConfig::default()
        };

        let report = run_benchmark(config).expect("tiny remote benchmark should run");

        assert_eq!(report.targets.len(), 1);
        assert_eq!(report.targets[0].phases.len(), 5);
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
                CommandResult::Status(StatusResult::Drawers(vec![
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
                CommandResult::Status(StatusResult::Storage(StorageDiagnosis {
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
                CommandResult::Status(StatusResult::Wal(wardrobe_core::WalVerification {
                    path: "/data/wardrobe/.wal".to_string(),
                    entry_count: 4,
                    last_sequence: Some(4),
                })),
                CommandResult::Status(StatusResult::Wal(wardrobe_core::WalVerification {
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
        };

        let profile = LibraryProfile {
            entity_records: 1,
            book_records: 2,
            chunk_size: 1,
            traversal_queries: 2,
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
    fn wardrobe_purge_uses_single_delete_by_filter_command() {
        let state = Rc::new(RefCell::new(MockRunnerState {
            commands: Vec::new(),
            responses: VecDeque::from(vec![CommandResult::Delete(wardrobe_core::DeleteResult {
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
        };

        let profile = LibraryProfile {
            entity_records: 2,
            book_records: 4,
            chunk_size: 1,
            traversal_queries: 1,
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
