use super::{
    AlterRequest, BackupArchive, CheckReport, Command, CommandResult, CompactMode, CompactRequest,
    CreateRequest, CreateResult, DeleteResult, DrawerInspectionMetrics, DropRequest, InspectResult,
    OperationFilter, OperationOptions, PaginatedReadResult, ReadResult, RestoreReport,
    StatusRequest, StorageDiagnosis, UpsertResult,
};
use crate::engine::{
    OperationSelection, ReturnShapeResolution, UpsertContext, merge_payload_into_record,
    record_pointer, resolve_read_shape, upsert_record_target,
};
use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::drawer::{VacuumReport, hydration};
use crate::wrdb_lib::pointer;
use crate::wrdb_lib::query::{QueryModifiers, QueryRecords};
use crate::wrdb_lib::registry::CatalogRegistry;
use crate::wrdb_lib::routing::ExecutionContext;
use crate::wrdb_lib::storage::{StorageCoordinate, StorageInventory, StorageLocator, StorageScope};
use crate::wrdb_lib::wal::WalVerification;
use serde_json::Value;
use std::collections::HashSet;
use std::io::{Error, ErrorKind, Result};
use std::sync::RwLock;

pub(crate) trait BoundaryCommandExecutor {
    fn append_boundary_wal(&self, command: &Command) -> Result<()>;
    fn show_tenants(&self) -> Result<Vec<String>>;
    fn show_databases(&self) -> Result<Vec<StorageInventory>>;
    fn verify_wal(&self, database_name: Option<&str>) -> Result<WalVerification>;
    fn show_schemas(&self, database_name: &str) -> Result<Vec<String>>;
    fn show_drawers(&self, database_name: &str, schema_name: &str)
    -> Result<Vec<StorageInventory>>;
    fn create_database(&self, database_name: &str) -> Result<StorageInventory>;
    fn create_schema(&self, database_name: &str, schema_name: &str) -> Result<StorageInventory>;
    fn create_drawer(
        &self,
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> Result<StorageInventory>;
    fn register_tenant_route(
        &self,
        tenant_id: &str,
        database_name: &str,
        location: &str,
    ) -> Result<StorageInventory>;
    fn drop_database(&self, database_name: &str) -> Result<Value>;
    fn drop_schema(&self, database_name: &str, schema_name: &str) -> Result<Value>;
    fn drop_drawer(
        &self,
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> Result<Value>;
    fn inspect_drawer(&self, drawer_name: &str) -> Result<DrawerInspectionMetrics>;
    fn check_path(&self, path: &str) -> Result<CheckReport>;
    fn diagnose_storage(&self) -> Result<StorageDiagnosis>;
    fn list_drawer_names(&self) -> Result<Vec<String>>;
    fn cached_drawer_count(&self) -> Result<usize>;
    fn backup_archive(&self, source_path: &str) -> Result<BackupArchive>;
    fn restore_archive(
        &self,
        destination_path: &str,
        archive: BackupArchive,
    ) -> Result<RestoreReport>;
    fn manage_user(&self, action: &str, payload: Value) -> Result<Value>;
    fn execute_for_tenant(
        &self,
        tenant_id: &str,
        database_name: &str,
        schema_name: &str,
        command: Command,
    ) -> Result<CommandResult>;
    fn execute(&self, coordinate: StorageCoordinate, command: Command) -> Result<CommandResult>;
    fn execute_in_scope(&self, scope: StorageScope, command: Command) -> Result<CommandResult>;
    fn execute_local(&self, command: Command) -> Result<CommandResult>;
}

pub(crate) trait DatabaseCommandExecutor {
    fn upsert_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<String>;

    fn bulk_upsert_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        records: Vec<Value>,
        atomic: bool,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<String>>;

    fn find_all_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
        options: &OperationOptions,
        hydrate: bool,
    ) -> Result<Vec<Value>>;

    fn find_by_id_in_database(
        database: &RwLock<Database>,
        pointer: &str,
        context: ExecutionContext<'_>,
        options: &OperationOptions,
        hydrate: bool,
    ) -> Result<Option<Value>>;

    fn find_by_filter_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        modifiers: Option<QueryModifiers>,
        context: ExecutionContext<'_>,
        options: &OperationOptions,
        hydrate: bool,
    ) -> Result<QueryRecords>;

    fn count_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        filter: Option<Value>,
        modifiers: Option<QueryModifiers>,
        context: ExecutionContext<'_>,
    ) -> Result<usize>;

    fn delete_by_id_in_database(
        database: &RwLock<Database>,
        locator: StorageLocator,
        context: ExecutionContext<'_>,
    ) -> Result<bool>;

    fn delete_by_filter_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        context: ExecutionContext<'_>,
    ) -> Result<usize>;

    fn manage_schema_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        action: &str,
        kind: &str,
        field_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<Value>;

    fn vacuum_drawer_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
    ) -> Result<VacuumReport>;

    fn migrate_drawer_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
    ) -> Result<VacuumReport>;
}

pub(crate) fn execute_command<E>(engine: &E, command: Command) -> Result<CommandResult>
where
    E: BoundaryCommandExecutor,
{
    if should_append_boundary_wal(&command) {
        engine.append_boundary_wal(&command)?;
    }

    match command {
        Command::Create(request) => execute_create_command(engine, request),
        Command::Drop(request) => execute_drop_command(engine, request),
        Command::Status(request) => execute_status_command(engine, request),
        Command::Inspect { filter, options } => {
            let selection = OperationSelection::from_filter(filter)?;
            let drawer_name = selection.required_drawer("inspect")?;
            let _ = options;
            engine
                .inspect_drawer(&drawer_name)
                .map(InspectResult::Drawer)
                .map(CommandResult::Inspect)
        }
        Command::Backup { source_path } => engine
            .backup_archive(&source_path)
            .map(CommandResult::Backup),
        Command::Restore {
            destination_path,
            archive,
        } => engine
            .restore_archive(&destination_path, archive)
            .map(CommandResult::Restore),
        Command::Grant(request) => engine
            .manage_user("grant_permission", request.into_payload())
            .map(CommandResult::Grant),
        Command::Revoke(request) => engine
            .manage_user("revoke_permission", request.into_payload())
            .map(CommandResult::Revoke),
        Command::ExecuteForTenant {
            tenant_id,
            database_name,
            schema_name,
            command,
        } => engine.execute_for_tenant(&tenant_id, &database_name, &schema_name, *command),
        Command::Execute {
            coordinate,
            command,
        } => engine.execute(coordinate, *command),
        Command::ExecuteInScope { scope, command } => engine.execute_in_scope(scope, *command),
        command => engine.execute_local(command),
    }
}

