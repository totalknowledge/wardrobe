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
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Error, ErrorKind, Result};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use uuid::Uuid;

pub use crate::wrdb_lib::command::{
    BackupArchive, BackupArchiveFile, CheckEntry, CheckReport, Command, CommandResult,
    DrawerInspectionMetrics, RestoreReport, StorageDiagnosis,
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
}

const BACKUP_ARCHIVE_FORMAT: &str = "wardrobe-cli-backup-v1";
const ACCESS_CONTROL_FILE_NAME: &str = "_wardrobe_access_control.json";

#[derive(Debug)]
struct InspectTarget {
    data_dir: PathBuf,
    drawer_name: String,
    label: String,
}

struct DrawerFiles {
    data: PathBuf,
    index: PathBuf,
    meta: PathBuf,
}

#[derive(Default)]
struct StorageBreakdown {
    total_bytes: u64,
    data_bytes: u64,
    index_bytes: u64,
    metadata_bytes: u64,
    logical_wal_bytes: u64,
    transaction_wal_bytes: u64,
    other_bytes: u64,
}

enum StorageFileKind {
    Data,
    Index,
    Metadata,
    LogicalWal,
    TransactionWal,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackupScope {
    Wardrobe,
    Bay,
    Drawer,
}

impl BackupScope {
    fn from_segment_count(segment_count: usize, label: &str) -> Result<Self> {
        match segment_count {
            1 => Ok(Self::Wardrobe),
            2 => Ok(Self::Bay),
            3 => Ok(Self::Drawer),
            _ => Err(Error::new(
                ErrorKind::InvalidInput,
                format!("{label} must identify a wardrobe, bay, or drawer"),
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Wardrobe => "wardrobe",
            Self::Bay => "bay",
            Self::Drawer => "drawer",
        }
    }

    fn expected_segments(self) -> usize {
        match self {
            Self::Wardrobe => 1,
            Self::Bay => 2,
            Self::Drawer => 3,
        }
    }
}

#[derive(Debug)]
struct StructuralBackupTarget {
    scope: BackupScope,
    segments: Vec<String>,
    logical_path: String,
    storage_path: PathBuf,
}

#[derive(Default)]
struct RequestHydrationCache {
    records: hydration::HydrationCache,
    virtual_children: HashMap<VirtualRelationshipCacheKey, Vec<Value>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct VirtualRelationshipCacheKey {
    target_drawer: String,
    mapped_by: String,
    parent_pointer: String,
    include_ids: bool,
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

    pub fn bulk_upsert(
        &self,
        drawer_name: &str,
        records: Vec<Value>,
        atomic: bool,
    ) -> Result<Vec<String>> {
        Self::bulk_upsert_in_database(
            &self.database_core,
            drawer_name,
            records,
            atomic,
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

    pub fn delete_by_filter(&self, drawer_name: &str, filter: Value) -> Result<usize> {
        Self::delete_by_filter_in_database(
            &self.database_core,
            drawer_name,
            filter,
            ExecutionContext::root(),
        )
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

    pub fn inspect_drawer(&self, drawer_name: &str) -> Result<DrawerInspectionMetrics> {
        let target = inspect_target(&self.root_directory, drawer_name)?;
        let files = drawer_files(&target.data_dir, &target.drawer_name);
        let data_bytes = file_size_or_zero(&files.data)?;
        let index_bytes = file_size_or_zero(&files.index)?;
        let meta_bytes = file_size_or_zero(&files.meta)?;
        let total_bytes = data_bytes
            .saturating_add(index_bytes)
            .saturating_add(meta_bytes);
        let register_file_count = [&files.data, &files.index, &files.meta]
            .iter()
            .filter(|path| path.is_file())
            .count();
        let record_count = self.count(&target.label, None, None)?;

        Ok(DrawerInspectionMetrics {
            path: target.label,
            data_bytes,
            index_bytes,
            meta_bytes,
            total_bytes,
            record_count,
            register_file_count,
            tombstone_fragmentation_percent: None,
        })
    }

    pub fn check_path(&self, raw_path: &str) -> Result<CheckReport> {
        let segments = split_structural_path(raw_path, "check path")?;
        let logical_path = segments.join("/");
        let mut entries = Vec::new();

        let kind = match segments.len() {
            1 => {
                let path = self.root_directory.join(&segments[0]);
                entries.push(check_entry("directory", &path)?);
                "wardrobe"
            }
            2 => {
                let path = self.root_directory.join(&segments[0]).join(&segments[1]);
                entries.push(check_entry("directory", &path)?);
                "bay"
            }
            3 => {
                let files = drawer_files(&self.root_directory, &logical_path);
                entries.push(check_entry("data", &files.data)?);
                entries.push(check_entry("index", &files.index)?);
                entries.push(check_entry("meta", &files.meta)?);
                "drawer"
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "check path must identify a wardrobe, bay, or drawer",
                ));
            }
        };

        Ok(CheckReport {
            path: logical_path,
            kind: kind.to_string(),
            entries,
        })
    }

    pub fn diagnose_storage(&self) -> Result<StorageDiagnosis> {
        let drawers = self.list_drawer_names()?;
        let breakdown = storage_breakdown(&self.root_directory)?;
        Ok(StorageDiagnosis {
            storage_directory: self.root_directory.display().to_string(),
            storage_bytes: breakdown.total_bytes,
            data_bytes: breakdown.data_bytes,
            index_bytes: breakdown.index_bytes,
            metadata_bytes: breakdown.metadata_bytes,
            logical_wal_bytes: breakdown.logical_wal_bytes,
            transaction_wal_bytes: breakdown.transaction_wal_bytes,
            other_bytes: breakdown.other_bytes,
            drawer_count: drawers.len(),
            status: if drawers.is_empty() {
                "empty".to_string()
            } else {
                "ok".to_string()
            },
            drawers,
        })
    }

    pub fn list_drawer_names(&self) -> Result<Vec<String>> {
        let mut drawers = Vec::new();
        collect_drawer_names(&self.root_directory, &self.root_directory, &mut drawers)?;
        drawers.sort();
        drawers.dedup();
        Ok(drawers)
    }

    pub fn backup_archive(&self, source_path: &str) -> Result<BackupArchive> {
        let target =
            structural_backup_target(&self.root_directory, source_path, "backup source path")?;
        let files = collect_backup_archive_files(&target)?;
        Ok(BackupArchive {
            format: BACKUP_ARCHIVE_FORMAT.to_string(),
            source_path: target.logical_path,
            scope: target.scope.as_str().to_string(),
            files,
        })
    }

    pub fn restore_archive(
        &self,
        destination_path: &str,
        archive: BackupArchive,
    ) -> Result<RestoreReport> {
        validate_backup_archive_format(&archive)?;
        let target = structural_backup_target(
            &self.root_directory,
            destination_path,
            "restore destination path",
        )?;
        validate_archive_scope(&archive, &target)?;
        let decoded_files = decoded_restore_files(&archive, &target)?;
        let byte_count = decoded_files
            .iter()
            .map(|(_, bytes)| bytes.len())
            .sum::<usize>();

        clear_restore_target(&self.root_directory, &target)?;
        for (relative_path, bytes) in &decoded_files {
            let destination = target.storage_path.join(relative_path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(destination, bytes)?;
        }
        self.register_restored_catalog(&target)?;

        Ok(RestoreReport {
            destination_path: target.logical_path,
            scope: target.scope.as_str().to_string(),
            file_count: decoded_files.len(),
            byte_count,
        })
    }

    fn register_restored_catalog(&self, target: &StructuralBackupTarget) -> Result<()> {
        let Some(wardrobe) = target.segments.first() else {
            return Ok(());
        };

        self.create_database(wardrobe)?;
        match target.scope {
            BackupScope::Wardrobe => {
                for bay in restored_bay_names(&target.storage_path)? {
                    self.create_schema(wardrobe, &bay)?;
                    let bay_path = target.storage_path.join(&bay);
                    for drawer in restored_drawer_names(&bay_path)? {
                        self.create_drawer(wardrobe, &bay, &drawer)?;
                    }
                }
            }
            BackupScope::Bay => {
                let Some(bay) = target.segments.get(1) else {
                    return Ok(());
                };
                self.create_schema(wardrobe, bay)?;
                for drawer in restored_drawer_names(&target.storage_path)? {
                    self.create_drawer(wardrobe, bay, &drawer)?;
                }
            }
            BackupScope::Drawer => {
                let (Some(bay), Some(drawer)) = (target.segments.get(1), target.segments.get(2))
                else {
                    return Ok(());
                };
                self.create_schema(wardrobe, bay)?;
                self.create_drawer(wardrobe, bay, drawer)?;
            }
        }

        Ok(())
    }

    pub fn manage_user(&self, action: &str, payload: Value) -> Result<Value> {
        let normalized_action = action.replace('-', "_").to_ascii_lowercase();
        let mut registry = read_access_control_registry(&self.root_directory)?;

        match normalized_action.as_str() {
            "add_user" | "add" | "create_user" => {
                let username = user_payload_username(&payload)?;
                let users = access_control_users_mut(&mut registry)?;
                let mut user_payload = payload;
                if let Value::Object(map) = &mut user_payload {
                    map.insert("username".to_string(), Value::String(username.clone()));
                }
                users.insert(username.clone(), user_payload);
                write_access_control_registry(&self.root_directory, &registry)?;
                Ok(json!({
                    "ok": true,
                    "action": "add_user",
                    "username": username,
                }))
            }
            "grant_permission" | "revoke_permission" => {
                let username = permission_payload_username(&payload)?;
                let scope = permission_payload_scope(&payload)?;
                let users = access_control_users_mut(&mut registry)?;
                let user = users
                    .entry(username.clone())
                    .or_insert_with(|| json!({ "username": username.clone() }));
                let permissions = access_control_permissions_mut(user)?;

                if normalized_action == "grant_permission" {
                    if !permissions
                        .iter()
                        .any(|permission| permission.as_str() == Some(scope.as_str()))
                    {
                        permissions.push(Value::String(scope.clone()));
                    }
                } else {
                    permissions.retain(|permission| permission.as_str() != Some(scope.as_str()));
                }

                write_access_control_registry(&self.root_directory, &registry)?;
                Ok(json!({
                    "ok": true,
                    "action": normalized_action,
                    "username": username,
                    "permission_scope": scope,
                }))
            }
            _ => {
                let username = payload
                    .get("username")
                    .or_else(|| payload.get("user"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let operations = access_control_operations_mut(&mut registry)?;
                operations.push(json!({
                    "action": action,
                    "username": username,
                    "payload": payload,
                }));
                write_access_control_registry(&self.root_directory, &registry)?;
                Ok(json!({
                    "ok": true,
                    "action": action,
                    "username": username,
                }))
            }
        }
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

    fn bulk_upsert_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        records: Vec<Value>,
        atomic: bool,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<String>> {
        if !atomic {
            let mut pointers = Vec::with_capacity(records.len());
            for record in records {
                pointers.push(Self::upsert_in_database(
                    database_core,
                    drawer_name,
                    record,
                    context,
                )?);
            }
            return Ok(pointers);
        }

        let wal_records = records.clone();
        engine_wal::run_bulk_upsert_transaction(
            database_core,
            drawer_name,
            &wal_records,
            context,
            || Self::bulk_upsert_in_database_unlogged(database_core, drawer_name, records, context),
        )
    }

    fn bulk_upsert_in_database_unlogged(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        records: Vec<Value>,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<String>> {
        let target_primary_key = "_id";
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let mut pointers = Vec::with_capacity(records.len());
        let mut prepared_records = Vec::with_capacity(records.len());

        for payload in records {
            let Value::Object(map) = payload else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Payload root must be a JSON object",
                ));
            };

            let record_key = match map.get(target_primary_key).and_then(|v| v.as_str()) {
                Some(existing_id) => {
                    pointer::normalize_primary_key(&physical_drawer_name, drawer_name, existing_id)
                }
                None => Uuid::new_v4().simple().to_string(),
            };
            pointers.push(pointer::format_pointer(&physical_drawer_name, &record_key));
            prepared_records.push((record_key, map));
        }

        let drawer_handle = Self::load_drawer_handle(
            database_core,
            &physical_drawer_name,
            target_primary_key,
            Vec::new(),
        )?;
        let mut full_records = Vec::with_capacity(prepared_records.len());

        for (record_key, map) in prepared_records {
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
            full_records.push(Value::Object(full_record));
        }

        match Self::write_lock(&drawer_handle)?.upsert_records_atomic(full_records)? {
            Ok(_) => Ok(pointers),
            Err(validation_error) => Err(Error::new(ErrorKind::InvalidData, validation_error)),
        }
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

        let mut hydration_cache = RequestHydrationCache::default();
        hydration::hydrate_records_with_cache(
            &mut records,
            true,
            &mut hydration_cache.records,
            |drawer_name, record_key| {
                Self::fetch_record_for_hydration(database_core, drawer_name, record_key)
            },
        )?;
        Self::attach_virtual_relationships(
            database_core,
            &physical_drawer_name,
            &mut records,
            true,
            context,
            &mut hydration_cache,
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
            let mut drawer = Self::write_lock(&drawer)?;
            if let Some(offsets) = drawer.indexed_candidate_offsets(filter_map) {
                drawer.records_at_offsets_with_migration(offsets)?
            } else {
                drawer.find_all_records_with_migration()?
            }
        } else {
            Vec::new()
        };

        records.retain(|record| {
            query::record_matches_filter(record, filter_map, context.drawer_namespace)
        });
        query::apply_query_modifiers(&mut records, modifiers.as_ref());
        let mut hydration_cache = RequestHydrationCache::default();
        hydration::hydrate_records_with_cache(
            &mut records,
            true,
            &mut hydration_cache.records,
            |drawer_name, record_key| {
                Self::fetch_record_for_hydration(database_core, drawer_name, record_key)
            },
        )?;
        Self::attach_virtual_relationships(
            database_core,
            &physical_drawer_name,
            &mut records,
            true,
            context,
            &mut hydration_cache,
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
            let mut drawer = Self::write_lock(&drawer)?;
            if let Some(offsets) = drawer.indexed_candidate_offsets(filter_map) {
                offsets.len()
            } else {
                drawer
                    .find_all_records_with_migration()?
                    .into_iter()
                    .filter(|record| {
                        query::record_matches_filter(record, filter_map, context.drawer_namespace)
                    })
                    .count()
            }
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
                let mut hydration_cache = RequestHydrationCache::default();
                hydration::hydrate_value_with_cache(
                    &mut record,
                    false,
                    &mut active_pointer_path,
                    &mut hydration_cache.records,
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
                    &mut hydration_cache,
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

    fn delete_by_filter_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        context: ExecutionContext<'_>,
    ) -> Result<usize> {
        let records =
            Self::find_by_filter_in_database(database_core, drawer_name, filter, None, context)?;

        let mut deleted_count = 0_usize;
        for record in records {
            let id = record.get("_id").and_then(Value::as_str).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    "record is missing a string _id for delete-by-filter",
                )
            })?;
            let pointer = if id.starts_with('@') {
                id.to_string()
            } else {
                format!("@{drawer_name}:{id}")
            };
            if Self::delete_by_id_in_database(
                database_core,
                StorageLocator::Inline(pointer),
                context,
            )? {
                deleted_count += 1;
            }
        }

        Ok(deleted_count)
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
        hydration_cache: &mut RequestHydrationCache,
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
                    hydration_cache,
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
        hydration_cache: &mut RequestHydrationCache,
    ) -> Result<Vec<Value>> {
        let physical_target_drawer =
            routing::scoped_drawer_name(target_drawer, context.drawer_namespace);
        let cache_key = VirtualRelationshipCacheKey {
            target_drawer: physical_target_drawer.clone(),
            mapped_by: mapped_by.to_string(),
            parent_pointer: parent_pointer.to_string(),
            include_ids,
        };

        if let Some(child_records) = hydration_cache.virtual_children.get(&cache_key) {
            return Ok(child_records.clone());
        }

        let mut child_records = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_target_drawer,
            "_id",
            Vec::new(),
        )? {
            let mut drawer = Self::write_lock(&drawer)?;
            let mut filter_map = serde_json::Map::new();
            filter_map.insert(
                mapped_by.to_string(),
                Value::String(parent_pointer.to_string()),
            );
            if let Some(offsets) = drawer.indexed_candidate_offsets(&filter_map) {
                drawer.records_at_offsets_with_migration(offsets)?
            } else {
                drawer.find_all_records_with_migration()?
            }
        } else {
            Vec::new()
        };

        child_records.retain(|record| {
            record.get(mapped_by).and_then(|value| value.as_str()) == Some(parent_pointer)
        });
        hydration::hydrate_records_with_cache(
            &mut child_records,
            include_ids,
            &mut hydration_cache.records,
            |drawer_name, record_key| {
                Self::fetch_record_for_hydration(database_core, drawer_name, record_key)
            },
        )?;

        hydration_cache
            .virtual_children
            .insert(cache_key, child_records.clone());

        Ok(child_records)
    }
}

fn inspect_target(root_directory: &Path, raw_path: &str) -> Result<InspectTarget> {
    let mut segments = split_structural_path(raw_path, "inspect path")?;
    let drawer_name = segments
        .pop()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "inspect requires a drawer name"))?;
    let mut data_dir = root_directory.to_path_buf();
    for segment in &segments {
        data_dir.push(segment);
    }
    let label = if segments.is_empty() {
        drawer_name.clone()
    } else {
        format!("{}/{}", segments.join("/"), drawer_name)
    };

    Ok(InspectTarget {
        data_dir,
        drawer_name,
        label,
    })
}

fn split_structural_path(raw_path: &str, label: &str) -> Result<Vec<String>> {
    let mut segments = Vec::new();
    for segment in raw_path.split(|c| c == '/' || c == '\\') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid {label} segment: {segment}"),
            ));
        }
        segments.push(segment.to_string());
    }
    if segments.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{label} cannot be empty"),
        ));
    }
    Ok(segments)
}

