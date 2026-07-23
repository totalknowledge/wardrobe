use super::wardrobe_remote::TcpWardrobeRunner;
use super::{
    BenchmarkTarget, WardrobeCommandRunner, expect_admin, expect_count, expect_delete,
    expect_inventory, expect_missing_record, expect_pointers, expect_record, expect_records,
    expect_vacuumed, verify_deleted_count, verify_record_id, verify_record_range,
};
use crate::config::{BOOK_DRAWER, ENTITY_DRAWER, LibraryProfile};
use crate::engine::{PhaseRecorder, ProgressReporter, WardrobeNamespace, report_record_progress};
use crate::utils::{chunk_ranges, directory_size, fsync_tree, optional_count};
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use wardrobe_core::{
    AlterRequest, Command, CommandResult, CompactRequest, CreateRequest, DurabilityPolicy,
    OperationFilter, OperationOptions, ReturnShape, SecurityConfig, SecurityMode, StatusRequest,
    StorageDiagnosis, StorageInventory, StorageScope, WardrobeEngine, initialize_managed_pki,
    issue_managed_client_certificate,
};

pub(crate) struct WardrobeTarget {
    pub(crate) name: String,
    pub(crate) runner: Option<Box<dyn WardrobeCommandRunner>>,
    pub(crate) storage_root: Option<PathBuf>,
    pub(crate) server_handle: Option<JoinHandle<io::Result<()>>>,
    pub(crate) namespace: WardrobeNamespace,
    pub(crate) profile: Option<LibraryProfile>,
    pub(crate) last_storage_snapshot: Option<WardrobeStorageSnapshot>,
    pub(crate) pre_compaction_storage_snapshot: Option<WardrobeStorageSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WardrobeStorageSnapshot {
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
            let expected_book_records = profile.expected_book_records_after_mutating_phases();
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
    pub(crate) fn embedded(
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
            pre_compaction_storage_snapshot: None,
        })
    }

    pub(crate) fn remote_auto(
        path: PathBuf,
        namespace: WardrobeNamespace,
        durability_policy: DurabilityPolicy,
    ) -> io::Result<Self> {
        fs::create_dir_all(&path)?;
        let security_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("wardrobe-remote-security");
        initialize_managed_pki(
            &security_dir,
            &["localhost".to_string()],
            &["127.0.0.1".parse().map_err(|error| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Invalid loopback IP: {error}"),
                )
            })?],
        )?;
        let certificate = issue_managed_client_certificate(
            &security_dir,
            "wardrobe:service:benchmark",
            "in-process",
            None,
            "localhost",
        )?;
        let engine = WardrobeEngine::open_with_durability_policy(
            path.to_string_lossy().as_ref(),
            durability_policy,
        )?;
        engine.create(CreateRequest::user(json!({
            "username": "benchmark",
            "role": "administrator",
            "permissions": ["*"],
            "certificate_identities": ["wardrobe:service:benchmark"],
        })))?;
        let engine = Arc::new(engine);
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let security = SecurityConfig {
            mode: SecurityMode::Managed,
            security_dir,
            ..SecurityConfig::default()
        };
        let handle = thread::spawn(move || {
            wardrobe_server::serve_tls_tcp_listener(listener, engine, Some(1), security)
        });
        let runner = TcpWardrobeRunner::connect(
            &format!("wardrobe://{address}"),
            certificate.profile.as_path(),
        )?;
        Ok(Self {
            name: "Wardrobe (Remote TCP Server Mode)".to_string(),
            runner: Some(Box::new(runner)),
            storage_root: Some(path),
            server_handle: Some(handle),
            namespace,
            profile: None,
            last_storage_snapshot: None,
            pre_compaction_storage_snapshot: None,
        })
    }

    pub(crate) fn remote_uri(
        uri: &str,
        profile: Option<&Path>,
        namespace: WardrobeNamespace,
    ) -> io::Result<Self> {
        let profile = profile.ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                "--wardrobe-client-profile is required with --wardrobe-remote-uri",
            )
        })?;
        let runner = TcpWardrobeRunner::connect(uri, profile)?;
        Ok(Self {
            name: "Wardrobe (Remote TCP Server Mode)".to_string(),
            runner: Some(Box::new(runner)),
            storage_root: None,
            server_handle: None,
            namespace,
            profile: None,
            last_storage_snapshot: None,
            pre_compaction_storage_snapshot: None,
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
        let diagnosis = match self.execute(Command::Status(StatusRequest::storage().into_request()))
        {
            Ok(CommandResult::Status(payload)) => serde_json::from_value(payload).ok(),
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
        match self.execute(Command::Status(
            StatusRequest::drawers(
                self.namespace.database.clone(),
                self.namespace.schema.clone(),
            )
            .into_request(),
        ))? {
            CommandResult::Status(payload) => serde_json::from_value(payload).map_err(|error| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Invalid Wardrobe drawer inventory: {error}"),
                )
            }),
            other => Err(Error::new(
                ErrorKind::InvalidData,
                format!("Expected Wardrobe drawer inventory, got {other:?}"),
            )),
        }
    }

    fn wal_entry_count(&mut self, database_name: Option<&str>) -> io::Result<Option<usize>> {
        match self.execute(Command::Status(
            StatusRequest::wal(database_name.map(str::to_string)).into_request(),
        ))? {
            CommandResult::Status(payload) => {
                let report: wardrobe_core::WalVerification = serde_json::from_value(payload)
                    .map_err(|error| {
                        Error::new(
                            ErrorKind::InvalidData,
                            format!("Invalid Wardrobe WAL status: {error}"),
                        )
                    })?;
                Ok(Some(report.entry_count))
            }
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
        for field_name in ["author_id", "editor_id", "purge_bucket", "quantity"] {
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

    fn point_lookup(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let ids = profile.point_lookup_book_ids();
        progress.log(format!(
            "{}: reading {} book records by primary id",
            self.name(),
            ids.len()
        ));
        for (index, id) in ids.iter().enumerate() {
            recorder.measure(1, || {
                let record = expect_record(self.execute_scoped(Command::Read {
                    filter: OperationFilter::pointer(format!("@{BOOK_DRAWER}:{id}")),
                    options: OperationOptions::new().return_shape(ReturnShape::Record),
                })?)?;
                verify_record_id(&record, id)
            })?;
            report_record_progress(
                progress,
                &format!("{}: point lookups completed", self.name()),
                index + 1,
                ids.len(),
            );
        }
        Ok(ids.len() as u64)
    }

    fn range_lookup(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let bounds = profile.range_lookup_bounds();
        progress.log(format!(
            "{}: reading {} book records with numeric quantity ranges",
            self.name(),
            bounds.len()
        ));
        for (index, (low, high)) in bounds.iter().enumerate() {
            recorder.measure(1, || {
                let records = expect_records(self.execute_scoped(Command::Read {
                    filter: OperationFilter::query_in(
                        BOOK_DRAWER,
                        json!({ "quantity": { "$gte": low, "$lte": high } }),
                    ),
                    options: OperationOptions::new().return_shape(ReturnShape::Records),
                })?)?;
                for record in records {
                    verify_record_range(&record, "quantity", *low, *high)?;
                }
                Ok(())
            })?;
            report_record_progress(
                progress,
                &format!("{}: range lookups completed", self.name()),
                index + 1,
                bounds.len(),
            );
        }
        Ok(bounds.len() as u64)
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

    fn delete_by_id(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let ids = profile.delete_by_id_book_ids();
        progress.log(format!(
            "{}: deleting {} book records by primary id",
            self.name(),
            ids.len()
        ));
        for (index, id) in ids.iter().enumerate() {
            recorder.measure(1, || {
                let pointer = format!("@{BOOK_DRAWER}:{id}");
                verify_deleted_count(
                    expect_delete(self.execute_scoped(Command::Delete {
                        filter: OperationFilter::pointer(pointer.clone()),
                        options: OperationOptions::default(),
                    })?)?,
                    id,
                )?;
                expect_missing_record(self.execute_scoped(Command::Read {
                    filter: OperationFilter::pointer(pointer),
                    options: OperationOptions::new().return_shape(ReturnShape::Record),
                })?)
            })?;
            report_record_progress(
                progress,
                &format!("{}: delete-by-ID operations completed", self.name()),
                index + 1,
                ids.len(),
            );
        }
        Ok(ids.len() as u64)
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
        let pre_snap = self.capture_storage_snapshot()?;
        self.pre_compaction_storage_snapshot = Some(pre_snap);

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
        } else if let Ok(CommandResult::Status(payload)) =
            self.execute(Command::Status(StatusRequest::storage().into_request()))
        {
            if let Ok(diagnosis) = serde_json::from_value::<StorageDiagnosis>(payload) {
                let path = PathBuf::from(diagnosis.storage_directory);
                if path.exists() {
                    fsync_tree(&path)?;
                }
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
        let post_lines = self
            .last_storage_snapshot
            .as_ref()
            .map(|snapshot| snapshot.diagnostic_lines(&self.namespace, self.profile.as_ref()))
            .unwrap_or_default();

        if let Some(pre_snap) = &self.pre_compaction_storage_snapshot {
            let pre_lines = pre_snap.diagnostic_lines(&self.namespace, self.profile.as_ref());
            let mut combined = Vec::new();
            combined.push("**Pre-Compaction:**".to_string());
            for line in pre_lines {
                combined.push(format!("  - {}", line));
            }
            combined.push("**Post-Compaction:**".to_string());
            for line in post_lines {
                combined.push(format!("  - {}", line));
            }
            Ok(combined)
        } else {
            Ok(post_lines)
        }
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
