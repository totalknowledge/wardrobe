use crate::wrdb_lib::application_logging::{
    ApplicationLogEvent, ApplicationLogLevel, emit_application_log,
};
use crate::wrdb_lib::catalog::{backup, diagnostics};
use crate::wrdb_lib::config::WardrobeConfig;
use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::drawer::VacuumReport;
use crate::wrdb_lib::registry::CatalogRegistry;
use crate::wrdb_lib::routing::{DatabaseRoute, ExecutionContext};
use crate::wrdb_lib::storage_lock::{self, StorageRootLockGuard};
use crate::wrdb_lib::wal::{self, DurabilityPolicy, WalJournal, WalVerification};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Instant;

#[path = "wrdb_lib/access_control.rs"]
mod access_control;
#[path = "wrdb_lib/boundary_execution.rs"]
mod boundary_execution;
#[path = "wrdb_lib/database_execution.rs"]
mod database_execution;

pub use crate::wrdb_lib::command::{
    AlterRequest, BackupArchive, BackupArchiveFile, CheckEntry, CheckReport, Command,
    CommandResult, CompactMode, CompactRequest, CreateRequest, CreateResult, DeleteResult,
    DrawerInspectionMetrics, DropRequest, InspectResult, OperationFilter, OperationOptions,
    PermissionRequest, PermissionScopeDescriptor, ReadResult, RestoreReport, ReturnShape,
    StatusRequest, StatusResult, StorageDiagnosis, UpsertResult,
};
pub use crate::wrdb_lib::query::{OrderDirection, QueryModifiers};
pub use crate::wrdb_lib::storage::{
    StorageCoordinate, StorageInventory, StorageLocator, StorageScope,
};

pub struct WardrobeEngine {
    root_directory: PathBuf,
    registry: RwLock<CatalogRegistry>,
    database_core: RwLock<Database>,
    routed_databases: RwLock<HashMap<DatabaseRoute, Arc<RwLock<Database>>>>,
    max_cached_drawers: Option<usize>,
    wal_size_threshold_bytes: u64,
    wal_ops_threshold_count: u64,
    durability_policy: DurabilityPolicy,
    server_lock: Option<StorageRootLockGuard>,
    logical_wal_journals: RwLock<HashMap<PathBuf, WalJournal>>,
}

#[derive(Debug, Clone)]
pub struct WardrobeEngineBuilder {
    config: WardrobeConfig,
}

#[derive(Debug, Clone)]
pub(crate) enum PointerTarget {
    Full(String),
    Local(String),
}

#[derive(Debug, Default)]
pub(crate) struct OperationSelection {
    pub(crate) drawer_name: Option<String>,
    pub(crate) query: Option<Value>,
    pub(crate) pointers: Vec<PointerTarget>,
}

impl OperationSelection {
    pub(crate) fn from_filter(filter: OperationFilter) -> Result<Self> {
        let mut selection = Self::default();
        selection.merge_filter(filter)?;
        Ok(selection)
    }

    fn merge_filter(&mut self, filter: OperationFilter) -> Result<()> {
        match filter {
            OperationFilter::None => Ok(()),
            OperationFilter::Drawer(drawer_name) => self.merge_drawer(drawer_name),
            OperationFilter::Pointer(pointer) => self.merge_pointer(pointer),
            OperationFilter::Query(query) => self.merge_query(query),
            OperationFilter::Many(filters) => {
                for filter in filters {
                    self.merge_filter(filter)?;
                }
                Ok(())
            }
        }
    }

    fn merge_drawer(&mut self, drawer_name: String) -> Result<()> {
        let drawer_name = drawer_name.trim_start_matches('@').to_string();
        match &self.drawer_name {
            Some(existing) if existing != &drawer_name => Err(Error::new(
                ErrorKind::InvalidInput,
                format!("conflicting drawer filters '{existing}' and '{drawer_name}'"),
            )),
            Some(_) => Ok(()),
            None => {
                self.drawer_name = Some(drawer_name);
                Ok(())
            }
        }
    }

    fn merge_pointer(&mut self, pointer: String) -> Result<()> {
        if pointer.starts_with('@') && !pointer.contains(':') {
            return self.merge_drawer(pointer.trim_start_matches('@').to_string());
        }
        match crate::wrdb_lib::pointer::parse_pointer(&pointer) {
            Ok((drawer_name, record_key)) => {
                self.merge_drawer(drawer_name.clone())?;
                self.pointers.push(PointerTarget::Full(
                    crate::wrdb_lib::pointer::format_pointer(&drawer_name, &record_key),
                ));
            }
            Err(_) => self.pointers.push(PointerTarget::Local(pointer)),
        }
        Ok(())
    }

    fn merge_query(&mut self, query: Value) -> Result<()> {
        if query == Value::Null {
            return Ok(());
        }
        if matches!(&query, Value::Object(fields) if fields.is_empty()) {
            return Ok(());
        }
        if self.query.is_some() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "operation filters may contain only one query object",
            ));
        }
        self.query = Some(query);
        Ok(())
    }

    pub(crate) fn required_drawer(&self, operation_name: &str) -> Result<String> {
        self.drawer_name.clone().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("{operation_name} requires a drawer filter"),
            )
        })
    }

    pub(crate) fn resolved_pointers(&self) -> Result<Vec<String>> {
        self.pointers
            .iter()
            .map(|pointer| match pointer {
                PointerTarget::Full(pointer) => Ok(pointer.clone()),
                PointerTarget::Local(record_key) => {
                    let drawer_name = self.required_drawer("local pointer resolution")?;
                    Ok(crate::wrdb_lib::pointer::format_pointer(
                        &drawer_name,
                        record_key,
                    ))
                }
            })
            .collect()
    }
}

#[derive(Debug)]
pub(crate) struct UpsertContext {
    pub(crate) drawer_name: Option<String>,
    pub(crate) pointer: Option<String>,
    pub(crate) query: Option<Value>,
}