fn drawer_files(data_dir: &Path, drawer_name: &str) -> DrawerFiles {
    DrawerFiles {
        data: data_dir.join(format!("{drawer_name}.drw")),
        index: data_dir.join(format!("{drawer_name}_index.drw")),
        meta: data_dir.join(format!("{drawer_name}_meta.drw")),
    }
}

fn file_size_or_zero(path: &Path) -> Result<u64> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

fn storage_breakdown(path: &Path) -> Result<StorageBreakdown> {
    let mut breakdown = StorageBreakdown::default();
    collect_storage_breakdown(path, &mut breakdown)?;
    Ok(breakdown)
}

fn collect_storage_breakdown(path: &Path, breakdown: &mut StorageBreakdown) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child_path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_storage_breakdown(&child_path, breakdown)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }

        let bytes = metadata.len();
        breakdown.total_bytes = breakdown.total_bytes.saturating_add(bytes);
        match storage_file_kind(&child_path) {
            StorageFileKind::Data => {
                breakdown.data_bytes = breakdown.data_bytes.saturating_add(bytes)
            }
            StorageFileKind::Index => {
                breakdown.index_bytes = breakdown.index_bytes.saturating_add(bytes)
            }
            StorageFileKind::Metadata => {
                breakdown.metadata_bytes = breakdown.metadata_bytes.saturating_add(bytes)
            }
            StorageFileKind::LogicalWal => {
                breakdown.logical_wal_bytes = breakdown.logical_wal_bytes.saturating_add(bytes)
            }
            StorageFileKind::TransactionWal => {
                breakdown.transaction_wal_bytes =
                    breakdown.transaction_wal_bytes.saturating_add(bytes)
            }
            StorageFileKind::Other => {
                breakdown.other_bytes = breakdown.other_bytes.saturating_add(bytes)
            }
        }
    }

    Ok(())
}