fn should_append_boundary_wal(command: &Command) -> bool {
    matches!(
        command,
        Command::Upsert { .. }
            | Command::Delete { .. }
            | Command::Compact(_)
            | Command::Alter(_)
            | Command::Drop(DropRequest::SchemaRule { .. })
    )
}

fn execute_create_command<E>(engine: &E, request: CreateRequest) -> Result<CommandResult>
where
    E: BoundaryCommandExecutor,
{
    match request {
        CreateRequest::Database { database_name } => engine
            .create_database(&database_name)
            .map(CreateResult::StorageInventory)
            .map(CommandResult::Create),
        CreateRequest::Schema {
            database_name,
            schema_name,
        } => engine
            .create_schema(&database_name, &schema_name)
            .map(CreateResult::StorageInventory)
            .map(CommandResult::Create),
        CreateRequest::Drawer {
            database_name,
            schema_name,
            drawer_name,
        } => engine
            .create_drawer(&database_name, &schema_name, &drawer_name)
            .map(CreateResult::StorageInventory)
            .map(CommandResult::Create),
        CreateRequest::TenantRoute {
            tenant_id,
            database_name,
            location,
        } => engine
            .register_tenant_route(&tenant_id, &database_name, &location)
            .map(CreateResult::StorageInventory)
            .map(CommandResult::Create),
        CreateRequest::User { payload } => engine
            .manage_user("add_user", payload)
            .map(CreateResult::Admin)
            .map(CommandResult::Create),
    }
}

fn execute_drop_command<E>(engine: &E, request: DropRequest) -> Result<CommandResult>
where
    E: BoundaryCommandExecutor,
{
    match request {
        DropRequest::Database { database_name } => engine
            .drop_database(&database_name)
            .map(CommandResult::Drop),
        DropRequest::Schema {
            database_name,
            schema_name,
        } => engine
            .drop_schema(&database_name, &schema_name)
            .map(CommandResult::Drop),
        DropRequest::Drawer {
            database_name,
            schema_name,
            drawer_name,
        } => engine
            .drop_drawer(&database_name, &schema_name, &drawer_name)
            .map(CommandResult::Drop),
        DropRequest::User { username } => engine
            .manage_user("drop_user", serde_json::json!({ "username": username }))
            .map(CommandResult::Drop),
        request @ DropRequest::SchemaRule { .. } => engine.execute_local(Command::Drop(request)),
    }
}

fn execute_status_command<E>(engine: &E, request: StatusRequest) -> Result<CommandResult>
where
    E: BoundaryCommandExecutor,
{
    match request {
        StatusRequest::Tenants => engine
            .show_tenants()
            .and_then(super::encode_status_payload)
            .map(CommandResult::Status),
        StatusRequest::Databases => engine
            .show_databases()
            .and_then(super::encode_status_payload)
            .map(CommandResult::Status),
        StatusRequest::Schemas { database_name } => engine
            .show_schemas(&database_name)
            .and_then(super::encode_status_payload)
            .map(CommandResult::Status),
        StatusRequest::Drawers {
            database_name,
            schema_name,
        } => engine
            .show_drawers(&database_name, &schema_name)
            .and_then(super::encode_status_payload)
            .map(CommandResult::Status),
        StatusRequest::Wal { database_name } => engine
            .verify_wal(database_name.as_deref())
            .and_then(super::encode_status_payload)
            .map(CommandResult::Status),
        StatusRequest::Storage => engine
            .diagnose_storage()
            .and_then(super::encode_status_payload)
            .map(CommandResult::Status),
        StatusRequest::Path { path } => engine
            .check_path(&path)
            .and_then(super::encode_status_payload)
            .map(CommandResult::Status),
        StatusRequest::DrawerNames => engine
            .list_drawer_names()
            .and_then(super::encode_status_payload)
            .map(CommandResult::Status),
        StatusRequest::CachedDrawerCount => engine
            .cached_drawer_count()
            .and_then(super::encode_status_payload)
            .map(CommandResult::Status),
    }
}

pub(crate) fn execute_in_database<E>(
    database: &RwLock<Database>,
    command: Command,
    drawer_namespace: Option<&str>,
) -> Result<CommandResult>
where
    E: DatabaseCommandExecutor,
{
    let context = ExecutionContext { drawer_namespace };

    match command {
        Command::Upsert {
            payload,
            filter,
            options,
        } => execute_upsert_in_database::<E>(database, payload, filter, options, context),
        Command::Read { filter, options } => {
            execute_read_in_database::<E>(database, filter, options, context)
        }
        Command::Count { filter, options } => {
            execute_count_in_database::<E>(database, filter, options, context)
        }
        Command::Delete { filter, options } => {
            execute_delete_in_database::<E>(database, filter, options, context)
        }
        Command::Compact(request) => execute_compact_in_database::<E>(database, request, context),
        Command::Alter(request) => execute_alter_in_database::<E>(database, request, context),
        Command::Drop(DropRequest::SchemaRule {
            drawer_name,
            kind,
            field_name,
            payload,
        }) => E::manage_schema_in_database(
            database,
            &drawer_name,
            "remove",
            &kind,
            &field_name,
            payload,
            context,
        )
        .map(CommandResult::Drop),
        Command::Inspect { .. }
        | Command::Backup { .. }
        | Command::Restore { .. }
        | Command::Create(_)
        | Command::Drop(_)
        | Command::Grant(_)
        | Command::Revoke(_)
        | Command::Status(_)
        | Command::ExecuteForTenant { .. }
        | Command::Execute { .. }
        | Command::ExecuteInScope { .. } => Err(Error::new(
            ErrorKind::InvalidInput,
            "Catalog, status, diagnostics, recovery, and scoped command routing is only available at the WardrobeEngine boundary",
        )),
    }
}

fn execute_upsert_in_database<E>(
    database: &RwLock<Database>,
    payload: Value,
    filter: OperationFilter,
    options: OperationOptions,
    context: ExecutionContext<'_>,
) -> Result<CommandResult>
where
    E: DatabaseCommandExecutor,
{
    upsert_payload_in_database::<E>(
        database,
        payload,
        UpsertContext::from_filter(filter)?,
        options,
        context,
    )
    .map(UpsertResult::Pointers)
    .map(CommandResult::Upsert)
}

