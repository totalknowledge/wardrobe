use super::{
    BackupArchive, CheckReport, Command, CommandResult, DrawerInspectionMetrics, RestoreReport,
    StorageCoordinate, StorageDiagnosis, StorageInventory, StorageScope, WardrobeEngine,
};
use crate::wrdb_lib::catalog_lifecycle;
use crate::wrdb_lib::catalog_validation;
use crate::wrdb_lib::command_dispatch;
use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::routing::{self, DatabaseRoute};
use crate::wrdb_lib::wal::{self, WalVerification};
use serde_json::Value;
use std::io::{Error, ErrorKind, Result};
use std::sync::RwLock;

pub(super) fn show_tenants(engine: &WardrobeEngine) -> Result<Vec<String>> {
    let registry = WardrobeEngine::read_lock(&engine.registry)?;
    crate::wrdb_lib::discovery::show_tenants(&engine.root_directory, &registry)
}

pub(super) fn show_databases(engine: &WardrobeEngine) -> Result<Vec<StorageInventory>> {
    let registry = WardrobeEngine::read_lock(&engine.registry)?;
    crate::wrdb_lib::discovery::show_databases(&engine.root_directory, &registry)
}

pub(super) fn verify_wal(
    engine: &WardrobeEngine,
    database_name: Option<&str>,
) -> Result<WalVerification> {
    wal::verify(&engine.root_directory, database_name)
}

pub(super) fn show_schemas(engine: &WardrobeEngine, database_name: &str) -> Result<Vec<String>> {
    let registry = WardrobeEngine::read_lock(&engine.registry)?;
    crate::wrdb_lib::discovery::show_schemas(&engine.root_directory, &registry, database_name)
}

pub(super) fn show_drawers(
    engine: &WardrobeEngine,
    database_name: &str,
    schema_name: &str,
) -> Result<Vec<StorageInventory>> {
    let registry = WardrobeEngine::read_lock(&engine.registry)?;
    crate::wrdb_lib::discovery::show_drawers(
        &engine.root_directory,
        &registry,
        database_name,
        schema_name,
    )
}

pub(super) fn execute(
    engine: &WardrobeEngine,
    coordinate: StorageCoordinate,
    command: Command,
) -> Result<CommandResult> {
    let registry = WardrobeEngine::read_lock(&engine.registry)?;
    command_dispatch::validate_command_against_registry(
        &registry,
        &routing::coordinate_catalog_database(&coordinate),
        coordinate.schema(),
        &command,
    )?;
    let database_path = routing::coordinate_database_path(&engine.root_directory, &coordinate)?;
    wal::append_command(
        &database_path,
        Some(coordinate.schema()),
        &command,
        engine.durability_policy.clone(),
    )?;
    let database = engine.database_for_route(DatabaseRoute::Coordinate(coordinate))?;
    command_dispatch::execute_in_database::<WardrobeEngine>(&database, command, None)
}

pub(super) fn execute_in_scope(
    engine: &WardrobeEngine,
    scope: StorageScope,
    command: Command,
) -> Result<CommandResult> {
    routing::validate_scope(&scope)?;
    if let StorageScope::Schema { database, schema } = &scope {
        let registry = WardrobeEngine::read_lock(&engine.registry)?;
        command_dispatch::validate_command_against_registry(&registry, database, schema, &command)?;
    }

    match scope {
        StorageScope::Tenant {
            tenant_id,
            database,
            schema,
        } => execute_for_tenant(engine, &tenant_id, &database, &schema, command),
        StorageScope::Database { database } => {
            let database_path = routing::database_scope_path(&engine.root_directory, &database)?;
            wal::append_command(
                &database_path,
                None,
                &command,
                engine.durability_policy.clone(),
            )?;
            let database = engine.database_for_route(DatabaseRoute::Database(database))?;
            command_dispatch::execute_in_database::<WardrobeEngine>(&database, command, None)
        }
        StorageScope::Schema { database, schema } => {
            let database_path =
                routing::schema_scope_path(&engine.root_directory, &database, &schema)?;
            wal::append_command(
                &database_path,
                Some(&schema),
                &command,
                engine.durability_policy.clone(),
            )?;
            let database = engine.database_for_route(DatabaseRoute::Schema { database, schema })?;
            command_dispatch::execute_in_database::<WardrobeEngine>(&database, command, None)
        }
        StorageScope::Drawer { namespace } => {
            wal::append_command(
                &engine.root_directory,
                Some(&namespace),
                &command,
                engine.durability_policy.clone(),
            )?;
            command_dispatch::execute_in_database::<WardrobeEngine>(
                &engine.database_core,
                command,
                Some(namespace.as_str()),
            )
        }
    }
}