fn storage_file_kind(path: &Path) -> StorageFileKind {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return StorageFileKind::Other;
    };

    if file_name == ".wal" {
        return StorageFileKind::LogicalWal;
    }
    if file_name == "wardrobe.wal" {
        return StorageFileKind::TransactionWal;
    }
    if file_name.ends_with("_index.drw") {
        return StorageFileKind::Index;
    }
    if file_name.ends_with("_meta.drw") || file_name.ends_with(".wal.meta") {
        return StorageFileKind::Metadata;
    }
    if path.extension().and_then(|extension| extension.to_str()) == Some("drw") {
        return StorageFileKind::Data;
    }
    StorageFileKind::Other
}

fn check_entry(label: &str, path: &Path) -> Result<CheckEntry> {
    let metadata = fs::metadata(path);
    let (exists, bytes) = match metadata {
        Ok(metadata) => (
            true,
            if metadata.is_file() {
                Some(metadata.len())
            } else {
                None
            },
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => (false, None),
        Err(error) => return Err(error),
    };
    Ok(CheckEntry {
        label: label.to_string(),
        path: path.display().to_string(),
        exists,
        bytes,
    })
}

fn collect_drawer_names(root: &Path, current: &Path, drawers: &mut Vec<String>) -> Result<()> {
    if !current.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_drawer_names(root, &path, drawers)?;
            continue;
        }
        if !is_drawer_data_file(&path) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let parent = path.parent().unwrap_or(current);
        let relative_parent = parent.strip_prefix(root).unwrap_or(parent);
        let name = if relative_parent.as_os_str().is_empty() {
            stem.to_string()
        } else {
            format!("{}/{}", relative_path_string(relative_parent), stem)
        };
        drawers.push(name);
    }
    Ok(())
}

