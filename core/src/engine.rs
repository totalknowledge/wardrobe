use crate::wrdb_lib::catalog_lifecycle;
use crate::wrdb_lib::catalog_validation;
use crate::wrdb_lib::command_dispatch;
use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::delete_rules;
use crate::wrdb_lib::discovery;
use crate::wrdb_lib::drawer::{Drawer, VacuumReport};
use crate::wrdb_lib::engine_wal;
use crate::wrdb_lib::hydration;
use crate::wrdb_lib::nested_decomposition;
use crate::wrdb_lib::pointer;
use crate::wrdb_lib::query;
use crate::wrdb_lib::registry::CatalogRegistry;
use crate::wrdb_lib::relationship;
use crate::wrdb_lib::routing::{self, DatabaseRoute, ExecutionContext};
use crate::wrdb_lib::wal::WalVerification;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind, Result};
use std::path::PathBuf;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use uuid::Uuid;

pub use crate::wrdb_lib::command::{Command, CommandResult};
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
        let root_directory = PathBuf::from(directory);
        let registry = CatalogRegistry::open_or_initialize(&root_directory)?;
        let (default_wal_size_threshold, default_wal_ops_threshold) =
            Database::default_wal_thresholds();
        let (wal_size_threshold_bytes, wal_ops_threshold_count) =
            wal_thresholds.unwrap_or((default_wal_size_threshold, default_wal_ops_threshold));
        let database_core = Database::initialize_with_cache_limit_and_wal_thresholds(
            &root_directory,
            max_cached_drawers,
            wal_size_threshold_bytes,
            wal_ops_threshold_count,
        )?;
        let database_core = RwLock::new(database_core);
        engine_wal::recover_database::<Self>(&database_core)?;
        Ok(Self {
            root_directory,
            registry: RwLock::new(registry),
            database_core,
            routed_databases: RwLock::new(HashMap::new()),
            max_cached_drawers,
            wal_size_threshold_bytes,
            wal_ops_threshold_count,
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

    pub fn manage_schema(
        &self,
        drawer_name: &str,
        action: &str,
        kind: &str,
        field_name: &str,
        payload: Value,
    ) -> Result<Value> {
        Self::manage_schema_in_database(
            &self.database_core,
            drawer_name,
            action,
            kind,
            field_name,
            payload,
            ExecutionContext::root(),
        )
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
        engine_wal::verify(&self.root_directory, database_name)
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
        let registry = Self::read_lock(&self.registry)?;
        command_dispatch::validate_command_against_registry(
            &registry,
            &routing::coordinate_catalog_database(&coordinate),
            coordinate.schema(),
            &command,
        )?;
        let database_path = routing::coordinate_database_path(&self.root_directory, &coordinate)?;
        engine_wal::append_command(&database_path, Some(coordinate.schema()), &command)?;
        let database = self.database_for_route(DatabaseRoute::Coordinate(coordinate))?;
        command_dispatch::execute_in_database::<Self>(&database, command, None)
    }

    pub fn execute_in_scope(&self, scope: StorageScope, command: Command) -> Result<CommandResult> {
        routing::validate_scope(&scope)?;
        if let StorageScope::Schema { database, schema } = &scope {
            let registry = Self::read_lock(&self.registry)?;
            command_dispatch::validate_command_against_registry(
                &registry, database, schema, &command,
            )?;
        }

        match scope {
            StorageScope::Tenant {
                tenant_id,
                database,
                schema,
            } => self.execute_for_tenant(&tenant_id, &database, &schema, command),
            StorageScope::Database { database } => {
                let database_path = routing::database_scope_path(&self.root_directory, &database)?;
                engine_wal::append_command(&database_path, None, &command)?;
                let database = self.database_for_route(DatabaseRoute::Database(database))?;
                command_dispatch::execute_in_database::<Self>(&database, command, None)
            }
            StorageScope::Schema { database, schema } => {
                let database_path =
                    routing::schema_scope_path(&self.root_directory, &database, &schema)?;
                engine_wal::append_command(&database_path, Some(&schema), &command)?;
                let database =
                    self.database_for_route(DatabaseRoute::Schema { database, schema })?;
                command_dispatch::execute_in_database::<Self>(&database, command, None)
            }
            StorageScope::Drawer { namespace } => {
                engine_wal::append_command(&self.root_directory, Some(&namespace), &command)?;
                command_dispatch::execute_in_database::<Self>(
                    &self.database_core,
                    command,
                    Some(namespace.as_str()),
                )
            }
        }
    }

    pub fn create_database(&self, database_name: &str) -> Result<StorageInventory> {
        catalog_lifecycle::create_database(
            &self.root_directory,
            &self.registry,
            database_name,
            |command| engine_wal::append_command(&self.root_directory, None, command),
        )
    }

    pub fn create_schema(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<StorageInventory> {
        catalog_lifecycle::create_schema(
            &self.root_directory,
            &self.registry,
            database_name,
            schema_name,
            |command| engine_wal::append_command(&self.root_directory, None, command),
        )
    }

    pub fn create_drawer(
        &self,
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> Result<StorageInventory> {
        catalog_lifecycle::create_drawer(
            &self.root_directory,
            &self.registry,
            database_name,
            schema_name,
            drawer_name,
            |command| engine_wal::append_command(&self.root_directory, None, command),
        )
    }

    pub fn register_tenant_route(
        &self,
        tenant_id: &str,
        database_name: &str,
        location: &str,
    ) -> Result<StorageInventory> {
        catalog_lifecycle::register_tenant_route(
            &self.root_directory,
            &self.registry,
            tenant_id,
            database_name,
            location,
            |command| engine_wal::append_command(&self.root_directory, None, command),
        )
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

        let registry = Self::read_lock(&self.registry)?;
        command_dispatch::validate_command_against_registry(
            &registry,
            database_name,
            schema_name,
            &command,
        )?;

        let route_path =
            catalog_validation::catalog_location_path(&self.root_directory, &tenant_route.location);
        let schema_path = routing::tenant_schema_path(&route_path, schema_name);
        engine_wal::append_command(&schema_path, Some(schema_name), &command)?;
        let routed_database =
            RwLock::new(Database::initialize_with_cache_limit_and_wal_thresholds(
                &schema_path,
                self.max_cached_drawers,
                self.wal_size_threshold_bytes,
                self.wal_ops_threshold_count,
            )?);
        engine_wal::recover_database::<Self>(&routed_database)?;
        command_dispatch::execute_in_database::<Self>(&routed_database, command, None)
    }

    pub fn execute_command(&self, command: Command) -> Result<CommandResult> {
        command_dispatch::execute_command(self, command)
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
            let database = Database::initialize_with_cache_limit_and_wal_thresholds(
                storage_path,
                self.max_cached_drawers,
                self.wal_size_threshold_bytes,
                self.wal_ops_threshold_count,
            )?;
            let database = Arc::new(RwLock::new(database));
            engine_wal::recover_database::<Self>(&database)?;
            routed_databases.insert(route.clone(), database);
        }

        routed_databases.get(&route).cloned().ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                "Failed to acquire routed database handle",
            )
        })
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

    fn upsert_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<String> {
        let wal_payload = payload.clone();
        engine_wal::run_upsert_transaction(
            database_core,
            drawer_name,
            &wal_payload,
            context,
            || Self::upsert_in_database_unlogged(database_core, drawer_name, payload, context),
        )
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
            nested_decomposition::register_inline_relationship_aliases(
                &map,
                &mut relationship_constraints,
                |field_name, rule| {
                    Self::write_lock(&drawer_handle)?
                        .register_relationship_constraint(field_name, rule)
                        .map_err(|error| Error::new(ErrorKind::InvalidData, error))
                },
            )?;
            let processed_map = nested_decomposition::decompose_nested_objects(
                map,
                &physical_drawer_name,
                &relationship_constraints,
                context,
                |drawer_name, value, child_context| {
                    Self::upsert_in_database_unlogged(
                        database_core,
                        drawer_name,
                        value,
                        child_context,
                    )
                },
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

        hydration::hydrate_records(&mut records, true, |drawer_name, record_key| {
            Self::fetch_record_for_hydration(database_core, drawer_name, record_key)
        })?;
        Self::attach_virtual_relationships(
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
        hydration::hydrate_records(&mut records, true, |drawer_name, record_key| {
            Self::fetch_record_for_hydration(database_core, drawer_name, record_key)
        })?;
        Self::attach_virtual_relationships(
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
                hydration::hydrate_value(
                    &mut record,
                    false,
                    &mut active_pointer_path,
                    &mut |drawer_name, record_key| {
                        Self::fetch_record_for_hydration(database_core, drawer_name, record_key)
                    },
                )?;
                Self::attach_virtual_relationships(
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
        engine_wal::run_delete_transaction(database_core, &pointer, context, || {
            Self::delete_by_id_in_database_unlogged(database_core, &pointer, context)
        })
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
                delete_rules::inverse_delete_rules(
                    drawer.delete_rules(),
                    drawer.relationship_constraints(),
                ),
            )
        };
        let Some(record) = record else {
            return Ok(false);
        };

        delete_rules::evaluate_restrict_delete_rules(
            pointer,
            &inverse_delete_rules,
            context.drawer_namespace,
            |target_drawer, mapped_by, parent_pointer| {
                Self::records_matching_parent_pointer(
                    database_core,
                    target_drawer,
                    mapped_by,
                    parent_pointer,
                    context,
                )
            },
        )?;

        active_delete_path.insert(pointer.to_string());

        let cascade_child_pointers = delete_rules::collect_inverse_delete_rule_pointers(
            pointer,
            &inverse_delete_rules,
            delete_rules::DeleteAction::Cascade,
            context.drawer_namespace,
            |target_drawer, mapped_by, parent_pointer| {
                Self::records_matching_parent_pointer(
                    database_core,
                    target_drawer,
                    mapped_by,
                    parent_pointer,
                    context,
                )
            },
        )?;
        for cascade_pointer in cascade_child_pointers {
            Self::delete_by_id_inner(database_core, &cascade_pointer, active_delete_path, context)?;
        }

        let cascade_pointers = delete_rules::collect_cascade_pointers(&record, &cascade_fields);
        for cascade_pointer in cascade_pointers {
            Self::delete_by_id_inner(database_core, &cascade_pointer, active_delete_path, context)?;
        }

        delete_rules::apply_set_null_delete_rules(
            pointer,
            &inverse_delete_rules,
            context.drawer_namespace,
            |target_drawer, mapped_by, parent_pointer| {
                Self::records_matching_parent_pointer(
                    database_core,
                    target_drawer,
                    mapped_by,
                    parent_pointer,
                    context,
                )
            },
            |physical_target_drawer, field_name, child_record| {
                let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
                    database_core,
                    physical_target_drawer,
                    "_id",
                    Vec::new(),
                )?
                else {
                    return Err(Error::new(
                        ErrorKind::NotFound,
                        format!(
                            "Drawer '{}' could not be loaded for SetNull delete rule '{}'",
                            physical_target_drawer, field_name
                        ),
                    ));
                };

                match Self::write_lock(&drawer)?.upsert_record(child_record)? {
                    Ok(_) => Ok(()),
                    Err(validation_error) => {
                        Err(Error::new(ErrorKind::InvalidData, validation_error))
                    }
                }
            },
        )?;

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

    fn manage_schema_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        action: &str,
        kind: &str,
        field_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<Value> {
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
                format!("Drawer '{drawer_name}' could not be loaded for schema management"),
            ));
        };

        Self::write_lock(&drawer)?.manage_schema_rule(action, kind, field_name, payload)
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
                record.get(mapped_by).is_some_and(|value| {
                    delete_rules::value_contains_pointer(value, parent_pointer)
                })
            })
            .collect();

        Ok(records)
    }

    fn fetch_record_for_hydration(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        record_key: &str,
    ) -> Result<Option<Value>> {
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Ok(None);
        };

        Self::write_lock(&drawer)?.find_by_primary_key_with_migration(record_key)
    }

    fn attach_virtual_relationships(
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

            relationship::virtual_relationships(
                Self::read_lock(&drawer)?.relationship_constraints(),
            )
        };

        hydration::hydrate_virtual_relationships(
            drawer_name,
            records,
            &virtual_relationships,
            include_ids,
            |relationship, parent_pointer, include_ids| {
                Self::virtual_relationship_children(
                    database_core,
                    &relationship.target_drawer,
                    &relationship.mapped_by,
                    parent_pointer,
                    include_ids,
                    context,
                )
            },
        )
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
        hydration::hydrate_records(
            &mut child_records,
            include_ids,
            |drawer_name, record_key| {
                Self::fetch_record_for_hydration(database_core, drawer_name, record_key)
            },
        )?;

        Ok(child_records)
    }
}