impl UpsertContext {
    pub(crate) fn from_filter(filter: OperationFilter) -> Result<Self> {
        let selection = OperationSelection::from_filter(filter)?;
        if selection.pointers.len() > 1 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "upsert accepts at most one pointer filter",
            ));
        }
        let pointer = selection.resolved_pointers()?.into_iter().next();
        Ok(Self {
            drawer_name: selection.drawer_name,
            pointer,
            query: selection.query,
        })
    }

    pub(crate) fn drawer_name(&self) -> Option<&str> {
        self.drawer_name.as_deref()
    }
}

pub(crate) enum ReturnShapeResolution {
    Record,
    Records,
    Pointers,
    Exists,
}

pub(crate) fn resolve_read_shape(
    return_shape: Option<ReturnShape>,
    selection: &OperationSelection,
) -> ReturnShapeResolution {
    match return_shape {
        Some(ReturnShape::Record) => ReturnShapeResolution::Record,
        Some(ReturnShape::Pointers) => ReturnShapeResolution::Pointers,
        Some(ReturnShape::Exists) => ReturnShapeResolution::Exists,
        Some(ReturnShape::Records)
        | Some(ReturnShape::Default)
        | Some(ReturnShape::Diagnostics)
        | None => {
            if selection.pointers.len() == 1 && selection.query.is_none() {
                ReturnShapeResolution::Record
            } else {
                ReturnShapeResolution::Records
            }
        }
    }
}

pub(crate) fn record_pointer(record: &Value, fallback_drawer_name: Option<&str>) -> Option<String> {
    let record_id = record.get("_id")?.as_str()?;
    if let Ok((drawer_name, record_key)) = crate::wrdb_lib::pointer::parse_pointer(record_id) {
        return Some(crate::wrdb_lib::pointer::format_pointer(
            &drawer_name,
            &record_key,
        ));
    }
    fallback_drawer_name
        .map(|drawer_name| crate::wrdb_lib::pointer::format_pointer(drawer_name, record_id))
}

fn merge_drawer_name(target: &mut Option<String>, drawer_name: impl Into<String>) -> Result<()> {
    let drawer_name = drawer_name.into().trim_start_matches('@').to_string();
    match target {
        Some(existing) if existing != &drawer_name => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("conflicting drawer targets '{existing}' and '{drawer_name}'"),
        )),
        Some(_) => Ok(()),
        None => {
            *target = Some(drawer_name);
            Ok(())
        }
    }
}

pub(crate) fn upsert_record_target(
    mut payload: Value,
    context: &UpsertContext,
) -> Result<(String, Value)> {
    let Value::Object(ref mut fields) = payload else {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "upsert payload entries must be JSON objects",
        ));
    };

    let mut drawer_name = context.drawer_name.clone();

    if let Some(pointer) = &context.pointer {
        let (pointer_drawer, pointer_key) = crate::wrdb_lib::pointer::parse_pointer(pointer)?;
        merge_drawer_name(&mut drawer_name, pointer_drawer)?;
        fields
            .entry("_id".to_string())
            .or_insert(Value::String(pointer_key));
    }

    if let Some(Value::String(record_id)) = fields.get("_id").cloned() {
        if record_id.starts_with('@') && !record_id.contains(':') {
            merge_drawer_name(&mut drawer_name, record_id.trim_start_matches('@'))?;
            fields.remove("_id");
        } else if let Ok((pointer_drawer, pointer_key)) =
            crate::wrdb_lib::pointer::parse_pointer(&record_id)
        {
            merge_drawer_name(&mut drawer_name, pointer_drawer)?;
            let _ = pointer_key;
        }
    }

    let drawer_name = drawer_name.ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "upsert requires a drawer filter, drawer-only _id, or full pointer _id",
        )
    })?;

    Ok((drawer_name, payload))
}

pub(crate) fn merge_payload_into_record(
    existing_record: &mut Value,
    payload: &Value,
) -> Result<()> {
    let Value::Object(existing_fields) = existing_record else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "matched upsert record was not a JSON object",
        ));
    };
    let Value::Object(payload_fields) = payload else {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "query upsert payload must be a JSON object",
        ));
    };
    for (key, value) in payload_fields {
        if key != "_id" {
            existing_fields.insert(key.clone(), value.clone());
        }
    }
    Ok(())
}

