use crate::config::{
    BenchmarkConfig, DEFAULT_WARDROBE_DATABASE_PREFIX, DEFAULT_WARDROBE_SCHEMA_NAME,
    LibraryProfile, identifier_fragment, validate_wardrobe_namespace_component,
};
use crate::report::{BenchmarkReport, PhaseMetrics, TargetReport};
use crate::targets::{
    BenchmarkTarget, MongoTarget, MySqlTarget, Neo4jTarget, SqliteTarget, WardrobeTarget,
};
use crate::utils::unix_timestamp_micros;
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::Path;
use std::time::{Duration, Instant};

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
    pub(crate) fn all() -> Vec<Self> {
        vec![
            Self::WardrobeEmbedded,
            Self::WardrobeRemote,
            Self::Sqlite,
            Self::MongoDb,
            Self::MySql,
            Self::Neo4j,
        ]
    }

    pub(crate) fn label(self) -> &'static str {
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
pub(crate) struct WardrobeNamespace {
    pub(crate) database: String,
    pub(crate) schema: String,
    pub(crate) generated: bool,
}

impl WardrobeNamespace {
    pub(crate) fn from_config(config: &BenchmarkConfig, run_id: &str) -> io::Result<Self> {
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

    pub(crate) fn label(&self) -> String {
        format!("{}/{}", self.database, self.schema)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseName {
    MassiveIngestion,
    IndexMutation,
    PointLookup,
    RangeLookup,
    ComplexTraversal,
    DeleteById,
    TargetedPurge,
    Compaction,
}

impl PhaseName {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::MassiveIngestion => "Massive Ingestion",
            Self::IndexMutation => "Index Mutation",
            Self::PointLookup => "Point Lookup",
            Self::RangeLookup => "Range Lookup",
            Self::ComplexTraversal => "Complex Traversal",
            Self::DeleteById => "Delete by ID",
            Self::TargetedPurge => "Targeted Purge",
            Self::Compaction => "Compaction",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LatencySample {
    pub(crate) operations: u64,
    pub(crate) elapsed: Duration,
}

#[derive(Debug)]
pub struct PhaseRecorder {
    phase: PhaseName,
    pub(crate) samples: Vec<LatencySample>,
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

pub(crate) fn run_benchmark_with_builder(
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
        "profile: {} entities, {} books, chunk size {}, {} traversal queries, {} point lookups, {} range lookups, {} delete-by-ID operations",
        config.profile.entity_records,
        config.profile.book_records,
        config.profile.chunk_size,
        config.profile.traversal_queries,
        config.profile.point_lookups,
        config.profile.range_lookups,
        config.profile.delete_by_id_operations
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

pub(crate) fn unavailable_target_report(name: &str, reason: String) -> TargetReport {
    TargetReport {
        name: name.to_string(),
        phases: Vec::new(),
        storage_bytes: 0,
        storage_diagnostics: vec![format!("Unavailable: {reason}")],
        unavailable_reason: Some(reason),
    }
}

pub(crate) fn run_target(
    target: &mut dyn BenchmarkTarget,
    profile: &LibraryProfile,
    progress: &ProgressReporter,
) -> io::Result<TargetReport> {
    progress.log(format!("{}: provisioning schema", target.name()));
    target.provision_schema(profile, progress)?;
    progress.log(format!("{}: schema ready", target.name()));

    let phases = [
        PhaseName::MassiveIngestion,
        PhaseName::PointLookup,
        PhaseName::ComplexTraversal,
        PhaseName::IndexMutation,
        PhaseName::DeleteById,
        PhaseName::TargetedPurge,
        PhaseName::Compaction,
        PhaseName::RangeLookup,
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
            PhaseName::PointLookup => {
                target.point_lookup(profile, &mut recorder, progress)?;
            }
            PhaseName::RangeLookup => {
                target.range_lookup(profile, &mut recorder, progress)?;
            }
            PhaseName::ComplexTraversal => {
                target.complex_traversal(profile, &mut recorder, progress)?;
            }
            PhaseName::DeleteById => {
                target.delete_by_id(profile, &mut recorder, progress)?;
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

pub(crate) fn build_target(
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
                    config.wardrobe_client_profile.as_deref(),
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

pub(crate) fn report_record_progress(
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

pub(crate) fn weighted_percentile(samples: &[LatencySample], percentile: f64) -> f64 {
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
