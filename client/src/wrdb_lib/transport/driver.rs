use crate::wrdb_lib::command::*;
use crate::wrdb_lib::connection::ConnectionTarget;
use crate::wrdb_lib::network::NetworkTransport;
use crate::wrdb_lib::unix::UnixSocketTransport;
use serde_json::{json, Value};
use std::io::{Error, ErrorKind, Result};

pub enum ClientDriver {
    Network(NetworkTransport),
    UnixSocket(UnixSocketTransport),
}

impl ClientDriver {
    pub fn open(target: &ConnectionTarget) -> Result<Self> {
        Self::open_with_tls(target, None)
    }

    pub fn open_with_tls(
        target: &ConnectionTarget,
        tls: Option<&ClientTlsConfig>,
    ) -> Result<Self> {
        match target {
            ConnectionTarget::EmbeddedPath(path) => Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "Embedded storage path {} requires wardrobe-embedded database engine crate",
                    path.display()
                ),
            )),
            ConnectionTarget::Network { host, port } => Ok(Self::Network(
                NetworkTransport::connect(host.clone(), *port, tls)?,
            )),
            ConnectionTarget::UnixSocket { path } => {
                if tls.is_some() {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "certificate profiles apply only to TCP Wardrobe connections",
                    ));
                }
                Ok(Self::UnixSocket(UnixSocketTransport::connect(
                    path.clone(),
                )?))
            }
        }
    }

    pub fn execute_transport(&self, command: Value) -> Result<Value> {
        match self {
            Self::Network(transport) => transport.execute(&command),
            Self::UnixSocket(transport) => transport.execute(&command),
        }
    }

    pub fn upsert(
        &self,
        payload: Value,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<UpsertResult> {
        let cmd = json!({
            "type": "Upsert",
            "payload": payload,
            "filter": filter,
            "options": options,
        });
        let res = self.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(Error::other)
    }

    pub fn read(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<ReadResult> {
        let cmd = json!({
            "type": "Read",
            "filter": filter,
            "options": options,
        });
        let res = self.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(Error::other)
    }

    pub fn count(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<usize> {
        let cmd = json!({
            "type": "Count",
            "filter": filter,
            "options": options,
        });
        let res = self.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(Error::other)
    }

    pub fn delete(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<DeleteResult> {
        let cmd = json!({
            "type": "Delete",
            "filter": filter,
            "options": options,
        });
        let res = self.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(Error::other)
    }

    pub fn compact(&self, request: CompactRequest) -> Result<Value> {
        let cmd = json!({
            "type": "Compact",
            "request": request,
        });
        self.execute_transport(cmd)
    }

    pub fn inspect(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<InspectResult> {
        let cmd = json!({
            "type": "Inspect",
            "filter": filter,
            "options": options,
        });
        let res = self.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(Error::other)
    }

    pub fn backup(&self, source_path: &str) -> Result<BackupArchive> {
        let cmd = json!({
            "type": "Backup",
            "source_path": source_path,
        });
        let res = self.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(Error::other)
    }

    pub fn restore(&self, destination_path: &str, archive: BackupArchive) -> Result<RestoreReport> {
        let cmd = json!({
            "type": "Restore",
            "destination_path": destination_path,
            "archive": archive,
        });
        let res = self.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(Error::other)
    }

    pub fn create(&self, request: CreateRequest) -> Result<CreateResult> {
        let cmd = json!({
            "type": "Create",
            "request": request,
        });
        let res = self.execute_transport(cmd)?;
        serde_json::from_value(res).map_err(Error::other)
    }

    pub fn alter(&self, request: AlterRequest) -> Result<Value> {
        let cmd = json!({
            "type": "Alter",
            "request": request,
        });
        self.execute_transport(cmd)
    }

    pub fn drop(&self, request: DropRequest) -> Result<Value> {
        let cmd = json!({
            "type": "Drop",
            "request": request,
        });
        self.execute_transport(cmd)
    }

    pub fn grant(&self, request: PermissionRequest) -> Result<Value> {
        let cmd = json!({
            "type": "Grant",
            "request": request,
        });
        self.execute_transport(cmd)
    }

    pub fn revoke(&self, request: PermissionRequest) -> Result<Value> {
        let cmd = json!({
            "type": "Revoke",
            "request": request,
        });
        self.execute_transport(cmd)
    }

    pub fn status(&self, request: Value) -> Result<Value> {
        let cmd = json!({
            "type": "Status",
            "request": request,
        });
        self.execute_transport(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_driver_open_embedded_err() {
        let target = ConnectionTarget::EmbeddedPath(PathBuf::from("/tmp/db"));
        assert!(ClientDriver::open(&target).is_err());
    }

    #[test]
    fn test_driver_open_unix_tls_err() {
        let target = ConnectionTarget::UnixSocket {
            path: PathBuf::from("/tmp/sock"),
        };
        let dummy_tls = ClientTlsConfig {
            ca_cert: PathBuf::from("ca.crt"),
            client_cert: PathBuf::from("client.crt"),
            client_key: PathBuf::from("client.key"),
        };
        assert!(ClientDriver::open_with_tls(&target, Some(&dummy_tls)).is_err());
    }
}
