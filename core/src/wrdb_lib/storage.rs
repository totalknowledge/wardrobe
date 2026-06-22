use serde::{Deserialize, Serialize};
use std::io::Result;
use std::path::{Path, PathBuf};

use super::catalog_validation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageInventory {
    pub name: String,
    pub record_count: usize,
    pub disk_size_bytes: u64,
    pub register_file_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StorageCoordinate {
    tenant: String,
    database: String,
    schema: String,
}

impl StorageCoordinate {
    pub fn new(tenant: &str, database: &str, schema: &str) -> Self {
        Self {
            tenant: tenant.to_string(),
            database: database.to_string(),
            schema: schema.to_string(),
        }
    }

    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    pub(crate) fn validate(&self) -> Result<()> {
        catalog_validation::validate_storage_coordinate_component("tenant", &self.tenant)?;
        catalog_validation::validate_storage_coordinate_component("database", &self.database)?;
        catalog_validation::validate_storage_coordinate_component("schema", &self.schema)
    }

    pub(crate) fn path_under(&self, root_directory: &Path) -> PathBuf {
        root_directory
            .join(&self.tenant)
            .join(&self.database)
            .join(&self.schema)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StorageLocator {
    Explicit { drawer: String, id: String },
    Inline(String),
}

impl StorageLocator {
    pub fn explicit(drawer: &str, id: &str) -> Self {
        Self::Explicit {
            drawer: drawer.to_string(),
            id: id.to_string(),
        }
    }

    pub fn inline(locator: &str) -> Self {
        Self::Inline(locator.to_string())
    }
}

impl From<&str> for StorageLocator {
    fn from(locator: &str) -> Self {
        Self::Inline(locator.to_string())
    }
}

impl From<String> for StorageLocator {
    fn from(locator: String) -> Self {
        Self::Inline(locator)
    }
}

impl From<&String> for StorageLocator {
    fn from(locator: &String) -> Self {
        Self::Inline(locator.clone())
    }
}

impl From<(&str, &str)> for StorageLocator {
    fn from((drawer, id): (&str, &str)) -> Self {
        Self::explicit(drawer, id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StorageScope {
    Tenant {
        tenant_id: String,
        database: String,
        schema: String,
    },
    Database {
        database: String,
    },
    Schema {
        database: String,
        schema: String,
    },
    Drawer {
        namespace: String,
    },
}

impl StorageScope {
    pub fn tenant(
        tenant_id: impl Into<String>,
        database: impl Into<String>,
        schema: impl Into<String>,
    ) -> Self {
        Self::Tenant {
            tenant_id: tenant_id.into(),
            database: database.into(),
            schema: schema.into(),
        }
    }

    pub fn database(database: &str) -> Self {
        Self::Database {
            database: database.to_string(),
        }
    }

    pub fn schema(database: &str, schema: &str) -> Self {
        Self::Schema {
            database: database.to_string(),
            schema: schema.to_string(),
        }
    }

    pub fn drawer(namespace: &str) -> Self {
        Self::Drawer {
            namespace: namespace.to_string(),
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        match self {
            Self::Tenant { .. } => Ok(()),
            Self::Database { database } => {
                catalog_validation::validate_storage_coordinate_component("database", database)
            }
            Self::Schema { database, schema } => {
                catalog_validation::validate_storage_coordinate_component("database", database)?;
                catalog_validation::validate_storage_coordinate_component("schema", schema)
            }
            Self::Drawer { namespace } => {
                catalog_validation::validate_storage_coordinate_component("namespace", namespace)
            }
        }
    }
}
