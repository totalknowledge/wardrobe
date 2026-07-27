use crate::wrdb_lib::command::*;
use crate::wrdb_lib::connection::{ConnectionTarget, DriverKind};
use crate::wrdb_lib::driver::ClientDriver;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wardrobe_client_invalid_scheme() {
        assert!(WardrobeClient::open("invalid+scheme://test").is_err());
    }

    #[test]
    #[cfg(unix)]
    fn test_wardrobe_client_unix() {
        use crate::wrdb_lib::protocol::{ProtocolFrame, ProtocolOpcode};
        use std::os::unix::net::UnixListener;

        let temp_dir = std::env::temp_dir();
        let sock_path = temp_dir.join(format!("test_client_{}.sock", uuid::Uuid::new_v4()));

        let listener = UnixListener::bind(&sock_path).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let frame = ProtocolFrame::read_from_stream(&mut stream).unwrap();
            assert_eq!(frame.opcode, ProtocolOpcode::Command);
            let resp_frame = ProtocolFrame::new(
                ProtocolOpcode::Result,
                b"{\"record\":null,\"pointers\":[\"@b:1\"],\"created\":true}".to_vec(),
            );
            resp_frame.write_to_stream(&mut stream).unwrap();
        });

        let uri = format!("wardrobe+unix://{}", sock_path.display());
        let client = WardrobeClient::open(&uri).unwrap();
        assert_eq!(client.driver_kind(), DriverKind::UnixSocket);
        assert!(!client.requires_embedded_engine());
        assert!(client.uses_socket_transport());
        assert_eq!(
            client.connection_target(),
            &ConnectionTarget::UnixSocket {
                path: sock_path.clone()
            }
        );

        let res = client
            .upsert(
                serde_json::json!({"title": "Rust"}),
                OperationFilter::drawer("books"),
                OperationOptions::default(),
            )
            .unwrap();

        assert_eq!(res.pointers, vec!["@b:1".to_string()]);

        handle.join().unwrap();
        let _ = std::fs::remove_file(sock_path);
    }

    #[test]
    #[cfg(unix)]
    fn test_wardrobe_client_canonical_command_surface() {
        use crate::wrdb_lib::protocol::{ProtocolFrame, ProtocolOpcode};
        use std::os::unix::net::UnixListener;

        let temp_dir = std::env::temp_dir();
        let sock_path = temp_dir.join(format!("test_commands_{}.sock", uuid::Uuid::new_v4()));
        let responses = vec![
            (
                "Upsert",
                serde_json::json!({
                    "record": {"_id": "books:1"},
                    "created": true,
                    "pointers": ["@books:1"]
                }),
            ),
            (
                "Read",
                serde_json::json!({"records": [{"_id": "books:1"}], "count": 1}),
            ),
            ("Count", serde_json::json!(3)),
            ("Delete", serde_json::json!({"deleted_count": 2})),
            ("Compact", serde_json::json!({"status": "compacted"})),
            (
                "Inspect",
                serde_json::json!({"metadata": {"name": "books"}}),
            ),
            ("Backup", serde_json::json!({"path": "/tmp/backup.wrb"})),
            ("Restore", serde_json::json!({"status": "restored"})),
            ("Create", serde_json::json!({"status": "created"})),
            ("Alter", serde_json::json!({"status": "altered"})),
            ("Drop", serde_json::json!({"status": "dropped"})),
            ("Grant", serde_json::json!({"status": "granted"})),
            ("Revoke", serde_json::json!({"status": "revoked"})),
            (
                "Status",
                serde_json::json!({"status": "ready", "details": {}}),
            ),
        ];

        let listener = UnixListener::bind(&sock_path).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for (expected_type, response) in responses {
                let frame = ProtocolFrame::read_from_stream(&mut stream).unwrap();
                let command: Value = serde_json::from_slice(&frame.payload).unwrap();
                assert_eq!(command["type"], expected_type);
                ProtocolFrame::new(
                    ProtocolOpcode::Result,
                    serde_json::to_vec(&response).unwrap(),
                )
                .write_to_stream(&mut stream)
                .unwrap();
            }
        });

        let client = WardrobeClient::open(format!("wardrobe+unix://{}", sock_path.display()))
            .unwrap();
        let filter = OperationFilter::drawer("books");
        let options = OperationOptions::default();

        assert_eq!(
            client
                .upsert(
                    serde_json::json!({"title": "Rust"}),
                    filter.clone(),
                    options.clone(),
                )
                .unwrap()
                .pointers,
            vec!["@books:1".to_string()]
        );
        assert_eq!(
            client.read(filter.clone(), options.clone()).unwrap().count,
            1
        );
        assert_eq!(
            client.count(filter.clone(), options.clone()).unwrap(),
            3
        );
        assert_eq!(
            client
                .delete(filter.clone(), options.clone())
                .unwrap()
                .deleted_count,
            2
        );
        assert_eq!(
            client
                .compact(CompactRequest {
                    database: "library".to_string(),
                    schema: "public".to_string(),
                    drawer: "books".to_string(),
                })
                .unwrap()["status"],
            "compacted"
        );
        assert_eq!(
            client.inspect(filter, options).unwrap().metadata["name"],
            "books"
        );

        let archive = client.backup("/tmp/library").unwrap();
        assert_eq!(archive.path, std::path::PathBuf::from("/tmp/backup.wrb"));
        assert_eq!(
            client
                .restore("/tmp/restored", archive)
                .unwrap()
                .status,
            "restored"
        );
        assert_eq!(
            client.create(CreateRequest::database("library")).unwrap().status,
            "created"
        );
        assert_eq!(
            client
                .alter(AlterRequest {
                    kind: "drawer".to_string(),
                    name: "books".to_string(),
                    database: Some("library".to_string()),
                    schema: Some("public".to_string()),
                    drawer: None,
                    options: None,
                })
                .unwrap()["status"],
            "altered"
        );
        assert_eq!(
            client
                .drop(DropRequest {
                    kind: "drawer".to_string(),
                    name: "books".to_string(),
                    database: Some("library".to_string()),
                    schema: Some("public".to_string()),
                })
                .unwrap()["status"],
            "dropped"
        );

        let permission = PermissionRequest {
            user: "reader".to_string(),
            permission: "read".to_string(),
        };
        assert_eq!(client.grant(permission.clone()).unwrap()["status"], "granted");
        assert_eq!(client.revoke(permission).unwrap()["status"], "revoked");
        assert_eq!(
            client.status(StatusRequest::databases()).unwrap()["status"],
            "ready"
        );

        handle.join().unwrap();
        let _ = std::fs::remove_file(sock_path);
    }
}