fn is_drawer_data_file(path: &Path) -> bool {
    if path.extension().and_then(|extension| extension.to_str()) != Some("drw") {
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    !stem.starts_with('.') && !stem.ends_with("_index") && !stem.ends_with("_meta")
}

fn structural_backup_target(
    root_directory: &Path,
    raw_path: &str,
    label: &str,
) -> Result<StructuralBackupTarget> {
    let segments = split_structural_path(raw_path, label)?;
    let scope = BackupScope::from_segment_count(segments.len(), label)?;
    let storage_path = match scope {
        BackupScope::Wardrobe | BackupScope::Bay => segments
            .iter()
            .fold(root_directory.to_path_buf(), |path, segment| {
                path.join(segment)
            }),
        BackupScope::Drawer => root_directory.join(&segments[0]).join(&segments[1]),
    };
    Ok(StructuralBackupTarget {
        scope,
        logical_path: segments.join("/"),
        segments,
        storage_path,
    })
}

fn collect_backup_archive_files(target: &StructuralBackupTarget) -> Result<Vec<BackupArchiveFile>> {
    match target.scope {
        BackupScope::Wardrobe | BackupScope::Bay => {
            if !target.storage_path.is_dir() {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!("backup source path does not exist: {}", target.logical_path),
                ));
            }
            let mut files = Vec::new();
            collect_directory_archive_files(
                &target.storage_path,
                &target.storage_path,
                &mut files,
            )?;
            if files.is_empty() {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!(
                        "backup source path contains no files: {}",
                        target.logical_path
                    ),
                ));
            }
            files.sort_by(|left, right| left.path.cmp(&right.path));
            Ok(files)
        }
        BackupScope::Drawer => {
            let Some(drawer) = target.segments.get(2) else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "drawer backup requires a drawer path",
                ));
            };
            let mut files = Vec::new();
            for file_name in [
                format!("{drawer}.drw"),
                format!("{drawer}_index.drw"),
                format!("{drawer}_meta.drw"),
            ] {
                let path = target.storage_path.join(&file_name);
                if path.is_file() {
                    files.push(BackupArchiveFile {
                        path: file_name,
                        bytes_hex: encode_hex(&fs::read(path)?),
                    });
                }
            }
            if files.is_empty() {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!(
                        "drawer backup source contains no drawer files: {}",
                        target.logical_path
                    ),
                ));
            }
            Ok(files)
        }
    }
}

