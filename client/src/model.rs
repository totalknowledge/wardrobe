use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct QueryModifiers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_direction: Option<OrderDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationFilter {
    pub database: String,
    pub schema: String,
    pub drawer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<Value>,
}

impl OperationFilter {
    pub fn new(database: impl Into<String>, schema: impl Into<String>, drawer: impl Into<String>) -> Self {
        Self {
            database: database.into(),
            schema: schema.into(),
            drawer: drawer.into(),
            id: None,
            query: None,
        }
    }

    pub fn drawer(drawer: impl Into<String>) -> Self {
        Self::new("", "", drawer)
    }

    pub fn pointer(pointer: impl AsRef<str>) -> Self {
        let raw = pointer.as_ref();
        let id = raw.trim_start_matches('@');
        let (drawer, id) = id.split_once(':').unwrap_or(("", id));
        Self {
            database: String::new(),
            schema: String::new(),
            drawer: drawer.to_string(),
            id: Some(id.to_string()),
            query: None,
        }
    }

    pub fn query_in(drawer: impl Into<String>, query: impl Into<Value>) -> Self {
        Self {
            database: String::new(),
            schema: String::new(),
            drawer: drawer.into(),
            id: None,
            query: Some(query.into()),
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn with_query(mut self, query: impl Into<Value>) -> Self {
        self.query = Some(query.into());
        self
    }
}

impl<D, S, R> From<(D, S, R)> for OperationFilter
where
    D: Into<String>,
    S: Into<String>,
    R: Into<String>,
{
    fn from((database, schema, drawer): (D, S, R)) -> Self {
        Self::new(database, schema, drawer)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OperationOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydrate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_fields: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

impl OperationOptions {
    pub fn new() -> Self {
        Self::default()
    }
}

impl From<QueryModifiers> for OperationOptions {
    fn from(modifiers: QueryModifiers) -> Self {
        Self {
            hydrate: None,
            fields: None,
            exclude_fields: None,
            limit: modifiers.limit,
            offset: modifiers.offset,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadResult {
    pub records: Vec<Value>,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpsertResult {
    pub record: Value,
    pub created: bool,
    #[serde(default)]
    pub pointers: Vec<String>,
}

impl UpsertResult {
    pub fn into_pointers(self) -> Vec<String> {
        if self.pointers.is_empty() {
            if let Some(id) = self.record.get("_id").and_then(Value::as_str) {
                vec![format!("@{id}")]
            } else {
                vec![]
            }
        } else {
            self.pointers
        }
    }

    pub fn fmt_pointer(&self) -> Option<String> {
        self.pointers.first().cloned().or_else(|| {
            self.record.get("_id").and_then(Value::as_str).map(|id| format!("@{id}"))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteResult {
    pub deleted_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InspectResult {
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VacuumReport {
    pub status: String,
    pub bytes_reclaimed: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateRequest {
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

impl CreateRequest {
    pub fn database(name: impl Into<String>) -> Self {
        Self {
            kind: "database".to_string(),
            name: name.into(),
            database: None,
            schema: None,
        }
    }

    pub fn schema(database: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            kind: "schema".to_string(),
            name: name.into(),
            database: Some(database.into()),
            schema: None,
        }
    }

    pub fn drawer(database: impl Into<String>, schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            kind: "drawer".to_string(),
            name: name.into(),
            database: Some(database.into()),
            schema: Some(schema.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateResult {
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DropRequest {
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlterRequest {
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactRequest {
    pub database: String,
    pub schema: String,
    pub drawer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupArchive {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreReport {
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub user: String,
    pub permission: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusRequestOutput {
    pub status: String,
    pub details: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageInventory {
    pub name: String,
    pub record_count: usize,
    pub disk_size_bytes: u64,
    pub register_file_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusRequest {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

impl StatusRequest {
    pub fn databases() -> Self {
        Self {
            kind: "databases".to_string(),
            database: None,
            schema: None,
        }
    }

    pub fn schemas(database: impl Into<String>) -> Self {
        Self {
            kind: "schemas".to_string(),
            database: Some(database.into()),
            schema: None,
        }
    }

    pub fn drawers(database: impl Into<String>, schema: impl Into<String>) -> Self {
        Self {
            kind: "drawers".to_string(),
            database: Some(database.into()),
            schema: Some(schema.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTlsConfig {
    pub ca_cert: PathBuf,
    pub client_cert: PathBuf,
    pub client_key: PathBuf,
}

impl ClientTlsConfig {
    pub fn new(
        ca_cert: impl Into<PathBuf>,
        client_cert: impl Into<PathBuf>,
        client_key: impl Into<PathBuf>,
    ) -> Self {
        Self {
            ca_cert: ca_cert.into(),
            client_cert: client_cert.into(),
            client_key: client_key.into(),
        }
    }

    pub fn from_profile(profile_path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(profile_path.as_ref())?;
        let parsed: Value = toml::from_str(&content).map_err(|err| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Invalid client profile TOML: {err}"),
            )
        })?;

        let dir = profile_path.as_ref().parent().unwrap_or_else(|| Path::new("."));
        let ca_cert = resolve_path(dir, parsed.get("ca_cert").and_then(Value::as_str))?;
        let client_cert = resolve_path(dir, parsed.get("client_cert").and_then(Value::as_str))?;
        let client_key = resolve_path(dir, parsed.get("client_key").and_then(Value::as_str))?;

        Ok(Self {
            ca_cert,
            client_cert,
            client_key,
        })
    }
}

fn resolve_path(base_dir: &Path, raw_path: Option<&str>) -> Result<PathBuf> {
    let raw = raw_path.ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "Missing certificate path in client profile",
        )
    })?;
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(base_dir.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operation_filter_builders() {
        let filter = OperationFilter::new("db1", "sch1", "drw1");
        assert_eq!(filter.database, "db1");
        assert_eq!(filter.schema, "sch1");
        assert_eq!(filter.drawer, "drw1");

        let drw_filter = OperationFilter::drawer("books");
        assert_eq!(drw_filter.drawer, "books");

        let ptr_filter = OperationFilter::pointer("@users:123");
        assert_eq!(ptr_filter.drawer, "users");
        assert_eq!(ptr_filter.id.unwrap(), "123");

        let q_filter = OperationFilter::query_in("books", serde_json::json!({"author": "Tolkien"}));
        assert_eq!(q_filter.drawer, "books");
        assert!(q_filter.query.is_some());

        let tuple_filter: OperationFilter = ("d", "s", "r").into();
        assert_eq!(tuple_filter.database, "d");
        assert_eq!(tuple_filter.schema, "s");
        assert_eq!(tuple_filter.drawer, "r");
    }

    #[test]
    fn test_operation_options_builders() {
        let options = OperationOptions {
            hydrate: Some(true),
            fields: Some(vec!["title".to_string()]),
            limit: Some(10),
            offset: Some(5),
            ..Default::default()
        };

        assert_eq!(options.hydrate, Some(true));
        assert_eq!(options.fields, Some(vec!["title".to_string()]));
        assert_eq!(options.limit, Some(10));
        assert_eq!(options.offset, Some(5));
    }

    #[test]
    fn test_model_constructors_and_pointer_fallbacks() {
        let filter = OperationFilter::drawer("books")
            .with_id("book-1")
            .with_query(serde_json::json!({"active": true}));
        assert_eq!(filter.id.as_deref(), Some("book-1"));
        assert_eq!(filter.query, Some(serde_json::json!({"active": true})));

        let pointer_without_drawer = OperationFilter::pointer("book-2");
        assert!(pointer_without_drawer.drawer.is_empty());
        assert_eq!(pointer_without_drawer.id.as_deref(), Some("book-2"));

        assert_eq!(OperationOptions::new(), OperationOptions::default());
        let options = OperationOptions::from(QueryModifiers {
            order_by: Some("title".to_string()),
            order_direction: Some(OrderDirection::Descending),
            limit: Some(12),
            offset: Some(4),
        });
        assert_eq!(options.limit, Some(12));
        assert_eq!(options.offset, Some(4));

        let explicit_pointer = UpsertResult {
            record: serde_json::json!({"_id": "books:1"}),
            created: true,
            pointers: vec!["@books:1".to_string()],
        };
        assert_eq!(
            explicit_pointer.fmt_pointer().as_deref(),
            Some("@books:1")
        );
        assert_eq!(
            explicit_pointer.clone().into_pointers(),
            vec!["@books:1".to_string()]
        );

        let record_pointer = UpsertResult {
            record: serde_json::json!({"_id": "books:2"}),
            created: true,
            pointers: Vec::new(),
        };
        assert_eq!(record_pointer.fmt_pointer().as_deref(), Some("@books:2"));
        assert_eq!(
            record_pointer.into_pointers(),
            vec!["@books:2".to_string()]
        );

        let no_pointer = UpsertResult {
            record: serde_json::json!({}),
            created: false,
            pointers: Vec::new(),
        };
        assert_eq!(no_pointer.fmt_pointer(), None);
        assert!(no_pointer.into_pointers().is_empty());

        let database = CreateRequest::database("library");
        let schema = CreateRequest::schema("library", "public");
        let drawer = CreateRequest::drawer("library", "public", "books");
        assert_eq!(database.kind, "database");
        assert_eq!(schema.database.as_deref(), Some("library"));
        assert_eq!(drawer.schema.as_deref(), Some("public"));

        let databases = StatusRequest::databases();
        let schemas = StatusRequest::schemas("library");
        let drawers = StatusRequest::drawers("library", "public");
        assert_eq!(databases.kind, "databases");
        assert_eq!(schemas.database.as_deref(), Some("library"));
        assert_eq!(drawers.schema.as_deref(), Some("public"));

        let tls = ClientTlsConfig::new("ca.pem", "client.pem", "client.key");
        assert_eq!(tls.ca_cert, PathBuf::from("ca.pem"));
        assert_eq!(tls.client_cert, PathBuf::from("client.pem"));
        assert_eq!(tls.client_key, PathBuf::from("client.key"));
    }

    #[test]
    fn test_client_profile_parsing() {
        let temp_dir = std::env::temp_dir();
        let profile_file = temp_dir.join(format!("test_profile_{}.toml", uuid::Uuid::new_v4()));
        std::fs::write(
            &profile_file,
            r#"
ca_cert = "ca.crt"
client_cert = "client.crt"
client_key = "client.key"
"#,
        )
        .unwrap();

        let profile = ClientTlsConfig::from_profile(&profile_file).unwrap();
        assert_eq!(profile.ca_cert, temp_dir.join("ca.crt"));
        assert_eq!(profile.client_cert, temp_dir.join("client.crt"));
        assert_eq!(profile.client_key, temp_dir.join("client.key"));

        let _ = std::fs::remove_file(profile_file);
    }

    #[test]
    fn test_client_profile_validation_errors_and_absolute_paths() {
        let temp_dir = std::env::temp_dir();
        let suffix = uuid::Uuid::new_v4();
        let invalid_profile = temp_dir.join(format!("invalid_profile_{suffix}.toml"));
        let missing_path_profile = temp_dir.join(format!("missing_path_profile_{suffix}.toml"));
        let absolute_profile = temp_dir.join(format!("absolute_profile_{suffix}.toml"));

        std::fs::write(&invalid_profile, "not = [valid").unwrap();
        assert_eq!(
            ClientTlsConfig::from_profile(&invalid_profile)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidData
        );

        std::fs::write(&missing_path_profile, "ca_cert = \"ca.pem\"").unwrap();
        assert_eq!(
            ClientTlsConfig::from_profile(&missing_path_profile)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidData
        );

        std::fs::write(
            &absolute_profile,
            "ca_cert = \"/ca.pem\"\nclient_cert = \"/client.pem\"\nclient_key = \"/client.key\"\n",
        )
        .unwrap();
        let tls = ClientTlsConfig::from_profile(&absolute_profile).unwrap();
        assert_eq!(tls.ca_cert, PathBuf::from("/ca.pem"));
        assert_eq!(tls.client_cert, PathBuf::from("/client.pem"));
        assert_eq!(tls.client_key, PathBuf::from("/client.key"));

        let _ = std::fs::remove_file(invalid_profile);
        let _ = std::fs::remove_file(missing_path_profile);
        let _ = std::fs::remove_file(absolute_profile);
    }
}
