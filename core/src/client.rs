use crate::wrdb_lib::connection::{ConnectionTarget, DriverKind};
use crate::wrdb_lib::protocol::{ProtocolFrame, ProtocolOpcode};
use crate::{
    BackupArchive, CheckReport, Command, CommandResult, DrawerInspectionMetrics, QueryModifiers,
    RestoreReport, StorageDiagnosis, StorageInventory, StorageLocator, VacuumReport,
    WardrobeEngine,
};
use serde_json::Value;
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Mutex;

#[cfg(unix)]
use std::os::unix::net::UnixStream;

pub struct WardrobeClient {
    target: ConnectionTarget,
    driver: Driver,
}

enum Driver {
    Embedded(WardrobeEngine),
    Network(NetworkDriver),
    UnixSocket(UnixSocketDriver),
}

struct NetworkDriver {
    host: String,
    port: u16,
    stream: Mutex<TcpStream>,
}

struct UnixSocketDriver {
    path: PathBuf,
    #[cfg(unix)]
    stream: Mutex<UnixStream>,
}

impl WardrobeClient {
    pub fn open(connection_string: impl AsRef<str>) -> Result<Self> {
        let target = ConnectionTarget::parse(connection_string.as_ref())?;
        let driver = match &target {
            ConnectionTarget::EmbeddedPath(path) => {
                Driver::Embedded(WardrobeEngine::open(path.to_string_lossy().as_ref())?)
            }
            ConnectionTarget::Network { host, port } => {
                Driver::Network(NetworkDriver::connect(host.clone(), *port)?)
            }
            ConnectionTarget::UnixSocket { path } => {
                Driver::UnixSocket(UnixSocketDriver::connect(path.clone())?)
            }
        };

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
        match &self.driver {
            Driver::Embedded(engine) => engine.upsert(drawer_name, payload),
            Driver::Network(driver) => expect_pointer(driver.execute(Command::Upsert {
                drawer_name: drawer_name.to_string(),
                payload,
            })?),
            Driver::UnixSocket(driver) => expect_pointer(driver.execute(Command::Upsert {
                drawer_name: drawer_name.to_string(),
                payload,
            })?),
        }
    }

    pub fn bulk_upsert(
        &self,
        drawer_name: &str,
        records: Vec<Value>,
        atomic: bool,
    ) -> Result<Vec<String>> {
        match &self.driver {
            Driver::Embedded(engine) => engine.bulk_upsert(drawer_name, records, atomic),
            Driver::Network(driver) => expect_pointers(driver.execute(Command::BulkUpsert {
                drawer_name: drawer_name.to_string(),
                records,
                atomic,
            })?),
            Driver::UnixSocket(driver) => expect_pointers(driver.execute(Command::BulkUpsert {
                drawer_name: drawer_name.to_string(),
                records,
                atomic,
            })?),
        }
    }

    pub fn find_all(&self, drawer_name: &str) -> Result<Vec<Value>> {
        match &self.driver {
            Driver::Embedded(engine) => engine.find_all(drawer_name),
            Driver::Network(driver) => expect_records(driver.execute(Command::FindAll {
                drawer_name: drawer_name.to_string(),
            })?),
            Driver::UnixSocket(driver) => expect_records(driver.execute(Command::FindAll {
                drawer_name: drawer_name.to_string(),
            })?),
        }
    }

    pub fn find_by_filter(
        &self,
        drawer_name: &str,
        filter: Value,
        modifiers: Option<QueryModifiers>,
    ) -> Result<Vec<Value>> {
        match &self.driver {
            Driver::Embedded(engine) => engine.find_by_filter(drawer_name, filter, modifiers),
            Driver::Network(driver) => expect_records(driver.execute(Command::FindByFilter {
                drawer_name: drawer_name.to_string(),
                filter,
                modifiers,
            })?),
            Driver::UnixSocket(driver) => {
                expect_records(driver.execute(Command::FindByFilter {
                    drawer_name: drawer_name.to_string(),
                    filter,
                    modifiers,
                })?)
            }
        }
    }

    pub fn count(
        &self,
        drawer_name: &str,
        filter: Option<Value>,
        modifiers: Option<QueryModifiers>,
    ) -> Result<usize> {
        match &self.driver {
            Driver::Embedded(engine) => engine.count(drawer_name, filter, modifiers),
            Driver::Network(driver) => expect_count(driver.execute(Command::Count {
                drawer_name: drawer_name.to_string(),
                filter,
                modifiers,
            })?),
            Driver::UnixSocket(driver) => expect_count(driver.execute(Command::Count {
                drawer_name: drawer_name.to_string(),
                filter,
                modifiers,
            })?),
        }
    }