fn collect_directory_archive_files(
    base: &Path,
    current: &Path,
    files: &mut Vec<BackupArchiveFile>,
) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_directory_archive_files(base, &path, files)?;
        } else if path.is_file() {
            let relative_path = path.strip_prefix(base).map_err(|error| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Failed to compute backup relative path: {error}"),
                )
            })?;
            files.push(BackupArchiveFile {
                path: relative_path_string(relative_path),
                bytes_hex: encode_hex(&fs::read(path)?),
            });
        }
    }
    Ok(())
}

fn validate_backup_archive_format(archive: &BackupArchive) -> Result<()> {
    if archive.format != BACKUP_ARCHIVE_FORMAT {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "Invalid backup archive format: expected {BACKUP_ARCHIVE_FORMAT}, found {}",
                archive.format
            ),
        ));
    }
    Ok(())
}

fn validate_archive_scope(archive: &BackupArchive, target: &StructuralBackupTarget) -> Result<()> {
    let archive_scope = match archive.scope.as_str() {
        "wardrobe" => BackupScope::Wardrobe,
        "bay" => BackupScope::Bay,
        "drawer" => BackupScope::Drawer,
        other => {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Invalid backup archive scope: {other}"),
            ));
        }
    };
    if archive_scope != target.scope {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "restore destination '{}' is a {}, but archive contains a {} backup",
                target.logical_path,
                target.scope.as_str(),
                archive.scope
            ),
        ));
    }
    let source_segments =
        split_structural_path(&archive.source_path, "backup archive source path")?;
    if source_segments.len() != target.scope.expected_segments() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "restore destination path does not match archive scope",
        ));
    }
    Ok(())
}

