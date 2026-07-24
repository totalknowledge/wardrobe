use crate::connection::{ConnectionTarget, DriverKind};
use crate::driver::ClientDriver;
use crate::model::*;
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

    pub fn requires_embedded_engine(&self) -> bool {
        self.target.requires_embedded_engine()
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
        self.driver
            .upsert(payload.into(), filter.into(), options.into())
    }

    pub fn read<F, O>(&self, filter: F, options: O) -> Result<ReadResult>
    where
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        self.driver.read(filter.into(), options.into())
    }

    pub fn count<F, O>(&self, filter: F, options: O) -> Result<usize>
    where
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        self.driver.count(filter.into(), options.into())
    }

    pub fn delete<F, O>(&self, filter: F, options: O) -> Result<DeleteResult>
    where
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        self.driver.delete(filter.into(), options.into())
    }

    pub fn compact<C>(&self, request: C) -> Result<Value>
    where
        C: Into<CompactRequest>,
    {
        self.driver.compact(request.into())
    }

    pub fn inspect<F, O>(&self, filter: F, options: O) -> Result<InspectResult>
    where
        F: Into<OperationFilter>,
        O: Into<OperationOptions>,
    {
        self.driver.inspect(filter.into(), options.into())
    }

    pub fn backup(&self, source_path: &str) -> Result<BackupArchive> {
        self.driver.backup(source_path)
    }

    pub fn restore(&self, destination_path: &str, archive: BackupArchive) -> Result<RestoreReport> {
        self.driver.restore(destination_path, archive)
    }

    pub fn create<C>(&self, request: C) -> Result<CreateResult>
    where
        C: Into<CreateRequest>,
    {
        self.driver.create(request.into())
    }

    pub fn alter<A>(&self, request: A) -> Result<Value>
    where
        A: Into<AlterRequest>,
    {
        self.driver.alter(request.into())
    }

    pub fn drop<D>(&self, request: D) -> Result<Value>
    where
        D: Into<DropRequest>,
    {
        self.driver.drop(request.into())
    }

    pub fn grant(&self, request: PermissionRequest) -> Result<Value> {
        self.driver.grant(request)
    }

    pub fn revoke(&self, request: PermissionRequest) -> Result<Value> {
        self.driver.revoke(request)
    }

    pub fn status(&self, request: StatusRequest) -> Result<Value> {
        let req_val = serde_json::to_value(&request).map_err(std::io::Error::other)?;
        self.driver.status(req_val)
    }
}
