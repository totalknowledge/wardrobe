use crate::wrdb_lib::command::{Command, CommandResult};
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
            | Command::ManageUser { .. }
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
        Command::ManageUser { .. } => Err(Error::new(
            ErrorKind::Unsupported,
            "user management must be handled by an authenticated Wardrobe server",
        )),
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
        } => E::upsert_in_database(database, &drawer_name, payload, context)
            .map(CommandResult::Pointer),
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
        Command::Vacuum { drawer_name } => {
            E::vacuum_drawer_in_database(database, &drawer_name, context)
                .map(CommandResult::Vacuumed)
        }
        Command::Migrate { drawer_name } => {
            E::migrate_drawer_in_database(database, &drawer_name, context)
                .map(CommandResult::Migrated)
        }
        Command::DefineDatabase { .. }
        | Command::DefineSchema { .. }
        | Command::DefineDrawer { .. }
        | Command::DefineTenantRoute { .. }
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
        | Command::FindAll { drawer_name }
        | Command::FindByFilter { drawer_name, .. }
        | Command::Count { drawer_name, .. }
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
        | Command::ManageUser { .. }
        | Command::ExecuteForTenant { .. }
        | Command::ShowTenants
        | Command::ShowDatabases
        | Command::VerifyWal { .. }
        | Command::ShowSchemas { .. }
        | Command::ShowDrawers { .. } => None,
    }
}
