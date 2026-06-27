use crate::wrdb_lib::command::{
    BackupArchive, CheckReport, Command, CommandResult, DrawerInspectionMetrics, RestoreReport,
    StorageDiagnosis,
};
use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::drawer::VacuumReport;
use crate::wrdb_lib::pointer;
use crate::wrdb_lib::query::QueryModifiers;
use crate::wrdb_lib::registry::CatalogRegistry;
use crate::wrdb_lib::routing::ExecutionContext;
use crate::wrdb_lib::storage::{StorageCoordinate, StorageInventory, StorageLocator, StorageScope};
use crate::wrdb_lib::wal::WalVerification;
use serde_json::Value;
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
    ) -> Result<Vec<Value>>;

    fn find_by_id_in_database(
        database: &RwLock<Database>,
        pointer: &str,
        context: ExecutionContext<'_>,
    ) -> Result<Option<Value>>;

    fn find_by_filter_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        modifiers: Option<QueryModifiers>,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<Value>>;

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
    if !matches!(
        &command,
        Command::DefineDatabase { .. }
            | Command::DefineSchema { .. }
            | Command::DefineDrawer { .. }
            | Command::DefineTenantRoute { .. }
            | Command::DropDatabase { .. }
            | Command::DropSchema { .. }
            | Command::DropDrawer { .. }
            | Command::ManageUser { .. }
            | Command::Inspect { .. }
            | Command::Check { .. }
            | Command::Diagnose
            | Command::ListDrawers
            | Command::Backup { .. }
            | Command::Restore { .. }
    ) {
        engine.append_boundary_wal(&command)?;
    }

    match command {
        Command::ShowTenants => engine.show_tenants().map(CommandResult::Tenants),
        Command::ShowDatabases => engine.show_databases().map(CommandResult::Databases),
        Command::VerifyWal { database_name } => engine
            .verify_wal(database_name.as_deref())
            .map(CommandResult::WalVerification),
        Command::ShowSchemas { database_name } => engine
            .show_schemas(&database_name)
            .map(CommandResult::Schemas),
        Command::ShowDrawers {
            database_name,
            schema_name,
        } => engine
            .show_drawers(&database_name, &schema_name)
            .map(CommandResult::Drawers),
        Command::DefineDatabase { database_name } => engine
            .create_database(&database_name)
            .map(CommandResult::StorageInventory),
        Command::DefineSchema {
            database_name,
            schema_name,
        } => engine
            .create_schema(&database_name, &schema_name)
            .map(CommandResult::StorageInventory),
        Command::DefineDrawer {
            database_name,
            schema_name,
            drawer_name,
        } => engine
            .create_drawer(&database_name, &schema_name, &drawer_name)
            .map(CommandResult::StorageInventory),
        Command::DefineTenantRoute {
            tenant_id,
            database_name,
            location,
        } => engine
            .register_tenant_route(&tenant_id, &database_name, &location)
            .map(CommandResult::StorageInventory),
        Command::DropDatabase { database_name } => engine
            .drop_database(&database_name)
            .map(CommandResult::Admin),
        Command::DropSchema {
            database_name,
            schema_name,
        } => engine
            .drop_schema(&database_name, &schema_name)
            .map(CommandResult::Admin),
        Command::DropDrawer {
            database_name,
            schema_name,
            drawer_name,
        } => engine
            .drop_drawer(&database_name, &schema_name, &drawer_name)
            .map(CommandResult::Admin),
        Command::Inspect { drawer_name } => engine
            .inspect_drawer(&drawer_name)
            .map(CommandResult::Inspection),
        Command::Check { path } => engine.check_path(&path).map(CommandResult::Check),
        Command::Diagnose => engine.diagnose_storage().map(CommandResult::Diagnosis),
        Command::ListDrawers => engine.list_drawer_names().map(CommandResult::DrawerNames),
        Command::Backup { source_path } => engine
            .backup_archive(&source_path)
            .map(CommandResult::Backup),
        Command::Restore {
            destination_path,
            archive,
        } => engine
            .restore_archive(&destination_path, archive)
            .map(CommandResult::Restored),
        Command::ManageUser { action, payload } => engine
            .manage_user(&action, payload)
            .map(CommandResult::Admin),
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
        Command::ShowTenants => Err(Error::new(
            ErrorKind::InvalidInput,
            "Tenant discovery is only available at the WardrobeEngine boundary",
        )),
        Command::ShowDatabases => Err(Error::new(
            ErrorKind::InvalidInput,
            "Database discovery is only available at the WardrobeEngine boundary",
        )),
        Command::VerifyWal { .. } => Err(Error::new(
            ErrorKind::InvalidInput,
            "WAL verification is only available at the WardrobeEngine boundary",
        )),
        Command::ShowSchemas { .. } => Err(Error::new(
            ErrorKind::InvalidInput,
            "Schema discovery is only available at the WardrobeEngine boundary",
        )),
        Command::ShowDrawers { .. } => Err(Error::new(
            ErrorKind::InvalidInput,
            "Drawer discovery is only available at the WardrobeEngine boundary",
        )),
        Command::Upsert {
            drawer_name,
            payload,
        } => match payload {
            Value::Array(records) => {
                E::bulk_upsert_in_database(database, &drawer_name, records, true, context)
                    .map(CommandResult::Pointers)
            }
            payload => E::upsert_in_database(database, &drawer_name, payload, context)
                .map(CommandResult::Pointer),
        },
        Command::BulkUpsert {
            drawer_name,
            records,
            atomic,
        } => E::bulk_upsert_in_database(database, &drawer_name, records, atomic, context)
            .map(CommandResult::Pointers),
        Command::FindAll { drawer_name } => {
            E::find_all_in_database(database, &drawer_name, context).map(CommandResult::Records)
        }
        Command::FindById { pointer } => {
            E::find_by_id_in_database(database, &pointer, context).map(CommandResult::Record)
        }
        Command::FindByFilter {
            drawer_name,
            filter,
            modifiers,
        } => E::find_by_filter_in_database(database, &drawer_name, filter, modifiers, context)
            .map(CommandResult::Records),
        Command::Count {
            drawer_name,
            filter,
            modifiers,
        } => E::count_in_database(database, &drawer_name, filter, modifiers, context)
            .map(CommandResult::Count),
        Command::Delete { pointer } => {
            E::delete_by_id_in_database(database, StorageLocator::Inline(pointer), context)
                .map(CommandResult::Deleted)
        }
        Command::DeleteByFilter {
            drawer_name,
            filter,
        } => E::delete_by_filter_in_database(database, &drawer_name, filter, context)
            .map(CommandResult::Count),
        Command::ManageSchema {
            action,
            kind,
            drawer_name,
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
        )
        .map(CommandResult::Admin),
        Command::Vacuum { drawer_name } => {
            E::vacuum_drawer_in_database(database, &drawer_name, context)
                .map(CommandResult::Vacuumed)
        }
        Command::Migrate { drawer_name } => {
            E::migrate_drawer_in_database(database, &drawer_name, context)
                .map(CommandResult::Migrated)
        }
        Command::Inspect { .. }
        | Command::Check { .. }
        | Command::Diagnose
        | Command::ListDrawers
        | Command::Backup { .. }
        | Command::Restore { .. } => Err(Error::new(
            ErrorKind::InvalidInput,
            "Storage diagnostics and recovery commands are only available at the WardrobeEngine boundary",
        )),
        Command::DefineDatabase { .. }
        | Command::DefineSchema { .. }
        | Command::DefineDrawer { .. }
        | Command::DefineTenantRoute { .. }
        | Command::DropDatabase { .. }
        | Command::DropSchema { .. }
        | Command::DropDrawer { .. }
        | Command::ManageUser { .. }
        | Command::ExecuteForTenant { .. }
        | Command::Execute { .. }
        | Command::ExecuteInScope { .. } => Err(Error::new(
            ErrorKind::InvalidInput,
            "Catalog and scoped command routing is only available at the WardrobeEngine boundary",
        )),
    }
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
        Command::Upsert { drawer_name, .. }
        | Command::BulkUpsert { drawer_name, .. }
        | Command::FindAll { drawer_name }
        | Command::FindByFilter { drawer_name, .. }
        | Command::Count { drawer_name, .. }
        | Command::DeleteByFilter { drawer_name, .. }
        | Command::ManageSchema { drawer_name, .. }
        | Command::Vacuum { drawer_name }
        | Command::Migrate { drawer_name } => Some(drawer_name.clone()),
        Command::FindById { pointer } | Command::Delete { pointer } => {
            pointer::try_parse_pointer(pointer).map(|(drawer_name, _)| drawer_name)
        }
        Command::Execute { command, .. } | Command::ExecuteInScope { command, .. } => {
            command_drawer_name(command)
        }
        Command::DefineDatabase { .. }
        | Command::DefineSchema { .. }
        | Command::DefineDrawer { .. }
        | Command::DefineTenantRoute { .. }
        | Command::DropDatabase { .. }
        | Command::DropSchema { .. }
        | Command::DropDrawer { .. }
        | Command::ManageUser { .. }
        | Command::ExecuteForTenant { .. }
        | Command::ShowTenants
        | Command::ShowDatabases
        | Command::VerifyWal { .. }
        | Command::ShowSchemas { .. }
        | Command::ShowDrawers { .. }
        | Command::Inspect { .. }
        | Command::Check { .. }
        | Command::Diagnose
        | Command::ListDrawers
        | Command::Backup { .. }
        | Command::Restore { .. } => None,
    }
}

