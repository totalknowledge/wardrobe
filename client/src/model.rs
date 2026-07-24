use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadResult {
    pub records: Vec<Value>,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpsertResult {
    pub record: Value,
    pub created: bool,
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
