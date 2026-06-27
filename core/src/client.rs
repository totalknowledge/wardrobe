use crate::wrdb_lib::connection::{ConnectionTarget, DriverKind};
use crate::wrdb_lib::driver::ClientDriver;
use crate::{
    AlterRequest, BackupArchive, CompactRequest, CreateRequest, CreateResult, DeleteResult,
    DropRequest, InspectResult, OperationFilter, OperationOptions, PermissionRequest, ReadResult,
    RestoreReport, StatusRequest, StatusResult, UpsertResult, VacuumReport,
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

    pub fn compact<C>(&self, request: C) -> Result<VacuumReport>
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

    pub fn status<S>(&self, request: S) -> Result<StatusResult>
    where
        S: Into<StatusRequest>,
    {
        self.driver.status(request.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompactMode, OrderDirection, QueryModifiers, ReturnShape};
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(test_name: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir()
            .join(format!("wardrobe_client_unit_{test_name}_{nanos}"))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn client_wrappers_delegate_to_embedded_driver() {
        let path = temp_path("wrappers");
        let client = WardrobeClient::open(&path).expect("client should open");

        assert!(client.requires_embedded_engine());
        assert!(!client.uses_socket_transport());

        let pointers = client
            .upsert(
                json!({"_id": "ruby", "power": 42}),
                OperationFilter::drawer("gem"),
                OperationOptions::new().atomic(true),
            )
            .expect("upsert should work")
            .into_pointers();
        assert_eq!(pointers, vec!["@gem:ruby".to_string()]);

        let read = client
            .read(
                OperationFilter::query_in("gem", json!({"power": 42})),
                OperationOptions::new()
                    .return_shape(ReturnShape::Pointers)
                    .limit(1)
                    .offset(0)
                    .order_by("power")
                    .order_direction(OrderDirection::Descending),
            )
            .expect("read should work");
        assert_eq!(read, ReadResult::Pointers(vec!["@gem:ruby".to_string()]));

        assert_eq!(
            client
                .count(
                    OperationFilter::drawer("gem"),
                    OperationOptions::from(QueryModifiers {
                        limit: Some(1),
                        offset: None,
                        order_by: None,
                        order_direction: None,
                    }),
                )
                .expect("count should work"),
            1
        );

        assert!(matches!(
            client
                .inspect(OperationFilter::drawer("gem"), ())
                .expect("inspect should work"),
            InspectResult::Drawer(_)
        ));
        assert!(client.compact("gem").is_ok());
        assert!(
            client
                .compact(CompactRequest::drawer_with_mode(
                    "gem",
                    CompactMode::Migrate
                ))
                .is_ok()
        );
        assert_eq!(
            client
                .delete(OperationFilter::pointer("@gem:ruby"), ())
                .expect("delete should work")
                .deleted,
            1
        );
        assert!(matches!(
            client
                .create(CreateRequest::database("wardrobe"))
                .expect("create should work"),
            CreateResult::StorageInventory(_)
        ));
        assert!(
            client
                .alter(AlterRequest::schema_rule(
                    "gem",
                    "add",
                    "index",
                    "power",
                    json!({"kind": "index"}),
                ))
                .is_ok()
        );
        assert!(
            client
                .drop(DropRequest::schema_rule("gem", "index", "power", json!({})))
                .is_ok()
        );
        assert!(
            client
                .status(StatusRequest::cached_drawer_count())
                .expect("status should work")
                .eq(&StatusResult::CachedDrawerCount(1))
        );
        assert!(
            client
                .grant(PermissionRequest::new("alice", "armory:r"))
                .is_err()
        );
        assert!(
            client
                .revoke(PermissionRequest::new("alice", "armory:r"))
                .is_err()
        );

        let _ = std::fs::remove_dir_all(path);
    }
}
