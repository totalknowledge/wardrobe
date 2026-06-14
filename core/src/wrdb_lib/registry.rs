use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
        self.entries.is_empty()
    }

    pub fn register_drawer(
        &mut self,
        database: &str,
        schema: &str,
        drawer: &str,
        location: impl Into<String>,
    ) {
        let entry = CatalogEntry {
            database: database.to_string(),
            schema: schema.to_string(),
            drawer: drawer.to_string(),
            location: location.into(),
        };
        self.entries.insert(Self::entry_key(database, schema, drawer), entry);
    }

    pub fn contains_drawer(&self, database: &str, schema: &str, drawer: &str) -> bool {
        self.entries
            .contains_key(&Self::entry_key(database, schema, drawer))
    }

    pub fn database_names(&self) -> Vec<String> {
        let mut names = self
            .entries
            .values()
            .map(|entry| entry.database.clone())
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    }

    pub fn schema_names(&self, database: &str) -> Vec<String> {
        let mut names = self
            .entries
            .values()
            .filter(|entry| entry.database == database)
            .map(|entry| entry.schema.clone())
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    }

    pub fn drawer_entries(&self, database: &str, schema: &str) -> Vec<CatalogEntry> {
        let mut entries = self
            .entries
            .values()
            .filter(|entry| entry.database == database && entry.schema == schema)
            .cloned()
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.drawer.cmp(&right.drawer));
        entries
    }

    fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(CATALOG_MAGIC.len() + 64);
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

    fn from_bytes(bytes: &[u8]) -> std::result::Result<Self, String> {
        if !bytes.starts_with(CATALOG_MAGIC) {
            return Err("catalog magic header is missing or corrupt".to_string());
        }

        serde_json::from_slice(&bytes[CATALOG_MAGIC.len()..])
            .map_err(|error| format!("catalog payload is invalid: {error}"))
    }

    fn entry_key(database: &str, schema: &str, drawer: &str) -> String {
        format!("{database}\u{1f}{schema}\u{1f}{drawer}")
    }
}
