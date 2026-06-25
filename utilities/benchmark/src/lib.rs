#![deny(unsafe_code)]

use mongodb::IndexModel;
use mongodb::bson::{Bson, Document, doc};
use mongodb::sync::{Client as MongoClient, Collection};
use mysql::prelude::Queryable;
use mysql::{OptsBuilder, Pool, PooledConn, Row};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Error, ErrorKind};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use wardrobe_core::{
    Command, CommandResult, ConnectionTarget, ProtocolFrame, ProtocolOpcode, StorageScope,
    WardrobeEngine,
};

const DATABASE_NAME: &str = "wardrobe";
const SCHEMA_NAME: &str = "library";
const ENTITY_DRAWER: &str = "entity";
const BOOK_DRAWER: &str = "book";
const DEFAULT_WORK_DIR: &str = "target/wardrobe-benchmark";
const DEFAULT_ENTITY_RECORDS: usize = 10_000;
const DEFAULT_BOOK_RECORDS: usize = 50_000;
const DEFAULT_CHUNK_SIZE: usize = 500;
const DEFAULT_TRAVERSAL_QUERIES: usize = 100;
const DEFAULT_PURGE_BUCKETS: usize = 10;
const DEFAULT_MYSQL_USER: &str = "wardrobe_benchmark";
const DEFAULT_MYSQL_USER_ENV: &str = "WARDROBE_BENCH_MYSQL_USER";
const DEFAULT_MYSQL_PASSWORD_ENV: &str = "WARDROBE_BENCH_MYSQL_PASSWORD";
const DEFAULT_MYSQL_CREDENTIALS_FILE: &str = "target/wardrobe-benchmark/mysql-credentials.env";
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
    pub sqlite_db: Option<PathBuf>,
    pub mongo_uri: String,
    pub mongo_database: String,
    pub mysql_host: String,
    pub mysql_port: u16,
    pub mysql_database: String,
    pub mysql_user: Option<String>,
    pub mysql_password_env: Option<String>,
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
            sqlite_db: None,
            mongo_uri: "mongodb://127.0.0.1:27017".to_string(),
            mongo_database: "wardrobe_benchmark".to_string(),
            mysql_host: "127.0.0.1".to_string(),
            mysql_port: 3306,
            mysql_database: "wardrobe_benchmark".to_string(),
            mysql_user: None,
            mysql_password_env: Some(DEFAULT_MYSQL_PASSWORD_ENV.to_string()),
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
                unknown => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("Unknown benchmark argument: {unknown}"),
                    ));
                }
            }
        }

        config.profile.validate()?;
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
}

