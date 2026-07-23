use crate::engine::TargetSpec;
use crate::utils::{deterministic_range_bounds, repeated_shuffled_indices, shuffled_take};
use serde_json::{Value, json};
use std::io::{self, Error, ErrorKind};
use std::path::PathBuf;
use wardrobe_core::DurabilityPolicy;

pub(crate) const DEFAULT_WARDROBE_DATABASE_PREFIX: &str = "wardrobe_benchmark";
pub(crate) const DEFAULT_WARDROBE_SCHEMA_NAME: &str = "library";
pub(crate) const ENTITY_DRAWER: &str = "entity";
pub(crate) const BOOK_DRAWER: &str = "book";
pub(crate) const DEFAULT_WORK_DIR: &str = "target/wardrobe-benchmark";
pub(crate) const DEFAULT_ENTITY_RECORDS: usize = 10_000;
pub(crate) const DEFAULT_BOOK_RECORDS: usize = 50_000;
pub(crate) const DEFAULT_CHUNK_SIZE: usize = 500;
pub(crate) const DEFAULT_TRAVERSAL_QUERIES: usize = 100;
pub(crate) const DEFAULT_POINT_LOOKUPS: usize = 1_000;
pub(crate) const DEFAULT_RANGE_LOOKUPS: usize = 100;
pub(crate) const DEFAULT_DELETE_BY_ID_OPERATIONS: usize = 100;
pub(crate) const DEFAULT_PURGE_BUCKETS: usize = 10;
pub(crate) const DEFAULT_MYSQL_USER: &str = "wardrobe_benchmark";
pub(crate) const DEFAULT_MYSQL_PASSWORD: &str = "wardrobe_benchmark";
pub(crate) const DEFAULT_MYSQL_USER_ENV: &str = "WARDROBE_BENCH_MYSQL_USER";
pub(crate) const DEFAULT_MYSQL_PASSWORD_ENV: &str = "WARDROBE_BENCH_MYSQL_PASSWORD";
pub(crate) const DEFAULT_MYSQL_CREDENTIALS_FILE: &str =
    "target/wardrobe-benchmark/mysql-credentials.env";
pub(crate) const DEFAULT_NEO4J_USER: &str = "neo4j";
pub(crate) const DEFAULT_NEO4J_PASSWORD: &str = "wardrobe_benchmark";
pub(crate) const DEFAULT_NEO4J_USER_ENV: &str = "WARDROBE_BENCH_NEO4J_USER";
pub(crate) const DEFAULT_NEO4J_PASSWORD_ENV: &str = "WARDROBE_BENCH_NEO4J_PASSWORD";
pub(crate) const DEFAULT_NEO4J_CREDENTIALS_FILE: &str =
    "target/wardrobe-benchmark/neo4j-credentials.env";
pub(crate) const DEFAULT_WARDROBE_GROUP_COMMIT_WINDOW_MS: u64 = 5;
pub(crate) const DEFAULT_WARDROBE_GROUP_COMMIT_MAX_BATCH: usize = 128;

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
    pub wardrobe_client_profile: Option<PathBuf>,
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
            wardrobe_client_profile: None,
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
                "--point-lookups" => {
                    config.profile.point_lookups =
                        parse_positive_usize(&arg, &required_value(&mut args, &arg)?)?;
                }
                "--range-lookups" => {
                    config.profile.range_lookups =
                        parse_positive_usize(&arg, &required_value(&mut args, &arg)?)?;
                }
                "--delete-by-id" => {
                    config.profile.delete_by_id_operations =
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
                "--wardrobe-client-profile" => {
                    config.wardrobe_client_profile =
                        Some(PathBuf::from(required_value(&mut args, &arg)?));
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
        if config.wardrobe_remote_uri.is_some() && config.wardrobe_client_profile.is_none() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "--wardrobe-client-profile is required with --wardrobe-remote-uri",
            ));
        }

        Ok(ParseOutcome::Run(config))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryProfile {
    pub entity_records: usize,
    pub book_records: usize,
    pub chunk_size: usize,
    pub traversal_queries: usize,
    pub point_lookups: usize,
    pub range_lookups: usize,
    pub delete_by_id_operations: usize,
    pub purge_buckets: usize,
}

impl Default for LibraryProfile {
    fn default() -> Self {
        Self {
            entity_records: DEFAULT_ENTITY_RECORDS,
            book_records: DEFAULT_BOOK_RECORDS,
            chunk_size: DEFAULT_CHUNK_SIZE,
            traversal_queries: DEFAULT_TRAVERSAL_QUERIES,
            point_lookups: DEFAULT_POINT_LOOKUPS,
            range_lookups: DEFAULT_RANGE_LOOKUPS,
            delete_by_id_operations: DEFAULT_DELETE_BY_ID_OPERATIONS,
            purge_buckets: DEFAULT_PURGE_BUCKETS,
        }
    }
}

