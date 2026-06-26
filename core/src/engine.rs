use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::drawer::VacuumReport;
use crate::wrdb_lib::registry::CatalogRegistry;
use crate::wrdb_lib::routing::{DatabaseRoute, ExecutionContext};
use crate::wrdb_lib::wal::{self, DurabilityPolicy, WalVerification};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Result;
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
    durability_policy: DurabilityPolicy,
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
        diagnostics::inspect_drawer(self, drawer_name)
    }

    pub fn check_path(&self, raw_path: &str) -> Result<CheckReport> {
        diagnostics::check_path(self, raw_path)
    }

    pub fn diagnose_storage(&self) -> Result<StorageDiagnosis> {
        diagnostics::diagnose_storage(self)
    }

    pub fn list_drawer_names(&self) -> Result<Vec<String>> {
        diagnostics::list_drawer_names(self)
    }

    pub fn backup_archive(&self, source_path: &str) -> Result<BackupArchive> {
        backup::backup_archive(self, source_path)
    }

    pub fn restore_archive(
        &self,
        destination_path: &str,
        archive: BackupArchive,
    ) -> Result<RestoreReport> {
        backup::restore_archive(self, destination_path, archive)
    }

    pub fn manage_user(&self, action: &str, payload: Value) -> Result<Value> {
        access_control::manage_user(&self.root_directory, action, payload)
    }

    pub fn cached_drawer_count(&self) -> Result<usize> {
        Ok(Self::read_lock(&self.database_core)?.cached_drawer_count())
    }

    pub fn show_tenants(&self) -> Result<Vec<String>> {
        boundary_execution::show_tenants(self)
    }

    pub fn list_tenants(&self) -> Result<Vec<String>> {
        self.show_tenants()
    }

    pub fn show_databases(&self) -> Result<Vec<StorageInventory>> {
        boundary_execution::show_databases(self)
    }

    pub fn list_databases(&self) -> Result<Vec<StorageInventory>> {
        self.show_databases()
    }

    pub fn verify_wal(&self, database_name: Option<&str>) -> Result<WalVerification> {
        boundary_execution::verify_wal(self, database_name)
    }

    pub fn show_schemas(&self, database_name: &str) -> Result<Vec<String>> {
        boundary_execution::show_schemas(self, database_name)
    }

    pub fn list_schemas(&self, database_name: &str) -> Result<Vec<String>> {
        self.show_schemas(database_name)
    }

    pub fn show_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<Vec<StorageInventory>> {
        boundary_execution::show_drawers(self, database_name, schema_name)
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
        boundary_execution::execute(self, coordinate, command)
    }

    pub fn execute_in_scope(&self, scope: StorageScope, command: Command) -> Result<CommandResult> {
        boundary_execution::execute_in_scope(self, scope, command)
    }

    pub fn create_database(&self, database_name: &str) -> Result<StorageInventory> {
        boundary_execution::create_database(self, database_name)
    }

    pub fn create_schema(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<StorageInventory> {
        boundary_execution::create_schema(self, database_name, schema_name)
    }

    pub fn create_drawer(
        &self,
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> Result<StorageInventory> {
        boundary_execution::create_drawer(self, database_name, schema_name, drawer_name)
    }

    pub fn register_tenant_route(
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
