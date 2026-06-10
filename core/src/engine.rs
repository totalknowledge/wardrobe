use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::drawer::{Drawer, VacuumReport};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::io::{Error, ErrorKind, Result};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryModifiers {
    pub order_by: Option<String>,
    pub order_direction: Option<OrderDirection>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorageCoordinate {
    tenant: String,
    database: String,
    schema: String,
}

impl StorageCoordinate {
    pub fn new(tenant: &str, database: &str, schema: &str) -> Self {
        Self {
            tenant: tenant.to_string(),
            database: database.to_string(),
            schema: schema.to_string(),
        }
    }

    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    fn validate(&self) -> Result<()> {
        Self::validate_component("tenant", &self.tenant)?;
        Self::validate_component("database", &self.database)?;
        Self::validate_component("schema", &self.schema)
    }

    fn validate_component(label: &str, value: &str) -> Result<()> {
        if value.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Storage coordinate {label} cannot be empty"),
            ));
        }

        let mut components = Path::new(value).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Storage coordinate {label} must be a single path segment"),
            ));
        }

        Ok(())
    }

    fn path_under(&self, root_directory: &Path) -> PathBuf {
        root_directory
            .join(&self.tenant)
            .join(&self.database)
            .join(&self.schema)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StorageLocator {
    Explicit { drawer: String, id: String },
    Inline(String),
}

impl StorageLocator {
    pub fn explicit(drawer: &str, id: &str) -> Self {
        Self::Explicit {
            drawer: drawer.to_string(),
            id: id.to_string(),
        }
    }

    pub fn inline(locator: &str) -> Self {
        Self::Inline(locator.to_string())
    }
}

impl From<&str> for StorageLocator {
    fn from(locator: &str) -> Self {
        Self::Inline(locator.to_string())
    }
}

impl From<String> for StorageLocator {
    fn from(locator: String) -> Self {
        Self::Inline(locator)
    }
}

impl From<&String> for StorageLocator {
    fn from(locator: &String) -> Self {
        Self::Inline(locator.clone())
    }
}

impl From<(&str, &str)> for StorageLocator {
    fn from((drawer, id): (&str, &str)) -> Self {
        Self::explicit(drawer, id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StorageScope {
    Database { database: String },
    Schema { database: String, schema: String },
    Drawer { namespace: String },
}

impl StorageScope {
    pub fn database(database: &str) -> Self {
        Self::Database {
            database: database.to_string(),
        }
    }

    pub fn schema(database: &str, schema: &str) -> Self {
        Self::Schema {
            database: database.to_string(),
            schema: schema.to_string(),
        }
    }

    pub fn drawer(namespace: &str) -> Self {
        Self::Drawer {
            namespace: namespace.to_string(),
        }
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::Database { database } => {
                StorageCoordinate::validate_component("database", database)
            }
            Self::Schema { database, schema } => {
                StorageCoordinate::validate_component("database", database)?;
                StorageCoordinate::validate_component("schema", schema)
            }
            Self::Drawer { namespace } => {
                StorageCoordinate::validate_component("namespace", namespace)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DatabaseRoute {
    Coordinate(StorageCoordinate),
    Database(String),
    Schema { database: String, schema: String },
}

#[derive(Clone, Copy)]
struct ExecutionContext<'a> {
    drawer_namespace: Option<&'a str>,
}

impl ExecutionContext<'_> {
    fn root() -> Self {
        Self {
            drawer_namespace: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Upsert {
        drawer_name: String,
        payload: Value,
    },
    FindAll {
        drawer_name: String,
    },
    FindById {
        pointer: String,
    },
    FindByFilter {
        drawer_name: String,
        filter: Value,
        modifiers: Option<QueryModifiers>,
    },
    Count {
        drawer_name: String,
        filter: Option<Value>,
        modifiers: Option<QueryModifiers>,
    },
    Delete {
        pointer: String,
    },
    Vacuum {
        drawer_name: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandResult {
    Pointer(String),
    Records(Vec<Value>),
    Record(Option<Value>),
    Count(usize),
    Deleted(bool),
    Vacuumed(VacuumReport),
}

enum SortableValue<'a> {
    Bool(bool),
    Number(f64),
    String(&'a str),
}

#[derive(Clone, Copy)]
enum SortableType {
    Bool,
    Number,
    String,
}

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
    },
    Commit {
        tx_id: String,
    },
    Abort {
        tx_id: String,
    },
}

impl SortableValue<'_> {
    fn compare_same_type(&self, other: Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Bool(left), Self::Bool(right)) => Some(left.cmp(&right)),
            (Self::Number(left), Self::Number(right)) => left.partial_cmp(&right),
            (Self::String(left), Self::String(right)) => Some(left.cmp(&right)),
            _ => None,
        }
    }
}

pub struct WardrobeEngine {
    root_directory: PathBuf,
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
        let database_core = Database::initialize_with_cache_limit(directory, max_cached_drawers)?;
        let database_core = RwLock::new(database_core);
        Self::recover_database(&database_core)?;
        Ok(Self {
            root_directory: PathBuf::from(directory),
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

    pub fn cached_drawer_count(&self) -> Result<usize> {
        Ok(Self::read_lock(&self.database_core)?.cached_drawer_count())
    }

    pub fn execute(
        &self,
        coordinate: StorageCoordinate,
        command: Command,
    ) -> Result<CommandResult> {
        let database = self.database_for_route(DatabaseRoute::Coordinate(coordinate))?;
        Self::execute_in_database(&database, command, None)
    }

    pub fn execute_in_scope(&self, scope: StorageScope, command: Command) -> Result<CommandResult> {
        scope.validate()?;

        match scope {
            StorageScope::Database { database } => {
                let database = self.database_for_route(DatabaseRoute::Database(database))?;
                Self::execute_in_database(&database, command, None)
            }
            StorageScope::Schema { database, schema } => {
                let database =
                    self.database_for_route(DatabaseRoute::Schema { database, schema })?;
                Self::execute_in_database(&database, command, None)
            }
            StorageScope::Drawer { namespace } => {
                Self::execute_in_database(&self.database_core, command, Some(namespace.as_str()))
            }
        }
    }

    fn execute_in_database(
        database: &RwLock<Database>,
        command: Command,
        drawer_namespace: Option<&str>,
    ) -> Result<CommandResult> {
        let context = ExecutionContext { drawer_namespace };

        match command {
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
        }
    }

    fn database_for_route(&self, route: DatabaseRoute) -> Result<Arc<RwLock<Database>>> {
        let storage_path = match &route {
            DatabaseRoute::Coordinate(coordinate) => {
                coordinate.validate()?;
                coordinate.path_under(&self.root_directory)
            }
            DatabaseRoute::Database(database) => {
                StorageCoordinate::validate_component("database", database)?;
                self.root_directory.join(database)
            }
            DatabaseRoute::Schema { database, schema } => {
                StorageCoordinate::validate_component("database", database)?;
                StorageCoordinate::validate_component("schema", schema)?;
                self.root_directory.join(database).join(schema)
            }
        };

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
        Ok(())
    }

    fn recover_database(database_core: &RwLock<Database>) -> Result<()> {
        let wal_path = Self::wal_path(database_core)?;
        if !wal_path.exists() {
            return Ok(());
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
                WalRecord::Begin { tx_id, operation } => {
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

        Self::append_wal_record(
            database_core,
            &WalRecord::Begin {
                tx_id: tx_id.clone(),
                operation,
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
            let physical_drawer_name = Self::scoped_drawer_name(drawer_name, context);

            let record_key = match map.get(target_primary_key).and_then(|v| v.as_str()) {
                Some(existing_id) => Self::normalize_primary_key(
                    &physical_drawer_name,
                    drawer_name,
                    existing_id,
                    context,
                ),
                None => Uuid::new_v4().simple().to_string(),
            };
            let record_pointer = Self::format_pointer(&physical_drawer_name, &record_key);

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
        let physical_drawer_name = Self::scoped_drawer_name(drawer_name, context);
        let mut records = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )? {
            Self::read_lock(&drawer)?.find_all_records()?
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
        let filter_map = Self::filter_map(&filter)?;
        let physical_drawer_name = Self::scoped_drawer_name(drawer_name, context);

        let mut records = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )? {
            Self::read_lock(&drawer)?.find_all_records()?
        } else {
            Vec::new()
        };

        records.retain(|record| Self::record_matches_filter(record, filter_map, context));
        Self::apply_query_modifiers(&mut records, modifiers.as_ref());
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
        let physical_drawer_name = Self::scoped_drawer_name(drawer_name, context);
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

        let filter_map = Self::filter_map(&filter)?;
        let count = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )? {
            Self::read_lock(&drawer)?
                .find_all_records()?
                .into_iter()
                .filter(|record| Self::record_matches_filter(record, filter_map, context))
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
        let physical_drawer_name = Self::scoped_drawer_name(drawer_name, context);
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

    fn find_by_id_in_database(
        database_core: &RwLock<Database>,
        pointer: &str,
        context: ExecutionContext<'_>,
    ) -> Result<Option<Value>> {
        let physical_pointer = Self::scoped_pointer(pointer, context);
        let (drawer_name, record_key) = Self::parse_pointer(&physical_pointer)?;

        if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &drawer_name,
            "_id",
            Vec::new(),
        )? {
            let found_record = Self::read_lock(&drawer)?.find_by_primary_key(&record_key)?;
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
        let pointer = Self::locator_to_pointer(locator);
        let operation = WalOperation::DeleteById {
            pointer: pointer.clone(),
            drawer_namespace: context.drawer_namespace.map(str::to_string),
        };
        let tx_id = Uuid::new_v4().simple().to_string();

        Self::append_wal_record(
            database_core,
            &WalRecord::Begin {
                tx_id: tx_id.clone(),
                operation,
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
        let physical_pointer = Self::scoped_pointer(pointer, context);
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

        let (drawer_name, record_key) = Self::parse_pointer(pointer)?;
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
            let drawer = Self::read_lock(&drawer)?;
            (
                drawer.find_by_primary_key(&record_key)?,
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

            let physical_target_drawer = Self::scoped_drawer_name(&rule.target_drawer, context);
            for record in child_records {
                if let Some(child_key) = record.get("_id").and_then(|value| value.as_str()) {
                    pointers.push(Self::format_pointer(&physical_target_drawer, child_key));
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
                let physical_target_drawer = Self::scoped_drawer_name(&rule.target_drawer, context);
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
        let physical_target_drawer = Self::scoped_drawer_name(target_drawer, context);
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_target_drawer,
            "_id",
            Vec::new(),
        )?
        else {
            return Ok(Vec::new());
        };

        let records = Self::read_lock(&drawer)?
            .find_all_records()?
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
                let drawer_names = Self::inline_pointer_drawer_names(value);
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

    fn inline_pointer_drawer_names(value: &Value) -> Vec<String> {
        let mut drawer_names = Vec::new();
        Self::collect_inline_pointer_drawer_names(value, &mut drawer_names);
        drawer_names.sort();
        drawer_names.dedup();
        drawer_names
    }

    fn collect_inline_pointer_drawer_names(value: &Value, drawer_names: &mut Vec<String>) {
        match value {
            Value::String(pointer) => {
                if let Some((drawer_name, _)) = Self::try_parse_pointer(pointer) {
                    drawer_names.push(drawer_name);
                }
            }
            Value::Array(values) => {
                for value in values {
                    Self::collect_inline_pointer_drawer_names(value, drawer_names);
                }
            }
            Value::Object(map) => {
                for value in map.values() {
                    Self::collect_inline_pointer_drawer_names(value, drawer_names);
                }
            }
            _ => {}
        }
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
                    let normalized_pointer = Self::normalize_reference_pointer_for_context(
                        drawer_name,
                        reference_id,
                        context,
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
            Value::String(pointer) if Self::is_pointer(&pointer) => Ok(Value::String(
                Self::normalize_reference_pointer_for_context(drawer_name, &pointer, context),
            )),
            Value::String(reference_id)
                if Self::should_normalize_plain_string(&relation_target) =>
            {
                Ok(Value::String(
                    Self::normalize_reference_pointer_for_context(
                        drawer_name,
                        &reference_id,
                        context,
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

    fn normalize_reference_pointer(drawer_name: &str, reference_id: &str) -> String {
        if let Some((pointer_drawer, pointer_key)) = Self::try_parse_pointer(reference_id) {
            return Self::format_pointer(&pointer_drawer, &pointer_key);
        }

        Self::format_pointer(drawer_name, &Self::clean_primary_key_token(reference_id))
    }

    fn normalize_reference_pointer_for_context(
        drawer_name: &str,
        reference_id: &str,
        context: ExecutionContext<'_>,
    ) -> String {
        let Some(_) = context.drawer_namespace else {
            return Self::normalize_reference_pointer(drawer_name, reference_id);
        };

        if let Some((pointer_drawer, pointer_key)) = Self::try_parse_pointer(reference_id) {
            let physical_pointer_drawer = Self::scoped_drawer_name(&pointer_drawer, context);
            return Self::format_pointer(&physical_pointer_drawer, &pointer_key);
        }

        let physical_drawer_name = Self::scoped_drawer_name(drawer_name, context);
        Self::normalize_reference_pointer(&physical_drawer_name, reference_id)
    }

    fn normalize_primary_key(
        physical_drawer_name: &str,
        logical_drawer_name: &str,
        existing_id: &str,
        context: ExecutionContext<'_>,
    ) -> String {
        if let Some((pointer_drawer, pointer_key)) = Self::try_parse_pointer(existing_id) {
            if pointer_drawer == logical_drawer_name || pointer_drawer == physical_drawer_name {
                return pointer_key;
            }
        }

        let Some(_) = context.drawer_namespace else {
            return Self::clean_primary_key_token(existing_id);
        };

        Self::clean_primary_key_token(existing_id)
    }

    fn scoped_drawer_name(drawer_name: &str, context: ExecutionContext<'_>) -> String {
        let Some(namespace) = context.drawer_namespace else {
            return drawer_name.to_string();
        };

        let prefix = format!("{namespace}_");
        if drawer_name.starts_with(&prefix) {
            drawer_name.to_string()
        } else {
            format!("{prefix}{drawer_name}")
        }
    }

    fn scoped_pointer(pointer: &str, context: ExecutionContext<'_>) -> String {
        let Some((drawer_name, record_key)) = Self::try_parse_pointer(pointer) else {
            return pointer.to_string();
        };

        let Some(_) = context.drawer_namespace else {
            return Self::format_pointer(&drawer_name, &record_key);
        };

        let physical_drawer_name = Self::scoped_drawer_name(&drawer_name, context);
        Self::format_pointer(&physical_drawer_name, &record_key)
    }

    fn locator_to_pointer(locator: StorageLocator) -> String {
        match locator {
            StorageLocator::Explicit { drawer, id } => Self::format_pointer(&drawer, &id),
            StorageLocator::Inline(pointer) => pointer,
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
                    Self::collect_pointer_strings(value, &mut pointers);
                }
            }
        }

        pointers
    }

    fn record_matches_filter(
        record: &Value,
        filter_map: &Map<String, Value>,
        context: ExecutionContext<'_>,
    ) -> bool {
        let Value::Object(record_map) = record else {
            return false;
        };

        filter_map.iter().all(|(field_name, expected_value)| {
            record_map.get(field_name).is_some_and(|actual_value| {
                Self::field_matches_filter(field_name, actual_value, expected_value, context)
            })
        })
    }

    fn field_matches_filter(
        field_name: &str,
        actual_value: &Value,
        expected_value: &Value,
        context: ExecutionContext<'_>,
    ) -> bool {
        match expected_value {
            Value::String(expected_string) => actual_value.as_str().is_some_and(|actual_string| {
                Self::matches_string_filter(actual_string, expected_string)
            }),
            Value::Object(expected_map) => {
                if let Some(reference_id) = Self::id_only_reference(expected_map) {
                    let relationship_drawer = Self::relationship_drawer_name(field_name);
                    let normalized_pointer = Self::normalize_reference_pointer_for_context(
                        &relationship_drawer,
                        reference_id,
                        context,
                    );
                    return actual_value.as_str() == Some(normalized_pointer.as_str());
                }

                let Value::Object(actual_map) = actual_value else {
                    return false;
                };

                expected_map.iter().all(|(nested_field, nested_expected)| {
                    actual_map.get(nested_field).is_some_and(|nested_actual| {
                        Self::field_matches_filter(
                            nested_field,
                            nested_actual,
                            nested_expected,
                            context,
                        )
                    })
                })
            }
            Value::Array(expected_array) => {
                let Value::Array(actual_array) = actual_value else {
                    return false;
                };

                actual_array.len() == expected_array.len()
                    && actual_array.iter().zip(expected_array.iter()).all(
                        |(actual_item, expected_item)| {
                            Self::field_matches_filter(
                                field_name,
                                actual_item,
                                expected_item,
                                context,
                            )
                        },
                    )
            }
            _ => actual_value == expected_value,
        }
    }

    fn matches_string_filter(actual_value: &str, expected_filter: &str) -> bool {
        if !expected_filter.contains('%') {
            return actual_value == expected_filter;
        }

        let actual_bytes = actual_value.as_bytes();
        let filter_bytes = expected_filter.as_bytes();
        let mut actual_index = 0usize;
        let mut filter_index = 0usize;
        let mut wildcard_index = None;
        let mut wildcard_match_start = 0usize;

        while actual_index < actual_bytes.len() {
            if filter_index < filter_bytes.len()
                && filter_bytes[filter_index] == actual_bytes[actual_index]
            {
                actual_index += 1;
                filter_index += 1;
            } else if filter_index < filter_bytes.len() && filter_bytes[filter_index] == b'%' {
                wildcard_index = Some(filter_index);
                filter_index += 1;
                wildcard_match_start = actual_index;
            } else if let Some(last_wildcard_index) = wildcard_index {
                filter_index = last_wildcard_index + 1;
                wildcard_match_start += 1;
                actual_index = wildcard_match_start;
            } else {
                return false;
            }
        }

        while filter_index < filter_bytes.len() && filter_bytes[filter_index] == b'%' {
            filter_index += 1;
        }

        filter_index == filter_bytes.len()
    }

    fn filter_map(filter: &Value) -> Result<&Map<String, Value>> {
        filter
            .as_object()
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Filter root must be a JSON object"))
    }

    fn apply_query_modifiers(records: &mut Vec<Value>, modifiers: Option<&QueryModifiers>) {
        let Some(modifiers) = modifiers else {
            return;
        };

        if let Some(order_by) = modifiers.order_by.as_deref() {
            let direction = modifiers
                .order_direction
                .unwrap_or(OrderDirection::Ascending);
            let sort_type = Self::sort_type_for_records(records, order_by);
            records.sort_by(|left, right| {
                Self::compare_records_by_field(left, right, order_by, direction, sort_type)
            });
        }

        let offset = modifiers.offset.unwrap_or(0);
        if offset > 0 {
            if offset >= records.len() {
                records.clear();
            } else {
                records.drain(0..offset);
            }
        }

        if let Some(limit) = modifiers.limit {
            records.truncate(limit);
        }
    }

    fn compare_records_by_field(
        left: &Value,
        right: &Value,
        field_name: &str,
        direction: OrderDirection,
        sort_type: Option<SortableType>,
    ) -> Ordering {
        let Some(sort_type) = sort_type else {
            return Ordering::Equal;
        };

        let left_value = left.get(field_name);
        let right_value = right.get(field_name);

        match (
            left_value.and_then(|value| Self::sortable_value(value, sort_type)),
            right_value.and_then(|value| Self::sortable_value(value, sort_type)),
        ) {
            (Some(left_sortable), Some(right_sortable)) => {
                match left_sortable.compare_same_type(right_sortable) {
                    Some(ordering) => match direction {
                        OrderDirection::Ascending => ordering,
                        OrderDirection::Descending => ordering.reverse(),
                    },
                    None => Ordering::Equal,
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }

    fn sort_type_for_records(records: &[Value], field_name: &str) -> Option<SortableType> {
        records.iter().find_map(|record| {
            record.get(field_name).and_then(|value| match value {
                Value::Bool(_) => Some(SortableType::Bool),
                Value::Number(_) => Some(SortableType::Number),
                Value::String(_) => Some(SortableType::String),
                _ => None,
            })
        })
    }

    fn sortable_value(value: &Value, sort_type: SortableType) -> Option<SortableValue<'_>> {
        match (value, sort_type) {
            (Value::Bool(value), SortableType::Bool) => Some(SortableValue::Bool(*value)),
            (Value::Number(value), SortableType::Number) => {
                value.as_f64().map(SortableValue::Number)
            }
            (Value::String(value), SortableType::String) => Some(SortableValue::String(value)),
            _ => None,
        }
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
            let parent_pointer = Self::format_pointer(drawer_name, parent_key);

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
        let physical_target_drawer = Self::scoped_drawer_name(target_drawer, context);
        let mut child_records = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_target_drawer,
            "_id",
            Vec::new(),
        )? {
            Self::read_lock(&drawer)?.find_all_records()?
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

    fn collect_pointer_strings(value: &Value, pointers: &mut Vec<String>) {
        match value {
            Value::String(pointer) if Self::is_pointer(pointer) => {
                pointers.push(pointer.to_string());
            }
            Value::Array(values) => {
                for value in values {
                    Self::collect_pointer_strings(value, pointers);
                }
            }
            Value::Object(map) => {
                for value in map.values() {
                    Self::collect_pointer_strings(value, pointers);
                }
            }
            _ => {}
        }
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
                            .filter(|pointer| Self::is_pointer(pointer))
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
                        .filter(|pointer| Self::is_pointer(pointer))
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
        let (drawer_name, record_key) = Self::parse_pointer(pointer)?;
        let canonical_pointer = Self::format_pointer(&drawer_name, &record_key);

        if active_pointer_path.contains(&canonical_pointer) {
            return Ok(None);
        }

        let mut record = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &drawer_name,
            "_id",
            Vec::new(),
        )? {
            Self::read_lock(&drawer)?.find_by_primary_key(&record_key)?
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

    fn is_pointer(value: &str) -> bool {
        Self::try_parse_pointer_parts(value).is_some()
    }

    fn try_parse_pointer(pointer: &str) -> Option<(String, String)> {
        let (drawer_name, record_key) = Self::try_parse_pointer_parts(pointer)?;
        Some((drawer_name.to_string(), record_key.to_string()))
    }

    fn try_parse_pointer_parts(pointer: &str) -> Option<(&str, &str)> {
        let clean_pointer = pointer.strip_prefix('@')?;
        let (drawer_name, record_key) = clean_pointer.split_once(':')?;
        let record_key = record_key.strip_prefix("lnk_").unwrap_or(record_key);

        if drawer_name.is_empty() || record_key.is_empty() || record_key.contains(':') {
            return None;
        }

        Some((drawer_name, record_key))
    }

    fn parse_pointer(pointer: &str) -> Result<(String, String)> {
        Self::try_parse_pointer(pointer).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Malformed pointer reference encountered: {}", pointer),
            )
        })
    }

    fn clean_primary_key_token(value: &str) -> String {
        if let Some((_, record_key)) = Self::try_parse_pointer_parts(value) {
            return record_key.to_string();
        }

        value
            .trim_start_matches('@')
            .strip_prefix("lnk_")
            .unwrap_or_else(|| value.trim_start_matches('@'))
            .to_string()
    }

    fn format_pointer(drawer_name: &str, record_key: &str) -> String {
        format!(
            "@{}:{}",
            drawer_name.trim_start_matches('@'),
            Self::clean_primary_key_token(record_key)
        )
    }
}
