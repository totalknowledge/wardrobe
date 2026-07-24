//! Thin network transport library crate for connecting to Wardrobe database servers.

pub mod connection;
pub mod driver;
pub mod model;
pub mod network;
pub mod protocol;
pub mod unix;

pub use connection::{ConnectionTarget, DriverKind};
pub use driver::ClientDriver;
pub use model::*;

use serde_json::json;
use serde_json::Value;
use std::io::Result;
use std::path::Path;

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

    pub fn open_with_tls(connection_string: impl AsRef<str>, tls: ClientTlsConfig) -> Result<Self> {
        let target = ConnectionTarget::parse(connection_string.as_ref())?;
        let driver = ClientDriver::open_with_tls(&target, Some(&tls))?;
        Ok(Self { target, driver })
    }

    pub fn open_with_profile(
        connection_string: impl AsRef<str>,
        profile: impl AsRef<Path>,
    ) -> Result<Self> {
        Self::open_with_tls(connection_string, ClientTlsConfig::from_profile(profile)?)
    }

    pub fn connection_target(&self) -> &ConnectionTarget {
        &self.target
    }

    pub fn driver_kind(&self) -> DriverKind {
        self.target.driver_kind()
    }

    pub fn uses_socket_transport(&self) -> bool {
        self.target.uses_socket_transport()
    }

    pub fn upsert<P, F, O>(&self, payload: P, filter: F, options: O) -> Result<UpsertResult>
    where
        P: Into<Value>,
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        let filter = filter.into();
        let options = options.into();
        let cmd = json!({
            "type": "Upsert",
            "payload": payload.into(),
            "filter": filter,
            "options": options,
        });
        let res = self.driver.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(std::io::Error::other)
    }

    pub fn read<F, O>(&self, filter: F, options: O) -> Result<ReadResult>
    where
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        let filter = filter.into();
        let options = options.into();
        let cmd = json!({
            "type": "Read",
            "filter": filter,
            "options": options,
        });
        let res = self.driver.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(std::io::Error::other)
    }

    pub fn count<F, O>(&self, filter: F, options: O) -> Result<usize>
    where
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        let filter = filter.into();
        let options = options.into();
        let cmd = json!({
            "type": "Count",
            "filter": filter,
            "options": options,
        });
        let res = self.driver.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(std::io::Error::other)
    }

    pub fn delete<F, O>(&self, filter: F, options: O) -> Result<DeleteResult>
    where
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        let filter = filter.into();
        let options = options.into();
        let cmd = json!({
            "type": "Delete",
            "filter": filter,
            "options": options,
        });
        let res = self.driver.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(std::io::Error::other)
    }

    pub fn inspect<F, O>(&self, filter: F, options: O) -> Result<InspectResult>
    where
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        let filter = filter.into();
        let options = options.into();
        let cmd = json!({
            "type": "Inspect",
            "filter": filter,
            "options": options,
        });
        let res = self.driver.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(std::io::Error::other)
    }
}
