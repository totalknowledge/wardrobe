use crate::wrdb_lib::connection::{ConnectionTarget, DriverKind};
use crate::{QueryModifiers, StorageLocator, VacuumReport, WardrobeEngine};
use serde_json::Value;
use std::io::{Error, ErrorKind, Result};
use std::path::PathBuf;

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
}

struct UnixSocketDriver {
    path: PathBuf,
}

impl WardrobeClient {
    pub fn open(connection_string: impl AsRef<str>) -> Result<Self> {
        let target = ConnectionTarget::parse(connection_string.as_ref())?;
        let driver = match &target {
            ConnectionTarget::EmbeddedPath(path) => {
                Driver::Embedded(WardrobeEngine::open(path.to_string_lossy().as_ref())?)
            }
            ConnectionTarget::Network { host, port } => Driver::Network(NetworkDriver {
                host: host.clone(),
                port: *port,
            }),
            ConnectionTarget::UnixSocket { path } => {
                Driver::UnixSocket(UnixSocketDriver { path: path.clone() })
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
            Driver::Network(driver) => driver.unsupported(),
            Driver::UnixSocket(driver) => driver.unsupported(),
        }
    }

    pub fn find_all(&self, drawer_name: &str) -> Result<Vec<Value>> {
        match &self.driver {
            Driver::Embedded(engine) => engine.find_all(drawer_name),
            Driver::Network(driver) => driver.unsupported(),
            Driver::UnixSocket(driver) => driver.unsupported(),
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
            Driver::Network(driver) => driver.unsupported(),
            Driver::UnixSocket(driver) => driver.unsupported(),
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
            Driver::Network(driver) => driver.unsupported(),
            Driver::UnixSocket(driver) => driver.unsupported(),
        }
    }

    pub fn find_by_id(&self, pointer: &str) -> Result<Option<Value>> {
        match &self.driver {
            Driver::Embedded(engine) => engine.find_by_id(pointer),
            Driver::Network(driver) => driver.unsupported(),
            Driver::UnixSocket(driver) => driver.unsupported(),
        }
    }

    pub fn delete_by_id(&self, pointer: &str) -> Result<bool> {
        match &self.driver {
            Driver::Embedded(engine) => engine.delete_by_id(pointer),
            Driver::Network(driver) => driver.unsupported(),
            Driver::UnixSocket(driver) => driver.unsupported(),
        }
    }

    pub fn delete<L>(&self, locator: L) -> Result<bool>
    where
        L: Into<StorageLocator>,
    {
        match &self.driver {
            Driver::Embedded(engine) => engine.delete(locator),
            Driver::Network(driver) => driver.unsupported(),
            Driver::UnixSocket(driver) => driver.unsupported(),
        }
    }

    pub fn vacuum_drawer(&self, drawer_name: &str) -> Result<VacuumReport> {
        match &self.driver {
            Driver::Embedded(engine) => engine.vacuum_drawer(drawer_name),
            Driver::Network(driver) => driver.unsupported(),
            Driver::UnixSocket(driver) => driver.unsupported(),
        }
    }

    pub fn migrate_drawer(&self, drawer_name: &str) -> Result<VacuumReport> {
        match &self.driver {
            Driver::Embedded(engine) => engine.migrate_drawer(drawer_name),
            Driver::Network(driver) => driver.unsupported(),
            Driver::UnixSocket(driver) => driver.unsupported(),
        }
    }
}

impl NetworkDriver {
    fn unsupported<T>(&self) -> Result<T> {
        Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "Wardrobe network driver selected for {}:{}, but the network protocol is not implemented yet",
                self.host, self.port
            ),
        ))
    }
}

impl UnixSocketDriver {
    fn unsupported<T>(&self) -> Result<T> {
        Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "Wardrobe Unix socket driver selected for {}, but the socket protocol is not implemented yet",
                self.path.display()
            ),
        ))
    }
}
