use super::command::{BackupArchive, CheckReport, Command, CommandResult, RestoreReport};
use super::connection::ConnectionTarget;
use super::drawer::VacuumReport;
use super::pointer;
use super::query::QueryModifiers;
use super::result_expectations;
use super::storage::{StorageInventory, StorageLocator};
use super::transport::{NetworkTransport, UnixSocketTransport};
use super::wal::WalVerification;
use crate::{DrawerInspectionMetrics, StorageDiagnosis, WardrobeEngine};
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

    pub(crate) fn upsert(&self, drawer_name: &str, payload: Value) -> Result<String> {
        match self {
            Self::Embedded(engine) => engine.upsert(drawer_name, payload),
            _ => result_expectations::pointer(self.execute_transport(Command::Upsert {
                drawer_name: drawer_name.to_string(),
                payload,
            })?),
        }
    }

    pub(crate) fn bulk_upsert(
        &self,
        drawer_name: &str,
        records: Vec<Value>,
        atomic: bool,
    ) -> Result<Vec<String>> {
        match self {
            Self::Embedded(engine) => engine.bulk_upsert(drawer_name, records, atomic),
            _ => result_expectations::pointers(self.execute_transport(Command::BulkUpsert {
                drawer_name: drawer_name.to_string(),
                records,
                atomic,
            })?),
        }
    }

    pub(crate) fn find_all(&self, drawer_name: &str) -> Result<Vec<Value>> {
        match self {
            Self::Embedded(engine) => engine.find_all(drawer_name),
            _ => result_expectations::records(self.execute_transport(Command::FindAll {
                drawer_name: drawer_name.to_string(),
            })?),
        }
    }

    pub(crate) fn find_by_filter(
        &self,
        drawer_name: &str,
        filter: Value,
        modifiers: Option<QueryModifiers>,
    ) -> Result<Vec<Value>> {
        match self {
            Self::Embedded(engine) => engine.find_by_filter(drawer_name, filter, modifiers),
            _ => result_expectations::records(self.execute_transport(Command::FindByFilter {
                drawer_name: drawer_name.to_string(),
                filter,
                modifiers,
            })?),
        }
    }

    pub(crate) fn count(
        &self,
        drawer_name: &str,
        filter: Option<Value>,
        modifiers: Option<QueryModifiers>,
    ) -> Result<usize> {
        match self {
            Self::Embedded(engine) => engine.count(drawer_name, filter, modifiers),
            _ => result_expectations::count(self.execute_transport(Command::Count {
                drawer_name: drawer_name.to_string(),
                filter,
                modifiers,
            })?),
        }
    }

    pub(crate) fn find_by_id(&self, pointer: &str) -> Result<Option<Value>> {
        match self {
            Self::Embedded(engine) => engine.find_by_id(pointer),
            _ => result_expectations::record(self.execute_transport(Command::FindById {
                pointer: pointer.to_string(),
            })?),
        }
    }

    pub(crate) fn delete_by_id(&self, pointer: &str) -> Result<bool> {
        match self {
            Self::Embedded(engine) => engine.delete_by_id(pointer),
            _ => result_expectations::deleted(self.execute_transport(Command::Delete {
                pointer: pointer.to_string(),
            })?),
        }
    }

    pub(crate) fn delete_by_filter(&self, drawer_name: &str, filter: Value) -> Result<usize> {
        match self {
            Self::Embedded(engine) => engine.delete_by_filter(drawer_name, filter),
            _ => result_expectations::count(self.execute_transport(Command::DeleteByFilter {
                drawer_name: drawer_name.to_string(),
                filter,
            })?),
        }
    }

    pub(crate) fn delete(&self, locator: StorageLocator) -> Result<bool> {
        match self {
            Self::Embedded(engine) => engine.delete(locator),
            _ => result_expectations::deleted(self.execute_transport(Command::Delete {
                pointer: pointer::locator_to_pointer(locator),
            })?),
        }
    }

    pub(crate) fn vacuum_drawer(&self, drawer_name: &str) -> Result<VacuumReport> {
        match self {
            Self::Embedded(engine) => engine.vacuum_drawer(drawer_name),
            _ => result_expectations::vacuumed(self.execute_transport(Command::Vacuum {
                drawer_name: drawer_name.to_string(),
            })?),
        }
    }

    pub(crate) fn migrate_drawer(&self, drawer_name: &str) -> Result<VacuumReport> {
        match self {
            Self::Embedded(engine) => engine.migrate_drawer(drawer_name),
            _ => result_expectations::migrated(self.execute_transport(Command::Migrate {
                drawer_name: drawer_name.to_string(),
            })?),
        }
    }

    pub(crate) fn inspect_drawer(&self, drawer_name: &str) -> Result<DrawerInspectionMetrics> {
        match self {
            Self::Embedded(engine) => engine.inspect_drawer(drawer_name),
            _ => result_expectations::inspection(self.execute_transport(Command::Inspect {
                drawer_name: drawer_name.to_string(),
            })?),
        }
    }

    pub(crate) fn check_path(&self, path: &str) -> Result<CheckReport> {
        match self {
            Self::Embedded(engine) => engine.check_path(path),
            _ => result_expectations::check(self.execute_transport(Command::Check {
                path: path.to_string(),
            })?),
        }
    }

    pub(crate) fn diagnose_storage(&self) -> Result<StorageDiagnosis> {
        match self {
            Self::Embedded(engine) => engine.diagnose_storage(),
            _ => result_expectations::diagnosis(self.execute_transport(Command::Diagnose)?),
        }
    }

    pub(crate) fn list_drawer_names(&self) -> Result<Vec<String>> {
        match self {
            Self::Embedded(engine) => engine.list_drawer_names(),
            _ => result_expectations::drawer_names(self.execute_transport(Command::ListDrawers)?),
        }
    }

    pub(crate) fn backup_archive(&self, source_path: &str) -> Result<BackupArchive> {
        match self {
            Self::Embedded(engine) => engine.backup_archive(source_path),
            _ => result_expectations::backup(self.execute_transport(Command::Backup {
                source_path: source_path.to_string(),
            })?),
        }
    }

    pub(crate) fn restore_archive(
        &self,
        destination_path: &str,
        archive: BackupArchive,
    ) -> Result<RestoreReport> {
        match self {
            Self::Embedded(engine) => engine.restore_archive(destination_path, archive),
            _ => result_expectations::restored(self.execute_transport(Command::Restore {
                destination_path: destination_path.to_string(),
                archive,
            })?),
        }
    }

    pub(crate) fn create_database(&self, database_name: &str) -> Result<StorageInventory> {
        match self {
            Self::Embedded(engine) => engine.create_database(database_name),
            _ => result_expectations::storage_inventory(self.execute_transport(
                Command::DefineDatabase {
                    database_name: database_name.to_string(),
                },
            )?),
        }
    }

    pub(crate) fn create_schema(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<StorageInventory> {
        match self {
            Self::Embedded(engine) => engine.create_schema(database_name, schema_name),
            _ => result_expectations::storage_inventory(self.execute_transport(
                Command::DefineSchema {
                    database_name: database_name.to_string(),
                    schema_name: schema_name.to_string(),
                },
            )?),
        }
    }

    pub(crate) fn create_drawer(
        &self,
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> Result<StorageInventory> {
        match self {
            Self::Embedded(engine) => engine.create_drawer(database_name, schema_name, drawer_name),
            _ => result_expectations::storage_inventory(self.execute_transport(
                Command::DefineDrawer {
                    database_name: database_name.to_string(),
                    schema_name: schema_name.to_string(),
                    drawer_name: drawer_name.to_string(),
                },
            )?),
        }
    }

    pub(crate) fn register_tenant_route(
        &self,
        tenant_id: &str,
        database_name: &str,
        location: &str,
    ) -> Result<StorageInventory> {
        match self {
            Self::Embedded(engine) => {
                engine.register_tenant_route(tenant_id, database_name, location)
            }
            _ => result_expectations::storage_inventory(self.execute_transport(
                Command::DefineTenantRoute {
                    tenant_id: tenant_id.to_string(),
                    database_name: database_name.to_string(),
                    location: location.to_string(),
                },
            )?),
        }
    }

    pub(crate) fn manage_user(&self, action: &str, payload: Value) -> Result<Value> {
        match self {
            Self::Embedded(_) => Err(Error::new(
                ErrorKind::Unsupported,
                "manage user requires a remote Wardrobe server with administrative authorization",
            )),
            _ => result_expectations::admin(self.execute_transport(Command::ManageUser {
                action: action.to_string(),
                payload,
            })?),
        }
    }

    pub(crate) fn manage_schema(
        &self,
        drawer_name: &str,
        action: &str,
        kind: &str,
        field_name: &str,
        payload: Value,
    ) -> Result<Value> {
        match self {
            Self::Embedded(engine) => {
                engine.manage_schema(drawer_name, action, kind, field_name, payload)
            }
            _ => result_expectations::admin(self.execute_transport(Command::ManageSchema {
                action: action.to_string(),
                kind: kind.to_string(),
                drawer_name: drawer_name.to_string(),
                field_name: field_name.to_string(),
                payload,
            })?),
        }
    }

    pub(crate) fn show_tenants(&self) -> Result<Vec<String>> {
        match self {
            Self::Embedded(engine) => engine.show_tenants(),
            _ => result_expectations::tenants(self.execute_transport(Command::ShowTenants)?),
        }
    }

    pub(crate) fn show_databases(&self) -> Result<Vec<StorageInventory>> {
        match self {
            Self::Embedded(engine) => engine.show_databases(),
            _ => result_expectations::databases(self.execute_transport(Command::ShowDatabases)?),
        }
    }

    pub(crate) fn verify_wal(&self, database_name: Option<&str>) -> Result<WalVerification> {
        match self {
            Self::Embedded(engine) => engine.verify_wal(database_name),
            _ => result_expectations::wal_verification(self.execute_transport(
                Command::VerifyWal {
                    database_name: database_name.map(ToOwned::to_owned),
                },
            )?),
        }
    }

    pub(crate) fn show_schemas(&self, database_name: &str) -> Result<Vec<String>> {
        match self {
            Self::Embedded(engine) => engine.show_schemas(database_name),
            _ => result_expectations::schemas(self.execute_transport(Command::ShowSchemas {
                database_name: database_name.to_string(),
            })?),
        }
    }

    pub(crate) fn show_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> Result<Vec<StorageInventory>> {
        match self {
            Self::Embedded(engine) => engine.show_drawers(database_name, schema_name),
            _ => result_expectations::drawers(self.execute_transport(Command::ShowDrawers {
                database_name: database_name.to_string(),
                schema_name: schema_name.to_string(),
            })?),
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