fn decoded_restore_files(
    archive: &BackupArchive,
    target: &StructuralBackupTarget,
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut files = Vec::new();
    for file in &archive.files {
        let relative_path = restore_relative_path(archive, target, &file.path)?;
        let bytes = decode_hex(&file.bytes_hex)?;
        files.push((relative_path, bytes));
    }
    Ok(files)
}

fn restore_relative_path(
    archive: &BackupArchive,
    target: &StructuralBackupTarget,
    archive_path: &str,
) -> Result<PathBuf> {
    validate_archive_relative_path(archive_path)?;
    if target.scope != BackupScope::Drawer {
        return Ok(PathBuf::from(archive_path));
    }

    let source_segments =
        split_structural_path(&archive.source_path, "backup archive source path")?;
    let source_drawer = source_segments.last().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "drawer archive source path does not include a drawer",
        )
    })?;
    let destination_drawer = target.segments.get(2).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "drawer restore requires a destination drawer path",
        )
    })?;
    let archive_file = Path::new(archive_path);
    if archive_file.components().count() != 1 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "drawer backup archive cannot contain nested file paths",
        ));
    }
    let Some(file_name) = archive_file.file_name().and_then(|file| file.to_str()) else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "drawer backup archive contains an invalid file path",
        ));
    };

    let mapped_name = if file_name == format!("{source_drawer}.drw") {
        format!("{destination_drawer}.drw")
    } else if file_name == format!("{source_drawer}_index.drw") {
        format!("{destination_drawer}_index.drw")
    } else if file_name == format!("{source_drawer}_meta.drw") {
        format!("{destination_drawer}_meta.drw")
    } else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("Unexpected drawer backup file: {file_name}"),
        ));
    };

    Ok(PathBuf::from(mapped_name))
}