impl LibraryProfile {
    pub(crate) fn validate(&self) -> io::Result<()> {
        for (name, value) in [
            ("entities", self.entity_records),
            ("books", self.book_records),
            ("chunk-size", self.chunk_size),
            ("traversal-queries", self.traversal_queries),
            ("point-lookups", self.point_lookups),
            ("range-lookups", self.range_lookups),
            ("delete-by-id", self.delete_by_id_operations),
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

    pub(crate) fn entity_id(&self, index: usize) -> String {
        format!("entity_{index:08}")
    }

    pub(crate) fn book_id(&self, index: usize) -> String {
        format!("book_{index:08}")
    }

    pub(crate) fn entity_payload(&self, index: usize) -> Value {
        let entity_id = self.entity_id(index);
        json!({
            "_id": entity_id,
            "entity_id": entity_id,
            "display_name": format!("Library Entity {index:08}"),
            "role": if index % 2 == 0 { "author" } else { "editor" },
            "cohort": index % 97,
        })
    }

    pub(crate) fn book_payload(&self, index: usize) -> Value {
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

    pub(crate) fn traversal_entity_id(&self, query_index: usize) -> String {
        self.entity_id(query_index % self.entity_records)
    }

    pub(crate) fn point_lookup_book_ids(&self) -> Vec<String> {
        repeated_shuffled_indices(self.book_records, self.point_lookups, 0x9E37_79B9_7F4A_7C15)
            .into_iter()
            .map(|index| self.book_id(index))
            .collect()
    }

    pub(crate) fn range_lookup_bounds(&self) -> Vec<(i64, i64)> {
        deterministic_range_bounds(self.range_lookups, 1, 23, 0xC4CE_B9FE_1A85_1C3D)
    }

    pub(crate) fn delete_by_id_book_ids(&self) -> Vec<String> {
        let candidates = (0..self.book_records)
            .filter(|index| index % self.purge_buckets != 0)
            .collect::<Vec<_>>();
        shuffled_take(
            candidates,
            self.delete_by_id_operations,
            0xD1B5_4A32_D192_ED03,
        )
        .into_iter()
        .map(|index| self.book_id(index))
        .collect()
    }

    pub(crate) fn expected_purge_count(&self) -> usize {
        if self.book_records == 0 {
            0
        } else {
            ((self.book_records - 1) / self.purge_buckets) + 1
        }
    }

    pub(crate) fn expected_book_records_after_mutating_phases(&self) -> usize {
        self.book_records
            .saturating_sub(self.delete_by_id_book_ids().len())
            .saturating_sub(self.expected_purge_count())
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
    println!(
        "  --point-lookups <count>         Random primary-key lookup operations, default {DEFAULT_POINT_LOOKUPS}"
    );
    println!(
        "  --range-lookups <count>         Random numeric range lookup operations, default {DEFAULT_RANGE_LOOKUPS}"
    );
    println!(
        "  --delete-by-id <count>          Random primary-key delete operations, default {DEFAULT_DELETE_BY_ID_OPERATIONS}"
    );
    println!("  --wardrobe-embedded-path <path> Override embedded Wardrobe storage path");
    println!(
        "  --wardrobe-remote-uri <uri>     Use an existing Wardrobe TCP server instead of auto-spawning one"
    );
    println!(
        "  --wardrobe-client-profile <path>  Client certificate profile for an existing Wardrobe server"
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

pub(crate) fn parse_targets(raw: &str) -> io::Result<Vec<TargetSpec>> {
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

pub(crate) fn required_value(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
) -> io::Result<String> {
    args.next().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("{flag} requires a following value"),
        )
    })
}

pub(crate) fn parse_positive_usize(flag: &str, raw: &str) -> io::Result<usize> {
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

pub(crate) fn parse_positive_u64(flag: &str, raw: &str) -> io::Result<u64> {
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

pub(crate) fn parse_wardrobe_durability_policy(raw: &str) -> io::Result<DurabilityPolicy> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "strict" => Ok(DurabilityPolicy::Strict),
        "grouped" | "group" | "group-commit" => Ok(default_grouped_durability_policy()),
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unsupported --wardrobe-durability value: {other}"),
        )),
    }
}

pub(crate) fn default_grouped_durability_policy() -> DurabilityPolicy {
    DurabilityPolicy::Grouped {
        commit_window_ms: DEFAULT_WARDROBE_GROUP_COMMIT_WINDOW_MS,
        max_batch_size: DEFAULT_WARDROBE_GROUP_COMMIT_MAX_BATCH,
    }
}

pub(crate) fn update_group_commit_window(
    policy: DurabilityPolicy,
    commit_window_ms: u64,
) -> DurabilityPolicy {
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

pub(crate) fn update_group_commit_max_batch(
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

pub(crate) fn validate_wardrobe_namespace_component(flag: &str, value: &str) -> io::Result<()> {
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

pub(crate) fn identifier_fragment(value: &str) -> String {
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