fn upsert_payload_in_database<E>(
    database: &RwLock<Database>,
    payload: Value,
    context_filter: UpsertContext,
    options: OperationOptions,
    context: ExecutionContext<'_>,
) -> Result<Vec<String>>
where
    E: DatabaseCommandExecutor,
{
    if context_filter.query.is_some() {
        return upsert_by_query_in_database::<E>(
            database,
            payload,
            context_filter,
            options,
            context,
        );
    }

    match payload {
        Value::Array(records)
            if context_filter.pointer.is_none()
                && context_filter.query.is_none()
                && context_filter.drawer_name().is_some() =>
        {
            let drawer_name = context_filter.drawer_name().unwrap();
            E::bulk_upsert_in_database(
                database,
                drawer_name,
                records,
                options.atomic_enabled(),
                context,
            )
        }
        Value::Array(records) => {
            let mut grouped_records = std::collections::HashMap::<String, Vec<Value>>::new();
            for record in records {
                let (drawer_name, record) = upsert_record_target(record, &context_filter)?;
                grouped_records.entry(drawer_name).or_default().push(record);
            }
            let mut pointers = Vec::new();
            for (drawer_name, records) in grouped_records {
                pointers.extend(E::bulk_upsert_in_database(
                    database,
                    &drawer_name,
                    records,
                    options.atomic_enabled(),
                    context,
                )?);
            }
            Ok(pointers)
        }
        payload => {
            let (drawer_name, payload) = upsert_record_target(payload, &context_filter)?;
            E::upsert_in_database(database, &drawer_name, payload, context)
                .map(|pointer| vec![pointer])
        }
    }
}

fn upsert_by_query_in_database<E>(
    database: &RwLock<Database>,
    payload: Value,
    context_filter: UpsertContext,
    options: OperationOptions,
    context: ExecutionContext<'_>,
) -> Result<Vec<String>>
where
    E: DatabaseCommandExecutor,
{
    let drawer_name = context_filter.drawer_name().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "query upsert requires a drawer filter",
        )
    })?;
    let query = context_filter.query.clone().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "query upsert requires a query filter",
        )
    })?;
    let matched_records =
        E::find_by_filter_in_database(database, drawer_name, query, None, context, &options, false)?.records;
    if matched_records.is_empty() {
        if options.create_if_missing == Some(false) {
            return Ok(Vec::new());
        }
        return upsert_payload_in_database::<E>(
            database,
            payload,
            UpsertContext::from_filter(OperationFilter::drawer(drawer_name))?,
            options,
            context,
        );
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
        pointers.extend(upsert_payload_in_database::<E>(
            database,
            existing_record,
            UpsertContext::from_filter(OperationFilter::pointer(pointer))?,
            options.clone(),
            context,
        )?);
    }
    Ok(pointers)
}

fn execute_read_in_database<E>(
    database: &RwLock<Database>,
    filter: OperationFilter,
    options: OperationOptions,
    context: ExecutionContext<'_>,
) -> Result<CommandResult>
where
    E: DatabaseCommandExecutor,
{
    let selection = OperationSelection::from_filter(filter)?;
    let hydrate = options.hydrate.unwrap_or(true);
    let query_records = if !selection.pointers.is_empty() {
        let mut records = Vec::new();
        for pointer in selection.resolved_pointers()? {
            if let Some(record) = E::find_by_id_in_database(database, &pointer, context, &options, hydrate)? {
                records.push(record);
            }
        }
        let pagination = crate::wrdb_lib::query::apply_query_modifiers(
            &mut records,
            options.query_modifiers().as_ref(),
        )?;
        QueryRecords {
            records,
            pagination,
        }
    } else {
        let drawer_name = selection.required_drawer("read")?;
        if let Some(query) = selection.query.clone() {
            E::find_by_filter_in_database(
                database,
                &drawer_name,
                query,
                options.query_modifiers(),
                context,
                &options,
                hydrate,
            )?
        } else {
            let mut records = E::find_all_in_database(database, &drawer_name, context, &options, hydrate)?;
            let pagination = crate::wrdb_lib::query::apply_query_modifiers(
                &mut records,
                options.query_modifiers().as_ref(),
            )?;
            QueryRecords {
                records,
                pagination,
            }
        }
    };

    let mut records = query_records.records;
    let pagination = query_records.pagination;

    let read_shape = resolve_read_shape(options.return_shape, &selection);
    if pagination.is_some() && !matches!(&read_shape, ReturnShapeResolution::Records) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "cursor and page pagination require the records return shape",
        ));
    }
    if hydrate && selection.pointers.is_empty() {
        match read_shape {
            ReturnShapeResolution::Record | ReturnShapeResolution::Records => {
                hydrate_read_records_in_database::<E>(database, &mut records, &options, context)?;
            }
            ReturnShapeResolution::Pointers | ReturnShapeResolution::Exists => {}
        }
    }

    let result = match read_shape {
        ReturnShapeResolution::Record => ReadResult::Record(records.into_iter().next()),
        ReturnShapeResolution::Records => match pagination {
            Some(pagination) => ReadResult::Page(PaginatedReadResult {
                records,
                pagination,
            }),
            None => ReadResult::Records(records),
        },
        ReturnShapeResolution::Pointers => ReadResult::Pointers(
            records
                .iter()
                .filter_map(|record| record_pointer(record, selection.drawer_name.as_deref()))
                .collect(),
        ),
        ReturnShapeResolution::Exists => ReadResult::Exists(!records.is_empty()),
    };
    Ok(CommandResult::Read(result))
}

fn hydrate_read_records_in_database<E>(
    database: &RwLock<Database>,
    records: &mut [Value],
    options: &OperationOptions,
    context: ExecutionContext<'_>,
) -> Result<()>
where
    E: DatabaseCommandExecutor,
{
    let excluded_set: HashSet<String> = options
        .exclude_hydration
        .as_ref()
        .map(|list| list.iter().cloned().collect())
        .unwrap_or_default();
    let mut cache = hydration::HydrationCache::default();
    hydration::hydrate_records_with_cache(
        records,
        true,
        &excluded_set,
        &mut cache,
        |drawer_name, record_key| {
            let pointer = pointer::format_pointer(drawer_name, record_key);
            E::find_by_id_in_database(database, &pointer, context, options, false)
        },
    )
}