    pub fn find_by_id(&self, pointer: &str) -> Result<Option<Value>> {
        match &self.driver {
            Driver::Embedded(engine) => engine.find_by_id(pointer),
            Driver::Network(driver) => expect_record(driver.execute(Command::FindById {
                pointer: pointer.to_string(),
            })?),
            Driver::UnixSocket(driver) => expect_record(driver.execute(Command::FindById {
                pointer: pointer.to_string(),
            })?),
        }
    }

    pub fn delete_by_id(&self, pointer: &str) -> Result<bool> {
        match &self.driver {
            Driver::Embedded(engine) => engine.delete_by_id(pointer),
            Driver::Network(driver) => expect_deleted(driver.execute(Command::Delete {
                pointer: pointer.to_string(),
            })?),
            Driver::UnixSocket(driver) => expect_deleted(driver.execute(Command::Delete {
                pointer: pointer.to_string(),
            })?),
        }
    }

    pub fn delete<L>(&self, locator: L) -> Result<bool>
    where
        L: Into<StorageLocator>,
    {
        match &self.driver {
            Driver::Embedded(engine) => engine.delete(locator),
            Driver::Network(driver) => expect_deleted(driver.execute(Command::Delete {
                pointer: locator_to_pointer(locator.into()),
            })?),
            Driver::UnixSocket(driver) => expect_deleted(driver.execute(Command::Delete {
                pointer: locator_to_pointer(locator.into()),
            })?),
        }
    }

    pub fn vacuum_drawer(&self, drawer_name: &str) -> Result<VacuumReport> {
        match &self.driver {
            Driver::Embedded(engine) => engine.vacuum_drawer(drawer_name),
            Driver::Network(driver) => expect_vacuumed(driver.execute(Command::Vacuum {
                drawer_name: drawer_name.to_string(),
            })?),
            Driver::UnixSocket(driver) => expect_vacuumed(driver.execute(Command::Vacuum {
                drawer_name: drawer_name.to_string(),
            })?),
        }
    }

    pub fn migrate_drawer(&self, drawer_name: &str) -> Result<VacuumReport> {
        match &self.driver {
            Driver::Embedded(engine) => engine.migrate_drawer(drawer_name),
            Driver::Network(driver) => expect_migrated(driver.execute(Command::Migrate {
                drawer_name: drawer_name.to_string(),
            })?),
            Driver::UnixSocket(driver) => expect_migrated(driver.execute(Command::Migrate {
                drawer_name: drawer_name.to_string(),
            })?),
        }
    }

    pub fn inspect_drawer(&self, drawer_name: &str) -> Result<DrawerInspectionMetrics> {
        match &self.driver {
            Driver::Embedded(engine) => engine.inspect_drawer(drawer_name),
            Driver::Network(driver) => expect_inspection(driver.execute(Command::Inspect {
                drawer_name: drawer_name.to_string(),
            })?),
            Driver::UnixSocket(driver) => expect_inspection(driver.execute(Command::Inspect {
                drawer_name: drawer_name.to_string(),
            })?),
        }
    }

    pub fn check_path(&self, path: &str) -> Result<CheckReport> {
        match &self.driver {
            Driver::Embedded(engine) => engine.check_path(path),
            Driver::Network(driver) => expect_check(driver.execute(Command::Check {
                path: path.to_string(),
            })?),
            Driver::UnixSocket(driver) => expect_check(driver.execute(Command::Check {
                path: path.to_string(),
            })?),
        }
    }

    pub fn diagnose_storage(&self) -> Result<StorageDiagnosis> {
        match &self.driver {
            Driver::Embedded(engine) => engine.diagnose_storage(),
            Driver::Network(driver) => expect_diagnosis(driver.execute(Command::Diagnose)?),
            Driver::UnixSocket(driver) => expect_diagnosis(driver.execute(Command::Diagnose)?),
        }
    }

    pub fn list_drawer_names(&self) -> Result<Vec<String>> {
        match &self.driver {
            Driver::Embedded(engine) => engine.list_drawer_names(),
            Driver::Network(driver) => expect_drawer_names(driver.execute(Command::ListDrawers)?),
            Driver::UnixSocket(driver) => {
                expect_drawer_names(driver.execute(Command::ListDrawers)?)
            }
        }
    }