#[cfg(test)]
mod tests {
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
        ) -> Result<Vec<Value>> {
            Ok(vec![json!({
                "drawer": drawer_name,
                "scope": context.drawer_namespace.unwrap_or("root")
            })])
        }

        fn find_by_id_in_database(
            _database: &RwLock<Database>,
            pointer: &str,
            _context: ExecutionContext<'_>,
        ) -> Result<Option<Value>> {
            Ok(Some(json!({"pointer": pointer})))
        }

        fn find_by_filter_in_database(
            _database: &RwLock<Database>,
            drawer_name: &str,
            filter: Value,
            modifiers: Option<QueryModifiers>,
            _context: ExecutionContext<'_>,
        ) -> Result<Vec<Value>> {
            Ok(vec![json!({
                "drawer": drawer_name,
                "filter": filter,
                "has_modifiers": modifiers.is_some()
            })])
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

    #[test]
    fn execute_command_routes_boundary_commands_and_wal_policy() {
        let engine = FakeBoundary::default();
        let archive = backup_archive();

        assert_eq!(
            execute_command(&engine, Command::ShowTenants).unwrap(),
            CommandResult::Tenants(vec!["tenant_a".to_string()])
        );
        assert!(matches!(
            execute_command(&engine, Command::ShowDatabases).unwrap(),
            CommandResult::Databases(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::VerifyWal {
                    database_name: Some("db".to_string()),
                }
            )
            .unwrap(),
            CommandResult::WalVerification(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::ShowSchemas {
                    database_name: "db".to_string(),
                }
            )
            .unwrap(),
            CommandResult::Schemas(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::ShowDrawers {
                    database_name: "db".to_string(),
                    schema_name: "public".to_string(),
                }
            )
            .unwrap(),
            CommandResult::Drawers(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::DefineDatabase {
                    database_name: "db".to_string(),
                }
            )
            .unwrap(),
            CommandResult::StorageInventory(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::DefineSchema {
                    database_name: "db".to_string(),
                    schema_name: "public".to_string(),
                }
            )
            .unwrap(),
            CommandResult::StorageInventory(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::DefineDrawer {
                    database_name: "db".to_string(),
                    schema_name: "public".to_string(),
                    drawer_name: "gem".to_string(),
                }
            )
            .unwrap(),
            CommandResult::StorageInventory(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::DefineTenantRoute {
                    tenant_id: "tenant".to_string(),
                    database_name: "db".to_string(),
                    location: "tenant/db/public".to_string(),
                }
            )
            .unwrap(),
            CommandResult::StorageInventory(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::DropDatabase {
                    database_name: "db".to_string(),
                }
            )
            .unwrap(),
            CommandResult::Admin(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::DropSchema {
                    database_name: "db".to_string(),
                    schema_name: "public".to_string(),
                }
            )
            .unwrap(),
            CommandResult::Admin(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::DropDrawer {
                    database_name: "db".to_string(),
                    schema_name: "public".to_string(),
                    drawer_name: "gem".to_string(),
                }
            )
            .unwrap(),
            CommandResult::Admin(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Inspect {
                    drawer_name: "gem".to_string(),
                }
            )
            .unwrap(),
            CommandResult::Inspection(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Check {
                    path: "db/public/gem".to_string(),
                }
            )
            .unwrap(),
            CommandResult::Check(_)
        ));
        assert!(matches!(
            execute_command(&engine, Command::Diagnose).unwrap(),
            CommandResult::Diagnosis(_)
        ));
        assert!(matches!(
            execute_command(&engine, Command::ListDrawers).unwrap(),
            CommandResult::DrawerNames(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::Backup {
                    source_path: "source".to_string(),
                }
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
                }
            )
            .unwrap(),
            CommandResult::Restored(_)
        ));
        assert!(matches!(
            execute_command(
                &engine,
                Command::ManageUser {
                    action: "grant_permission".to_string(),
                    payload: json!({"username": "alice"}),
                }
            )
            .unwrap(),
            CommandResult::Admin(_)
        ));

        assert_eq!(engine.wal_count(), 5);
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
                    command: Box::new(Command::FindAll {
                        drawer_name: "gem".to_string(),
                    }),
                }
            )
            .unwrap(),
            CommandResult::Count(11)
        );
        assert_eq!(
            execute_command(
                &engine,
                Command::Execute {
                    coordinate: StorageCoordinate::new("tenant", "db", "public"),
                    command: Box::new(Command::Count {
                        drawer_name: "gem".to_string(),
                        filter: None,
                        modifiers: None,
                    }),
                }
            )
            .unwrap(),
            CommandResult::Count(22)
        );
        assert_eq!(
            execute_command(
                &engine,
                Command::ExecuteInScope {
                    scope: StorageScope::schema("db", "public"),
                    command: Box::new(Command::DeleteByFilter {
                        drawer_name: "gem".to_string(),
                        filter: json!({}),
                    }),
                }
            )
            .unwrap(),
            CommandResult::Count(33)
        );
        assert_eq!(
            execute_command(
                &engine,
                Command::FindAll {
                    drawer_name: "gem".to_string(),
                }
            )
            .unwrap(),
            CommandResult::Count(44)
        );

        assert_eq!(engine.wal_count(), 4);
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
                    drawer_name: "gem".to_string(),
                    payload: json!({"_id": "one"}),
                },
                Some("tenant/db/public"),
            )
            .unwrap(),
            CommandResult::Pointer("@gem:single-tenant/db/public".to_string())
        );
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Upsert {
                    drawer_name: "gem".to_string(),
                    payload: json!([{"_id": "one"}, {"_id": "two"}]),
                },
                None,
            )
            .unwrap(),
            CommandResult::Pointers(pointers) if pointers.len() == 2
        ));
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::BulkUpsert {
                    drawer_name: "gem".to_string(),
                    records: vec![json!({"_id": "one"})],
                    atomic: false,
                },
                None,
            )
            .unwrap(),
            CommandResult::Pointers(pointers) if pointers[0].contains("false")
        ));
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::FindAll {
                    drawer_name: "gem".to_string(),
                },
                Some("tenant/db/public"),
            )
            .unwrap(),
            CommandResult::Records(records) if records[0]["scope"] == "tenant/db/public"
        ));
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::FindById {
                    pointer: "@gem:one".to_string(),
                },
                None,
            )
            .unwrap(),
            CommandResult::Record(Some(record)) if record["pointer"] == "@gem:one"
        ));
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::FindByFilter {
                    drawer_name: "gem".to_string(),
                    filter: json!({"element": "Fire"}),
                    modifiers: Some(QueryModifiers::default()),
                },
                None,
            )
            .unwrap(),
            CommandResult::Records(records) if records[0]["has_modifiers"] == true
        ));
        assert_eq!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Count {
                    drawer_name: "gem".to_string(),
                    filter: Some(json!({})),
                    modifiers: Some(QueryModifiers::default()),
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
                    pointer: "@gem:one".to_string(),
                },
                None,
            )
            .unwrap(),
            CommandResult::Deleted(true)
        );
        assert_eq!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::DeleteByFilter {
                    drawer_name: "gem".to_string(),
                    filter: json!({}),
                },
                None,
            )
            .unwrap(),
            CommandResult::Count(3)
        );
        assert!(matches!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::ManageSchema {
                    action: "add".to_string(),
                    kind: "index".to_string(),
                    drawer_name: "gem".to_string(),
                    field_name: "element".to_string(),
                    payload: json!({"type": "hash"}),
                },
                None,
            )
            .unwrap(),
            CommandResult::Admin(payload) if payload["kind"] == "index"
        ));
        assert_eq!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Vacuum {
                    drawer_name: "gem".to_string(),
                },
                None,
            )
            .unwrap(),
            CommandResult::Vacuumed(vacuum_report())
        );
        assert_eq!(
            execute_in_database::<FakeDatabaseExecutor>(
                &database,
                Command::Migrate {
                    drawer_name: "gem".to_string(),
                },
                None,
            )
            .unwrap(),
            CommandResult::Migrated(vacuum_report())
        );

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn execute_in_database_rejects_boundary_only_commands() {
        let path = temp_path("boundary_rejections");
        let database = RwLock::new(Database::initialize(&path).expect("database should init"));
        let archive = backup_archive();
        let boundary_only_commands = vec![
            Command::ShowTenants,
            Command::ShowDatabases,
            Command::VerifyWal {
                database_name: None,
            },
            Command::ShowSchemas {
                database_name: "db".to_string(),
            },
            Command::ShowDrawers {
                database_name: "db".to_string(),
                schema_name: "public".to_string(),
            },
            Command::Inspect {
                drawer_name: "gem".to_string(),
            },
            Command::Check {
                path: "db/public/gem".to_string(),
            },
            Command::Diagnose,
            Command::ListDrawers,
            Command::Backup {
                source_path: "source".to_string(),
            },
            Command::Restore {
                destination_path: "destination".to_string(),
                archive,
            },
            Command::DefineDatabase {
                database_name: "db".to_string(),
            },
            Command::DefineSchema {
                database_name: "db".to_string(),
                schema_name: "public".to_string(),
            },
            Command::DefineDrawer {
                database_name: "db".to_string(),
                schema_name: "public".to_string(),
                drawer_name: "gem".to_string(),
            },
            Command::DefineTenantRoute {
                tenant_id: "tenant".to_string(),
                database_name: "db".to_string(),
                location: "tenant/db/public".to_string(),
            },
            Command::DropDatabase {
                database_name: "db".to_string(),
            },
            Command::DropSchema {
                database_name: "db".to_string(),
                schema_name: "public".to_string(),
            },
            Command::DropDrawer {
                database_name: "db".to_string(),
                schema_name: "public".to_string(),
                drawer_name: "gem".to_string(),
            },
            Command::ManageUser {
                action: "grant_permission".to_string(),
                payload: json!({"username": "alice"}),
            },
            Command::ExecuteForTenant {
                tenant_id: "tenant".to_string(),
                database_name: "db".to_string(),
                schema_name: "public".to_string(),
                command: Box::new(Command::FindAll {
                    drawer_name: "gem".to_string(),
                }),
            },
            Command::Execute {
                coordinate: StorageCoordinate::new("tenant", "db", "public"),
                command: Box::new(Command::FindAll {
                    drawer_name: "gem".to_string(),
                }),
            },
            Command::ExecuteInScope {
                scope: StorageScope::database("db"),
                command: Box::new(Command::FindAll {
                    drawer_name: "gem".to_string(),
                }),
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
            command_drawer_name(&Command::FindById {
                pointer: "@gem:one".to_string(),
            }),
            Some("gem".to_string())
        );
        assert_eq!(
            command_drawer_name(&Command::Delete {
                pointer: "not-a-pointer".to_string(),
            }),
            None
        );
        assert_eq!(
            command_drawer_name(&Command::ExecuteInScope {
                scope: StorageScope::drawer("namespace"),
                command: Box::new(Command::Vacuum {
                    drawer_name: "nested".to_string(),
                }),
            }),
            Some("nested".to_string())
        );
        assert_eq!(
            command_drawer_name(&Command::DefineDatabase {
                database_name: "db".to_string(),
            }),
            None
        );

        let empty_registry = CatalogRegistry::new();
        validate_command_against_registry(
            &empty_registry,
            "db",
            "public",
            &Command::FindAll {
                drawer_name: "anything".to_string(),
            },
        )
        .expect("empty registry should not restrict commands");

        let mut registry = CatalogRegistry::new();
        registry.register_drawer("db", "public", "gem", "db/public");
        validate_command_against_registry(
            &registry,
            "db",
            "public",
            &Command::FindAll {
                drawer_name: "gem".to_string(),
            },
        )
        .expect("registered drawer should pass");
        validate_command_against_registry(
            &registry,
            "db",
            "public",
            &Command::FindAll {
                drawer_name: "missing".to_string(),
            },
        )
        .expect_err("unregistered drawer should fail");
        validate_command_against_registry(&registry, "db", "public", &Command::ShowTenants)
            .expect("commands without drawer names should pass");
    }
}