fn execute_count_in_database<E>(
    database: &RwLock<Database>,
    filter: OperationFilter,
    options: OperationOptions,
    context: ExecutionContext<'_>,
) -> Result<CommandResult>
where
    E: DatabaseCommandExecutor,
{
    let selection = OperationSelection::from_filter(filter)?;
    if !selection.pointers.is_empty() {
        let modifiers = options.query_modifiers();
        let mut records = Vec::new();
        for pointer in selection.resolved_pointers()? {
            if let Some(record) = E::find_by_id_in_database(database, &pointer, context, &options, false)? {
                records.push(record);
            }
        }
        crate::wrdb_lib::query::apply_query_modifiers(&mut records, modifiers.as_ref())?;
        return Ok(CommandResult::Count(records.len()));
    }
    let drawer_name = selection.required_drawer("count")?;
    E::count_in_database(
        database,
        &drawer_name,
        selection.query,
        options.query_modifiers(),
        context,
    )
    .map(CommandResult::Count)
}

fn execute_delete_in_database<E>(
    database: &RwLock<Database>,
    filter: OperationFilter,
    options: OperationOptions,
    context: ExecutionContext<'_>,
) -> Result<CommandResult>
where
    E: DatabaseCommandExecutor,
{
    let _ = options;
    let selection = OperationSelection::from_filter(filter)?;
    if !selection.pointers.is_empty() {
        let mut deleted = 0;
        for pointer in selection.resolved_pointers()? {
            deleted += usize::from(E::delete_by_id_in_database(
                database,
                StorageLocator::Inline(pointer),
                context,
            )?);
        }
        return Ok(CommandResult::Delete(DeleteResult { deleted }));
    }
    let drawer_name = selection.required_drawer("delete-by-filter")?;
    let Some(query) = selection.query else {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "delete requires a record pointer or a drawer query filter",
        ));
    };
    E::delete_by_filter_in_database(database, &drawer_name, query, context)
        .map(|deleted| CommandResult::Delete(DeleteResult { deleted }))
}

fn execute_compact_in_database<E>(
    database: &RwLock<Database>,
    request: CompactRequest,
    context: ExecutionContext<'_>,
) -> Result<CommandResult>
where
    E: DatabaseCommandExecutor,
{
    match request {
        CompactRequest::Drawer { drawer_name, mode } => match mode {
            CompactMode::Vacuum => E::vacuum_drawer_in_database(database, &drawer_name, context),
            CompactMode::Migrate => E::migrate_drawer_in_database(database, &drawer_name, context),
        },
    }
    .map(CommandResult::Compact)
}

fn execute_alter_in_database<E>(
    database: &RwLock<Database>,
    request: AlterRequest,
    context: ExecutionContext<'_>,
) -> Result<CommandResult>
where
    E: DatabaseCommandExecutor,
{
    match request {
        AlterRequest::SchemaRule {
            drawer_name,
            action,
            kind,
            field_name,
            payload,
        } => E::manage_schema_in_database(
            database,
            &drawer_name,
            &action,
            &kind,
            &field_name,
            payload,
            context,
        ),
    }
    .map(CommandResult::Alter)
}

pub(crate) fn validate_command_against_registry(
    registry: &CatalogRegistry,
    database: &str,
    schema: &str,
    command: &Command,
) -> Result<()> {
    if registry.is_empty() {
        return Ok(());
    }

    let Some(drawer_name) = command_drawer_name(command) else {
        return Ok(());
    };

    if registry.contains_drawer(database, schema, &drawer_name) {
        return Ok(());
    }

    Err(Error::new(
        ErrorKind::NotFound,
        format!(
            "InvalidLocation: drawer '{}' is not registered for database '{}' schema '{}'",
            drawer_name, database, schema
        ),
    ))
}

pub(crate) fn command_drawer_name(command: &Command) -> Option<String> {
    match command {
        Command::Upsert {
            payload, filter, ..
        } => upsert_command_drawer_name(payload, filter),
        Command::Read { filter, .. }
        | Command::Delete { filter, .. }
        | Command::Inspect { filter, .. }
        | Command::Count { filter, .. } => selection_drawer_name(filter),
        Command::Compact(CompactRequest::Drawer { drawer_name, .. }) => Some(drawer_name.clone()),
        Command::Alter(AlterRequest::SchemaRule { drawer_name, .. })
        | Command::Drop(DropRequest::SchemaRule { drawer_name, .. }) => Some(drawer_name.clone()),
        Command::Execute { command, .. } | Command::ExecuteInScope { command, .. } => {
            command_drawer_name(command)
        }
        Command::Create(_)
        | Command::Drop(_)
        | Command::Grant(_)
        | Command::Revoke(_)
        | Command::ExecuteForTenant { .. }
        | Command::Status(_)
        | Command::Backup { .. }
        | Command::Restore { .. } => None,
    }
}

fn upsert_command_drawer_name(payload: &Value, filter: &OperationFilter) -> Option<String> {
    UpsertContext::from_filter(filter.clone())
        .ok()
        .and_then(|context| context.drawer_name().map(str::to_string))
        .or_else(|| payload_pointer_drawer_name(payload))
}

fn selection_drawer_name(filter: &OperationFilter) -> Option<String> {
    OperationSelection::from_filter(filter.clone())
        .ok()
        .and_then(|selection| selection.drawer_name)
}