    pub fn backup_archive(&self, source_path: &str) -> Result<BackupArchive> {
        match &self.driver {
            Driver::Embedded(engine) => engine.backup_archive(source_path),
            Driver::Network(driver) => expect_backup(driver.execute(Command::Backup {
                source_path: source_path.to_string(),
            })?),
            Driver::UnixSocket(driver) => expect_backup(driver.execute(Command::Backup {
                source_path: source_path.to_string(),
            })?),
        }
    }

    pub fn restore_archive(
        &self,
        destination_path: &str,
        archive: BackupArchive,
    ) -> Result<RestoreReport> {
        match &self.driver {
            Driver::Embedded(engine) => engine.restore_archive(destination_path, archive),
            Driver::Network(driver) => expect_restored(driver.execute(Command::Restore {
                destination_path: destination_path.to_string(),
                archive,
            })?),
            Driver::UnixSocket(driver) => expect_restored(driver.execute(Command::Restore {
                destination_path: destination_path.to_string(),
                archive,
            })?),
        }
    }

    pub fn create_database(&self, database_name: &str) -> Result<StorageInventory> {
        match &self.driver {
            Driver::Embedded(engine) => engine.create_database(database_name),
            Driver::Network(driver) => {
                expect_storage_inventory(driver.execute(Command::DefineDatabase {
                    database_name: database_name.to_string(),
                })?)
            }
            Driver::UnixSocket(driver) => {
                expect_storage_inventory(driver.execute(Command::DefineDatabase {
                    database_name: database_name.to_string(),
                })?)
            }
        }
    }