pub(super) fn create_database(
    engine: &WardrobeEngine,
    database_name: &str,
) -> Result<StorageInventory> {
    catalog_lifecycle::create_database(
        &engine.root_directory,
        &engine.registry,
        database_name,
        |command| {
            wal::append_command(
                &engine.root_directory,
                None,
                command,
                engine.durability_policy.clone(),
            )
        },
    )
}

pub(super) fn create_schema(
    engine: &WardrobeEngine,
    database_name: &str,
    schema_name: &str,
) -> Result<StorageInventory> {
    catalog_lifecycle::create_schema(
        &engine.root_directory,
        &engine.registry,
        database_name,
        schema_name,
        |command| {
            wal::append_command(
                &engine.root_directory,
                None,
                command,
                engine.durability_policy.clone(),
            )
        },
    )
}

pub(super) fn create_drawer(
    engine: &WardrobeEngine,
    database_name: &str,
    schema_name: &str,
    drawer_name: &str,
) -> Result<StorageInventory> {
    catalog_lifecycle::create_drawer(
        &engine.root_directory,
        &engine.registry,
        database_name,
        schema_name,
        drawer_name,
        |command| {
            wal::append_command(
                &engine.root_directory,
                None,
                command,
                engine.durability_policy.clone(),
            )
        },
    )
}

pub(super) fn register_tenant_route(
    engine: &WardrobeEngine,
    tenant_id: &str,
    database_name: &str,
    location: &str,
) -> Result<StorageInventory> {
    catalog_lifecycle::register_tenant_route(
        &engine.root_directory,
        &engine.registry,
        tenant_id,
        database_name,
        location,
        |command| {
            wal::append_command(
                &engine.root_directory,
                None,
                command,
                engine.durability_policy.clone(),
            )
        },
    )
}

pub(super) fn drop_database(engine: &WardrobeEngine, database_name: &str) -> Result<Value> {
    catalog_lifecycle::drop_database(
        &engine.root_directory,
        &engine.registry,
        database_name,
        |command| {
            wal::append_command(
                &engine.root_directory,
                None,
                command,
                engine.durability_policy.clone(),
            )
        },
    )
}

pub(super) fn drop_schema(
    engine: &WardrobeEngine,
    database_name: &str,
    schema_name: &str,
) -> Result<Value> {
    catalog_lifecycle::drop_schema(
        &engine.root_directory,
        &engine.registry,
        database_name,
        schema_name,
        |command| {
            wal::append_command(
                &engine.root_directory,
                None,
                command,
                engine.durability_policy.clone(),
            )
        },
    )
}

pub(super) fn drop_drawer(
    engine: &WardrobeEngine,
    database_name: &str,
    schema_name: &str,
    drawer_name: &str,
) -> Result<Value> {
    catalog_lifecycle::drop_drawer(
        &engine.root_directory,
        &engine.registry,
        database_name,
        schema_name,
        drawer_name,
        |command| {
            wal::append_command(
                &engine.root_directory,
                None,
                command,
                engine.durability_policy.clone(),
            )
        },
    )
}

pub(super) fn execute_for_tenant(
    engine: &WardrobeEngine,
    tenant_id: &str,
    database_name: &str,
    schema_name: &str,
    command: Command,
) -> Result<CommandResult> {
    catalog_validation::validate_tenant_identifier(tenant_id)?;
    catalog_validation::validate_database_name(database_name)?;
    catalog_validation::validate_schema_name(schema_name)?;

    let tenant_route = {
        let registry = WardrobeEngine::read_lock(&engine.registry)?;
        registry.tenant_route(tenant_id).ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("Tenant '{tenant_id}' is not registered in the catalog"),
            )
        })?
    };

    if tenant_route.database != database_name {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("Tenant '{tenant_id}' is not routed to database '{database_name}'"),
        ));
    }

    let registry = WardrobeEngine::read_lock(&engine.registry)?;
    command_dispatch::validate_command_against_registry(
        &registry,
        database_name,
        schema_name,
        &command,
    )?;

    let route_path =
        catalog_validation::catalog_location_path(&engine.root_directory, &tenant_route.location);
    let schema_path = routing::tenant_schema_path(&route_path, schema_name);
    wal::append_command(
        &schema_path,
        Some(schema_name),
        &command,
        engine.durability_policy.clone(),
    )?;
    let routed_database = RwLock::new(
        Database::initialize_with_cache_limit_wal_thresholds_and_durability(
            &schema_path,
            engine.max_cached_drawers,
            engine.wal_size_threshold_bytes,
            engine.wal_ops_threshold_count,
            engine.durability_policy.clone(),
        )?,
    );
    wal::recover_database::<WardrobeEngine>(&routed_database)?;
    command_dispatch::execute_in_database::<WardrobeEngine>(&routed_database, command, None)
}

