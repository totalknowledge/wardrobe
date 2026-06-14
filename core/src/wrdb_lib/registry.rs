use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Error, ErrorKind, Result};
use std::path::Path;

pub const CATALOG_FILE_NAME: &str = ".catalog.drw";
const CATALOG_MAGIC: &[u8] = b"WRDBCAT1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub database: String,
    pub schema: String,
    pub drawer: String,
    pub location: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogRegistry {
    #[serde(default)]
    databases: BTreeSet<String>,
    #[serde(default)]
    schemas: BTreeSet<String>,
    entries: BTreeMap<String, CatalogEntry>,
}

impl CatalogRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open_or_initialize(root_directory: &Path) -> Result<Self> {
        let catalog_path = root_directory.join(CATALOG_FILE_NAME);
        if catalog_path.exists() {
            return Self::load_from_root(root_directory);
        }

        let registry = Self::new();
        registry.persist_to_root(root_directory)?;
        Ok(registry)
    }

    pub fn load_from_root(root_directory: &Path) -> Result<Self> {
        let catalog_path = root_directory.join(CATALOG_FILE_NAME);
        let contents = fs::read(&catalog_path)?;
        Self::from_bytes(&contents).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to parse {}: {error}", catalog_path.display()),
            )
        })
    }

    pub fn persist_to_root(&self, root_directory: &Path) -> Result<()> {
        fs::create_dir_all(root_directory)?;
        fs::write(root_directory.join(CATALOG_FILE_NAME), self.to_bytes()?)
    }

    pub fn is_empty(&self) -> bool {
        self.databases.is_empty() && self.schemas.is_empty() && self.entries.is_empty()
    }

    pub fn register_database(&mut self, database: &str) {
        self.databases.insert(database.to_string());
    }

    pub fn register_schema(&mut self, database: &str, schema: &str) {
        self.register_database(database);
        self.schemas.insert(Self::schema_key(database, schema));
    }

    pub fn register_drawer(
        &mut self,
        database: &str,
        schema: &str,
        drawer: &str,
        location: impl Into<String>,
    ) {
        self.register_schema(database, schema);
        let entry = CatalogEntry {
            database: database.to_string(),
            schema: schema.to_string(),
            drawer: drawer.to_string(),
            location: location.into(),
        };
        self.entries
            .insert(Self::entry_key(database, schema, drawer), entry);
    }

    pub fn contains_database(&self, database: &str) -> bool {
        self.databases.contains(database)
            || self
                .entries
                .values()
                .any(|entry| entry.database == database)
    }

    pub fn contains_schema(&self, database: &str, schema: &str) -> bool {
        self.schemas.contains(&Self::schema_key(database, schema))
            || self
                .entries
                .values()
                .any(|entry| entry.database == database && entry.schema == schema)
    }

    pub fn contains_drawer(&self, database: &str, schema: &str, drawer: &str) -> bool {
        self.entries
            .contains_key(&Self::entry_key(database, schema, drawer))
    }

    pub fn database_names(&self) -> Vec<String> {
        let mut names = self.databases.clone();
        names.extend(self.entries.values().map(|entry| entry.database.clone()));
        names.into_iter().collect()
    }

    pub fn schema_names(&self, database: &str) -> Vec<String> {
        let mut names = self
            .schemas
            .iter()
            .filter_map(|schema_key| schema_key.split_once('\u{1e}'))
            .filter(|(entry_database, _)| *entry_database == database)
            .map(|(_, schema)| schema.to_string())
            .collect::<BTreeSet<_>>();

        names.extend(
            self.entries
                .values()
                .filter(|entry| entry.database == database)
                .map(|entry| entry.schema.clone()),
        );

        names.into_iter().collect()
    }

    pub fn drawer_entries(&self, database: &str, schema: &str) -> Vec<CatalogEntry> {
        self.entries
            .values()
            .filter(|entry| entry.database == database && entry.schema == schema)
            .cloned()
            .collect()
    }

    fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(CATALOG_MAGIC);
        let payload = serde_json::to_vec(self).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to serialize catalog payload: {error}"),
            )
        })?;
        bytes.extend_from_slice(&payload);
        Ok(bytes)
    }

    fn from_bytes(bytes: &[u8]) -> serde_json::Result<Self> {
        if !bytes.starts_with(CATALOG_MAGIC) {
            return Err(serde::de::Error::custom("catalog magic header is invalid"));
        }

        serde_json::from_slice(&bytes[CATALOG_MAGIC.len()..])
    }

    fn entry_key(database: &str, schema: &str, drawer: &str) -> String {
        format!("{database}\u{1f}{schema}\u{1f}{drawer}")
    }

    fn schema_key(database: &str, schema: &str) -> String {
        format!("{database}\u{1e}{schema}")
    }
}