fn clear_restore_target(root_directory: &Path, target: &StructuralBackupTarget) -> Result<()> {
    match target.scope {
        BackupScope::Wardrobe | BackupScope::Bay => {
            ensure_path_is_under_root(root_directory, &target.storage_path)?;
            if target.storage_path.exists() {
                fs::remove_dir_all(&target.storage_path)?;
            }
        }
        BackupScope::Drawer => {
            let Some(drawer) = target.segments.get(2) else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "drawer restore requires a drawer path",
                ));
            };
            ensure_path_is_under_root(root_directory, &target.storage_path)?;
            for file_name in [
                format!("{drawer}.drw"),
                format!("{drawer}_index.drw"),
                format!("{drawer}_meta.drw"),
            ] {
                let path = target.storage_path.join(file_name);
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
        }
    }
    Ok(())
}

fn restored_bay_names(wardrobe_path: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    if !wardrobe_path.exists() {
        return Ok(names);
    }
    for entry in fs::read_dir(wardrobe_path)? {
        let entry = entry?;
        if entry.path().is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

fn restored_drawer_names(bay_path: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    if !bay_path.exists() {
        return Ok(names);
    }
    for entry in fs::read_dir(bay_path)? {
        let entry = entry?;
        let path = entry.path();
        if !is_drawer_data_file(&path) {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            names.push(stem.to_string());
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn validate_archive_relative_path(path: &str) -> Result<()> {
    let relative_path = Path::new(path);
    if relative_path.is_absolute() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "backup archive file paths must be relative",
        ));
    }
    for component in relative_path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "backup archive file path escapes the restore target",
            ));
        }
    }
    Ok(())
}

fn ensure_path_is_under_root(root_directory: &Path, target: &Path) -> Result<()> {
    let root = absolute_lexical_path(root_directory);
    let target = absolute_lexical_path(target);
    if !target.starts_with(&root) {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "Refusing to restore outside the storage root: {}",
                target.display()
            ),
        ));
    }
    Ok(())
}

fn absolute_lexical_path(path: &Path) -> PathBuf {
    let mut absolute = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                absolute.pop();
            }
            other => absolute.push(other.as_os_str()),
        }
    }
    absolute
}

fn relative_path_string(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str().map(ToOwned::to_owned),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(raw: &str) -> Result<Vec<u8>> {
    let raw_bytes = raw.as_bytes();
    if raw_bytes.len() % 2 != 0 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Invalid backup archive hex payload length",
        ));
    }
    let mut bytes = Vec::with_capacity(raw_bytes.len() / 2);
    for pair in raw_bytes.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            "Invalid backup archive hex payload",
        )),
    }
}

fn read_access_control_registry(root_directory: &Path) -> Result<Value> {
    let path = root_directory.join(ACCESS_CONTROL_FILE_NAME);
    if !path.exists() {
        return Ok(json!({ "users": {}, "operations": [] }));
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("Invalid access-control registry JSON: {error}"),
        )
    })
}