fn payload_pointer_drawer_name(payload: &Value) -> Option<String> {
    match payload {
        Value::Object(object) => object
            .get("_id")
            .and_then(Value::as_str)
            .and_then(|pointer| pointer::try_parse_pointer(pointer).map(|(drawer, _)| drawer)),
        Value::Array(records) => records.iter().find_map(payload_pointer_drawer_name),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{PermissionRequest, ReturnShape};
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct FakeBoundary {
        calls: Mutex<Vec<String>>,
        wal_commands: Mutex<Vec<Command>>,
    }

    impl FakeBoundary {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn wal_count(&self) -> usize {
            self.wal_commands.lock().unwrap().len()
        }

        fn record(&self, label: impl Into<String>) {
            self.calls.lock().unwrap().push(label.into());
        }
    }

    fn inventory(name: &str) -> StorageInventory {
        StorageInventory {
            name: name.to_string(),
            record_count: 0,
            disk_size_bytes: 0,
            register_file_count: 0,
        }
    }

    fn vacuum_report() -> VacuumReport {
        VacuumReport {
            records_rewritten: 1,
            data_bytes_before: 10,
            data_bytes_after: 7,
            index_bytes_before: 4,
            index_bytes_after: 3,
            bytes_reclaimed: 4,
        }
    }

    fn backup_archive() -> BackupArchive {
        BackupArchive {
            format: "wardrobe-backup-v1".to_string(),
            source_path: "source".to_string(),
            scope: "directory".to_string(),
            files: vec![crate::BackupArchiveFile {
                path: "gem.drw".to_string(),
                bytes_hex: "00".to_string(),
            }],
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("wardrobe_command_dispatch_{name}_{nanos}"))
    }

    impl BoundaryCommandExecutor for FakeBoundary {
        fn append_boundary_wal(&self, command: &Command) -> Result<()> {
            self.wal_commands.lock().unwrap().push(command.clone());
            Ok(())
        }

        fn show_tenants(&self) -> Result<Vec<String>> {
            self.record("show_tenants");
            Ok(vec!["tenant_a".to_string()])
        }

        fn show_databases(&self) -> Result<Vec<StorageInventory>> {
            self.record("show_databases");
            Ok(vec![inventory("db")])
        }

        fn verify_wal(&self, database_name: Option<&str>) -> Result<WalVerification> {
            self.record(format!("verify_wal:{}", database_name.unwrap_or("all")));
            Ok(WalVerification {
                path: "wal".to_string(),
                entry_count: 1,
                last_sequence: Some(1),
            })
        }

        fn show_schemas(&self, database_name: &str) -> Result<Vec<String>> {
            self.record(format!("show_schemas:{database_name}"));
            Ok(vec!["public".to_string()])
        }

        fn show_drawers(
            &self,
            database_name: &str,
            schema_name: &str,
        ) -> Result<Vec<StorageInventory>> {
            self.record(format!("show_drawers:{database_name}/{schema_name}"));
            Ok(vec![inventory("gem")])
        }

        fn create_database(&self, database_name: &str) -> Result<StorageInventory> {
            self.record(format!("create_database:{database_name}"));
            Ok(inventory(database_name))
        }

        fn create_schema(
            &self,
            database_name: &str,
            schema_name: &str,
        ) -> Result<StorageInventory> {
            self.record(format!("create_schema:{database_name}/{schema_name}"));
            Ok(inventory(schema_name))
        }

        fn create_drawer(
            &self,
            database_name: &str,
            schema_name: &str,
            drawer_name: &str,
        ) -> Result<StorageInventory> {
            self.record(format!(
                "create_drawer:{database_name}/{schema_name}/{drawer_name}"
            ));
            Ok(inventory(drawer_name))
        }

        fn register_tenant_route(
            &self,
            tenant_id: &str,
            database_name: &str,
            location: &str,
        ) -> Result<StorageInventory> {
            self.record(format!(
                "tenant_route:{tenant_id}/{database_name}/{location}"
            ));
            Ok(inventory(database_name))
        }

        fn drop_database(&self, database_name: &str) -> Result<Value> {
            self.record(format!("drop_database:{database_name}"));
            Ok(json!({"dropped": database_name}))
        }

        fn drop_schema(&self, database_name: &str, schema_name: &str) -> Result<Value> {
            self.record(format!("drop_schema:{database_name}/{schema_name}"));
            Ok(json!({"dropped": schema_name}))
        }

        fn drop_drawer(
            &self,
            database_name: &str,
            schema_name: &str,
            drawer_name: &str,
        ) -> Result<Value> {
            self.record(format!(
                "drop_drawer:{database_name}/{schema_name}/{drawer_name}"
            ));
            Ok(json!({"dropped": drawer_name}))
        }

        fn inspect_drawer(&self, drawer_name: &str) -> Result<DrawerInspectionMetrics> {
            self.record(format!("inspect:{drawer_name}"));
            Ok(DrawerInspectionMetrics {
                path: drawer_name.to_string(),
                data_bytes: 1,
                index_bytes: 2,
                meta_bytes: 3,
                total_bytes: 6,
                record_count: 1,
                register_file_count: 3,
                tombstone_fragmentation_percent: Some(0.0),
            })
        }

        fn check_path(&self, path: &str) -> Result<CheckReport> {
            self.record(format!("check:{path}"));
            Ok(CheckReport {
                path: path.to_string(),
                kind: "drawer".to_string(),
                entries: Vec::new(),
            })
        }

        fn diagnose_storage(&self) -> Result<StorageDiagnosis> {
            self.record("diagnose");
            Ok(StorageDiagnosis {
                storage_directory: "root".to_string(),
                storage_bytes: 0,
                data_bytes: 0,
                index_bytes: 0,
                metadata_bytes: 0,
                logical_wal_bytes: 0,
                transaction_wal_bytes: 0,
                other_bytes: 0,
                drawer_count: 1,
                status: "ok".to_string(),
                drawers: vec!["gem".to_string()],
            })
        }

        fn list_drawer_names(&self) -> Result<Vec<String>> {
            self.record("list_drawers");
            Ok(vec!["gem".to_string()])
        }

        fn cached_drawer_count(&self) -> Result<usize> {
            self.record("cached_drawer_count");
            Ok(1)
        }

        fn backup_archive(&self, source_path: &str) -> Result<BackupArchive> {
            self.record(format!("backup:{source_path}"));
            Ok(backup_archive())
        }

        fn restore_archive(
            &self,
            destination_path: &str,
            _archive: BackupArchive,
        ) -> Result<RestoreReport> {
            self.record(format!("restore:{destination_path}"));
            Ok(RestoreReport {
                destination_path: destination_path.to_string(),
                scope: "directory".to_string(),
                file_count: 1,
                byte_count: 1,
            })
        }

        fn manage_user(&self, action: &str, payload: Value) -> Result<Value> {
            let username = payload
                .get("username")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            self.record(format!("manage_user:{action}:{username}"));
            Ok(json!({"ok": true, "action": action}))
        }

        fn execute_for_tenant(
            &self,
            tenant_id: &str,
            database_name: &str,
            schema_name: &str,
            _command: Command,
        ) -> Result<CommandResult> {
            self.record(format!(
                "execute_for_tenant:{tenant_id}/{database_name}/{schema_name}"
            ));
            Ok(CommandResult::Count(11))
        }

        fn execute(
            &self,
            coordinate: StorageCoordinate,
            _command: Command,
        ) -> Result<CommandResult> {
            self.record(format!(
                "execute:{}/{}/{}",
                coordinate.tenant(),
                coordinate.database(),
                coordinate.schema()
            ));
            Ok(CommandResult::Count(22))
        }

        fn execute_in_scope(
            &self,
            scope: StorageScope,
            _command: Command,
        ) -> Result<CommandResult> {
            self.record(format!("execute_in_scope:{scope:?}"));
            Ok(CommandResult::Count(33))
        }

        fn execute_local(&self, command: Command) -> Result<CommandResult> {
            self.record(format!("execute_local:{:?}", command_drawer_name(&command)));
            Ok(CommandResult::Count(44))
        }
    }

    struct FakeDatabaseExecutor;

    impl DatabaseCommandExecutor for FakeDatabaseExecutor {
        fn upsert_in_database(
            _database: &RwLock<Database>,
            drawer_name: &str,
            _payload: Value,
            context: ExecutionContext<'_>,
        ) -> Result<String> {
            Ok(format!(
                "@{drawer_name}:single-{}",
                context.drawer_namespace.unwrap_or("root")
            ))
        }

        fn bulk_upsert_in_database(
            _database: &RwLock<Database>,
            drawer_name: &str,
            records: Vec<Value>,
            atomic: bool,
            context: ExecutionContext<'_>,
        ) -> Result<Vec<String>> {
            Ok((0..records.len())
                .map(|index| {
                    format!(
                        "@{drawer_name}:bulk-{atomic}-{index}-{}",
                        context.drawer_namespace.unwrap_or("root")
                    )
                })
                .collect())
        }

        fn find_all_in_database(
            _database: &RwLock<Database>,
            drawer_name: &str,
            context: ExecutionContext<'_>,
            _options: &OperationOptions,
            _hydrate: bool,
        ) -> Result<Vec<Value>> {
            if drawer_name == "nispuk/default/plants" {
                return Ok(vec![json!({
                    "_id": "c21b0f6f-b6cb-4a34-a72b-c39568a7e0c5",
                    "plantType": "@nispuk/default/plant_types:fab3d886c9094b61bd6cbd1806daac0e",
                    "bed": "1"
                })]);
            }

            Ok(vec![json!({
                "drawer": drawer_name,
                "scope": context.drawer_namespace.unwrap_or("root")
            })])
        }

        fn find_by_id_in_database(
            _database: &RwLock<Database>,
            pointer: &str,
            _context: ExecutionContext<'_>,
            _options: &OperationOptions,
            _hydrate: bool,
        ) -> Result<Option<Value>> {
            if pointer == "@nispuk/default/plant_types:fab3d886c9094b61bd6cbd1806daac0e" {
                return Ok(Some(json!({
                    "_id": "fab3d886c9094b61bd6cbd1806daac0e",
                    "name": "Aloha Mix",
                    "category": "flower"
                })));
            }

            Ok(Some(json!({"pointer": pointer})))
        }

        fn find_by_filter_in_database(
            _database: &RwLock<Database>,
            drawer_name: &str,
            filter: Value,
            modifiers: Option<QueryModifiers>,
            _context: ExecutionContext<'_>,
            _options: &OperationOptions,
            _hydrate: bool,
        ) -> Result<QueryRecords> {
            Ok(QueryRecords {
                records: vec![json!({
                    "drawer": drawer_name,
                    "filter": filter,
                    "has_modifiers": modifiers.is_some()
                })],
                pagination: None,
            })
        }

        fn count_in_database(
            _database: &RwLock<Database>,
            _drawer_name: &str,
            filter: Option<Value>,
            modifiers: Option<QueryModifiers>,
            _context: ExecutionContext<'_>,
        ) -> Result<usize> {
            Ok(usize::from(filter.is_some()) + usize::from(modifiers.is_some()))
        }

        fn delete_by_id_in_database(
            _database: &RwLock<Database>,
            locator: StorageLocator,
            _context: ExecutionContext<'_>,
        ) -> Result<bool> {
            Ok(matches!(locator, StorageLocator::Inline(_)))
        }

        fn delete_by_filter_in_database(
            _database: &RwLock<Database>,
            _drawer_name: &str,
            _filter: Value,
            _context: ExecutionContext<'_>,
        ) -> Result<usize> {
            Ok(3)
        }

        fn manage_schema_in_database(
            _database: &RwLock<Database>,
            drawer_name: &str,
            action: &str,
            kind: &str,
            field_name: &str,
            payload: Value,
            _context: ExecutionContext<'_>,
        ) -> Result<Value> {
            Ok(json!({
                "drawer": drawer_name,
                "action": action,
                "kind": kind,
                "field": field_name,
                "payload": payload
            }))
        }

        fn vacuum_drawer_in_database(
            _database: &RwLock<Database>,
            _drawer_name: &str,
            _context: ExecutionContext<'_>,
        ) -> Result<VacuumReport> {
            Ok(vacuum_report())
        }

        fn migrate_drawer_in_database(
            _database: &RwLock<Database>,
            _drawer_name: &str,
            _context: ExecutionContext<'_>,
        ) -> Result<VacuumReport> {
            Ok(vacuum_report())
        }
    }

    fn read_drawer(drawer_name: &str) -> Command {
        Command::Read {
            filter: OperationFilter::drawer(drawer_name),
            options: OperationOptions::default(),
        }
    }

    fn count_drawer(drawer_name: &str) -> Command {
        Command::Count {
            filter: OperationFilter::drawer(drawer_name),
            options: OperationOptions::default(),
        }
    }

    #[test]
    fn execute_command_routes_boundary_commands_and_wal_policy() {
        let engine = FakeBoundary::default();
        let archive = backup_archive();

        assert_eq!(
            execute_command(
                &engine,
                Command::Status(StatusRequest::tenants().into_request())
            )
            .unwrap(),
            CommandResult::Status(json!(["tenant_a"]))
        );
        assert_eq!(
            execute_command(
                &engine,
                Command::Status(StatusRequest::databases().into_request())
            )
            .unwrap(),
            CommandResult::Status(json!([inventory("db")]))
        );
        assert!(matches!(
            execute_command(
                &engine,
                Command::Status(StatusRequest::wal(Some("db")).into_request())
            )
            .unwrap(),
            CommandResult::Status(Value::Object(_))
        ));
        assert_eq!(
            execute_command(
                &engine,
                Command::Status(StatusRequest::schemas("db").into_request())
            )
            .unwrap(),
            CommandResult::Status(json!(["public"]))
        );
        assert!(matches!(
            execute_command(
                &engine,
                Command::Status(StatusRequest::drawers("db", "public").into_request()),
            )
            .unwrap(),
            CommandResult::Status(Value::Array(_))
        ));
        assert!(matches!(
            execute_command(&engine, Command::Create(CreateRequest::database("db"))).unwrap(),
            CommandResult::Create(CreateResult::StorageInventory(_))
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Create(CreateRequest::schema("db", "public"))
            )
            .unwrap(),
            CommandResult::Create(CreateResult::StorageInventory(_))
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Create(CreateRequest::drawer("db", "public", "gem")),
            )
            .unwrap(),
            CommandResult::Create(CreateResult::StorageInventory(_))
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Create(CreateRequest::tenant_route(
                    "tenant",
                    "db",
                    "tenant/db/public",
                )),
            )
            .unwrap(),
            CommandResult::Create(CreateResult::StorageInventory(_))
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Create(CreateRequest::user(json!({"username": "alice"}))),
            )
            .unwrap(),
            CommandResult::Create(CreateResult::Admin(_))
        ));
        assert!(matches!(
            execute_command(&engine, Command::Drop(DropRequest::database("db"))).unwrap(),
            CommandResult::Drop(_)
        ));
        assert!(matches!(
            execute_command(&engine, Command::Drop(DropRequest::schema("db", "public"))).unwrap(),
            CommandResult::Drop(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Drop(DropRequest::drawer("db", "public", "gem")),
            )
            .unwrap(),
            CommandResult::Drop(_)
        ));
        assert!(matches!(
            execute_command(&engine, Command::Drop(DropRequest::user("alice"))).unwrap(),
            CommandResult::Drop(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Inspect {
                    filter: OperationFilter::drawer("gem"),
                    options: OperationOptions::default(),
                },
            )
            .unwrap(),
            CommandResult::Inspect(InspectResult::Drawer(_))
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Status(StatusRequest::path("db/public/gem").into_request())
            )
            .unwrap(),
            CommandResult::Status(Value::Object(_))
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Status(StatusRequest::storage().into_request())
            )
            .unwrap(),
            CommandResult::Status(Value::Object(_))
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Status(StatusRequest::drawer_names().into_request())
            )
            .unwrap(),
            CommandResult::Status(Value::Array(_))
        ));
        assert_eq!(
            execute_command(
                &engine,
                Command::Status(StatusRequest::cached_drawer_count().into_request())
            )
            .unwrap(),
            CommandResult::Status(json!(1))
        );
        assert!(matches!(
            execute_command(
                &engine,
                Command::Backup {
                    source_path: "source".to_string(),
                },
            )
            .unwrap(),
            CommandResult::Backup(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Restore {
                    destination_path: "destination".to_string(),
                    archive,
                },
            )
            .unwrap(),
            CommandResult::Restore(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Grant(PermissionRequest::new("alice", "db:rud")),
            )
            .unwrap(),
            CommandResult::Grant(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Revoke(PermissionRequest::new("alice", "db:rud")),
            )
            .unwrap(),
            CommandResult::Revoke(_)
        ));
        assert_eq!(
            execute_command(
                &engine,
                Command::Alter(AlterRequest::schema_rule(
                    "gem",
                    "add",
                    "index",
                    "element",
                    json!({}),
                )),
            )
            .unwrap(),
            CommandResult::Count(44)
        );

        assert_eq!(engine.wal_count(), 1);
        let calls = engine.calls();
        assert!(calls.contains(&"create_drawer:db/public/gem".to_string()));
        assert!(calls.contains(&"drop_drawer:db/public/gem".to_string()));
        assert!(calls.contains(&"manage_user:grant_permission:alice".to_string()));
    }

    #[test]
    fn execute_command_routes_scoped_and_local_commands_with_wal() {
        let engine = FakeBoundary::default();

        assert_eq!(
            execute_command(
                &engine,
                Command::ExecuteForTenant {
                    tenant_id: "tenant".to_string(),
                    database_name: "db".to_string(),
                    schema_name: "public".to_string(),
                    command: Box::new(read_drawer("gem")),
                },
            )
            .unwrap(),
            CommandResult::Count(11)
        );
        assert_eq!(
            execute_command(
                &engine,
                Command::Execute {
                    coordinate: StorageCoordinate::new("tenant", "db", "public"),
                    command: Box::new(count_drawer("gem")),
                },
            )
            .unwrap(),
            CommandResult::Count(22)
        );
        assert_eq!(
            execute_command(
                &engine,
                Command::ExecuteInScope {
                    scope: StorageScope::schema("db", "public"),
                    command: Box::new(Command::Delete {
                        filter: OperationFilter::query_in("gem", json!({})),
                        options: OperationOptions::default(),
                    }),
                },
            )
            .unwrap(),
            CommandResult::Count(33)
        );
        assert_eq!(
            execute_command(&engine, read_drawer("gem")).unwrap(),
            CommandResult::Count(44)
        );
        assert_eq!(
            execute_command(
                &engine,
                Command::Upsert {
                    payload: json!({"_id": "one"}),
                    filter: OperationFilter::drawer("gem"),
                    options: OperationOptions::default(),
                },
            )
            .unwrap(),
            CommandResult::Count(44)
        );

        assert_eq!(engine.wal_count(), 1);
        assert!(
            engine
                .calls()
                .contains(&"execute_for_tenant:tenant/db/public".to_string())
        );
    }

    #[test]
    fn execute_in_database_covers_local_data_commands() {
        let path = temp_path("local_data_commands");
        let database = RwLock::new(Database::initialize(&path).expect("database should init"));

        assert_eq!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Upsert {
                    payload: json!({"_id": "one"}),
                    filter: OperationFilter::drawer("gem"),
                    options: OperationOptions::default(),
                },
                Some("tenant/db/public"),
            )
            .unwrap(),
            CommandResult::Upsert(UpsertResult::Pointers(vec![
                "@gem:single-tenant/db/public".to_string(),
            ]))
        );
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Upsert {
                    payload: json!([{"_id": "one"}, {"_id": "two"}]),
                    filter: OperationFilter::drawer("gem"),
                    options: OperationOptions::default(),
                },
                None,
            )
            .unwrap(),
            CommandResult::Upsert(UpsertResult::Pointers(pointers)) if pointers.len() == 2
        ));
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Upsert {
                    payload: json!([{"_id": "one"}]),
                    filter: OperationFilter::drawer("gem"),
                    options: OperationOptions::new().atomic(false),
                },
                None,
            )
            .unwrap(),
            CommandResult::Upsert(UpsertResult::Pointers(pointers)) if pointers[0].contains("false")
        ));
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                read_drawer("gem"),
                Some("tenant/db/public"),
            )
            .unwrap(),
            CommandResult::Read(ReadResult::Records(records)) if records[0]["scope"] == "tenant/db/public"
        ));
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Read {
                    filter: OperationFilter::pointer("@gem:one"),
                    options: OperationOptions::new().return_shape(ReturnShape::Record),
                },
                None,
            )
            .unwrap(),
            CommandResult::Read(ReadResult::Record(Some(record))) if record["pointer"] == "@gem:one"
        ));
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Read {
                    filter: OperationFilter::query_in("gem", json!({"element": "Fire"})),
                    options: OperationOptions::from(QueryModifiers {
                        limit: Some(1),
                        ..QueryModifiers::default()
                    }),
                },
                None,
            )
            .unwrap(),
            CommandResult::Read(ReadResult::Records(records)) if records[0]["has_modifiers"] == true
        ));
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Read {
                    filter: OperationFilter::drawer("nispuk/default/plants"),
                    options: OperationOptions::default(),
                },
                None,
            )
            .unwrap(),
            CommandResult::Read(ReadResult::Records(records)) if records[0]["plantType"]["name"] == "Aloha Mix"
        ));
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Read {
                    filter: OperationFilter::drawer("nispuk/default/plants"),
                    options: OperationOptions::new().hydrate(false),
                },
                None,
            )
            .unwrap(),
            CommandResult::Read(ReadResult::Records(records)) if records[0]["plantType"] == "@nispuk/default/plant_types:fab3d886c9094b61bd6cbd1806daac0e"
        ));
        assert_eq!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Count {
                    filter: OperationFilter::query_in("gem", json!({"element": "Fire"})),
                    options: OperationOptions::from(QueryModifiers {
                        limit: Some(1),
                        ..QueryModifiers::default()
                    }),
                },
                None,
            )
            .unwrap(),
            CommandResult::Count(2)
        );
        assert_eq!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Delete {
                    filter: OperationFilter::pointer("@gem:one"),
                    options: OperationOptions::default(),
                },
                None,
            )
            .unwrap(),
            CommandResult::Delete(DeleteResult { deleted: 1 })
        );
        assert_eq!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Delete {
                    filter: OperationFilter::query_in("gem", json!({"element": "Fire"})),
                    options: OperationOptions::default(),
                },
                None,
            )
            .unwrap(),
            CommandResult::Delete(DeleteResult { deleted: 3 })
        );
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Alter(AlterRequest::schema_rule(
                    "gem",
                    "add",
                    "index",
                    "element",
                    json!({"type": "hash"}),
                )),
                None,
            )
            .unwrap(),
            CommandResult::Alter(payload) if payload["kind"] == "index"
        ));
        assert_eq!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Compact(CompactRequest::drawer("gem")),
                None,
            )
            .unwrap(),
            CommandResult::Compact(vacuum_report())
        );
        assert_eq!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Compact(CompactRequest::drawer_with_mode(
                    "gem",
                    CompactMode::Migrate,
                )),
                None,
            )
            .unwrap(),
            CommandResult::Compact(vacuum_report())
        );

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn execute_in_database_rejects_boundary_only_commands() {
        let path = temp_path("boundary_rejections");
        let database = RwLock::new(Database::initialize(&path).expect("database should init"));
        let archive = backup_archive();
        let boundary_only_commands = vec![
            Command::Inspect {
                filter: OperationFilter::drawer("gem"),
                options: OperationOptions::default(),
            },
            Command::Backup {
                source_path: "source".to_string(),
            },
            Command::Restore {
                destination_path: "destination".to_string(),
                archive,
            },
            Command::Create(CreateRequest::database("db")),
            Command::Drop(DropRequest::database("db")),
            Command::Grant(PermissionRequest::new("alice", "db:r")),
            Command::Revoke(PermissionRequest::new("alice", "db:r")),
            Command::Status(StatusRequest::databases().into_request()),
            Command::ExecuteForTenant {
                tenant_id: "tenant".to_string(),
                database_name: "db".to_string(),
                schema_name: "public".to_string(),
                command: Box::new(read_drawer("gem")),
            },
            Command::Execute {
                coordinate: StorageCoordinate::new("tenant", "db", "public"),
                command: Box::new(read_drawer("gem")),
            },
            Command::ExecuteInScope {
                scope: StorageScope::database("db"),
                command: Box::new(read_drawer("gem")),
            },
        ];

        for command in boundary_only_commands {
            let error =
                execute_in_database::<FakeDatabaseExecutor>(&database, command, None).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidInput);
        }

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn drawer_name_resolution_and_registry_validation_cover_nested_commands() {
        assert_eq!(
            command_drawer_name(&Command::Read {
                filter: OperationFilter::pointer("@gem:one"),
                options: OperationOptions::default(),
            }),
            Some("gem".to_string())
        );
        assert_eq!(
            command_drawer_name(&Command::Delete {
                filter: OperationFilter::pointer("not-a-pointer"),
                options: OperationOptions::default(),
            }),
            None
        );
        assert_eq!(
            command_drawer_name(&Command::ExecuteInScope {
                scope: StorageScope::drawer("namespace"),
                command: Box::new(Command::Compact(CompactRequest::drawer("nested"))),
            }),
            Some("nested".to_string())
        );
        assert_eq!(
            command_drawer_name(&Command::Create(CreateRequest::database("db"))),
            None
        );

        let empty_registry = CatalogRegistry::new();
        validate_command_against_registry(
            &empty_registry,
            "db",
            "public",
            &read_drawer("anything"),
        )
        .expect("empty registry should not restrict commands");

        let mut registry = CatalogRegistry::new();
        registry.register_drawer("db", "public", "gem", "db/public");
        validate_command_against_registry(&registry, "db", "public", &read_drawer("gem"))
            .expect("registered drawer should pass");
        validate_command_against_registry(&registry, "db", "public", &read_drawer("missing"))
            .expect_err("unregistered drawer should fail");
        validate_command_against_registry(
            &registry,
            "db",
            "public",
            &Command::Status(StatusRequest::tenants().into_request()),
        )
        .expect("commands without drawer names should pass");
    }
}
