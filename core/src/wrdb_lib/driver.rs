use super::command::{
    AlterRequest, BackupArchive, Command, CommandResult, CompactMode, CompactRequest,
    CreateRequest, CreateResult, DeleteResult, DropRequest, InspectResult, OperationFilter,
    OperationOptions, PermissionRequest, ReadResult, RestoreReport, StatusRequest, StatusResult,
    UpsertResult,
};
use super::connection::ConnectionTarget;
use super::drawer::VacuumReport;
use super::query;
use super::result_expectations;
use super::transport::{NetworkTransport, UnixSocketTransport};
use crate::WardrobeEngine;
use crate::engine::{
    OperationSelection, ReturnShapeResolution, UpsertContext, merge_payload_into_record,
    record_pointer, resolve_read_shape, upsert_record_target,
};
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
            _ => self.upsert_remote(payload, filter, options),
        }
    }

    pub(crate) fn read(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<ReadResult> {
        match self {
            Self::Embedded(engine) => engine.read(filter, options),
            _ => self.read_remote(filter, options),
        }
    }

    pub(crate) fn count(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<usize> {
        match self {
            Self::Embedded(engine) => engine.count(filter, options),
            _ => self.count_remote(filter, options),
        }
    }

    pub(crate) fn delete(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<DeleteResult> {
        match self {
            Self::Embedded(engine) => engine.delete(filter, options),
            _ => self.delete_remote(filter),
        }
    }

    pub(crate) fn compact(&self, request: CompactRequest) -> Result<VacuumReport> {
        match self {
            Self::Embedded(engine) => engine.compact(request),
            _ => match request {
                CompactRequest::Drawer { drawer_name, mode } => match mode {
                    CompactMode::Vacuum => result_expectations::vacuumed(
                        self.execute_transport(Command::Vacuum { drawer_name })?,
                    ),
                    CompactMode::Migrate => result_expectations::migrated(
                        self.execute_transport(Command::Migrate { drawer_name })?,
                    ),
                },
            },
        }
    }

    pub(crate) fn inspect(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<InspectResult> {
        match self {
            Self::Embedded(engine) => engine.inspect(filter, options),
            _ => {
                let selection = OperationSelection::from_filter(filter)?;
                let drawer_name = selection.required_drawer("inspect")?;
                result_expectations::inspection(
                    self.execute_transport(Command::Inspect { drawer_name })?,
                )
                .map(InspectResult::Drawer)
            }
        }
    }

    fn upsert_remote(
        &self,
        payload: Value,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<UpsertResult> {
        let context = UpsertContext::from_filter(filter)?;
        if context.query.is_some() {
            return self.upsert_remote_by_query(payload, context, options);
        }
        match payload {
            Value::Array(records) => {
                let mut grouped_records = std::collections::HashMap::<String, Vec<Value>>::new();
                for record in records {
                    let (drawer_name, record) = upsert_record_target(record, &context)?;
                    grouped_records.entry(drawer_name).or_default().push(record);
                }
                let mut pointers = Vec::new();
                for (drawer_name, records) in grouped_records {
                    pointers.extend(result_expectations::upsert_pointers(
                        self.execute_transport(Command::BulkUpsert {
                            drawer_name,
                            records,
                            atomic: options.atomic_enabled(),
                        })?,
                    )?);
                }
                Ok(UpsertResult::Pointers(pointers))
            }
            payload => {
                let (drawer_name, payload) = upsert_record_target(payload, &context)?;
                result_expectations::upsert_pointers(self.execute_transport(Command::Upsert {
                    drawer_name,
                    payload,
                })?)
                .map(UpsertResult::Pointers)
            }
        }
    }

    fn upsert_remote_by_query(
        &self,
        payload: Value,
        context: UpsertContext,
        options: OperationOptions,
    ) -> Result<UpsertResult> {
        let drawer_name = context.drawer_name().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                "query upsert requires a drawer filter",
            )
        })?;
        let query = context.query.clone().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                "query upsert requires a query filter",
            )
        })?;
        let matched_records =
            result_expectations::records(self.execute_transport(Command::FindByFilter {
                drawer_name: drawer_name.to_string(),
                filter: query,
                modifiers: None,
            })?)?;
        if matched_records.is_empty() {
            if options.create_if_missing == Some(false) {
                return Ok(UpsertResult::Pointers(Vec::new()));
            }
            return self.upsert_remote(payload, OperationFilter::drawer(drawer_name), options);
        }
        if matched_records.len() > 1 && options.multi != Some(true) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "query upsert matched multiple records; set multi=true to update all matches",
            ));
        }
        let mut pointers = Vec::new();
        for mut existing_record in matched_records {
            merge_payload_into_record(&mut existing_record, &payload)?;
            let pointer = record_pointer(&existing_record, Some(drawer_name)).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    "query upsert match did not include an _id field",
                )
            })?;
            pointers.extend(
                self.upsert_remote(
                    existing_record,
                    OperationFilter::pointer(pointer),
                    options.clone(),
                )?
                .into_pointers(),
            );
        }
        Ok(UpsertResult::Pointers(pointers))
    }

    fn read_remote(
        &self,
        filter: OperationFilter,
        options: OperationOptions,
    ) -> Result<ReadResult> {
        let selection = OperationSelection::from_filter(filter)?;
        let mut records = if !selection.pointers.is_empty() {
            let mut records = Vec::new();
            for pointer in selection.resolved_pointers()? {
                if let Some(record) = result_expectations::record(
                    self.execute_transport(Command::FindById { pointer })?,
                )? {
                    records.push(record);
                }
            }
            records
        } else {
            let drawer_name = selection.required_drawer("read")?;
            if let Some(filter) = selection.query.clone() {
                result_expectations::records(self.execute_transport(Command::FindByFilter {
                    drawer_name,
                    filter,
                    modifiers: None,
                })?)?
            } else {
                let mut records = result_expectations::records(
                    self.execute_transport(Command::FindAll { drawer_name })?,
                )?;
                query::apply_query_modifiers(&mut records, options.query_modifiers().as_ref());
                records
            }
        };
        query::apply_query_modifiers(&mut records, options.query_modifiers().as_ref());
        match resolve_read_shape(options.return_shape, &selection) {
            ReturnShapeResolution::Record => Ok(ReadResult::Record(records.into_iter().next())),
            ReturnShapeResolution::Records => Ok(ReadResult::Records(records)),
            ReturnShapeResolution::Pointers => Ok(ReadResult::Pointers(
                records
                    .iter()
                    .filter_map(|record| record_pointer(record, selection.drawer_name.as_deref()))
                    .collect(),
            )),
            ReturnShapeResolution::Exists => Ok(ReadResult::Exists(!records.is_empty())),
        }
    }

    fn count_remote(&self, filter: OperationFilter, options: OperationOptions) -> Result<usize> {
        let selection = OperationSelection::from_filter(filter)?;
        if !selection.pointers.is_empty() {
            let mut count = 0;
            for pointer in selection.resolved_pointers()? {
                if result_expectations::record(
                    self.execute_transport(Command::FindById { pointer })?,
                )?
                .is_some()
                {
                    count += 1;
                }
            }
            return Ok(count);
        }
        let drawer_name = selection.required_drawer("count")?;
        result_expectations::count(self.execute_transport(Command::Count {
            drawer_name,
            filter: selection.query,
            modifiers: options.query_modifiers(),
        })?)
    }

    fn delete_remote(&self, filter: OperationFilter) -> Result<DeleteResult> {
        let selection = OperationSelection::from_filter(filter)?;
        if !selection.pointers.is_empty() {
            let mut deleted = 0;
            for pointer in selection.resolved_pointers()? {
                deleted += usize::from(result_expectations::deleted(
                    self.execute_transport(Command::Delete { pointer })?,
                )?);
            }
            return Ok(DeleteResult { deleted });
        }
        let drawer_name = selection.required_drawer("delete-by-filter")?;
        let Some(filter) = selection.query else {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "delete requires a record pointer or a drawer query filter",
            ));
        };
        result_expectations::count(self.execute_transport(Command::DeleteByFilter {
            drawer_name,
            filter,
        })?)
        .map(|deleted| DeleteResult { deleted })
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
            _ => match request {
                CreateRequest::Database { database_name } => {
                    result_expectations::storage_inventory(
                        self.execute_transport(Command::DefineDatabase { database_name })?,
                    )
                    .map(CreateResult::StorageInventory)
                }
                CreateRequest::Schema {
                    database_name,
                    schema_name,
                } => result_expectations::storage_inventory(self.execute_transport(
                    Command::DefineSchema {
                        database_name,
                        schema_name,
                    },
                )?)
                .map(CreateResult::StorageInventory),
                CreateRequest::Drawer {
                    database_name,
                    schema_name,
                    drawer_name,
                } => result_expectations::storage_inventory(self.execute_transport(
                    Command::DefineDrawer {
                        database_name,
                        schema_name,
                        drawer_name,
                    },
                )?)
                .map(CreateResult::StorageInventory),
                CreateRequest::TenantRoute {
                    tenant_id,
                    database_name,
                    location,
                } => result_expectations::storage_inventory(self.execute_transport(
                    Command::DefineTenantRoute {
                        tenant_id,
                        database_name,
                        location,
                    },
                )?)
                .map(CreateResult::StorageInventory),
                CreateRequest::User { payload } => {
                    result_expectations::admin(self.execute_transport(Command::ManageUser {
                        action: "add_user".to_string(),
                        payload,
                    })?)
                    .map(CreateResult::Admin)
                }
            },
        }
    }

    pub(crate) fn alter(&self, request: AlterRequest) -> Result<Value> {
        match self {
            Self::Embedded(engine) => engine.alter(request),
            _ => match request {
                AlterRequest::SchemaRule {
                    drawer_name,
                    action,
                    kind,
                    field_name,
                    payload,
                } => result_expectations::admin(self.execute_transport(Command::ManageSchema {
                    action,
                    kind,
                    drawer_name,
                    field_name,
                    payload,
                })?),
            },
        }
    }

    pub(crate) fn drop(&self, request: DropRequest) -> Result<Value> {
        match self {
            Self::Embedded(engine) => engine.drop(request),
            _ => match request {
                DropRequest::Database { database_name } => result_expectations::admin(
                    self.execute_transport(Command::DropDatabase { database_name })?,
                ),
                DropRequest::Schema {
                    database_name,
                    schema_name,
                } => result_expectations::admin(self.execute_transport(Command::DropSchema {
                    database_name,
                    schema_name,
                })?),
                DropRequest::Drawer {
                    database_name,
                    schema_name,
                    drawer_name,
                } => result_expectations::admin(self.execute_transport(Command::DropDrawer {
                    database_name,
                    schema_name,
                    drawer_name,
                })?),
                DropRequest::SchemaRule {
                    drawer_name,
                    kind,
                    field_name,
                    payload,
                } => result_expectations::admin(self.execute_transport(Command::ManageSchema {
                    action: "remove".to_string(),
                    kind,
                    drawer_name,
                    field_name,
                    payload,
                })?),
                DropRequest::User { username } => {
                    result_expectations::admin(self.execute_transport(Command::ManageUser {
                        action: "drop_user".to_string(),
                        payload: serde_json::json!({ "username": username }),
                    })?)
                }
            },
        }
    }

    pub(crate) fn grant(&self, request: PermissionRequest) -> Result<Value> {
        match self {
            Self::Embedded(_) => embedded_admin_error("grant"),
            _ => result_expectations::admin(self.execute_transport(Command::ManageUser {
                action: "grant_permission".to_string(),
                payload: request.into_payload(),
            })?),
        }
    }

    pub(crate) fn revoke(&self, request: PermissionRequest) -> Result<Value> {
        match self {
            Self::Embedded(_) => embedded_admin_error("revoke"),
            _ => result_expectations::admin(self.execute_transport(Command::ManageUser {
                action: "revoke_permission".to_string(),
                payload: request.into_payload(),
            })?),
        }
    }

    pub(crate) fn status(&self, request: StatusRequest) -> Result<StatusResult> {
        match self {
            Self::Embedded(engine) => engine.status(request),
            _ => match request {
                StatusRequest::Tenants => {
                    result_expectations::tenants(self.execute_transport(Command::ShowTenants)?)
                        .map(StatusResult::Tenants)
                }
                StatusRequest::Databases => {
                    result_expectations::databases(self.execute_transport(Command::ShowDatabases)?)
                        .map(StatusResult::Databases)
                }
                StatusRequest::Schemas { database_name } => result_expectations::schemas(
                    self.execute_transport(Command::ShowSchemas { database_name })?,
                )
                .map(StatusResult::Schemas),
                StatusRequest::Drawers {
                    database_name,
                    schema_name,
                } => {
                    result_expectations::drawers(self.execute_transport(Command::ShowDrawers {
                        database_name,
                        schema_name,
                    })?)
                    .map(StatusResult::Drawers)
                }
                StatusRequest::Wal { database_name } => result_expectations::wal_verification(
                    self.execute_transport(Command::VerifyWal { database_name })?,
                )
                .map(StatusResult::Wal),
                StatusRequest::Storage => {
                    result_expectations::diagnosis(self.execute_transport(Command::Diagnose)?)
                        .map(StatusResult::Storage)
                }
                StatusRequest::Path { path } => {
                    result_expectations::check(self.execute_transport(Command::Check { path })?)
                        .map(StatusResult::Check)
                }
                StatusRequest::DrawerNames => {
                    result_expectations::drawer_names(self.execute_transport(Command::ListDrawers)?)
                        .map(StatusResult::DrawerNames)
                }
                StatusRequest::CachedDrawerCount => Err(Error::new(
                    ErrorKind::Unsupported,
                    "cached drawer count is only available for embedded Wardrobe engines",
                )),
            },
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

fn embedded_admin_error<T>(operation: &str) -> Result<T> {
    Err(Error::new(
        ErrorKind::Unsupported,
        format!("{operation} requires a remote Wardrobe server with administrative authorization"),
    ))
}