fn engine_log(
    level: ApplicationLogLevel,
    message: &'static str,
    fields: Vec<(&'static str, String)>,
) {
    emit_application_log(ApplicationLogEvent::new(
        level,
        "wardrobe_engine",
        message,
        fields,
    ));
}

fn error_log_fields(error: &Error) -> Vec<(&'static str, String)> {
    vec![
        ("error_kind", format!("{:?}", error.kind())),
        ("error", error.to_string()),
    ]
}

impl WardrobeEngine {
    pub fn builder() -> WardrobeEngineBuilder {
        WardrobeEngineBuilder::default()
    }

    pub fn open(directory: &str) -> Result<Self> {
        Self::open_with_optional_limits(directory, None, None)
    }

    pub fn open_with_config(config: WardrobeConfig) -> Result<Self> {
        config.validate()?;
        Self::open_with_optional_limits_and_durability(
            config.data.directory.to_string_lossy().as_ref(),
            config.cache.max_cached_drawers,
            Some((config.wal.checkpoint_size_bytes, config.wal.checkpoint_ops)),
            config.wal.durability,
        )
    }

    pub fn open_with_drawer_cache_limit(
        directory: &str,
        max_cached_drawers: usize,
    ) -> Result<Self> {
        Self::open_with_optional_limits(directory, Some(max_cached_drawers), None)
    }

    pub fn open_with_wal_checkpoint_thresholds(
        directory: &str,
        wal_size_threshold_bytes: u64,
        wal_ops_threshold_count: u64,
    ) -> Result<Self> {
        Self::open_with_optional_limits(
            directory,
            None,
            Some((wal_size_threshold_bytes, wal_ops_threshold_count)),
        )
    }

    pub fn open_with_drawer_cache_limit_and_wal_checkpoint_thresholds(
        directory: &str,
        max_cached_drawers: usize,
        wal_size_threshold_bytes: u64,
        wal_ops_threshold_count: u64,
    ) -> Result<Self> {
        Self::open_with_optional_limits(
            directory,
            Some(max_cached_drawers),
            Some((wal_size_threshold_bytes, wal_ops_threshold_count)),
        )
    }

    fn open_with_optional_limits(
        directory: &str,
        max_cached_drawers: Option<usize>,
        wal_thresholds: Option<(u64, u64)>,
    ) -> Result<Self> {
        Self::open_with_optional_limits_and_durability(
            directory,
            max_cached_drawers,
            wal_thresholds,
            DurabilityPolicy::Strict,
        )
    }

    pub fn open_with_durability_policy(
        directory: &str,
        durability_policy: DurabilityPolicy,
    ) -> Result<Self> {
        Self::open_with_optional_limits_and_durability(directory, None, None, durability_policy)
    }

    pub fn open_for_server(directory: &str) -> Result<Self> {
        Self::open_for_server_with_durability_policy(directory, DurabilityPolicy::Strict)
    }

    pub fn open_for_server_with_durability_policy(
        directory: &str,
        durability_policy: DurabilityPolicy,
    ) -> Result<Self> {
        let root_directory = PathBuf::from(directory);
        let server_lock = storage_lock::acquire_server_lock(&root_directory)?;
        Self::open_with_optional_limits_and_durability_and_lock(
            directory,
            None,
            None,
            durability_policy,
            Some(server_lock),
        )
    }

    pub fn open_for_server_with_config(config: WardrobeConfig) -> Result<Self> {
        config.validate()?;
        let root_directory = config.data.directory.clone();
        let server_lock = storage_lock::acquire_server_lock(&root_directory)?;
        Self::open_with_optional_limits_and_durability_and_lock(
            root_directory.to_string_lossy().as_ref(),
            config.cache.max_cached_drawers,
            Some((config.wal.checkpoint_size_bytes, config.wal.checkpoint_ops)),
            config.wal.durability,
            Some(server_lock),
        )
    }

    fn open_with_optional_limits_and_durability(
        directory: &str,
        max_cached_drawers: Option<usize>,
        wal_thresholds: Option<(u64, u64)>,
        durability_policy: DurabilityPolicy,
    ) -> Result<Self> {
        Self::open_with_optional_limits_and_durability_and_lock(
            directory,
            max_cached_drawers,
            wal_thresholds,
            durability_policy,
            None,
        )
    }

    fn open_with_optional_limits_and_durability_and_lock(
        directory: &str,
        max_cached_drawers: Option<usize>,
        wal_thresholds: Option<(u64, u64)>,
        durability_policy: DurabilityPolicy,
        server_lock: Option<StorageRootLockGuard>,
    ) -> Result<Self> {
        let root_directory = PathBuf::from(directory);
        let registry = CatalogRegistry::open_or_initialize(&root_directory)?;
        let (default_wal_size_threshold, default_wal_ops_threshold) =
            Database::default_wal_thresholds();
        let (wal_size_threshold_bytes, wal_ops_threshold_count) =
            wal_thresholds.unwrap_or((default_wal_size_threshold, default_wal_ops_threshold));
        let database_core = Database::initialize_with_cache_limit_wal_thresholds_and_durability(
            &root_directory,
            max_cached_drawers,
            wal_size_threshold_bytes,
            wal_ops_threshold_count,
            durability_policy.clone(),
        )?;
        let database_core = RwLock::new(database_core);
        let recovery_started = Instant::now();
        engine_log(
            ApplicationLogLevel::Info,
            "recovery_start",
            vec![
                ("operation", "recovery".to_string()),
                ("storage_root", root_directory.display().to_string()),
            ],
        );
        if let Err(error) = wal::recover_database::<Self>(&database_core) {
            let mut fields = vec![
                ("operation", "recovery".to_string()),
                ("storage_root", root_directory.display().to_string()),
                (
                    "duration_us",
                    recovery_started.elapsed().as_micros().to_string(),
                ),
                ("success", "false".to_string()),
            ];
            fields.extend(error_log_fields(&error));
            engine_log(ApplicationLogLevel::Error, "recovery_failure", fields);
            return Err(error);
        }
        engine_log(
            ApplicationLogLevel::Info,
            "recovery_finish",
            vec![
                ("operation", "recovery".to_string()),
                ("storage_root", root_directory.display().to_string()),
                (
                    "duration_us",
                    recovery_started.elapsed().as_micros().to_string(),
                ),
                ("success", "true".to_string()),
            ],
        );
        Ok(Self {
            root_directory,
            registry: RwLock::new(registry),
            database_core,
            routed_databases: RwLock::new(HashMap::new()),
            max_cached_drawers,
            wal_size_threshold_bytes,
            wal_ops_threshold_count,
            durability_policy,
            server_lock,
            logical_wal_journals: RwLock::new(HashMap::new()),
        })
    }

    pub(crate) fn logical_wal_journal(&self, database_path: &Path) -> Result<WalJournal> {
        let mut journals = self
            .logical_wal_journals
            .write()
            .map_err(|_| Error::other("Lock poisoned"))?;
        let path = database_path.to_path_buf();
        if let Some(journal) = journals.get(&path) {
            return Ok(journal.clone());
        }
        let journal =
            WalJournal::at_database_path_with_policy(database_path, self.durability_policy.clone());
        journals.insert(path, journal.clone());
        Ok(journal)
    }

    #[deprecated(note = "Use WardrobeEngine::open for filesystem-backed initialization")]
    pub fn new(directory: &str) -> Result<Self> {
        Self::open(directory)
    }

    pub fn upsert<P, F, O>(&self, payload: P, filter: F, options: O) -> Result<UpsertResult>
    where
        P: Into<Value>,
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        let options = options.into();
        let context = UpsertContext::from_filter(filter.into())?;
        let pointers = self.upsert_normalized_payload(payload.into(), context, &options)?;
        Ok(UpsertResult::Pointers(pointers))
    }

    pub fn read<F, O>(&self, filter: F, options: O) -> Result<ReadResult>
    where
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        let options = options.into();
        let selection = OperationSelection::from_filter(filter.into())?;
        let records = self.read_selection_records(&selection, &options)?;
        match resolve_read_shape(options.return_shape, &selection) {
            ReturnShapeResolution::Record => Ok(ReadResult::Record(records.into_iter().next())),
            ReturnShapeResolution::Records => Ok(ReadResult::Records(records)),
            ReturnShapeResolution::Pointers => Ok(ReadResult::Pointers(
                records
                    .iter()
                    .filter_map(|record| record_pointer(record, selection.drawer_name.as_deref()))
                    .collect(),
            )),
            ReturnShapeResolution::Exists => Ok(ReadResult::Exists(!records.is_empty())),
        }
    }

    pub fn count<F, O>(&self, filter: F, options: O) -> Result<usize>
    where
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        let options = options.into();
        let selection = OperationSelection::from_filter(filter.into())?;
        if !selection.pointers.is_empty() {
            return self.count_existing_pointers(&selection);
        }
        let drawer_name = selection.required_drawer("count")?;
        Self::count_in_database(
            &self.database_core,
            &drawer_name,
            selection.query,
            options.query_modifiers(),
            ExecutionContext::root(),
        )
    }

    pub fn delete<F, O>(&self, filter: F, options: O) -> Result<DeleteResult>
    where
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        let _options = options.into();
        let selection = OperationSelection::from_filter(filter.into())?;
        if !selection.pointers.is_empty() {
            let mut deleted = 0;
            for pointer in selection.resolved_pointers()? {
                deleted += usize::from(Self::delete_by_id_in_database(
                    &self.database_core,
                    StorageLocator::Inline(pointer),
                    ExecutionContext::root(),
                )?);
            }
            return Ok(DeleteResult { deleted });
        }
        let drawer_name = selection.required_drawer("delete-by-filter")?;
        let Some(query) = selection.query else {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "delete requires a record pointer or a drawer query filter",
            ));
        };
        Self::delete_by_filter_in_database(
            &self.database_core,
            &drawer_name,
            query,
            ExecutionContext::root(),
        )
        .map(|deleted| DeleteResult { deleted })
    }

    pub fn compact<C>(&self, request: C) -> Result<VacuumReport>
    where
        C: Into<CompactRequest>,
    {
        match request.into() {
            CompactRequest::Drawer { drawer_name, mode } => {
                let mode_label = match mode {
                    CompactMode::Vacuum => "vacuum",
                    CompactMode::Migrate => "migrate",
                };
                let started = Instant::now();
                engine_log(
                    ApplicationLogLevel::Info,
                    "compact_start",
                    vec![
                        ("operation", "compact".to_string()),
                        ("drawer", drawer_name.clone()),
                        ("mode", mode_label.to_string()),
                    ],
                );
                let result = match mode {
                    CompactMode::Vacuum => Self::vacuum_drawer_in_database(
                        &self.database_core,
                        &drawer_name,
                        ExecutionContext::root(),
                    ),
                    CompactMode::Migrate => Self::migrate_drawer_in_database(
                        &self.database_core,
                        &drawer_name,
                        ExecutionContext::root(),
                    ),
                };
                match &result {
                    Ok(report) => engine_log(
                        ApplicationLogLevel::Info,
                        "compact_finish",
                        vec![
                            ("operation", "compact".to_string()),
                            ("drawer", drawer_name),
                            ("mode", mode_label.to_string()),
                            ("duration_us", started.elapsed().as_micros().to_string()),
                            ("success", "true".to_string()),
                            ("records_rewritten", report.records_rewritten.to_string()),
                            ("bytes_reclaimed", report.bytes_reclaimed.to_string()),
                        ],
                    ),
                    Err(error) => {
                        let mut fields = vec![
                            ("operation", "compact".to_string()),
                            ("drawer", drawer_name),
                            ("mode", mode_label.to_string()),
                            ("duration_us", started.elapsed().as_micros().to_string()),
                            ("success", "false".to_string()),
                        ];
                        fields.extend(error_log_fields(error));
                        engine_log(ApplicationLogLevel::Error, "compact_failure", fields);
                    }
                }
                result
            }
        }
    }

    pub fn inspect<F, O>(&self, filter: F, _options: O) -> Result<InspectResult>
    where
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        let selection = OperationSelection::from_filter(filter.into())?;
        let drawer_name = selection.required_drawer("inspect")?;
        diagnostics::inspect_drawer(self, &drawer_name).map(InspectResult::Drawer)
    }

    fn upsert_normalized_payload(
        &self,
        payload: Value,
        context: UpsertContext,
        options: &OperationOptions,
    ) -> Result<Vec<String>> {
        if context.query.is_some() {
            return self.upsert_by_query_context(payload, context, options);
        }

        match payload {
            Value::Array(records) => {
                if context.pointer.is_none() && context.query.is_none() {
                    if let Some(drawer_name) = context.drawer_name() {
                        return Self::bulk_upsert_in_database(
                            &self.database_core,
                            drawer_name,
                            records,
                            options.atomic_enabled(),
                            ExecutionContext::root(),
                        );
                    }
                }
                let mut grouped_records: HashMap<String, Vec<Value>> = HashMap::new();
                for record in records {
                    let (drawer_name, record) = upsert_record_target(record, &context)?;
                    grouped_records.entry(drawer_name).or_default().push(record);
                }
                let mut pointers = Vec::new();
                for (drawer_name, records) in grouped_records {
                    pointers.extend(Self::bulk_upsert_in_database(
                        &self.database_core,
                        &drawer_name,
                        records,
                        options.atomic_enabled(),
                        ExecutionContext::root(),
                    )?);
                }
                Ok(pointers)
            }
            payload => {
                let (drawer_name, payload) = upsert_record_target(payload, &context)?;
                Self::upsert_in_database(
                    &self.database_core,
                    &drawer_name,
                    payload,
                    ExecutionContext::root(),
                )
                .map(|pointer| vec![pointer])
            }
        }
    }

    fn upsert_by_query_context(
        &self,
        payload: Value,
        context: UpsertContext,
        options: &OperationOptions,
    ) -> Result<Vec<String>> {
        let drawer_name = context.drawer_name().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                "query upsert requires a drawer filter",
            )
        })?;
        let query = context.query.clone().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                "query upsert requires a query filter",
            )
        })?;
        let matched_records = Self::find_by_filter_in_database(
            &self.database_core,
            drawer_name,
            query,
            None,
            ExecutionContext::root(),
        )?;
        if matched_records.is_empty() {
            if options.create_if_missing == Some(false) {
                return Ok(Vec::new());
            }
            let create_context = UpsertContext {
                drawer_name: context.drawer_name,
                pointer: None,
                query: None,
            };
            return self.upsert_normalized_payload(payload, create_context, options);
        }
        if matched_records.len() > 1 && options.multi != Some(true) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "query upsert matched multiple records; set multi=true to update all matches",
            ));
        }

        let mut pointers = Vec::new();
        for mut existing_record in matched_records {
            merge_payload_into_record(&mut existing_record, &payload)?;
            let pointer = record_pointer(&existing_record, Some(drawer_name)).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    "query upsert match did not include an _id field",
                )
            })?;
            let update_context = UpsertContext {
                drawer_name: Some(drawer_name.to_string()),
                pointer: Some(pointer),
                query: None,
            };
            pointers.extend(self.upsert_normalized_payload(
                existing_record,
                update_context,
                options,
            )?);
        }
        Ok(pointers)
    }

    fn read_selection_records(
        &self,
        selection: &OperationSelection,
        options: &OperationOptions,
    ) -> Result<Vec<Value>> {
        if !selection.pointers.is_empty() {
            let mut records = Vec::new();
            for pointer in selection.resolved_pointers()? {
                if let Some(record) = Self::find_by_id_in_database(
                    &self.database_core,
                    &pointer,
                    ExecutionContext::root(),
                )? {
                    records.push(record);
                }
            }
            crate::wrdb_lib::query::apply_query_modifiers(
                &mut records,
                options.query_modifiers().as_ref(),
            );
            return Ok(records);
        }

        let drawer_name = selection.required_drawer("read")?;
        if let Some(query) = selection.query.clone() {
            return Self::find_by_filter_in_database(
                &self.database_core,
                &drawer_name,
                query,
                options.query_modifiers(),
                ExecutionContext::root(),
            );
        }

        let mut records = Self::find_all_in_database(
            &self.database_core,
            &drawer_name,
            ExecutionContext::root(),
        )?;
        crate::wrdb_lib::query::apply_query_modifiers(
            &mut records,
            options.query_modifiers().as_ref(),
        );
        Ok(records)
    }

    fn count_existing_pointers(&self, selection: &OperationSelection) -> Result<usize> {
        let mut count = 0;
        for pointer in selection.resolved_pointers()? {
            if Self::find_by_id_in_database(
                &self.database_core,
                &pointer,
                ExecutionContext::root(),
            )?
            .is_some()
            {
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn alter<A>(&self, request: A) -> Result<Value>
    where
        A: Into<AlterRequest>,
    {
        match request.into() {
            AlterRequest::SchemaRule {
                drawer_name,
                action,
                kind,
                field_name,
                payload,
            } => Self::manage_schema_in_database(
                &self.database_core,
                &drawer_name,
                &action,
                &kind,
                &field_name,
                payload,
                ExecutionContext::root(),
            ),
        }
    }

    pub fn backup(&self, source_path: &str) -> Result<BackupArchive> {
        let started = Instant::now();
        engine_log(
            ApplicationLogLevel::Info,
            "backup_start",
            vec![
                ("operation", "backup".to_string()),
                ("source_path", source_path.to_string()),
            ],
        );
        let result = backup::backup_archive(self, source_path);
        match &result {
            Ok(archive) => engine_log(
                ApplicationLogLevel::Info,
                "backup_finish",
                vec![
                    ("operation", "backup".to_string()),
                    ("source_path", source_path.to_string()),
                    ("scope", archive.scope.clone()),
                    ("file_count", archive.files.len().to_string()),
                    ("duration_us", started.elapsed().as_micros().to_string()),
                    ("success", "true".to_string()),
                ],
            ),
            Err(error) => {
                let mut fields = vec![
                    ("operation", "backup".to_string()),
                    ("source_path", source_path.to_string()),
                    ("duration_us", started.elapsed().as_micros().to_string()),
                    ("success", "false".to_string()),
                ];
                fields.extend(error_log_fields(error));
                engine_log(ApplicationLogLevel::Error, "backup_failure", fields);
            }
        }
        result
    }

    pub fn restore(&self, destination_path: &str, archive: BackupArchive) -> Result<RestoreReport> {
        let started = Instant::now();
        let archive_scope = archive.scope.clone();
        let archive_file_count = archive.files.len();
        engine_log(
            ApplicationLogLevel::Info,
            "restore_start",
            vec![
                ("operation", "restore".to_string()),
                ("destination_path", destination_path.to_string()),
                ("scope", archive_scope.clone()),
                ("file_count", archive_file_count.to_string()),
            ],
        );
        let result = backup::restore_archive(self, destination_path, archive);
        match &result {
            Ok(report) => engine_log(
                ApplicationLogLevel::Info,
                "restore_finish",
                vec![
                    ("operation", "restore".to_string()),
                    ("destination_path", report.destination_path.clone()),
                    ("scope", report.scope.clone()),
                    ("file_count", report.file_count.to_string()),
                    ("byte_count", report.byte_count.to_string()),
                    ("duration_us", started.elapsed().as_micros().to_string()),
                    ("success", "true".to_string()),
                ],
            ),
            Err(error) => {
                let mut fields = vec![
                    ("operation", "restore".to_string()),
                    ("destination_path", destination_path.to_string()),
                    ("scope", archive_scope),
                    ("file_count", archive_file_count.to_string()),
                    ("duration_us", started.elapsed().as_micros().to_string()),
                    ("success", "false".to_string()),
                ];
                fields.extend(error_log_fields(error));
                engine_log(ApplicationLogLevel::Error, "restore_failure", fields);
            }
        }
        result
    }

    pub fn create<C>(&self, request: C) -> Result<CreateResult>
    where
        C: Into<CreateRequest>,
    {
        match request.into() {
            CreateRequest::Database { database_name } => {
                boundary_execution::create_database(self, &database_name)
                    .map(CreateResult::StorageInventory)
            }
            CreateRequest::Schema {
                database_name,
                schema_name,
            } => boundary_execution::create_schema(self, &database_name, &schema_name)
                .map(CreateResult::StorageInventory),
            CreateRequest::Drawer {
                database_name,
                schema_name,
                drawer_name,
            } => {
                boundary_execution::create_drawer(self, &database_name, &schema_name, &drawer_name)
                    .map(CreateResult::StorageInventory)
            }
            CreateRequest::TenantRoute {
                tenant_id,
                database_name,
                location,
            } => boundary_execution::register_tenant_route(
                self,
                &tenant_id,
                &database_name,
                &location,
            )
            .map(CreateResult::StorageInventory),
            CreateRequest::User { payload } => self
                .manage_user_authorization("add_user", payload)
                .map(CreateResult::Admin),
        }
    }

    pub fn drop<D>(&self, request: D) -> Result<Value>
    where
        D: Into<DropRequest>,
    {
        match request.into() {
            DropRequest::Database { database_name } => {
                boundary_execution::drop_database(self, &database_name)
            }
            DropRequest::Schema {
                database_name,
                schema_name,
            } => boundary_execution::drop_schema(self, &database_name, &schema_name),
            DropRequest::Drawer {
                database_name,
                schema_name,
                drawer_name,
            } => boundary_execution::drop_drawer(self, &database_name, &schema_name, &drawer_name),
            DropRequest::SchemaRule {
                drawer_name,
                kind,
                field_name,
                payload,
            } => Self::manage_schema_in_database(
                &self.database_core,
                &drawer_name,
                "remove",
                &kind,
                &field_name,
                payload,
                ExecutionContext::root(),
            ),
            DropRequest::User { username } => self.manage_user_authorization(
                "drop_user",
                serde_json::json!({ "username": username }),
            ),
        }
    }

    pub fn grant(&self, request: PermissionRequest) -> Result<Value> {
        self.manage_user_authorization("grant_permission", request.into_payload())
    }

    pub fn revoke(&self, request: PermissionRequest) -> Result<Value> {
        self.manage_user_authorization("revoke_permission", request.into_payload())
    }

    pub(crate) fn flush_all_metadata(&self) -> Result<()> {
        {
            let db = Self::read_lock(&self.database_core)?;
            db.flush_all_drawers_metadata()?;
        }
        {
            let routed = Self::read_lock(&self.routed_databases)?;
            for db_lock in routed.values() {
                let db = Self::read_lock(db_lock)?;
                db.flush_all_drawers_metadata()?;
            }
        }
        Ok(())
    }

    pub fn status<S>(&self, request: S) -> Result<StatusResult>
    where
        S: Into<StatusRequest>,
    {
        let _ = self.flush_all_metadata();
        match request.into() {
            StatusRequest::Tenants => {
                boundary_execution::show_tenants(self).map(StatusResult::Tenants)
            }
            StatusRequest::Databases => {
                boundary_execution::show_databases(self).map(StatusResult::Databases)
            }
            StatusRequest::Schemas { database_name } => {
                boundary_execution::show_schemas(self, &database_name).map(StatusResult::Schemas)
            }
            StatusRequest::Drawers {
                database_name,
                schema_name,
            } => boundary_execution::show_drawers(self, &database_name, &schema_name)
                .map(StatusResult::Drawers),
            StatusRequest::Wal { database_name } => {
                boundary_execution::verify_wal(self, database_name.as_deref())
                    .map(StatusResult::Wal)
            }
            StatusRequest::Storage => {
                diagnostics::diagnose_storage(self).map(StatusResult::Storage)
            }
            StatusRequest::Path { path } => {
                diagnostics::check_path(self, &path).map(StatusResult::Check)
            }
            StatusRequest::DrawerNames => {
                diagnostics::list_drawer_names(self).map(StatusResult::DrawerNames)
            }
            StatusRequest::CachedDrawerCount => self
                .cached_drawer_count()
                .map(StatusResult::CachedDrawerCount),
        }
    }

    pub(crate) fn inspect_drawer(&self, drawer_name: &str) -> Result<DrawerInspectionMetrics> {
        let _ = self.flush_all_metadata();
        diagnostics::inspect_drawer(self, drawer_name)
    }

    pub(crate) fn check_path(&self, raw_path: &str) -> Result<CheckReport> {
        let _ = self.flush_all_metadata();
        diagnostics::check_path(self, raw_path)
    }

    pub(crate) fn diagnose_storage(&self) -> Result<StorageDiagnosis> {
        let _ = self.flush_all_metadata();
        diagnostics::diagnose_storage(self)
    }

    pub(crate) fn list_drawer_names(&self) -> Result<Vec<String>> {
        let _ = self.flush_all_metadata();
        diagnostics::list_drawer_names(self)
    }

    pub(crate) fn backup_archive(&self, source_path: &str) -> Result<BackupArchive> {
        backup::backup_archive(self, source_path)
    }

    pub(crate) fn restore_archive(
        &self,
        destination_path: &str,
        archive: BackupArchive,
    ) -> Result<RestoreReport> {
        backup::restore_archive(self, destination_path, archive)
    }

    pub(crate) fn manage_user(&self, action: &str, payload: Value) -> Result<Value> {
        self.manage_user_authorization(action, payload)
    }

    pub(crate) fn root_directory(&self) -> &Path {
        &self.root_directory
    }

    pub fn configured_max_cached_drawers(&self) -> Option<usize> {
        self.max_cached_drawers
    }

    pub fn configured_wal_thresholds(&self) -> (u64, u64) {
        (self.wal_size_threshold_bytes, self.wal_ops_threshold_count)
    }

    pub fn configured_durability_policy(&self) -> DurabilityPolicy {
        self.durability_policy.clone()
    }

    fn manage_user_authorization(&self, action: &str, payload: Value) -> Result<Value> {
        if self.server_lock.is_some() {
            access_control::manage_user_with_server_lock(&self.root_directory, action, payload)
        } else {
            access_control::manage_user(&self.root_directory, action, payload)
        }
    }

    pub(crate) fn cached_drawer_count(&self) -> Result<usize> {
        Ok(Self::read_lock(&self.database_core)?.cached_drawer_count())
    }

    pub(crate) fn show_tenants(&self) -> Result<Vec<String>> {
        boundary_execution::show_tenants(self)
    }

    pub(crate) fn show_databases(&self) -> Result<Vec<StorageInventory>> {
        boundary_execution::show_databases(self)
    }

    pub(crate) fn verify_wal(&self, database_name: Option<&str>) -> Result<WalVerification> {
        boundary_execution::verify_wal(self, database_name)
    }

    pub(crate) fn show_schemas(&self, database_name: &str) -> Result<Vec<String>> {
        boundary_execution::show_schemas(self, database_name)
    }

    pub(crate) fn show_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<Vec<StorageInventory>> {
        boundary_execution::show_drawers(self, database_name, schema_name)
    }

    pub fn execute(
        &self,
        coordinate: StorageCoordinate,
        command: Command,
    ) -> Result<CommandResult> {
        boundary_execution::execute(self, coordinate, command)
    }

    pub fn execute_in_scope(&self, scope: StorageScope, command: Command) -> Result<CommandResult> {
        boundary_execution::execute_in_scope(self, scope, command)
    }

    pub(crate) fn create_database(&self, database_name: &str) -> Result<StorageInventory> {
        boundary_execution::create_database(self, database_name)
    }

    pub(crate) fn create_schema(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<StorageInventory> {
        boundary_execution::create_schema(self, database_name, schema_name)
    }

    pub(crate) fn create_drawer(
        &self,
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> Result<StorageInventory> {
        boundary_execution::create_drawer(self, database_name, schema_name, drawer_name)
    }

    pub(crate) fn register_tenant_route(
        &self,
        tenant_id: &str,
        database_name: &str,
        location: &str,
    ) -> Result<StorageInventory> {
        boundary_execution::register_tenant_route(self, tenant_id, database_name, location)
    }

    pub(crate) fn drop_database(&self, database_name: &str) -> Result<Value> {
        boundary_execution::drop_database(self, database_name)
    }

    pub(crate) fn drop_schema(&self, database_name: &str, schema_name: &str) -> Result<Value> {
        boundary_execution::drop_schema(self, database_name, schema_name)
    }

    pub(crate) fn drop_drawer(
        &self,
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> Result<Value> {
        boundary_execution::drop_drawer(self, database_name, schema_name, drawer_name)
    }

    pub fn execute_for_tenant(
        &self,
        tenant_id: &str,
        database_name: &str,
        schema_name: &str,
        command: Command,
    ) -> Result<CommandResult> {
        boundary_execution::execute_for_tenant(self, tenant_id, database_name, schema_name, command)
    }

    pub fn execute_command(&self, command: Command) -> Result<CommandResult> {
        boundary_execution::execute_command(self, command)
    }
}

impl Default for WardrobeEngineBuilder {
    fn default() -> Self {
        Self {
            config: WardrobeConfig::default(),
        }
    }
}

impl WardrobeEngineBuilder {
    pub fn config(mut self, config: WardrobeConfig) -> Self {
        self.config = config;
        self
    }

    pub fn directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.config.data.directory = directory.into();
        self
    }

    pub fn max_cached_drawers(mut self, max_cached_drawers: usize) -> Self {
        self.config.cache.max_cached_drawers = Some(max_cached_drawers);
        self
    }

    pub fn durability(mut self, durability_policy: DurabilityPolicy) -> Self {
        self.config.wal.durability = durability_policy;
        self
    }

    pub fn wal_checkpoint_thresholds(
        mut self,
        checkpoint_size_bytes: u64,
        checkpoint_ops: u64,
    ) -> Self {
        self.config.wal.checkpoint_size_bytes = checkpoint_size_bytes;
        self.config.wal.checkpoint_ops = checkpoint_ops;
        self
    }

    pub fn open(self) -> Result<WardrobeEngine> {
        WardrobeEngine::open_with_config(self.config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(test_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("wardrobe_engine_{test_name}_{nanos}"))
    }

    #[test]
    fn engine_open_does_not_configure_application_logging_by_default() {
        let _guard = crate::wrdb_lib::application_logging::test_logging_guard();
        crate::wrdb_lib::application_logging::shutdown_application_logging();
        let storage = temp_dir("logging_default");
        let engine = WardrobeEngine::open(&storage.to_string_lossy()).expect("engine should open");

        assert!(!crate::wrdb_lib::application_logging::application_logging_is_configured());

        drop(engine);
        let _ = std::fs::remove_dir_all(storage);
    }

    #[test]
    fn operation_selection_merges_drawers_queries_and_pointers() {
        let selection = OperationSelection::from_filter(OperationFilter::many(vec![
            OperationFilter::drawer("@gem"),
            OperationFilter::pointer("ruby"),
            OperationFilter::query(json!({"power": 42})),
        ]))
        .expect("selection should merge");

        assert_eq!(selection.required_drawer("read").unwrap(), "gem");
        assert_eq!(selection.query, Some(json!({"power": 42})));
        assert_eq!(
            selection.resolved_pointers().unwrap(),
            vec!["@gem:ruby".to_string()]
        );

        let full_pointer =
            OperationSelection::from_filter(OperationFilter::pointer("@tool:hammer"))
                .expect("full pointer should merge");
        assert_eq!(full_pointer.required_drawer("read").unwrap(), "tool");
        assert_eq!(
            full_pointer.resolved_pointers().unwrap(),
            vec!["@tool:hammer".to_string()]
        );

        let empty_query =
            OperationSelection::from_filter(OperationFilter::query(json!({}))).unwrap();
        assert!(empty_query.query.is_none());
        assert!(
            OperationSelection::from_filter(OperationFilter::query(Value::Null))
                .unwrap()
                .query
                .is_none()
        );

        assert!(
            OperationSelection::from_filter(OperationFilter::many(vec![
                OperationFilter::drawer("gem"),
                OperationFilter::drawer("tool"),
            ]))
            .is_err()
        );
        assert!(
            OperationSelection::from_filter(OperationFilter::many(vec![
                OperationFilter::query(json!({"a": 1})),
                OperationFilter::query(json!({"b": 2})),
            ]))
            .is_err()
        );
        assert!(
            OperationSelection::default()
                .required_drawer("read")
                .is_err()
        );
        assert!(
            OperationSelection::from_filter(OperationFilter::pointer("ruby"))
                .unwrap()
                .resolved_pointers()
                .is_err()
        );
    }

    #[test]
    fn upsert_context_and_record_target_cover_pointer_and_error_paths() {
        let context = UpsertContext::from_filter(OperationFilter::many(vec![
            OperationFilter::drawer("gem"),
            OperationFilter::query(json!({"power": 42})),
        ]))
        .expect("context should build");
        assert_eq!(context.drawer_name(), Some("gem"));
        assert_eq!(context.query, Some(json!({"power": 42})));

        assert!(
            UpsertContext::from_filter(OperationFilter::many(vec![
                OperationFilter::pointer("@gem:ruby"),
                OperationFilter::pointer("@gem:sapphire"),
            ]))
            .is_err()
        );

        let pointer_context =
            UpsertContext::from_filter(OperationFilter::pointer("@gem:ruby")).unwrap();
        let (drawer, payload) = upsert_record_target(json!({"power": 42}), &pointer_context)
            .expect("pointer target should inject id");
        assert_eq!(drawer, "gem");
        assert_eq!(payload["_id"], "ruby");

        let drawer_context = UpsertContext::from_filter(OperationFilter::drawer("gem")).unwrap();
        let no_filter_context = UpsertContext {
            drawer_name: None,
            pointer: None,
            query: None,
        };
        let (drawer, payload) = upsert_record_target(json!({"_id": "@tool"}), &no_filter_context)
            .expect("drawer-only id should provide drawer");
        assert_eq!(drawer, "tool");
        assert!(payload.get("_id").is_none());
        assert!(upsert_record_target(json!({"_id": "@tool"}), &drawer_context).is_err());

        let (drawer, payload) =
            upsert_record_target(json!({"_id": "@tool:hammer"}), &no_filter_context)
                .expect("full pointer id should provide drawer");
        assert_eq!(drawer, "tool");
        assert_eq!(payload["_id"], "@tool:hammer");

        assert!(upsert_record_target(json!(["not", "object"]), &drawer_context).is_err());
        assert!(
            upsert_record_target(
                json!({"_id": "@tool:hammer"}),
                &UpsertContext {
                    drawer_name: Some("gem".to_string()),
                    pointer: None,
                    query: None,
                },
            )
            .is_err()
        );
        assert!(
            upsert_record_target(
                json!({"power": 42}),
                &UpsertContext {
                    drawer_name: None,
                    pointer: None,
                    query: None,
                },
            )
            .is_err()
        );
    }

    #[test]
    fn read_shape_record_pointer_and_merge_helpers_cover_branches() {
        let pointer_selection =
            OperationSelection::from_filter(OperationFilter::pointer("@gem:ruby")).unwrap();
        assert!(matches!(
            resolve_read_shape(None, &pointer_selection),
            ReturnShapeResolution::Record
        ));
        assert!(matches!(
            resolve_read_shape(Some(ReturnShape::Pointers), &pointer_selection),
            ReturnShapeResolution::Pointers
        ));
        assert!(matches!(
            resolve_read_shape(Some(ReturnShape::Exists), &pointer_selection),
            ReturnShapeResolution::Exists
        ));
        assert!(matches!(
            resolve_read_shape(Some(ReturnShape::Record), &pointer_selection),
            ReturnShapeResolution::Record
        ));

        let query_selection = OperationSelection::from_filter(OperationFilter::many(vec![
            OperationFilter::drawer("gem"),
            OperationFilter::query(json!({"power": 42})),
        ]))
        .unwrap();
        assert!(matches!(
            resolve_read_shape(Some(ReturnShape::Default), &query_selection),
            ReturnShapeResolution::Records
        ));
        assert!(matches!(
            resolve_read_shape(Some(ReturnShape::Diagnostics), &query_selection),
            ReturnShapeResolution::Records
        ));
        assert!(matches!(
            resolve_read_shape(Some(ReturnShape::Records), &query_selection),
            ReturnShapeResolution::Records
        ));

        assert_eq!(
            record_pointer(&json!({"_id": "@gem:lnk_ruby"}), None),
            Some("@gem:ruby".to_string())
        );
        assert_eq!(
            record_pointer(&json!({"_id": "ruby"}), Some("gem")),
            Some("@gem:ruby".to_string())
        );
        assert_eq!(record_pointer(&json!({"name": "ruby"}), Some("gem")), None);
        assert_eq!(record_pointer(&json!({"_id": 7}), Some("gem")), None);

        let mut existing = json!({"_id": "ruby", "power": 1, "keep": true});
        merge_payload_into_record(&mut existing, &json!({"_id": "ignored", "power": 99}))
            .expect("merge should succeed");
        assert_eq!(existing, json!({"_id": "ruby", "power": 99, "keep": true}));
        assert!(merge_payload_into_record(&mut json!(null), &json!({})).is_err());
        assert!(merge_payload_into_record(&mut existing, &json!(null)).is_err());
    }
}