pub(super) fn execute_command(engine: &WardrobeEngine, command: Command) -> Result<CommandResult> {
    command_dispatch::execute_command(engine, command)
}

impl command_dispatch::BoundaryCommandExecutor for WardrobeEngine {
    fn append_boundary_wal(&self, command: &Command) -> Result<()> {
        wal::append_command(
            &self.root_directory,
            None,
            command,
            self.durability_policy.clone(),
        )
    }

    fn show_tenants(&self) -> Result<Vec<String>> {
        WardrobeEngine::show_tenants(self)
    }

    fn show_databases(&self) -> Result<Vec<StorageInventory>> {
        WardrobeEngine::show_databases(self)
    }

    fn verify_wal(&self, database_name: Option<&str>) -> Result<WalVerification> {
        WardrobeEngine::verify_wal(self, database_name)
    }

    fn show_schemas(&self, database_name: &str) -> Result<Vec<String>> {
        WardrobeEngine::show_schemas(self, database_name)
    }

    fn show_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<Vec<StorageInventory>> {
        WardrobeEngine::show_drawers(self, database_name, schema_name)
    }

    fn create_database(&self, database_name: &str) -> Result<StorageInventory> {
        WardrobeEngine::create_database(self, database_name)
    }

    fn create_schema(&self, database_name: &str, schema_name: &str) -> Result<StorageInventory> {
        WardrobeEngine::create_schema(self, database_name, schema_name)
    }

    fn create_drawer(
        &self,
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> Result<StorageInventory> {
        WardrobeEngine::create_drawer(self, database_name, schema_name, drawer_name)
    }

    fn register_tenant_route(
        &self,
        tenant_id: &str,
        database_name: &str,
        location: &str,
    ) -> Result<StorageInventory> {
        WardrobeEngine::register_tenant_route(self, tenant_id, database_name, location)
    }

    fn drop_database(&self, database_name: &str) -> Result<Value> {
        WardrobeEngine::drop_database(self, database_name)
    }

    fn drop_schema(&self, database_name: &str, schema_name: &str) -> Result<Value> {
        WardrobeEngine::drop_schema(self, database_name, schema_name)
    }

    fn drop_drawer(
        &self,
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> Result<Value> {
        WardrobeEngine::drop_drawer(self, database_name, schema_name, drawer_name)
    }

    fn inspect_drawer(&self, drawer_name: &str) -> Result<DrawerInspectionMetrics> {
        WardrobeEngine::inspect_drawer(self, drawer_name)
    }

    fn check_path(&self, path: &str) -> Result<CheckReport> {
        WardrobeEngine::check_path(self, path)
    }

    fn diagnose_storage(&self) -> Result<StorageDiagnosis> {
        WardrobeEngine::diagnose_storage(self)
    }

    fn list_drawer_names(&self) -> Result<Vec<String>> {
        WardrobeEngine::list_drawer_names(self)
    }

    fn backup_archive(&self, source_path: &str) -> Result<BackupArchive> {
        WardrobeEngine::backup_archive(self, source_path)
    }

    fn restore_archive(
        &self,
        destination_path: &str,
        archive: BackupArchive,
    ) -> Result<RestoreReport> {
        WardrobeEngine::restore_archive(self, destination_path, archive)
    }

    fn manage_user(&self, action: &str, payload: Value) -> Result<Value> {
        WardrobeEngine::manage_user(self, action, payload)
    }

    fn execute_for_tenant(
        &self,
        tenant_id: &str,
        database_name: &str,
        schema_name: &str,
        command: Command,
    ) -> Result<CommandResult> {
        WardrobeEngine::execute_for_tenant(self, tenant_id, database_name, schema_name, command)
    }

    fn execute(&self, coordinate: StorageCoordinate, command: Command) -> Result<CommandResult> {
        WardrobeEngine::execute(self, coordinate, command)
    }

    fn execute_in_scope(&self, scope: StorageScope, command: Command) -> Result<CommandResult> {
        WardrobeEngine::execute_in_scope(self, scope, command)
    }

    fn execute_local(&self, command: Command) -> Result<CommandResult> {
        command_dispatch::execute_in_database::<WardrobeEngine>(&self.database_core, command, None)
    }
}
