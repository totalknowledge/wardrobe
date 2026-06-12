use crate::wrdb_lib::connection::{ConnectionTarget, DriverKind};
use crate::wrdb_lib::protocol::{ProtocolFrame, ProtocolOpcode};
use crate::{
    Command, CommandResult, QueryModifiers, StorageInventory, StorageLocator, VacuumReport,
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

        let explicit = StorageLocator::Explicit { drawer: "gem".to_string(), id: "lnk_abc".to_string() };
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

        let stream = FakeStream { read: Cursor::new(resp_bytes), write: Vec::new() };
        let mutex = Mutex::new(stream);

        let cmd = Command::Count { drawer_name: "gem".to_string(), filter: None, modifiers: None };
        let result = execute_on_stream(&mutex, cmd, "test-target".to_string()).expect("execute should succeed");
        assert_eq!(result, CommandResult::Count(3));
    }
}