    pub fn create_schema(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<StorageInventory> {
        match &self.driver {
            Driver::Embedded(engine) => engine.create_schema(database_name, schema_name),
            Driver::Network(driver) => {
                expect_storage_inventory(driver.execute(Command::DefineSchema {
                    database_name: database_name.to_string(),
                    schema_name: schema_name.to_string(),
                })?)
            }
            Driver::UnixSocket(driver) => {
                expect_storage_inventory(driver.execute(Command::DefineSchema {
                    database_name: database_name.to_string(),
                    schema_name: schema_name.to_string(),
                })?)
            }
        }
    }

    pub fn create_drawer(
        &self,
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> Result<StorageInventory> {
        match &self.driver {
            Driver::Embedded(engine) => {
                engine.create_drawer(database_name, schema_name, drawer_name)
            }
            Driver::Network(driver) => {
                expect_storage_inventory(driver.execute(Command::DefineDrawer {
                    database_name: database_name.to_string(),
                    schema_name: schema_name.to_string(),
                    drawer_name: drawer_name.to_string(),
                })?)
            }
            Driver::UnixSocket(driver) => {
                expect_storage_inventory(driver.execute(Command::DefineDrawer {
                    database_name: database_name.to_string(),
                    schema_name: schema_name.to_string(),
                    drawer_name: drawer_name.to_string(),
                })?)
            }
        }
    }

    pub fn register_tenant_route(
        &self,
        tenant_id: &str,
        database_name: &str,
        location: &str,
    ) -> Result<StorageInventory> {
        match &self.driver {
            Driver::Embedded(engine) => {
                engine.register_tenant_route(tenant_id, database_name, location)
            }
            Driver::Network(driver) => {
                expect_storage_inventory(driver.execute(Command::DefineTenantRoute {
                    tenant_id: tenant_id.to_string(),
                    database_name: database_name.to_string(),
                    location: location.to_string(),
                })?)
            }
            Driver::UnixSocket(driver) => {
                expect_storage_inventory(driver.execute(Command::DefineTenantRoute {
                    tenant_id: tenant_id.to_string(),
                    database_name: database_name.to_string(),
                    location: location.to_string(),
                })?)
            }
        }
    }

    pub fn manage_user(&self, action: &str, payload: Value) -> Result<Value> {
        match &self.driver {
            Driver::Embedded(_) => Err(Error::new(
                ErrorKind::Unsupported,
                "manage user requires a remote Wardrobe server with administrative authorization",
            )),
            Driver::Network(driver) => expect_admin(driver.execute(Command::ManageUser {
                action: action.to_string(),
                payload,
            })?),
            Driver::UnixSocket(driver) => expect_admin(driver.execute(Command::ManageUser {
                action: action.to_string(),
                payload,
            })?),
        }
    }

    pub fn manage_schema(
        &self,
        drawer_name: &str,
        action: &str,
        kind: &str,
        field_name: &str,
        payload: Value,
    ) -> Result<Value> {
        match &self.driver {
            Driver::Embedded(engine) => {
                engine.manage_schema(drawer_name, action, kind, field_name, payload)
            }
            Driver::Network(driver) => expect_admin(driver.execute(Command::ManageSchema {
                action: action.to_string(),
                kind: kind.to_string(),
                drawer_name: drawer_name.to_string(),
                field_name: field_name.to_string(),
                payload,
            })?),
            Driver::UnixSocket(driver) => expect_admin(driver.execute(Command::ManageSchema {
                action: action.to_string(),
                kind: kind.to_string(),
                drawer_name: drawer_name.to_string(),
                field_name: field_name.to_string(),
                payload,
            })?),
        }
    }

    pub fn show_tenants(&self) -> Result<Vec<String>> {
        match &self.driver {
            Driver::Embedded(engine) => engine.show_tenants(),
            Driver::Network(driver) => expect_tenants(driver.execute(Command::ShowTenants)?),
            Driver::UnixSocket(driver) => expect_tenants(driver.execute(Command::ShowTenants)?),
        }
    }

    pub fn list_tenants(&self) -> Result<Vec<String>> {
        self.show_tenants()
    }

    pub fn show_databases(&self) -> Result<Vec<StorageInventory>> {
        match &self.driver {
            Driver::Embedded(engine) => engine.show_databases(),
            Driver::Network(driver) => expect_databases(driver.execute(Command::ShowDatabases)?),
            Driver::UnixSocket(driver) => expect_databases(driver.execute(Command::ShowDatabases)?),
        }
    }

    pub fn list_databases(&self) -> Result<Vec<StorageInventory>> {
        self.show_databases()
    }

    pub fn verify_wal(&self, database_name: Option<&str>) -> Result<crate::WalVerification> {
        match &self.driver {
            Driver::Embedded(engine) => engine.verify_wal(database_name),
            Driver::Network(driver) => {
                expect_wal_verification(driver.execute(Command::VerifyWal {
                    database_name: database_name.map(ToOwned::to_owned),
                })?)
            }
            Driver::UnixSocket(driver) => {
                expect_wal_verification(driver.execute(Command::VerifyWal {
                    database_name: database_name.map(ToOwned::to_owned),
                })?)
            }
        }
    }

    pub fn show_schemas(&self, database_name: &str) -> Result<Vec<String>> {
        match &self.driver {
            Driver::Embedded(engine) => engine.show_schemas(database_name),
            Driver::Network(driver) => expect_schemas(driver.execute(Command::ShowSchemas {
                database_name: database_name.to_string(),
            })?),
            Driver::UnixSocket(driver) => expect_schemas(driver.execute(Command::ShowSchemas {
                database_name: database_name.to_string(),
            })?),
        }
    }

    pub fn list_schemas(&self, database_name: &str) -> Result<Vec<String>> {
        self.show_schemas(database_name)
    }

    pub fn show_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<Vec<StorageInventory>> {
        match &self.driver {
            Driver::Embedded(engine) => engine.show_drawers(database_name, schema_name),
            Driver::Network(driver) => expect_drawers(driver.execute(Command::ShowDrawers {
                database_name: database_name.to_string(),
                schema_name: schema_name.to_string(),
            })?),
            Driver::UnixSocket(driver) => expect_drawers(driver.execute(Command::ShowDrawers {
                database_name: database_name.to_string(),
                schema_name: schema_name.to_string(),
            })?),
        }
    }

    pub fn list_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<Vec<StorageInventory>> {
        self.show_drawers(database_name, schema_name)
    }
}

impl NetworkDriver {
    fn connect(host: String, port: u16) -> Result<Self> {
        let stream = TcpStream::connect((host.as_str(), port))?;
        Ok(Self {
            host,
            port,
            stream: Mutex::new(stream),
        })
    }

    fn execute(&self, command: Command) -> Result<CommandResult> {
        execute_on_stream(
            &self.stream,
            command,
            format!("{}:{}", self.host, self.port),
        )
    }
}

impl UnixSocketDriver {
    fn connect(path: PathBuf) -> Result<Self> {
        #[cfg(unix)]
        {
            let stream = UnixStream::connect(&path)?;
            Ok(Self {
                path,
                stream: Mutex::new(stream),
            })
        }

        #[cfg(not(unix))]
        {
            Err(Error::new(
                ErrorKind::Unsupported,
                format!(
                    "Wardrobe Unix socket driver selected for {}, but Unix sockets are not available on this platform",
                    path.display()
                ),
            ))
        }
    }

    fn execute(&self, command: Command) -> Result<CommandResult> {
        #[cfg(unix)]
        {
            execute_on_stream(&self.stream, command, self.path.display().to_string())
        }

        #[cfg(not(unix))]
        {
            let _ = command;
            Err(Error::new(
                ErrorKind::Unsupported,
                format!(
                    "Wardrobe Unix socket driver selected for {}, but Unix sockets are not available on this platform",
                    self.path.display()
                ),
            ))
        }
    }
}

fn execute_on_stream<S>(
    stream: &Mutex<S>,
    command: Command,
    target_description: String,
) -> Result<CommandResult>
where
    S: Read + Write,
{
    let payload = serde_json::to_vec(&command).map_err(|error| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("Failed to serialize Wardrobe command: {error}"),
        )
    })?;