impl command_dispatch::BoundaryCommandExecutor for WardrobeEngine {
    fn append_boundary_wal(&self, command: &Command) -> Result<()> {
        engine_wal::append_command(&self.root_directory, None, command)
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
        command_dispatch::execute_in_database::<Self>(&self.database_core, command, None)
    }
}

impl command_dispatch::DatabaseCommandExecutor for WardrobeEngine {
    fn upsert_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<String> {
        WardrobeEngine::upsert_in_database(database, drawer_name, payload, context)
    }

    fn find_all_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<Value>> {
        WardrobeEngine::find_all_in_database(database, drawer_name, context)
    }

    fn find_by_id_in_database(
        database: &RwLock<Database>,
        pointer: &str,
        context: ExecutionContext<'_>,
    ) -> Result<Option<Value>> {
        WardrobeEngine::find_by_id_in_database(database, pointer, context)
    }

    fn find_by_filter_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        modifiers: Option<QueryModifiers>,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<Value>> {
        WardrobeEngine::find_by_filter_in_database(
            database,
            drawer_name,
            filter,
            modifiers,
            context,
        )
    }

    fn count_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        filter: Option<Value>,
        modifiers: Option<QueryModifiers>,
        context: ExecutionContext<'_>,
    ) -> Result<usize> {
        WardrobeEngine::count_in_database(database, drawer_name, filter, modifiers, context)
    }

    fn delete_by_id_in_database(
        database: &RwLock<Database>,
        locator: StorageLocator,
        context: ExecutionContext<'_>,
    ) -> Result<bool> {
        WardrobeEngine::delete_by_id_in_database(database, locator, context)
    }

    fn manage_schema_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        action: &str,
        kind: &str,
        field_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<Value> {
        WardrobeEngine::manage_schema_in_database(
            database,
            drawer_name,
            action,
            kind,
            field_name,
            payload,
            context,
        )
    }

    fn vacuum_drawer_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
    ) -> Result<VacuumReport> {
        WardrobeEngine::vacuum_drawer_in_database(database, drawer_name, context)
    }

    fn migrate_drawer_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
    ) -> Result<VacuumReport> {
        WardrobeEngine::migrate_drawer_in_database(database, drawer_name, context)
    }
}

impl engine_wal::WalReplayExecutor for WardrobeEngine {
    fn replay_upsert(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<()> {
        WardrobeEngine::upsert_in_database_unlogged(database_core, drawer_name, payload, context)
            .map(|_| ())
    }

    fn replay_delete(
        database_core: &RwLock<Database>,
        pointer: &str,
        context: ExecutionContext<'_>,
    ) -> Result<()> {
        WardrobeEngine::delete_by_id_in_database_unlogged(database_core, pointer, context)
            .map(|_| ())
    }
}
