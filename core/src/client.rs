use crate::wrdb_lib::connection::{ConnectionTarget, DriverKind};
use crate::wrdb_lib::driver::ClientDriver;
use crate::{
    BackupArchive, CheckReport, DrawerInspectionMetrics, QueryModifiers, RestoreReport,
    StorageDiagnosis, StorageInventory, StorageLocator, VacuumReport, WalVerification,
};
use serde_json::Value;
use std::io::Result;

pub struct WardrobeClient {
    target: ConnectionTarget,
    driver: ClientDriver,
}

impl WardrobeClient {
    pub fn open(connection_string: impl AsRef<str>) -> Result<Self> {
        let target = ConnectionTarget::parse(connection_string.as_ref())?;
        let driver = ClientDriver::open(&target)?;
        Ok(Self { target, driver })
    }

    pub fn connection_target(&self) -> &ConnectionTarget {
        &self.target
    }

    pub fn driver_kind(&self) -> DriverKind {
        self.target.driver_kind()
    }

    pub fn requires_embedded_engine(&self) -> bool {
        self.target.requires_embedded_engine()
    }

    pub fn uses_socket_transport(&self) -> bool {
        self.target.uses_socket_transport()
    }

    pub fn upsert(&self, drawer_name: &str, payload: Value) -> Result<String> {
        self.driver.upsert(drawer_name, payload)
    }

    pub fn bulk_upsert(
        &self,
        drawer_name: &str,
        records: Vec<Value>,
        atomic: bool,
    ) -> Result<Vec<String>> {
        self.driver.bulk_upsert(drawer_name, records, atomic)
    }

    pub fn find_all(&self, drawer_name: &str) -> Result<Vec<Value>> {
        self.driver.find_all(drawer_name)
    }

    pub fn find_by_filter(
        &self,
        drawer_name: &str,
        filter: Value,
        modifiers: Option<QueryModifiers>,
    ) -> Result<Vec<Value>> {
        self.driver.find_by_filter(drawer_name, filter, modifiers)
    }

    pub fn count(
        &self,
        drawer_name: &str,
        filter: Option<Value>,
        modifiers: Option<QueryModifiers>,
    ) -> Result<usize> {
        self.driver.count(drawer_name, filter, modifiers)
    }

    pub fn find_by_id(&self, pointer: &str) -> Result<Option<Value>> {
        self.driver.find_by_id(pointer)
    }

    pub fn delete_by_id(&self, pointer: &str) -> Result<bool> {
        self.driver.delete_by_id(pointer)
    }

    pub fn delete_by_filter(&self, drawer_name: &str, filter: Value) -> Result<usize> {
        self.driver.delete_by_filter(drawer_name, filter)
    }

    pub fn delete<L>(&self, locator: L) -> Result<bool>
    where
        L: Into<StorageLocator>,
    {
        self.driver.delete(locator.into())
    }

    pub fn vacuum_drawer(&self, drawer_name: &str) -> Result<VacuumReport> {
        self.driver.vacuum_drawer(drawer_name)
    }

    pub fn migrate_drawer(&self, drawer_name: &str) -> Result<VacuumReport> {
        self.driver.migrate_drawer(drawer_name)
    }

    pub fn inspect_drawer(&self, drawer_name: &str) -> Result<DrawerInspectionMetrics> {
        self.driver.inspect_drawer(drawer_name)
    }

    pub fn check_path(&self, path: &str) -> Result<CheckReport> {
        self.driver.check_path(path)
    }

    pub fn diagnose_storage(&self) -> Result<StorageDiagnosis> {
        self.driver.diagnose_storage()
    }

    pub fn list_drawer_names(&self) -> Result<Vec<String>> {
        self.driver.list_drawer_names()
    }

    pub fn backup_archive(&self, source_path: &str) -> Result<BackupArchive> {
        self.driver.backup_archive(source_path)
    }

    pub fn restore_archive(
        &self,
        destination_path: &str,
        archive: BackupArchive,
    ) -> Result<RestoreReport> {
        self.driver.restore_archive(destination_path, archive)
    }

    pub fn create_database(&self, database_name: &str) -> Result<StorageInventory> {
        self.driver.create_database(database_name)
    }

    pub fn create_schema(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<StorageInventory> {
        self.driver.create_schema(database_name, schema_name)
    }

    pub fn create_drawer(
        &self,
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> Result<StorageInventory> {
        self.driver
            .create_drawer(database_name, schema_name, drawer_name)
    }

    pub fn register_tenant_route(
        &self,
        tenant_id: &str,
        database_name: &str,
        location: &str,
    ) -> Result<StorageInventory> {
        self.driver
            .register_tenant_route(tenant_id, database_name, location)
    }

    pub fn manage_user(&self, action: &str, payload: Value) -> Result<Value> {
        self.driver.manage_user(action, payload)
    }

    pub fn manage_schema(
        &self,
        drawer_name: &str,
        action: &str,
        kind: &str,
        field_name: &str,
        payload: Value,
    ) -> Result<Value> {
        self.driver
            .manage_schema(drawer_name, action, kind, field_name, payload)
    }

    pub fn show_tenants(&self) -> Result<Vec<String>> {
        self.driver.show_tenants()
    }

    pub fn list_tenants(&self) -> Result<Vec<String>> {
        self.show_tenants()
    }

    pub fn show_databases(&self) -> Result<Vec<StorageInventory>> {
        self.driver.show_databases()
    }

    pub fn list_databases(&self) -> Result<Vec<StorageInventory>> {
        self.show_databases()
    }

    pub fn verify_wal(&self, database_name: Option<&str>) -> Result<WalVerification> {
        self.driver.verify_wal(database_name)
    }

    pub fn show_schemas(&self, database_name: &str) -> Result<Vec<String>> {
        self.driver.show_schemas(database_name)
    }

    pub fn list_schemas(&self, database_name: &str) -> Result<Vec<String>> {
        self.show_schemas(database_name)
    }

    pub fn show_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<Vec<StorageInventory>> {
        self.driver.show_drawers(database_name, schema_name)
    }

    pub fn list_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<Vec<StorageInventory>> {
        self.show_drawers(database_name, schema_name)
    }
}