fn write_access_control_registry(root_directory: &Path, registry: &Value) -> Result<()> {
    fs::create_dir_all(root_directory)?;
    let bytes = serde_json::to_vec_pretty(registry).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("Failed to serialize access-control registry: {error}"),
        )
    })?;
    fs::write(root_directory.join(ACCESS_CONTROL_FILE_NAME), bytes)
}

fn access_control_users_mut(registry: &mut Value) -> Result<&mut serde_json::Map<String, Value>> {
    if !registry.is_object() {
        *registry = json!({});
    }
    let object = registry.as_object_mut().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "access-control registry must be a JSON object",
        )
    })?;
    let users = object.entry("users").or_insert_with(|| json!({}));
    if !users.is_object() {
        *users = json!({});
    }
    users.as_object_mut().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "access-control users registry must be a JSON object",
        )
    })
}

fn access_control_operations_mut(registry: &mut Value) -> Result<&mut Vec<Value>> {
    if !registry.is_object() {
        *registry = json!({});
    }
    let object = registry.as_object_mut().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "access-control registry must be a JSON object",
        )
    })?;
    let operations = object.entry("operations").or_insert_with(|| json!([]));
    if !operations.is_array() {
        *operations = json!([]);
    }
    operations.as_array_mut().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "access-control operations registry must be a JSON array",
        )
    })
}

fn access_control_permissions_mut(user: &mut Value) -> Result<&mut Vec<Value>> {
    if !user.is_object() {
        *user = json!({});
    }
    let object = user.as_object_mut().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "access-control user entry must be a JSON object",
        )
    })?;
    let permissions = object.entry("permissions").or_insert_with(|| json!([]));
    if !permissions.is_array() {
        *permissions = json!([]);
    }
    permissions.as_array_mut().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "access-control permissions must be a JSON array",
        )
    })
}

fn user_payload_username(payload: &Value) -> Result<String> {
    let username = payload
        .get("username")
        .or_else(|| payload.get("user"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if username.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "user admin payload requires a non-empty username",
        ));
    }
    Ok(username.to_string())
}

fn permission_payload_username(payload: &Value) -> Result<String> {
    user_payload_username(payload)
}

fn permission_payload_scope(payload: &Value) -> Result<String> {
    if let Some(scope) = payload.get("permission_scope").and_then(Value::as_str) {
        return parse_permission_scope(scope);
    }
    if let Some(scope) = payload.get("scope").and_then(Value::as_object) {
        let path = scope
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let rights = scope
            .get("rights")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return parse_permission_scope(&format!("{path}:{rights}"));
    }
    Err(Error::new(
        ErrorKind::InvalidInput,
        "permission payload requires a permission_scope",
    ))
}

fn parse_permission_scope(raw: &str) -> Result<String> {
    let raw = raw.trim();
    let mut parts = raw.split(':');
    let path_part = parts.next().unwrap_or_default().trim();
    let rights_part = parts.next().unwrap_or_default().trim();
    if parts.next().is_some() || path_part.is_empty() || rights_part.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "permission scope must use <path>:<rights>",
        ));
    }
    let segments = split_structural_path(path_part, "permission scope path")?;
    if segments.len() > 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "permission scope path must identify a wardrobe, bay, or drawer",
        ));
    }
    let mut rights = String::new();
    for right in rights_part.chars().map(|right| right.to_ascii_lowercase()) {
        if !matches!(right, 'r' | 'u' | 'd' | 'i') {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "permission rights must contain only r, u, d, or i",
            ));
        }
        if rights.contains(right) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("permission right '{right}' cannot be repeated"),
            ));
        }
        rights.push(right);
    }
    if rights.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "permission scope requires at least one right",
        ));
    }
    Ok(format!("{}:{rights}", segments.join("/")))
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

    fn bulk_upsert_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        records: Vec<Value>,
        atomic: bool,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<String>> {
        WardrobeEngine::bulk_upsert_in_database(database, drawer_name, records, atomic, context)
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

    fn delete_by_filter_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        context: ExecutionContext<'_>,
    ) -> Result<usize> {
        WardrobeEngine::delete_by_filter_in_database(database, drawer_name, filter, context)
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

    fn replay_bulk_upsert(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        records: Vec<Value>,
        context: ExecutionContext<'_>,
    ) -> Result<()> {
        WardrobeEngine::bulk_upsert_in_database_unlogged(
            database_core,
            drawer_name,
            records,
            context,
        )
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