    let mut stream = stream.lock().map_err(|_| {
        Error::other(format!(
            "Wardrobe protocol stream lock was poisoned for {target_description}",
        ))
    })?;

    ProtocolFrame::new(ProtocolOpcode::Command, payload).write_to_stream(&mut *stream)?;
    let response = ProtocolFrame::read_from_stream(&mut *stream)?;

    match response.opcode {
        ProtocolOpcode::Result => serde_json::from_slice(&response.payload).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to deserialize Wardrobe command result: {error}"),
            )
        }),
        ProtocolOpcode::Error => Err(Error::new(
            ErrorKind::Other,
            String::from_utf8_lossy(&response.payload).into_owned(),
        )),
        ProtocolOpcode::Command => Err(Error::new(
            ErrorKind::InvalidData,
            "Wardrobe server returned a command frame where a result was expected",
        )),
    }
}

fn locator_to_pointer(locator: StorageLocator) -> String {
    match locator {
        StorageLocator::Inline(pointer) => pointer,
        StorageLocator::Explicit { drawer, id } => format!(
            "@{}:{}",
            drawer.trim_start_matches('@'),
            clean_locator_id(&id)
        ),
    }
}

fn clean_locator_id(value: &str) -> String {
    let trimmed = value.trim_start_matches('@');
    let id_part = trimmed
        .split_once(':')
        .map(|(_, record_key)| record_key)
        .unwrap_or(trimmed);

    id_part.strip_prefix("lnk_").unwrap_or(id_part).to_string()
}

fn expect_pointer(result: CommandResult) -> Result<String> {
    match result {
        CommandResult::Pointer(pointer) => Ok(pointer),
        other => unexpected_result("pointer", other),
    }
}

fn expect_pointers(result: CommandResult) -> Result<Vec<String>> {
    match result {
        CommandResult::Pointers(pointers) => Ok(pointers),
        other => unexpected_result("pointers", other),
    }
}

fn expect_records(result: CommandResult) -> Result<Vec<Value>> {
    match result {
        CommandResult::Records(records) => Ok(records),
        other => unexpected_result("records", other),
    }
}

fn expect_record(result: CommandResult) -> Result<Option<Value>> {
    match result {
        CommandResult::Record(record) => Ok(record),
        other => unexpected_result("record", other),
    }
}

fn expect_count(result: CommandResult) -> Result<usize> {
    match result {
        CommandResult::Count(count) => Ok(count),
        other => unexpected_result("count", other),
    }
}

fn expect_deleted(result: CommandResult) -> Result<bool> {
    match result {
        CommandResult::Deleted(deleted) => Ok(deleted),
        other => unexpected_result("deleted flag", other),
    }
}

fn expect_vacuumed(result: CommandResult) -> Result<VacuumReport> {
    match result {
        CommandResult::Vacuumed(report) => Ok(report),
        other => unexpected_result("vacuum report", other),
    }
}

fn expect_migrated(result: CommandResult) -> Result<VacuumReport> {
    match result {
        CommandResult::Migrated(report) => Ok(report),
        other => unexpected_result("migration report", other),
    }
}

fn expect_inspection(result: CommandResult) -> Result<DrawerInspectionMetrics> {
    match result {
        CommandResult::Inspection(metrics) => Ok(metrics),
        other => unexpected_result("inspection metrics", other),
    }
}

fn expect_check(result: CommandResult) -> Result<CheckReport> {
    match result {
        CommandResult::Check(report) => Ok(report),
        other => unexpected_result("check report", other),
    }
}

fn expect_diagnosis(result: CommandResult) -> Result<StorageDiagnosis> {
    match result {
        CommandResult::Diagnosis(report) => Ok(report),
        other => unexpected_result("storage diagnosis", other),
    }
}