impl TargetSpec {
    fn all() -> Vec<Self> {
        vec![
            Self::WardrobeEmbedded,
            Self::WardrobeRemote,
            Self::Sqlite,
            Self::MongoDb,
            Self::MySql,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            Self::WardrobeEmbedded => "Wardrobe (Embedded Flat-File Mode)",
            Self::WardrobeRemote => "Wardrobe (Remote TCP Server Mode)",
            Self::Sqlite => "SQLite (Local WAL File Mode)",
            Self::MongoDb => "MongoDB (Document Store Base Comparison)",
            Self::MySql => "MySQL / MariaDB (Relational Pointer Base Comparison)",
        }
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
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetReport {
    pub name: String,
    pub phases: Vec<PhaseMetrics>,
    pub storage_bytes: u64,
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
    let progress = ProgressReporter::new(config.progress_enabled);
    let run_dir = config
        .work_dir
        .join(format!("run-{}", unix_timestamp_micros()));
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
        let mut target = build_target(*spec, &config, &run_dir, &progress)?;
        let target_report = run_target(target.as_mut(), &config.profile, &progress)?;
        progress.log(format!(
            "completed target: {} (storage footprint {} bytes)",
            target_report.name, target_report.storage_bytes
        ));
        report.targets.push(target_report);
    }

    progress.log("benchmark run complete; rendering Markdown report");
    Ok(report)
}

pub fn print_help() {
    println!("wardrobe-benchmark");
    println!(
        "  --targets <csv|all>             Targets: wardrobe-embedded,wardrobe-remote,sqlite,mongodb,mysql"
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
    Ok(TargetReport {
        name: target.name().to_string(),
        phases: phase_metrics,
        storage_bytes,
    })
}

fn build_target(
    spec: TargetSpec,
    config: &BenchmarkConfig,
    run_dir: &Path,
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
            Ok(Box::new(WardrobeTarget::embedded(path)?))
        }
        TargetSpec::WardrobeRemote => {
            if let Some(uri) = &config.wardrobe_remote_uri {
                progress.log(format!("{}: connecting to {uri}", spec.label()));
                Ok(Box::new(WardrobeTarget::remote_uri(uri)?))
            } else {
                progress.log(format!(
                    "{}: starting in-process TCP server under {}",
                    spec.label(),
                    run_dir.join("wardrobe-remote").display()
                ));
                Ok(Box::new(WardrobeTarget::remote_auto(
                    run_dir.join("wardrobe-remote"),
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
    }
}

struct WardrobeTarget {
    name: String,
    runner: Option<Box<dyn WardrobeCommandRunner>>,
    storage_root: Option<PathBuf>,
    server_handle: Option<JoinHandle<io::Result<()>>>,
}

impl WardrobeTarget {
    fn embedded(path: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&path)?;
        let engine = WardrobeEngine::open(path.to_string_lossy().as_ref())?;
        Ok(Self {
            name: "Wardrobe (Embedded Flat-File Mode)".to_string(),
            runner: Some(Box::new(EmbeddedWardrobeRunner {
                engine,
                storage_root: path.clone(),
            })),
            storage_root: Some(path),
            server_handle: None,
        })
    }

    fn remote_auto(path: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&path)?;
        let engine = Arc::new(WardrobeEngine::open(path.to_string_lossy().as_ref())?);
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
        })
    }

    fn remote_uri(uri: &str) -> io::Result<Self> {
        let runner = TcpWardrobeRunner::connect(uri, None)?;
        Ok(Self {
            name: "Wardrobe (Remote TCP Server Mode)".to_string(),
            runner: Some(Box::new(runner)),
            storage_root: None,
            server_handle: None,
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
            scope: StorageScope::schema(DATABASE_NAME, SCHEMA_NAME),
            command: Box::new(command),
        })
    }

    fn find_entity_record(&mut self, entity_reference: &str) -> io::Result<Value> {
        let pointer = if entity_reference.starts_with('@') {
            entity_reference.to_string()
        } else {
            format!("@{ENTITY_DRAWER}:{entity_reference}")
        };
        expect_record(self.execute_scoped(Command::FindById { pointer })?)?.ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("entity record '{entity_reference}' was not found"),
            )
        })
    }

    fn materialize_entity_field(&mut self, record: &Value, field_name: &str) -> io::Result<Value> {
        match record.get(field_name) {
            Some(Value::Object(entity)) => Ok(Value::Object(entity.clone())),
            Some(Value::String(entity_reference)) => self.find_entity_record(entity_reference),
            _ => Err(Error::new(
                ErrorKind::InvalidData,
                format!("record is missing a materializable {field_name}"),
            )),
        }
    }

    fn materialize_book_records(&mut self, mut records: Vec<Value>) -> io::Result<Vec<Value>> {
        for record in &mut records {
            let author = self.materialize_entity_field(record, "author_id")?;
            let editor = self.materialize_entity_field(record, "editor_id")?;
            let Some(record_map) = record.as_object_mut() else {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "book record is not a JSON object",
                ));
            };
            record_map.insert("author".to_string(), author);
            record_map.insert("editor".to_string(), editor);
        }
        Ok(records)
    }

    fn count_book_relationship_matches(&mut self, entity_reference: &str) -> io::Result<usize> {
        expect_count(self.execute_scoped(Command::Count {
            drawer_name: BOOK_DRAWER.to_string(),
            filter: Some(json!({
                "author_id": entity_reference,
                "editor_id": entity_reference,
            })),
            modifiers: None,
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
}

impl BenchmarkTarget for WardrobeTarget {
    fn name(&self) -> &str {
        &self.name
    }

    fn provision_schema(
        &mut self,
        _profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!(
            "{}: creating database '{}'",
            self.name(),
            DATABASE_NAME
        ));
        expect_inventory(self.execute(Command::DefineDatabase {
            database_name: DATABASE_NAME.to_string(),
        })?)?;
        progress.log(format!(
            "{}: creating schema '{}'",
            self.name(),
            SCHEMA_NAME
        ));
        expect_inventory(self.execute(Command::DefineSchema {
            database_name: DATABASE_NAME.to_string(),
            schema_name: SCHEMA_NAME.to_string(),
        })?)?;
        for drawer in [ENTITY_DRAWER, BOOK_DRAWER] {
            progress.log(format!("{}: creating drawer '{}'", self.name(), drawer));
            expect_inventory(self.execute(Command::DefineDrawer {
                database_name: DATABASE_NAME.to_string(),
                schema_name: SCHEMA_NAME.to_string(),
                drawer_name: drawer.to_string(),
            })?)?;
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
                expect_pointers(self.execute_scoped(Command::BulkUpsert {
                    drawer_name: ENTITY_DRAWER.to_string(),
                    records,
                    atomic: true,
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
                expect_pointers(self.execute_scoped(Command::BulkUpsert {
                    drawer_name: BOOK_DRAWER.to_string(),
                    records,
                    atomic: true,
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
        for (index, (action, kind)) in [("add", "key"), ("remove", "key"), ("add", "key")]
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
                expect_admin(self.execute_scoped(Command::ManageSchema {
                    action: action.to_string(),
                    kind: kind.to_string(),
                    drawer_name: BOOK_DRAWER.to_string(),
                    field_name: "isbn".to_string(),
                    payload: json!({ "kind": kind, "unique": true }),
                })?)
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
                let records = expect_records(self.execute_scoped(Command::FindByFilter {
                    drawer_name: BOOK_DRAWER.to_string(),
                    filter: json!({
                        "author_id": entity_reference,
                        "editor_id": entity_reference,
                    }),
                    modifiers: None,
                })?)?;
                self.materialize_book_records(records)
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
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        progress.log(format!(
            "{}: finding records where purge_bucket = 0",
            self.name()
        ));
        let records = recorder.measure(1, || {
            expect_records(self.execute_scoped(Command::FindByFilter {
                drawer_name: BOOK_DRAWER.to_string(),
                filter: json!({ "purge_bucket": 0 }),
                modifiers: None,
            })?)
        })?;
        progress.log(format!(
            "{}: purge matched {} book records",
            self.name(),
            records.len()
        ));
        let total_records = records.len();
        let mut operations = 1;
        for (index, record) in records.into_iter().enumerate() {
            let pointer = pointer_from_record(&record, BOOK_DRAWER)?;
            recorder.measure(1, || {
                expect_deleted(self.execute_scoped(Command::Delete { pointer })?)
            })?;
            report_record_progress(
                progress,
                &format!("{}: purge deletes completed", self.name()),
                index + 1,
                total_records,
            );
            operations += 1;
        }
        Ok(operations)
    }

    fn compaction(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        progress.log(format!("{}: vacuuming book drawer", self.name()));
        recorder.measure(1, || {
            expect_vacuumed(self.execute_scoped(Command::Vacuum {
                drawer_name: BOOK_DRAWER.to_string(),
            })?)
        })?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(root) = &self.storage_root {
            fsync_tree(root)?;
        } else if let Ok(CommandResult::Diagnosis(diagnosis)) = self.execute(Command::Diagnose) {
            let path = PathBuf::from(diagnosis.storage_directory);
            if path.exists() {
                fsync_tree(&path)?;
            }
        }
        Ok(())
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        if let Some(root) = &self.storage_root {
            return directory_size(root);
        }
        if let CommandResult::Diagnosis(diagnosis) = self.execute(Command::Diagnose)? {
            let path = PathBuf::from(diagnosis.storage_directory);
            if path.exists() {
                return directory_size(path);
            }
        }
        Ok(0)
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
        Ok(Self {
            stream: TcpStream::connect((host.as_str(), port))?,
        })
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
                    fallback_credentials.password
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

#[derive(Default)]
struct MySqlCredentials {
    user: Option<String>,
    password: Option<String>,
}

fn read_default_mysql_credentials() -> io::Result<MySqlCredentials> {
    let contents = match fs::read_to_string(DEFAULT_MYSQL_CREDENTIALS_FILE) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(MySqlCredentials::default()),
        Err(error) => return Err(error),
    };
    let mut credentials = MySqlCredentials::default();
    for line in contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                DEFAULT_MYSQL_USER_ENV => credentials.user = Some(value.trim().to_string()),
                DEFAULT_MYSQL_PASSWORD_ENV => credentials.password = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    Ok(credentials)
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
        CommandResult::StorageInventory(_) => Ok(()),
        other => unexpected_wardrobe_result("storage inventory", other),
    }
}

fn expect_pointers(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Pointers(_) => Ok(()),
        other => unexpected_wardrobe_result("pointers", other),
    }
}

fn expect_records(result: CommandResult) -> io::Result<Vec<Value>> {
    match result {
        CommandResult::Records(records) => Ok(records),
        other => unexpected_wardrobe_result("records", other),
    }
}

fn expect_count(result: CommandResult) -> io::Result<usize> {
    match result {
        CommandResult::Count(count) => Ok(count),
        other => unexpected_wardrobe_result("count", other),
    }
}

fn expect_record(result: CommandResult) -> io::Result<Option<Value>> {
    match result {
        CommandResult::Record(record) => Ok(record),
        other => unexpected_wardrobe_result("record", other),
    }
}

fn expect_deleted(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Deleted(_) => Ok(()),
        other => unexpected_wardrobe_result("deleted flag", other),
    }
}

fn expect_vacuumed(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Vacuumed(_) => Ok(()),
        other => unexpected_wardrobe_result("vacuum report", other),
    }
}

fn expect_admin(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Admin(_) => Ok(()),
        other => unexpected_wardrobe_result("admin response", other),
    }
}

fn unexpected_wardrobe_result<T>(expected: &str, actual: CommandResult) -> io::Result<T> {
    Err(Error::new(
        ErrorKind::InvalidData,
        format!("Expected Wardrobe {expected}, got {actual:?}"),
    ))
}

fn pointer_from_record(record: &Value, drawer: &str) -> io::Result<String> {
    let id = record
        .get("_id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "record is missing a string _id"))?;
    if id.starts_with('@') {
        Ok(id.to_string())
    } else {
        Ok(format!("@{drawer}:{id}"))
    }
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
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .or_else(|_| File::open(path))
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    match file.sync_all() {
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

fn unix_timestamp_micros() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
