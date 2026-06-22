use std::io::Result;
use std::path::{Path, PathBuf};

use super::catalog_validation;
use super::pointer;
use super::storage::{StorageCoordinate, StorageScope};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum DatabaseRoute {
    Coordinate(StorageCoordinate),
    Database(String),
    Schema { database: String, schema: String },
}

impl DatabaseRoute {
    pub(crate) fn storage_path(&self, root_directory: &Path) -> Result<PathBuf> {
        match self {
            Self::Coordinate(coordinate) => {
                coordinate.validate()?;
                Ok(coordinate.path_under(root_directory))
            }
            Self::Database(database) => {
                catalog_validation::validate_storage_coordinate_component("database", database)?;
                Ok(root_directory.join(database))
            }
            Self::Schema { database, schema } => {
                catalog_validation::validate_storage_coordinate_component("database", database)?;
                catalog_validation::validate_storage_coordinate_component("schema", schema)?;
                Ok(root_directory.join(database).join(schema))
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ExecutionContext<'a> {
    pub(crate) drawer_namespace: Option<&'a str>,
}

impl ExecutionContext<'_> {
    pub(crate) fn root() -> Self {
        Self {
            drawer_namespace: None,
        }
    }
}

pub(crate) fn validate_scope(scope: &StorageScope) -> Result<()> {
    scope.validate()
}

pub(crate) fn coordinate_catalog_database(coordinate: &StorageCoordinate) -> String {
    format!("{}/{}", coordinate.tenant(), coordinate.database())
}

pub(crate) fn coordinate_database_path(
    root_directory: &Path,
    coordinate: &StorageCoordinate,
) -> Result<PathBuf> {
    DatabaseRoute::Coordinate(coordinate.clone()).storage_path(root_directory)
}

pub(crate) fn database_scope_path(root_directory: &Path, database: &str) -> Result<PathBuf> {
    catalog_validation::database_path_from_name(root_directory, database)
}

pub(crate) fn schema_scope_path(
    root_directory: &Path,
    database: &str,
    schema: &str,
) -> Result<PathBuf> {
    catalog_validation::database_path_from_name(root_directory, &format!("{database}/{schema}"))
}

pub(crate) fn scoped_drawer_name(drawer_name: &str, drawer_namespace: Option<&str>) -> String {
    pointer::scoped_drawer_name(drawer_name, drawer_namespace)
}

pub(crate) fn scoped_pointer(pointer: &str, drawer_namespace: Option<&str>) -> String {
    pointer::scoped_pointer(pointer, drawer_namespace)
}
