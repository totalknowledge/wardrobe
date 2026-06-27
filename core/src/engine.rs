use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::drawer::VacuumReport;
use crate::wrdb_lib::registry::CatalogRegistry;
use crate::wrdb_lib::routing::{DatabaseRoute, ExecutionContext};
use crate::wrdb_lib::wal::{self, DurabilityPolicy, WalVerification};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Error, ErrorKind, Result};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

#[path = "wrdb_lib/access_control.rs"]
mod access_control;
#[path = "wrdb_lib/backup.rs"]
mod backup;
#[path = "wrdb_lib/boundary_execution.rs"]
mod boundary_execution;
#[path = "wrdb_lib/database_execution.rs"]
mod database_execution;
#[path = "wrdb_lib/diagnostics.rs"]
mod diagnostics;

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

impl WardrobeEngine {
    pub fn open(directory: &str) -> Result<Self> {
        Self::open_with_optional_limits(directory, None, None)
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

    fn open_with_optional_limits_and_durability(
        directory: &str,
        max_cached_drawers: Option<usize>,
        wal_thresholds: Option<(u64, u64)>,
        durability_policy: DurabilityPolicy,
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
        wal::recover_database::<Self>(&database_core)?;
        Ok(Self {
            root_directory,
            registry: RwLock::new(registry),
            database_core,
            routed_databases: RwLock::new(HashMap::new()),
            max_cached_drawers,
            wal_size_threshold_bytes,
            wal_ops_threshold_count,
            durability_policy,
        })
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
            CompactRequest::Drawer { drawer_name, mode } => match mode {
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
            },
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
        backup::backup_archive(self, source_path)
    }

    pub fn restore(&self, destination_path: &str, archive: BackupArchive) -> Result<RestoreReport> {
        backup::restore_archive(self, destination_path, archive)
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
            CreateRequest::User { payload } => {
                access_control::manage_user(&self.root_directory, "add_user", payload)
                    .map(CreateResult::Admin)
            }
        }
    }

    pub fn drop<D>(&self, request: D) -> Result<Value>
    where
        D: Into<DropRequest>,
    {
        match request.into() {
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
            DropRequest::User { username } => access_control::manage_user(
                &self.root_directory,
                "drop_user",
                serde_json::json!({ "username": username }),
            ),
        }
    }

    pub fn grant(&self, request: PermissionRequest) -> Result<Value> {
        access_control::manage_user(
            &self.root_directory,
            "grant_permission",
            request.into_payload(),
        )
    }

    pub fn revoke(&self, request: PermissionRequest) -> Result<Value> {
        access_control::manage_user(
            &self.root_directory,
            "revoke_permission",
            request.into_payload(),
        )
    }

    pub fn status<S>(&self, request: S) -> Result<StatusResult>
    where
        S: Into<StatusRequest>,
    {
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
        diagnostics::inspect_drawer(self, drawer_name)
    }

    pub(crate) fn check_path(&self, raw_path: &str) -> Result<CheckReport> {
        diagnostics::check_path(self, raw_path)
    }

    pub(crate) fn diagnose_storage(&self) -> Result<StorageDiagnosis> {
        diagnostics::diagnose_storage(self)
    }

    pub(crate) fn list_drawer_names(&self) -> Result<Vec<String>> {
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
        access_control::manage_user(&self.root_directory, action, payload)
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
