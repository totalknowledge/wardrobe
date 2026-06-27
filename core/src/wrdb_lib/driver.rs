use super::command::{
    AlterRequest, BackupArchive, Command, CommandResult, CompactRequest, CreateRequest,
    CreateResult, DeleteResult, DropRequest, InspectResult, OperationFilter, OperationOptions,
    PermissionRequest, ReadResult, RestoreReport, StatusRequest, StatusResult, UpsertResult,
};
use super::connection::ConnectionTarget;
use super::drawer::VacuumReport;
use super::result_expectations;
use super::transport::{NetworkTransport, UnixSocketTransport};
use crate::WardrobeEngine;
use serde_json::Value;
use std::io::{Error, ErrorKind, Result};

pub(crate) enum ClientDriver {
    Embedded(WardrobeEngine),
    Network(NetworkTransport),
    UnixSocket(UnixSocketTransport),
}

impl ClientDriver {
    pub(crate) fn open(target: &ConnectionTarget) -> Result<Self> {
        match target {
            ConnectionTarget::EmbeddedPath(path) => Ok(Self::Embedded(WardrobeEngine::open(
                path.to_string_lossy().as_ref(),
            )?)),
            ConnectionTarget::Network { host, port } => Ok(Self::Network(
                NetworkTransport::connect(host.clone(), *port)?,
            )),
            ConnectionTarget::UnixSocket { path } => Ok(Self::UnixSocket(
                UnixSocketTransport::connect(path.clone())?,
            )),
        }
    }

    pub(crate) fn upsert(
        &self,
        payload: Value,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<UpsertResult> {
        match self {
            Self::Embedded(engine) => engine.upsert(payload, filter, options),
            _ => result_expectations::upsert(self.execute_transport(Command::Upsert {
                payload,
                filter,
                options,
            })?),
        }
    }

    pub(crate) fn read(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<ReadResult> {
        match self {
            Self::Embedded(engine) => engine.read(filter, options),
            _ => result_expectations::read(
                self.execute_transport(Command::Read { filter, options })?,
            ),
        }
    }

    pub(crate) fn count(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<usize> {
        match self {
            Self::Embedded(engine) => engine.count(filter, options),
            _ => result_expectations::count(
                self.execute_transport(Command::Count { filter, options })?,
            ),
        }
    }

    pub(crate) fn delete(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<DeleteResult> {
        match self {
            Self::Embedded(engine) => engine.delete(filter, options),
            _ => result_expectations::delete(
                self.execute_transport(Command::Delete { filter, options })?,
            ),
        }
    }

    pub(crate) fn compact(&self, request: CompactRequest) -> Result<VacuumReport> {
        match self {
            Self::Embedded(engine) => engine.compact(request),
            _ => result_expectations::compact(self.execute_transport(Command::Compact(request))?),
        }
    }

    pub(crate) fn inspect(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<InspectResult> {
        match self {
            Self::Embedded(engine) => engine.inspect(filter, options),
            _ => result_expectations::inspect(
                self.execute_transport(Command::Inspect { filter, options })?,
            ),
        }
    }

    pub(crate) fn backup(&self, source_path: &str) -> Result<BackupArchive> {
        match self {
            Self::Embedded(engine) => engine.backup(source_path),
            _ => result_expectations::backup(self.execute_transport(Command::Backup {
                source_path: source_path.to_string(),
            })?),
        }
    }

    pub(crate) fn restore(
        &self,
        destination_path: &str,
        archive: BackupArchive,
    ) -> Result<RestoreReport> {
        match self {
            Self::Embedded(engine) => engine.restore(destination_path, archive),
            _ => result_expectations::restored(self.execute_transport(Command::Restore {
                destination_path: destination_path.to_string(),
                archive,
            })?),
        }
    }

    pub(crate) fn create(&self, request: CreateRequest) -> Result<CreateResult> {
        match self {
            Self::Embedded(engine) => engine.create(request),
            _ => result_expectations::create(self.execute_transport(Command::Create(request))?),
        }
    }

    pub(crate) fn alter(&self, request: AlterRequest) -> Result<Value> {
        match self {
            Self::Embedded(engine) => engine.alter(request),
            _ => result_expectations::admin(self.execute_transport(Command::Alter(request))?),
        }
    }

    pub(crate) fn drop(&self, request: DropRequest) -> Result<Value> {
        match self {
            Self::Embedded(engine) => engine.drop(request),
            _ => result_expectations::admin(self.execute_transport(Command::Drop(request))?),
        }
    }

    pub(crate) fn grant(&self, request: PermissionRequest) -> Result<Value> {
        match self {
            Self::Embedded(engine) => engine.grant(request),
            _ => result_expectations::admin(self.execute_transport(Command::Grant(request))?),
        }
    }

    pub(crate) fn revoke(&self, request: PermissionRequest) -> Result<Value> {
        match self {
            Self::Embedded(engine) => engine.revoke(request),
            _ => result_expectations::admin(self.execute_transport(Command::Revoke(request))?),
        }
    }

    pub(crate) fn status(&self, request: StatusRequest) -> Result<StatusResult> {
        match self {
            Self::Embedded(engine) => engine.status(request),
            _ => result_expectations::status(self.execute_transport(Command::Status(request))?),
        }
    }

    fn execute_transport(&self, command: Command) -> Result<CommandResult> {
        match self {
            Self::Embedded(_) => Err(Error::new(
                ErrorKind::Unsupported,
                "embedded Wardrobe client calls should execute directly through the engine",
            )),
            Self::Network(transport) => transport.execute(command),
            Self::UnixSocket(transport) => transport.execute(command),
        }
    }
}
