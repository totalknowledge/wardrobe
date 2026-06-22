use crate::wrdb_lib::catalog_validation;
use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::discovery;
use crate::wrdb_lib::drawer::{Drawer, VacuumReport};
use crate::wrdb_lib::pointer;
use crate::wrdb_lib::query;
use crate::wrdb_lib::registry::CatalogRegistry;
use crate::wrdb_lib::routing::{self, DatabaseRoute, ExecutionContext};
use crate::wrdb_lib::wal::{WalJournal, WalOperation as DurableWalOperation, WalVerification};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub use crate::wrdb_lib::command::{Command, CommandResult};
pub use crate::wrdb_lib::query::{OrderDirection, QueryModifiers};
pub use crate::wrdb_lib::storage::{
    StorageCoordinate, StorageInventory, StorageLocator, StorageScope,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeleteAction {
    Cascade,
    Restrict,
    SetNull,
}

#[derive(Clone, Debug)]
struct InverseDeleteRule {
    field_name: String,
    action: DeleteAction,
    target_drawer: String,
    mapped_by: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RelationTarget {
    Inferred(String),
    Static(String),
    Polymorphic,
    SelfReference,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WalOperation {
    Upsert {
        drawer_name: String,
        payload: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drawer_namespace: Option<String>,
    },
    DeleteById {
        pointer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drawer_namespace: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum WalRecord {
    Begin {
        tx_id: String,
        operation: WalOperation,
        #[serde(default)]
        ts: u64,
    },

    Commit {
        tx_id: String,
    },
    Abort {
        tx_id: String,
    },
}

pub struct WardrobeEngine {
    root_directory: PathBuf,
    registry: RwLock<CatalogRegistry>,
    database_core: RwLock<Database>,
    routed_databases: RwLock<HashMap<DatabaseRoute, Arc<RwLock<Database>>>>,
    max_cached_drawers: Option<usize>,
}

impl WardrobeEngine {
    pub fn open(directory: &str) -> Result<Self> {
        Self::open_with_optional_drawer_cache_limit(directory, None)
    }

    pub fn open_with_drawer_cache_limit(
        directory: &str,
        max_cached_drawers: usize,
    ) -> Result<Self> {
        Self::open_with_optional_drawer_cache_limit(directory, Some(max_cached_drawers))
    }

    fn open_with_optional_drawer_cache_limit(
        directory: &str,
        max_cached_drawers: Option<usize>,
    ) -> Result<Self> {
        let root_directory = PathBuf::from(directory);
        let registry = CatalogRegistry::open_or_initialize(&root_directory)?;
        let database_core =
            Database::initialize_with_cache_limit(&root_directory, max_cached_drawers)?;
        let database_core = RwLock::new(database_core);
        Self::recover_database(&database_core)?;
        Ok(Self {
            root_directory,
            registry: RwLock::new(registry),
            database_core,
            routed_databases: RwLock::new(HashMap::new()),
            max_cached_drawers,
        })
    }

    #[deprecated(note = "Use WardrobeEngine::open for filesystem-backed initialization")]
    pub fn new(directory: &str) -> Result<Self> {
        Self::open(directory)
    }

    pub fn upsert(&self, drawer_name: &str, payload: Value) -> Result<String> {
        Self::upsert_in_database(
            &self.database_core,
            drawer_name,
            payload,
            ExecutionContext::root(),
        )
    }

    pub fn find_all(&self, drawer_name: &str) -> std::io::Result<Vec<Value>> {
        Self::find_all_in_database(&self.database_core, drawer_name, ExecutionContext::root())
    }

    pub fn find_by_filter(
        &self,
        drawer_name: &str,
        filter: Value,
        modifiers: Option<QueryModifiers>,
    ) -> Result<Vec<Value>> {
        Self::find_by_filter_in_database(
            &self.database_core,
            drawer_name,
            filter,
            modifiers,
            ExecutionContext::root(),
        )
    }

    pub fn count(
        &self,
        drawer_name: &str,
        filter: Option<Value>,
        modifiers: Option<QueryModifiers>,
    ) -> Result<usize> {
        Self::count_in_database(
            &self.database_core,
            drawer_name,
            filter,
            modifiers,
            ExecutionContext::root(),
        )
    }

    pub fn find_by_id(&self, pointer: &str) -> Result<Option<Value>> {
        Self::find_by_id_in_database(&self.database_core, pointer, ExecutionContext::root())
    }

    pub fn delete<L>(&self, locator: L) -> Result<bool>
    where
        L: Into<StorageLocator>,
    {
        Self::delete_by_id_in_database(
            &self.database_core,
            locator.into(),
            ExecutionContext::root(),
        )
    }

    pub fn delete_by_id<L>(&self, locator: L) -> Result<bool>
    where
        L: Into<StorageLocator>,
    {
        self.delete(locator)
    }

    pub fn vacuum_drawer(&self, drawer_name: &str) -> Result<VacuumReport> {
        Self::vacuum_drawer_in_database(&self.database_core, drawer_name, ExecutionContext::root())
    }

    pub fn migrate_drawer(&self, drawer_name: &str) -> Result<VacuumReport> {
        Self::migrate_drawer_in_database(&self.database_core, drawer_name, ExecutionContext::root())
    }

    pub fn cached_drawer_count(&self) -> Result<usize> {
        Ok(Self::read_lock(&self.database_core)?.cached_drawer_count())
    }

    pub fn show_tenants(&self) -> Result<Vec<String>> {
        let registry = Self::read_lock(&self.registry)?;
        discovery::show_tenants(&self.root_directory, &registry)
    }

    pub fn list_tenants(&self) -> Result<Vec<String>> {
        self.show_tenants()
    }

    pub fn show_databases(&self) -> Result<Vec<StorageInventory>> {
        let registry = Self::read_lock(&self.registry)?;
        discovery::show_databases(&self.root_directory, &registry)
    }

    pub fn list_databases(&self) -> Result<Vec<StorageInventory>> {
        self.show_databases()
    }

    pub fn verify_wal(&self, database_name: Option<&str>) -> Result<WalVerification> {
        let database_path = match database_name {
            Some(database_name) => {
                catalog_validation::database_path_from_name(&self.root_directory, database_name)?
            }
            None => self.root_directory.clone(),
        };
        WalJournal::at_database_path(database_path).verify()
    }

    fn append_wal_for_command(
        database_path: &Path,
        schema_name: Option<&str>,
        command: &Command,
    ) -> Result<()> {
        let Some(operation) = Self::wal_operation(command) else {
            return Ok(());
        };

        let scope = schema_name
            .map(|schema_name| format!("schema:{schema_name}"))
            .unwrap_or_else(|| "database".to_string());
        let payload = serde_json::to_vec(command).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to serialize WAL command payload: {error}"),
            )
        })?;

        WalJournal::at_database_path(database_path).append(operation, &scope, &payload)?;
        Ok(())
    }

    fn wal_operation(command: &Command) -> Option<DurableWalOperation> {
        match command {
            Command::Upsert { .. } => Some(DurableWalOperation::Upsert),
            Command::Delete { .. } => Some(DurableWalOperation::Delete),
            Command::Vacuum { .. } | Command::Migrate { .. } => {
                Some(DurableWalOperation::Maintenance)
            }
            Command::DefineDatabase { .. }
            | Command::DefineSchema { .. }
            | Command::DefineDrawer { .. }
            | Command::DefineTenantRoute { .. } => Some(DurableWalOperation::Define),
            _ => None,
        }
    }

    pub fn show_schemas(&self, database_name: &str) -> Result<Vec<String>> {
        let registry = Self::read_lock(&self.registry)?;
        discovery::show_schemas(&self.root_directory, &registry, database_name)
    }

    pub fn list_schemas(&self, database_name: &str) -> Result<Vec<String>> {
        self.show_schemas(database_name)
    }

    pub fn show_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<Vec<StorageInventory>> {
        let registry = Self::read_lock(&self.registry)?;
        discovery::show_drawers(&self.root_directory, &registry, database_name, schema_name)
    }

    pub fn list_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<Vec<StorageInventory>> {
        self.show_drawers(database_name, schema_name)
    }

    pub fn execute(
        &self,
        coordinate: StorageCoordinate,
        command: Command,
    ) -> Result<CommandResult> {
        self.validate_command_against_registry(
            &routing::coordinate_catalog_database(&coordinate),
            coordinate.schema(),
            &command,
        )?;
        let database_path = routing::coordinate_database_path(&self.root_directory, &coordinate)?;
        Self::append_wal_for_command(&database_path, Some(coordinate.schema()), &command)?;
        let database = self.database_for_route(DatabaseRoute::Coordinate(coordinate))?;
        Self::execute_in_database(&database, command, None)
    }

    pub fn execute_in_scope(&self, scope: StorageScope, command: Command) -> Result<CommandResult> {
        routing::validate_scope(&scope)?;
        if let StorageScope::Schema { database, schema } = &scope {
            self.validate_command_against_registry(database, schema, &command)?;
        }

        match scope {
            StorageScope::Tenant {
                tenant_id,
                database,
                schema,
            } => self.execute_for_tenant(&tenant_id, &database, &schema, command),
            StorageScope::Database { database } => {
                let database_path = routing::database_scope_path(&self.root_directory, &database)?;
                Self::append_wal_for_command(&database_path, None, &command)?;
                let database = self.database_for_route(DatabaseRoute::Database(database))?;
                Self::execute_in_database(&database, command, None)
            }
            StorageScope::Schema { database, schema } => {
                let database_path =
                    routing::schema_scope_path(&self.root_directory, &database, &schema)?;
                Self::append_wal_for_command(&database_path, Some(&schema), &command)?;
                let database =
                    self.database_for_route(DatabaseRoute::Schema { database, schema })?;
                Self::execute_in_database(&database, command, None)
            }
            StorageScope::Drawer { namespace } => {
                Self::append_wal_for_command(&self.root_directory, Some(&namespace), &command)?;
                Self::execute_in_database(&self.database_core, command, Some(namespace.as_str()))
            }
        }
    }

    pub fn create_database(&self, database_name: &str) -> Result<StorageInventory> {
        catalog_validation::validate_database_name(database_name)?;
        Self::append_wal_for_command(
            &self.root_directory,
            None,
            &Command::DefineDatabase {
                database_name: database_name.to_string(),
            },
        )?;
        let database_path =
            catalog_validation::database_path_from_name(&self.root_directory, database_name)?;
        std::fs::create_dir_all(&database_path)?;

        {
            let mut registry = Self::write_lock(&self.registry)?;
            registry.register_database(database_name);
            registry.persist_to_root(&self.root_directory)?;
        }

        discovery::storage_inventory(database_name.to_string(), &database_path)
    }

    pub fn create_schema(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<StorageInventory> {
        catalog_validation::validate_database_name(database_name)?;
        catalog_validation::validate_schema_name(schema_name)?;
        Self::append_wal_for_command(
            &self.root_directory,
            None,
            &Command::DefineSchema {
                database_name: database_name.to_string(),
                schema_name: schema_name.to_string(),
            },
        )?;

        {
            let registry = Self::read_lock(&self.registry)?;
            if !registry.contains_database(database_name) {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!("Database '{database_name}' is not registered in the catalog"),
                ));
            }
        }

        let schema_path =
            catalog_validation::database_path_from_name(&self.root_directory, database_name)?
                .join(schema_name);
        std::fs::create_dir_all(&schema_path)?;

        {
            let mut registry = Self::write_lock(&self.registry)?;
            registry.register_schema(database_name, schema_name);
            registry.persist_to_root(&self.root_directory)?;
        }

        discovery::storage_inventory(schema_name.to_string(), &schema_path)
    }

    pub fn create_drawer(
        &self,
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> Result<StorageInventory> {
        catalog_validation::validate_database_name(database_name)?;
        catalog_validation::validate_schema_name(schema_name)?;
        catalog_validation::validate_drawer_name(drawer_name)?;
        Self::append_wal_for_command(
            &self.root_directory,
            None,
            &Command::DefineDrawer {
                database_name: database_name.to_string(),
                schema_name: schema_name.to_string(),
                drawer_name: drawer_name.to_string(),
            },
        )?;

        {
            let registry = Self::read_lock(&self.registry)?;
            if !registry.contains_schema(database_name, schema_name) {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!(
                        "Schema '{schema_name}' is not registered for database '{database_name}'"
                    ),
                ));
            }
        }

        let schema_path =
            catalog_validation::database_path_from_name(&self.root_directory, database_name)?
                .join(schema_name);
        std::fs::create_dir_all(&schema_path)?;
        let drawer_path = schema_path.join(format!("{drawer_name}.drw"));
        if !drawer_path.exists() {
            std::fs::File::create(&drawer_path)?;
        }
        let index_path = schema_path.join(format!("{drawer_name}_index.drw"));
        if !index_path.exists() {
            std::fs::File::create(&index_path)?;
        }

        {
            let mut registry = Self::write_lock(&self.registry)?;
            registry.register_drawer(
                database_name,
                schema_name,
                drawer_name,
                drawer_path.to_string_lossy().into_owned(),
            );
            registry.persist_to_root(&self.root_directory)?;
        }

        discovery::drawer_inventory(drawer_name.to_string(), &schema_path, drawer_name)
    }

    pub fn register_tenant_route(
        &self,
        tenant_id: &str,
        database_name: &str,
        location: &str,
    ) -> Result<StorageInventory> {
        catalog_validation::validate_tenant_identifier(tenant_id)?;
        catalog_validation::validate_database_name(database_name)?;
        catalog_validation::validate_catalog_location(location)?;
        Self::append_wal_for_command(
            &self.root_directory,
            None,
            &Command::DefineTenantRoute {
                tenant_id: tenant_id.to_string(),
                database_name: database_name.to_string(),
                location: location.to_string(),
            },
        )?;

        let route_path = catalog_validation::catalog_location_path(&self.root_directory, location);
        std::fs::create_dir_all(&route_path)?;

        {
            let mut registry = Self::write_lock(&self.registry)?;
            registry.register_tenant_route(tenant_id, database_name, location);
            registry.persist_to_root(&self.root_directory)?;
        }

        discovery::storage_inventory(tenant_id.to_string(), &route_path)
    }

    pub fn execute_for_tenant(
        &self,
        tenant_id: &str,
        database_name: &str,
        schema_name: &str,
        command: Command,
    ) -> Result<CommandResult> {
        catalog_validation::validate_tenant_identifier(tenant_id)?;
        catalog_validation::validate_database_name(database_name)?;
        catalog_validation::validate_schema_name(schema_name)?;

        let tenant_route = {
            let registry = Self::read_lock(&self.registry)?;
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

        self.validate_command_against_registry(database_name, schema_name, &command)?;

        let route_path =
            catalog_validation::catalog_location_path(&self.root_directory, &tenant_route.location);
        let schema_path = routing::tenant_schema_path(&route_path, schema_name);
        Self::append_wal_for_command(&schema_path, Some(schema_name), &command)?;
        let routed_database = RwLock::new(Database::initialize_with_cache_limit(
            &schema_path,
            self.max_cached_drawers,
        )?);
        Self::recover_database(&routed_database)?;
        Self::execute_in_database(&routed_database, command, None)
    }

    pub fn execute_command(&self, command: Command) -> Result<CommandResult> {
        if !matches!(
            &command,
            Command::DefineDatabase { .. }
                | Command::DefineSchema { .. }
                | Command::DefineDrawer { .. }
                | Command::DefineTenantRoute { .. }
        ) {
            Self::append_wal_for_command(&self.root_directory, None, &command)?;
        }
        match command {
            Command::ShowTenants => self.show_tenants().map(CommandResult::Tenants),
            Command::ShowDatabases => self.show_databases().map(CommandResult::Databases),
            Command::VerifyWal { database_name } => self
                .verify_wal(database_name.as_deref())
                .map(CommandResult::WalVerification),
            Command::ShowSchemas { database_name } => self
                .show_schemas(&database_name)
                .map(CommandResult::Schemas),
            Command::ShowDrawers {
                database_name,
                schema_name,
            } => self
                .show_drawers(&database_name, &schema_name)
                .map(CommandResult::Drawers),
            Command::DefineDatabase { database_name } => self
                .create_database(&database_name)
                .map(CommandResult::StorageInventory),
            Command::DefineSchema {
                database_name,
                schema_name,
            } => self
                .create_schema(&database_name, &schema_name)
                .map(CommandResult::StorageInventory),
            Command::DefineDrawer {
                database_name,
                schema_name,
                drawer_name,
            } => self
                .create_drawer(&database_name, &schema_name, &drawer_name)
                .map(CommandResult::StorageInventory),
            Command::DefineTenantRoute {
                tenant_id,
                database_name,
                location,
            } => self
                .register_tenant_route(&tenant_id, &database_name, &location)
                .map(CommandResult::StorageInventory),
            Command::ExecuteForTenant {
                tenant_id,
                database_name,
                schema_name,
                command,
            } => self.execute_for_tenant(&tenant_id, &database_name, &schema_name, *command),
            Command::Execute {
                coordinate,
                command,
            } => self.execute(coordinate, *command),
            Command::ExecuteInScope { scope, command } => self.execute_in_scope(scope, *command),
            command => Self::execute_in_database(&self.database_core, command, None),
        }
    }

    fn execute_in_database(
        database: &RwLock<Database>,
        command: Command,
        drawer_namespace: Option<&str>,
    ) -> Result<CommandResult> {
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
            } => Self::upsert_in_database(database, &drawer_name, payload, context)
                .map(CommandResult::Pointer),
            Command::FindAll { drawer_name } => {
                Self::find_all_in_database(database, &drawer_name, context)
                    .map(CommandResult::Records)
            }
            Command::FindById { pointer } => {
                Self::find_by_id_in_database(database, &pointer, context).map(CommandResult::Record)
            }
            Command::FindByFilter {
                drawer_name,
                filter,
                modifiers,
            } => {
                Self::find_by_filter_in_database(database, &drawer_name, filter, modifiers, context)
                    .map(CommandResult::Records)
            }
            Command::Count {
                drawer_name,
                filter,
                modifiers,
            } => Self::count_in_database(database, &drawer_name, filter, modifiers, context)
                .map(CommandResult::Count),
            Command::Delete { pointer } => {
                Self::delete_by_id_in_database(database, StorageLocator::Inline(pointer), context)
                    .map(CommandResult::Deleted)
            }
            Command::Vacuum { drawer_name } => {
                Self::vacuum_drawer_in_database(database, &drawer_name, context)
                    .map(CommandResult::Vacuumed)
            }
            Command::Migrate { drawer_name } => {
                Self::migrate_drawer_in_database(database, &drawer_name, context)
                    .map(CommandResult::Migrated)
            }
            Command::DefineDatabase { .. }
            | Command::DefineSchema { .. }
            | Command::DefineDrawer { .. }
            | Command::DefineTenantRoute { .. }
            | Command::ExecuteForTenant { .. }
            | Command::Execute { .. }
            | Command::ExecuteInScope { .. } => Err(Error::new(
                ErrorKind::InvalidInput,
                "Catalog and scoped command routing is only available at the WardrobeEngine boundary",
            )),
        }
    }

    fn database_for_route(&self, route: DatabaseRoute) -> Result<Arc<RwLock<Database>>> {
        let storage_path = route.storage_path(&self.root_directory)?;

        if let Some(database) = Self::read_lock(&self.routed_databases)?
            .get(&route)
            .cloned()
        {
            return Ok(database);
        }

        let mut routed_databases = Self::write_lock(&self.routed_databases)?;
        if !routed_databases.contains_key(&route) {
            let database =
                Database::initialize_with_cache_limit(storage_path, self.max_cached_drawers)?;
            let database = Arc::new(RwLock::new(database));
            Self::recover_database(&database)?;
            routed_databases.insert(route.clone(), database);
        }

        routed_databases.get(&route).cloned().ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                "Failed to acquire routed database handle",
            )
        })
    }

    fn validate_command_against_registry(
        &self,
        database: &str,
        schema: &str,
        command: &Command,
    ) -> Result<()> {
        let registry = Self::read_lock(&self.registry)?;
        if registry.is_empty() {
            return Ok(());
        }

        let Some(drawer_name) = Self::command_drawer_name(command) else {
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

    fn command_drawer_name(command: &Command) -> Option<String> {
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
                Self::command_drawer_name(command)
            }
            Command::DefineDatabase { .. }
            | Command::DefineSchema { .. }
            | Command::DefineDrawer { .. }
            | Command::DefineTenantRoute { .. }
            | Command::ExecuteForTenant { .. }
            | Command::ShowTenants
            | Command::ShowDatabases
            | Command::VerifyWal { .. }
            | Command::ShowSchemas { .. }
            | Command::ShowDrawers { .. } => None,
        }
    }

    fn read_lock<T>(lock: &RwLock<T>) -> Result<RwLockReadGuard<'_, T>> {
        lock.read()
            .map_err(|_| Error::other("Wardrobe lock was poisoned during read"))
    }

    fn write_lock<T>(lock: &RwLock<T>) -> Result<RwLockWriteGuard<'_, T>> {
        lock.write()
            .map_err(|_| Error::other("Wardrobe lock was poisoned during write"))
    }

    fn load_drawer_handle(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        primary_key: &str,
        unique_constraints: Vec<String>,
    ) -> Result<Arc<RwLock<Drawer>>> {
        let mut database = Self::write_lock(database_core)?;
        database.load_drawer(drawer_name, primary_key, unique_constraints)?;
        database.use_drawer(drawer_name).ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{}' could not be loaded", drawer_name),
            )
        })
    }

    fn active_drawer_handle_or_load_from_disk(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        primary_key: &str,
        unique_constraints: Vec<String>,
    ) -> Result<Option<Arc<RwLock<Drawer>>>> {
        if let Some(drawer) = Self::read_lock(database_core)?.use_drawer(drawer_name) {
            return Ok(Some(drawer));
        }

        let mut database = Self::write_lock(database_core)?;
        database.active_drawer_or_load_from_disk(drawer_name, primary_key, unique_constraints)
    }

    fn wal_path(database_core: &RwLock<Database>) -> Result<PathBuf> {
        Ok(Self::read_lock(database_core)?
            .storage_directory_path()
            .join("wardrobe.wal"))
    }

    fn append_wal_record(database_core: &RwLock<Database>, record: &WalRecord) -> Result<()> {
        let wal_path = Self::wal_path(database_core)?;
        let mut wal_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(wal_path)?;
        let serialized = serde_json::to_vec(record)?;
        wal_file.write_all(&serialized)?;
        wal_file.write_all(b"\n")?;
        wal_file.sync_all()?;
        let bytes_written = serialized.len() as u64 + 1;
        {
            let db = Self::read_lock(database_core)?;
            db.record_wal_activity(bytes_written, 1);
        }
        Self::check_wal_thresholds(database_core)?;
        Ok(())
    }

    fn recover_database(database_core: &RwLock<Database>) -> Result<()> {
        let wal_path = Self::wal_path(database_core)?;
        if !wal_path.exists() {
            return Ok(());
        }

        let checkpoint_path = wal_path.with_extension("wal.meta");
        let mut last_checkpoint: u64 = 0;
        let mut checkpoint_found = false;
        if checkpoint_path.exists() {
            if let Ok(contents) = std::fs::read_to_string(&checkpoint_path) {
                if let Ok(value) = serde_json::from_str::<Value>(&contents) {
                    last_checkpoint = value
                        .get("last_checkpoint")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    checkpoint_found = true;
                }
            }
        }

        let contents = std::fs::read_to_string(wal_path)?;
        let mut begun_transactions = Vec::new();
        let mut closed_transactions = HashSet::new();

        for line in contents
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let record: WalRecord = serde_json::from_str(line).map_err(|error| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Failed to parse WAL record during recovery: {error}"),
                )
            })?;

            match record {
                WalRecord::Begin {
                    tx_id,
                    operation,
                    ts,
                } => {
                    if checkpoint_found && ts <= last_checkpoint {
                        continue;
                    }
                    begun_transactions.push((tx_id, operation));
                }
                WalRecord::Commit { tx_id } => {
                    closed_transactions.insert(tx_id);
                }
                WalRecord::Abort { tx_id } => {
                    closed_transactions.insert(tx_id);
                }
            }
        }

        for (tx_id, operation) in begun_transactions {
            if closed_transactions.contains(&tx_id) {
                continue;
            }

            Self::replay_wal_operation(database_core, &operation)?;
            Self::append_wal_record(database_core, &WalRecord::Commit { tx_id })?;
        }

        Ok(())
    }

    fn replay_wal_operation(
        database_core: &RwLock<Database>,
        operation: &WalOperation,
    ) -> Result<()> {
        match operation {
            WalOperation::Upsert {
                drawer_name,
                payload,
                drawer_namespace,
            } => {
                let context = ExecutionContext {
                    drawer_namespace: drawer_namespace.as_deref(),
                };
                Self::upsert_in_database_unlogged(
                    database_core,
                    drawer_name,
                    payload.clone(),
                    context,
                )?;
            }
            WalOperation::DeleteById {
                pointer,
                drawer_namespace,
            } => {
                let context = ExecutionContext {
                    drawer_namespace: drawer_namespace.as_deref(),
                };
                Self::delete_by_id_in_database_unlogged(database_core, pointer, context)?;
            }
        }

        Ok(())
    }

    fn check_wal_thresholds(database_core: &RwLock<Database>) -> Result<()> {
        let (bytes, ops) = Self::read_lock(database_core)?.get_wal_counters();
        let (threshold_bytes, threshold_ops) = Self::read_lock(database_core)?.wal_thresholds();
        if bytes >= threshold_bytes || ops >= threshold_ops {
            Self::flush_checkpoint(database_core)?;
        }
        Ok(())
    }

    fn flush_checkpoint(database_core: &RwLock<Database>) -> Result<()> {
        let wal_path = Self::wal_path(database_core)?;
        let wal_file = OpenOptions::new().write(true).open(&wal_path)?;
        wal_file.sync_all()?;

        let drawers = Self::read_lock(database_core)?.get_all_drawers();
        for (_name, drawer) in drawers {
            let mut guard = Self::write_lock(&drawer)?;
            guard.checkpoint()?;
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let checkpoint_path = wal_path.with_extension("wal.meta");
        let checkpoint_body = serde_json::json!({"last_checkpoint": now});
        let serialized = serde_json::to_vec(&checkpoint_body)?;
        std::fs::write(&checkpoint_path, &serialized)?;
        let meta_f = OpenOptions::new().write(true).open(&checkpoint_path)?;
        meta_f.sync_all()?;

        let wal_handle = OpenOptions::new().write(true).open(&wal_path)?;
        wal_handle.set_len(0)?;
        wal_handle.sync_all()?;

        Self::read_lock(database_core)?.reset_wal_counters();

        Ok(())
    }

    fn upsert_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<String> {
        let operation = WalOperation::Upsert {
            drawer_name: drawer_name.to_string(),
            payload: payload.clone(),
            drawer_namespace: context.drawer_namespace.map(str::to_string),
        };
        let tx_id = Uuid::new_v4().simple().to_string();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self::append_wal_record(
            database_core,
            &WalRecord::Begin {
                tx_id: tx_id.clone(),
                operation,
                ts: now,
            },
        )?;
        let result =
            Self::upsert_in_database_unlogged(database_core, drawer_name, payload, context);

        if result.is_ok() {
            Self::append_wal_record(database_core, &WalRecord::Commit { tx_id })?;
        } else {
            let _ = Self::append_wal_record(database_core, &WalRecord::Abort { tx_id });
        }

        result
    }

    fn upsert_in_database_unlogged(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<String> {
        if let Value::Object(map) = payload {
            let target_primary_key = "_id";
            let physical_drawer_name =
                routing::scoped_drawer_name(drawer_name, context.drawer_namespace);

            let record_key = match map.get(target_primary_key).and_then(|v| v.as_str()) {
                Some(existing_id) => {
                    pointer::normalize_primary_key(&physical_drawer_name, drawer_name, existing_id)
                }
                None => Uuid::new_v4().simple().to_string(),
            };
            let record_pointer = pointer::format_pointer(&physical_drawer_name, &record_key);

            let drawer_handle = Self::load_drawer_handle(
                database_core,
                &physical_drawer_name,
                target_primary_key,
                Vec::new(),
            )?;
            let mut relationship_constraints =
                Self::read_lock(&drawer_handle)?.relationship_constraints();
            Self::register_inline_relationship_aliases(
                &drawer_handle,
                &map,
                &mut relationship_constraints,
            )?;
            let processed_map = Self::decompose_nested_objects(
                database_core,
                map,
                &physical_drawer_name,
                &relationship_constraints,
                context,
            )?;

            let mut full_record = processed_map;
            full_record.insert(
                target_primary_key.to_string(),
                Value::String(record_key.clone()),
            );

            match Self::write_lock(&drawer_handle)?.upsert_record(Value::Object(full_record))? {
                Ok(_) => Ok(record_pointer),
                Err(validation_error) => Err(Error::new(ErrorKind::InvalidData, validation_error)),
            }
        } else {
            Err(Error::new(
                ErrorKind::InvalidInput,
                "Payload root must be a JSON object",
            ))
        }
    }

    fn find_all_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
    ) -> std::io::Result<Vec<Value>> {
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let mut records = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )? {
            Self::write_lock(&drawer)?.find_all_records_with_migration()?
        } else {
            Vec::new()
        };

        Self::hydrate_records(database_core, &mut records, true)?;
        Self::hydrate_virtual_relationships(
            database_core,
            &physical_drawer_name,
            &mut records,
            true,
            context,
        )?;

        Ok(records)
    }

    fn find_by_filter_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        modifiers: Option<QueryModifiers>,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<Value>> {
        let filter_map = query::filter_map(&filter)?;
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);

        let mut records = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )? {
            Self::write_lock(&drawer)?.find_all_records_with_migration()?
        } else {
            Vec::new()
        };

        records.retain(|record| {
            query::record_matches_filter(record, filter_map, context.drawer_namespace)
        });
        query::apply_query_modifiers(&mut records, modifiers.as_ref());
        Self::hydrate_records(database_core, &mut records, true)?;
        Self::hydrate_virtual_relationships(
            database_core,
            &physical_drawer_name,
            &mut records,
            true,
            context,
        )?;

        Ok(records)
    }

    fn count_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        filter: Option<Value>,
        _modifiers: Option<QueryModifiers>,
        context: ExecutionContext<'_>,
    ) -> Result<usize> {
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let Some(filter) = filter else {
            let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
                database_core,
                &physical_drawer_name,
                "_id",
                Vec::new(),
            )?
            else {
                return Ok(0);
            };

            return Ok(Self::read_lock(&drawer)?.record_count());
        };

        let filter_map = query::filter_map(&filter)?;
        let count = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )? {
            Self::write_lock(&drawer)?
                .find_all_records_with_migration()?
                .into_iter()
                .filter(|record| {
                    query::record_matches_filter(record, filter_map, context.drawer_namespace)
                })
                .count()
        } else {
            0
        };

        Ok(count)
    }

    fn vacuum_drawer_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
    ) -> Result<VacuumReport> {
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{}' was not found", drawer_name),
            ));
        };

        Self::write_lock(&drawer)?.vacuum()
    }

    fn migrate_drawer_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
    ) -> Result<VacuumReport> {
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{}' was not found", drawer_name),
            ));
        };

        Self::write_lock(&drawer)?.migrate_all_records()
    }

    fn find_by_id_in_database(
        database_core: &RwLock<Database>,
        pointer: &str,
        context: ExecutionContext<'_>,
    ) -> Result<Option<Value>> {
        let physical_pointer = routing::scoped_pointer(pointer, context.drawer_namespace);
        let (drawer_name, record_key) = pointer::parse_pointer(&physical_pointer)?;

        if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &drawer_name,
            "_id",
            Vec::new(),
        )? {
            let found_record =
                Self::write_lock(&drawer)?.find_by_primary_key_with_migration(&record_key)?;
            if let Some(mut record) = found_record {
                let mut active_pointer_path = HashSet::from([physical_pointer]);
                Self::hydrate_value(database_core, &mut record, false, &mut active_pointer_path)?;
                Self::hydrate_virtual_relationships(
                    database_core,
                    &drawer_name,
                    std::slice::from_mut(&mut record),
                    false,
                    context,
                )?;
                if let Value::Object(ref mut map) = record {
                    map.remove("_id");
                }
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    fn delete_by_id_in_database(
        database_core: &RwLock<Database>,
        locator: StorageLocator,
        context: ExecutionContext<'_>,
    ) -> Result<bool> {
        let pointer = pointer::locator_to_pointer(locator);
        let operation = WalOperation::DeleteById {
            pointer: pointer.clone(),
            drawer_namespace: context.drawer_namespace.map(str::to_string),
        };
        let tx_id = Uuid::new_v4().simple().to_string();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self::append_wal_record(
            database_core,
            &WalRecord::Begin {
                tx_id: tx_id.clone(),
                operation,
                ts: now,
            },
        )?;
        let result = Self::delete_by_id_in_database_unlogged(database_core, &pointer, context);

        if result.is_ok() {
            Self::append_wal_record(database_core, &WalRecord::Commit { tx_id })?;
        } else {
            let _ = Self::append_wal_record(database_core, &WalRecord::Abort { tx_id });
        }

        result
    }

    fn delete_by_id_in_database_unlogged(
        database_core: &RwLock<Database>,
        pointer: &str,
        context: ExecutionContext<'_>,
    ) -> Result<bool> {
        let mut active_delete_path = HashSet::new();
        let physical_pointer = routing::scoped_pointer(pointer, context.drawer_namespace);
        Self::delete_by_id_inner(
            database_core,
            &physical_pointer,
            &mut active_delete_path,
            context,
        )
    }

    fn delete_by_id_inner(
        database_core: &RwLock<Database>,
        pointer: &str,
        active_delete_path: &mut HashSet<String>,
        context: ExecutionContext<'_>,
    ) -> Result<bool> {
        if active_delete_path.contains(pointer) {
            return Ok(false);
        }

        let (drawer_name, record_key) = pointer::parse_pointer(pointer)?;
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{}' could not be loaded for delete", drawer_name),
            ));
        };

        let (record, cascade_fields, inverse_delete_rules) = {
            let mut drawer = Self::write_lock(&drawer)?;
            (
                drawer.find_by_primary_key_with_migration(&record_key)?,
                drawer.cascade_delete_fields(),
                Self::inverse_delete_rules(
                    drawer.delete_rules(),
                    drawer.relationship_constraints(),
                ),
            )
        };
        let Some(record) = record else {
            return Ok(false);
        };

        Self::evaluate_restrict_delete_rules(
            database_core,
            pointer,
            &inverse_delete_rules,
            context,
        )?;

        active_delete_path.insert(pointer.to_string());

        let cascade_child_pointers = Self::collect_inverse_delete_rule_pointers(
            database_core,
            pointer,
            &inverse_delete_rules,
            DeleteAction::Cascade,
            context,
        )?;
        for cascade_pointer in cascade_child_pointers {
            Self::delete_by_id_inner(database_core, &cascade_pointer, active_delete_path, context)?;
        }

        let cascade_pointers = Self::collect_cascade_pointers(&record, &cascade_fields);
        for cascade_pointer in cascade_pointers {
            Self::delete_by_id_inner(database_core, &cascade_pointer, active_delete_path, context)?;
        }

        Self::apply_set_null_delete_rules(database_core, pointer, &inverse_delete_rules, context)?;

        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            active_delete_path.remove(pointer);
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{}' could not be loaded for delete", drawer_name),
            ));
        };

        let deleted_record = Self::write_lock(&drawer)?.delete_by_primary_key(&record_key)?;
        active_delete_path.remove(pointer);

        Ok(deleted_record.is_some())
    }

    fn inverse_delete_rules(
        delete_rules: BTreeMap<String, Value>,
        relationship_constraints: BTreeMap<String, Value>,
    ) -> Vec<InverseDeleteRule> {
        delete_rules
            .into_iter()
            .filter_map(|(field_name, rule)| {
                let action = Self::delete_rule_action(&rule)?;
                let relationship_rule = relationship_constraints.get(&field_name)?;
                let target_drawer = Self::relationship_target_drawer(relationship_rule)?;
                let mapped_by = Self::relationship_mapped_by(relationship_rule)?;

                Some(InverseDeleteRule {
                    field_name,
                    action,
                    target_drawer: target_drawer.to_string(),
                    mapped_by: mapped_by.to_string(),
                })
            })
            .collect()
    }

    fn evaluate_restrict_delete_rules(
        database_core: &RwLock<Database>,
        pointer: &str,
        inverse_delete_rules: &[InverseDeleteRule],
        context: ExecutionContext<'_>,
    ) -> Result<()> {
        let restricted_pointers = Self::collect_inverse_delete_rule_pointers(
            database_core,
            pointer,
            inverse_delete_rules,
            DeleteAction::Restrict,
            context,
        )?;

        if let Some(blocking_pointer) = restricted_pointers.first() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Delete restricted: '{}' is still referenced by '{}' through a Restrict rule",
                    pointer, blocking_pointer
                ),
            ));
        }

        Ok(())
    }

    fn collect_inverse_delete_rule_pointers(
        database_core: &RwLock<Database>,
        pointer: &str,
        inverse_delete_rules: &[InverseDeleteRule],
        action: DeleteAction,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<String>> {
        let mut pointers = Vec::new();

        for rule in inverse_delete_rules
            .iter()
            .filter(|rule| rule.action == action)
        {
            let child_records = Self::records_matching_parent_pointer(
                database_core,
                &rule.target_drawer,
                &rule.mapped_by,
                pointer,
                context,
            )?;

            let physical_target_drawer =
                routing::scoped_drawer_name(&rule.target_drawer, context.drawer_namespace);
            for record in child_records {
                if let Some(child_key) = record.get("_id").and_then(|value| value.as_str()) {
                    pointers.push(pointer::format_pointer(&physical_target_drawer, child_key));
                }
            }
        }

        Ok(pointers)
    }

    fn apply_set_null_delete_rules(
        database_core: &RwLock<Database>,
        pointer: &str,
        inverse_delete_rules: &[InverseDeleteRule],
        context: ExecutionContext<'_>,
    ) -> Result<()> {
        for rule in inverse_delete_rules
            .iter()
            .filter(|rule| rule.action == DeleteAction::SetNull)
        {
            let mut child_records = Self::records_matching_parent_pointer(
                database_core,
                &rule.target_drawer,
                &rule.mapped_by,
                pointer,
                context,
            )?;

            for child_record in &mut child_records {
                Self::clear_parent_pointer_field(child_record, &rule.mapped_by, pointer);
            }

            for child_record in child_records {
                let physical_target_drawer =
                    routing::scoped_drawer_name(&rule.target_drawer, context.drawer_namespace);
                let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
                    database_core,
                    &physical_target_drawer,
                    "_id",
                    Vec::new(),
                )?
                else {
                    return Err(Error::new(
                        ErrorKind::NotFound,
                        format!(
                            "Drawer '{}' could not be loaded for SetNull delete rule '{}'",
                            physical_target_drawer, rule.field_name
                        ),
                    ));
                };

                match Self::write_lock(&drawer)?.upsert_record(child_record)? {
                    Ok(_) => {}
                    Err(validation_error) => {
                        return Err(Error::new(ErrorKind::InvalidData, validation_error));
                    }
                }
            }
        }

        Ok(())
    }

    fn records_matching_parent_pointer(
        database_core: &RwLock<Database>,
        target_drawer: &str,
        mapped_by: &str,
        parent_pointer: &str,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<Value>> {
        let physical_target_drawer =
            routing::scoped_drawer_name(target_drawer, context.drawer_namespace);
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_target_drawer,
            "_id",
            Vec::new(),
        )?
        else {
            return Ok(Vec::new());
        };

        let records = Self::write_lock(&drawer)?
            .find_all_records_with_migration()?
            .into_iter()
            .filter(|record| {
                record
                    .get(mapped_by)
                    .is_some_and(|value| Self::value_contains_pointer(value, parent_pointer))
            })
            .collect();

        Ok(records)
    }

    fn value_contains_pointer(value: &Value, pointer: &str) -> bool {
        match value {
            Value::String(value) => value == pointer,
            Value::Array(values) => values
                .iter()
                .any(|value| Self::value_contains_pointer(value, pointer)),
            Value::Object(map) => map
                .values()
                .any(|value| Self::value_contains_pointer(value, pointer)),
            _ => false,
        }
    }

    fn clear_parent_pointer_field(record: &mut Value, field_name: &str, pointer: &str) -> bool {
        let Value::Object(map) = record else {
            return false;
        };

        let Some(field_value) = map.get_mut(field_name) else {
            return false;
        };

        let mut remove_field = false;
        let changed = match field_value {
            Value::String(value) if value == pointer => {
                remove_field = true;
                true
            }
            Value::Array(values) => {
                let original_len = values.len();
                values.retain(|value| !Self::value_contains_pointer(value, pointer));
                if values.is_empty() {
                    remove_field = true;
                }
                values.len() != original_len
            }
            _ => false,
        };

        if remove_field {
            map.remove(field_name);
        }

        changed
    }

    fn delete_rule_action(rule: &Value) -> Option<DeleteAction> {
        let action = rule
            .as_str()
            .or_else(|| rule.get("action").and_then(|action| action.as_str()))?;

        if action.eq_ignore_ascii_case("Cascade") {
            Some(DeleteAction::Cascade)
        } else if action.eq_ignore_ascii_case("Restrict") {
            Some(DeleteAction::Restrict)
        } else if action.eq_ignore_ascii_case("SetNull") {
            Some(DeleteAction::SetNull)
        } else {
            None
        }
    }

    fn decompose_nested_objects(
        database_core: &RwLock<Database>,
        map: Map<String, Value>,
        current_drawer_name: &str,
        relationship_constraints: &BTreeMap<String, Value>,
        context: ExecutionContext<'_>,
    ) -> Result<Map<String, Value>> {
        let mut continuous_map = Map::new();

        for (key, value) in map {
            let relation_target = Self::relation_target_for_field(
                &key,
                current_drawer_name,
                relationship_constraints,
            );
            let drawer_name =
                Self::drawer_name_for_relation_target(&relation_target, &key, current_drawer_name);
            let processed_value = Self::decompose_relationship_value(
                database_core,
                &drawer_name,
                value,
                relation_target,
                context,
            )?;
            continuous_map.insert(key, processed_value);
        }

        Ok(continuous_map)
    }

    fn register_inline_relationship_aliases(
        drawer_handle: &Arc<RwLock<Drawer>>,
        map: &Map<String, Value>,
        relationship_constraints: &mut BTreeMap<String, Value>,
    ) -> Result<()> {
        let aliases = map
            .iter()
            .filter(|(field_name, _)| field_name.as_str() != "_id")
            .filter(|(field_name, _)| !relationship_constraints.contains_key(field_name.as_str()))
            .filter_map(|(field_name, value)| {
                let drawer_names = pointer::inline_pointer_drawer_names(value);
                if drawer_names.is_empty() {
                    return None;
                }

                Some((
                    field_name.clone(),
                    serde_json::json!({
                        "type": "polymorphic",
                        "target_drawers": drawer_names
                    }),
                ))
            })
            .collect::<Vec<_>>();

        if aliases.is_empty() {
            return Ok(());
        }

        let mut drawer = Self::write_lock(drawer_handle)?;
        for (field_name, rule) in aliases {
            drawer.register_relationship_constraint(&field_name, rule.clone())?;
            relationship_constraints.insert(field_name, rule);
        }

        Ok(())
    }

    fn decompose_relationship_value(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        value: Value,
        relation_target: RelationTarget,
        context: ExecutionContext<'_>,
    ) -> Result<Value> {
        match value {
            Value::Object(child_map) => {
                if let Some(reference_id) = Self::id_only_reference(&child_map) {
                    let normalized_pointer = pointer::normalize_reference_pointer_for_namespace(
                        drawer_name,
                        reference_id,
                        context.drawer_namespace,
                    );
                    Ok(Value::String(normalized_pointer))
                } else {
                    let child_pointer = Self::upsert_in_database_unlogged(
                        database_core,
                        drawer_name,
                        Value::Object(child_map),
                        context,
                    )?;
                    Ok(Value::String(child_pointer))
                }
            }
            Value::Array(values) => values
                .into_iter()
                .map(|item| {
                    Self::decompose_relationship_value(
                        database_core,
                        drawer_name,
                        item,
                        relation_target.clone(),
                        context,
                    )
                })
                .collect::<Result<Vec<_>>>()
                .map(Value::Array),
            Value::String(pointer) if pointer::is_pointer(&pointer) => Ok(Value::String(
                pointer::normalize_reference_pointer_for_namespace(
                    drawer_name,
                    &pointer,
                    context.drawer_namespace,
                ),
            )),
            Value::String(reference_id)
                if Self::should_normalize_plain_string(&relation_target) =>
            {
                Ok(Value::String(
                    pointer::normalize_reference_pointer_for_namespace(
                        drawer_name,
                        &reference_id,
                        context.drawer_namespace,
                    ),
                ))
            }
            other => Ok(other),
        }
    }

    fn id_only_reference(map: &Map<String, Value>) -> Option<&str> {
        if map.len() == 1 {
            map.get("_id").and_then(|value| value.as_str())
        } else {
            None
        }
    }

    fn relationship_drawer_name(field_name: &str) -> String {
        if let Some(stem) = field_name.strip_suffix("ies") {
            return format!("{}y", stem);
        }

        if field_name.ends_with('s')
            && !field_name.ends_with("ss")
            && !field_name.ends_with("us")
            && field_name.len() > 1
        {
            return field_name[..field_name.len() - 1].to_string();
        }

        field_name.to_string()
    }

    fn relation_target_for_field(
        field_name: &str,
        current_drawer_name: &str,
        relationship_constraints: &BTreeMap<String, Value>,
    ) -> RelationTarget {
        let Some(rule) = relationship_constraints.get(field_name) else {
            return RelationTarget::Inferred(Self::relationship_drawer_name(field_name));
        };

        if Self::relationship_constraint_type(rule)
            .is_some_and(|relationship_type| relationship_type.eq_ignore_ascii_case("polymorphic"))
        {
            return RelationTarget::Polymorphic;
        }

        let Some(target_drawer) = Self::relationship_target_drawer(rule) else {
            return RelationTarget::Inferred(Self::relationship_drawer_name(field_name));
        };

        if target_drawer == current_drawer_name
            || current_drawer_name
                .strip_suffix(target_drawer)
                .is_some_and(|prefix| prefix.ends_with('_'))
        {
            RelationTarget::SelfReference
        } else {
            RelationTarget::Static(target_drawer.to_string())
        }
    }

    fn drawer_name_for_relation_target(
        target: &RelationTarget,
        field_name: &str,
        current_drawer_name: &str,
    ) -> String {
        match target {
            RelationTarget::Inferred(drawer_name) => drawer_name.clone(),
            RelationTarget::Static(drawer_name) => drawer_name.clone(),
            RelationTarget::SelfReference => current_drawer_name.to_string(),
            RelationTarget::Polymorphic => Self::relationship_drawer_name(field_name),
        }
    }

    fn should_normalize_plain_string(target: &RelationTarget) -> bool {
        matches!(
            target,
            RelationTarget::Static(_) | RelationTarget::SelfReference
        )
    }

    fn collect_cascade_pointers(record: &Value, cascade_fields: &[String]) -> Vec<String> {
        let mut pointers = Vec::new();

        if let Value::Object(map) = record {
            for field in cascade_fields {
                if let Some(value) = map.get(field) {
                    pointer::collect_pointer_strings(value, &mut pointers);
                }
            }
        }

        pointers
    }

    fn hydrate_records(
        database_core: &RwLock<Database>,
        records: &mut [Value],
        include_ids: bool,
    ) -> Result<()> {
        for record in records {
            let mut active_pointer_path = HashSet::new();
            if let Value::Object(map) = record {
                if let Some(pointer) = map.get("_id").and_then(|value| value.as_str()) {
                    active_pointer_path.insert(pointer.to_string());
                }
            }
            Self::hydrate_value(database_core, record, include_ids, &mut active_pointer_path)?;
        }

        Ok(())
    }

    fn hydrate_virtual_relationships(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        records: &mut [Value],
        include_ids: bool,
        context: ExecutionContext<'_>,
    ) -> Result<()> {
        let virtual_relationships = {
            let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
                database_core,
                drawer_name,
                "_id",
                Vec::new(),
            )?
            else {
                return Ok(());
            };

            Self::read_lock(&drawer)?
                .relationship_constraints()
                .into_iter()
                .filter_map(|(field_name, rule)| {
                    if Self::relationship_constraint_type(&rule) != Some("1:M") {
                        return None;
                    }

                    let target_drawer = Self::relationship_target_drawer(&rule)?.to_string();
                    let mapped_by = Self::relationship_mapped_by(&rule)?.to_string();
                    Some((field_name, target_drawer, mapped_by))
                })
                .collect::<Vec<_>>()
        };

        if virtual_relationships.is_empty() {
            return Ok(());
        }

        for record in records {
            let Some(record_map) = record.as_object_mut() else {
                continue;
            };
            let Some(parent_key) = record_map.get("_id").and_then(|value| value.as_str()) else {
                continue;
            };
            let parent_pointer = pointer::format_pointer(drawer_name, parent_key);

            for (field_name, target_drawer, mapped_by) in &virtual_relationships {
                let mut child_records = Self::virtual_relationship_children(
                    database_core,
                    target_drawer,
                    mapped_by,
                    &parent_pointer,
                    include_ids,
                    context,
                )?;
                if !include_ids {
                    Self::remove_root_ids(&mut child_records);
                }
                record_map.insert(field_name.clone(), Value::Array(child_records));
            }
        }

        Ok(())
    }

    fn virtual_relationship_children(
        database_core: &RwLock<Database>,
        target_drawer: &str,
        mapped_by: &str,
        parent_pointer: &str,
        include_ids: bool,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<Value>> {
        let physical_target_drawer =
            routing::scoped_drawer_name(target_drawer, context.drawer_namespace);
        let mut child_records = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_target_drawer,
            "_id",
            Vec::new(),
        )? {
            Self::write_lock(&drawer)?.find_all_records_with_migration()?
        } else {
            Vec::new()
        };

        child_records.retain(|record| {
            record.get(mapped_by).and_then(|value| value.as_str()) == Some(parent_pointer)
        });
        Self::hydrate_records(database_core, &mut child_records, include_ids)?;

        Ok(child_records)
    }

    fn remove_root_ids(records: &mut [Value]) {
        for record in records {
            if let Value::Object(map) = record {
                map.remove("_id");
            }
        }
    }

    fn relationship_constraint_type(rule: &Value) -> Option<&str> {
        rule.get("type").and_then(|value| value.as_str())
    }

    fn relationship_target_drawer(rule: &Value) -> Option<&str> {
        rule.get("target_drawer").and_then(|value| value.as_str())
    }

    fn relationship_mapped_by(rule: &Value) -> Option<&str> {
        rule.get("mapped_by").and_then(|value| value.as_str())
    }

    fn hydrate_value(
        database_core: &RwLock<Database>,
        current_value: &mut Value,
        include_ids: bool,
        active_pointer_path: &mut HashSet<String>,
    ) -> Result<()> {
        match current_value {
            Value::Object(map) => {
                let pointer_updates = map
                    .iter()
                    .filter_map(|(field_name, field_value)| {
                        if field_name == "_id" {
                            return None;
                        }

                        field_value
                            .as_str()
                            .filter(|pointer| pointer::is_pointer(pointer))
                            .map(|pointer| (field_name.clone(), pointer.to_string()))
                    })
                    .collect::<Vec<_>>();

                for (field_name, pointer) in pointer_updates {
                    if let Some(resolved_value) = Self::resolve_pointer(
                        database_core,
                        &pointer,
                        include_ids,
                        active_pointer_path,
                    )? {
                        if let Some(value_ref) = map.get_mut(&field_name) {
                            *value_ref = resolved_value;
                        }
                    }
                }

                for (field_name, field_value) in map.iter_mut() {
                    if field_name != "_id" {
                        Self::hydrate_value(
                            database_core,
                            field_value,
                            include_ids,
                            active_pointer_path,
                        )?;
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    if let Some(pointer) = value
                        .as_str()
                        .filter(|pointer| pointer::is_pointer(pointer))
                        .map(|pointer| pointer.to_string())
                    {
                        if let Some(resolved_value) = Self::resolve_pointer(
                            database_core,
                            &pointer,
                            include_ids,
                            active_pointer_path,
                        )? {
                            *value = resolved_value;
                            continue;
                        }
                    }

                    Self::hydrate_value(database_core, value, include_ids, active_pointer_path)?;
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn resolve_pointer(
        database_core: &RwLock<Database>,
        pointer: &str,
        include_ids: bool,
        active_pointer_path: &mut HashSet<String>,
    ) -> Result<Option<Value>> {
        let (drawer_name, record_key) = pointer::parse_pointer(pointer)?;
        let canonical_pointer = pointer::format_pointer(&drawer_name, &record_key);

        if active_pointer_path.contains(&canonical_pointer) {
            return Ok(None);
        }

        let mut record = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &drawer_name,
            "_id",
            Vec::new(),
        )? {
            Self::write_lock(&drawer)?.find_by_primary_key_with_migration(&record_key)?
        } else {
            None
        };

        if let Some(ref mut record_value) = record {
            active_pointer_path.insert(canonical_pointer.clone());
            Self::hydrate_value(
                database_core,
                record_value,
                include_ids,
                active_pointer_path,
            )?;
            active_pointer_path.remove(&canonical_pointer);

            if !include_ids {
                if let Value::Object(map) = record_value {
                    map.remove("_id");
                }
            }
        }

        Ok(record)
    }
}