fn expect_drawer_names(result: CommandResult) -> Result<Vec<String>> {
    match result {
        CommandResult::DrawerNames(drawers) => Ok(drawers),
        other => unexpected_result("drawer names", other),
    }
}

fn expect_backup(result: CommandResult) -> Result<BackupArchive> {
    match result {
        CommandResult::Backup(archive) => Ok(archive),
        other => unexpected_result("backup archive", other),
    }
}

fn expect_restored(result: CommandResult) -> Result<RestoreReport> {
    match result {
        CommandResult::Restored(report) => Ok(report),
        other => unexpected_result("restore report", other),
    }
}

fn expect_storage_inventory(result: CommandResult) -> Result<StorageInventory> {
    match result {
        CommandResult::StorageInventory(inventory) => Ok(inventory),
        other => unexpected_result("storage inventory", other),
    }
}

fn expect_admin(result: CommandResult) -> Result<Value> {
    match result {
        CommandResult::Admin(payload) => Ok(payload),
        other => unexpected_result("admin response", other),
    }
}

fn expect_tenants(result: CommandResult) -> Result<Vec<String>> {
    match result {
        CommandResult::Tenants(tenants) => Ok(tenants),
        other => unexpected_result("tenants", other),
    }
}

fn expect_databases(result: CommandResult) -> Result<Vec<StorageInventory>> {
    match result {
        CommandResult::Databases(databases) => Ok(databases),
        other => unexpected_result("databases", other),
    }
}

fn expect_schemas(result: CommandResult) -> Result<Vec<String>> {
    match result {
        CommandResult::Schemas(schemas) => Ok(schemas),
        other => unexpected_result("schemas", other),
    }
}

fn expect_drawers(result: CommandResult) -> Result<Vec<StorageInventory>> {
    match result {
        CommandResult::Drawers(drawers) => Ok(drawers),
        other => unexpected_result("drawers", other),
    }
}

fn expect_wal_verification(result: CommandResult) -> Result<crate::WalVerification> {
    match result {
        CommandResult::WalVerification(verification) => Ok(verification),
        other => unexpected_result("wal verification", other),
    }
}

fn unexpected_result<T>(expected: &str, actual: CommandResult) -> Result<T> {
    Err(Error::new(
        ErrorKind::InvalidData,
        format!(
            "Wardrobe protocol returned an unexpected result; expected {expected}, got {actual:?}",
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Write};
    use std::sync::Mutex;

    #[test]
    fn clean_locator_id_variants() {
        assert_eq!(clean_locator_id("@gem:lnk_hero"), "hero");
        assert_eq!(clean_locator_id("@gem:hero"), "hero");
        assert_eq!(clean_locator_id("plain"), "plain");
    }

    #[test]
    fn locator_to_pointer_explicit_and_inline() {
        let inline = StorageLocator::Inline("@gem:abc".to_string());
        assert_eq!(locator_to_pointer(inline), "@gem:abc".to_string());

        let explicit = StorageLocator::Explicit {
            drawer: "gem".to_string(),
            id: "lnk_abc".to_string(),
        };
        assert_eq!(locator_to_pointer(explicit), "@gem:abc".to_string());
    }

    #[test]
    fn unexpected_result_returns_invaliddata() {
        let res: Result<String> = unexpected_result("pointer", CommandResult::Count(5));
        assert!(res.is_err());
        assert_eq!(res.err().unwrap().kind(), ErrorKind::InvalidData);
    }

    struct FakeStream {
        read: Cursor<Vec<u8>>,
        write: Vec<u8>,
    }

    impl Read for FakeStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.read.read(buf)
        }
    }

    impl Write for FakeStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.write.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn execute_on_stream_roundtrips_result() {
        // Prepare a response frame containing a Count(3)
        let payload = serde_json::to_vec(&CommandResult::Count(3)).expect("serialize");
        let mut resp_bytes = Vec::new();
        ProtocolFrame::new(ProtocolOpcode::Result, payload)
            .write_to_stream(&mut resp_bytes)
            .expect("frame write");

        let stream = FakeStream {
            read: Cursor::new(resp_bytes),
            write: Vec::new(),
        };
        let mutex = Mutex::new(stream);

        let cmd = Command::Count {
            drawer_name: "gem".to_string(),
            filter: None,
            modifiers: None,
        };
        let result = execute_on_stream(&mutex, cmd, "test-target".to_string())
            .expect("execute should succeed");
        assert_eq!(result, CommandResult::Count(3));
    }
}
