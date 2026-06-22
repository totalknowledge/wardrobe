use crate::wrdb_lib::catalog_validation;
use crate::wrdb_lib::command::Command;
use crate::wrdb_lib::discovery;
use crate::wrdb_lib::registry::CatalogRegistry;
use crate::wrdb_lib::storage::StorageInventory;
use std::fs;
use std::io::{Error, ErrorKind, Result};
use std::path::Path;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub(crate) fn create_database<F>(
    root_directory: &Path,
    registry_lock: &RwLock<CatalogRegistry>,
    database_name: &str,
    append_wal: F,
) -> Result<StorageInventory>
where
    F: FnOnce(&Command) -> Result<()>,
{
    catalog_validation::validate_database_name(database_name)?;
    let command = Command::DefineDatabase {
        database_name: database_name.to_string(),
    };
    append_wal(&command)?;

    let database_path = catalog_validation::database_path_from_name(root_directory, database_name)?;
    fs::create_dir_all(&database_path)?;

    {
        let mut registry = write_registry(registry_lock)?;
        registry.register_database(database_name);
        registry.persist_to_root(root_directory)?;
    }

    discovery::storage_inventory(database_name.to_string(), &database_path)
}

pub(crate) fn create_schema<F>(
    root_directory: &Path,
    registry_lock: &RwLock<CatalogRegistry>,
    database_name: &str,
    schema_name: &str,
    append_wal: F,
) -> Result<StorageInventory>
where
    F: FnOnce(&Command) -> Result<()>,
{
    catalog_validation::validate_database_name(database_name)?;
    catalog_validation::validate_schema_name(schema_name)?;
    let command = Command::DefineSchema {
        database_name: database_name.to_string(),
        schema_name: schema_name.to_string(),
    };
    append_wal(&command)?;

    {
        let registry = read_registry(registry_lock)?;
        if !registry.contains_database(database_name) {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Database '{database_name}' is not registered in the catalog"),
            ));
        }
    }

    let schema_path = catalog_validation::database_path_from_name(root_directory, database_name)?
        .join(schema_name);
    fs::create_dir_all(&schema_path)?;

    {
        let mut registry = write_registry(registry_lock)?;
        registry.register_schema(database_name, schema_name);
        registry.persist_to_root(root_directory)?;
    }

    discovery::storage_inventory(schema_name.to_string(), &schema_path)
}

pub(crate) fn create_drawer<F>(
    root_directory: &Path,
    registry_lock: &RwLock<CatalogRegistry>,
    database_name: &str,
    schema_name: &str,
    drawer_name: &str,
    append_wal: F,
) -> Result<StorageInventory>
where
    F: FnOnce(&Command) -> Result<()>,
{
    catalog_validation::validate_database_name(database_name)?;
    catalog_validation::validate_schema_name(schema_name)?;
    catalog_validation::validate_drawer_name(drawer_name)?;
    let command = Command::DefineDrawer {
        database_name: database_name.to_string(),
        schema_name: schema_name.to_string(),
        drawer_name: drawer_name.to_string(),
    };
    append_wal(&command)?;

    {
        let registry = read_registry(registry_lock)?;
        if !registry.contains_schema(database_name, schema_name) {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Schema '{schema_name}' is not registered for database '{database_name}'"),
            ));
        }
    }

    let schema_path = catalog_validation::database_path_from_name(root_directory, database_name)?
        .join(schema_name);
    fs::create_dir_all(&schema_path)?;
    let drawer_path = schema_path.join(format!("{drawer_name}.drw"));
    if !drawer_path.exists() {
        fs::File::create(&drawer_path)?;
    }
    let index_path = schema_path.join(format!("{drawer_name}_index.drw"));
    if !index_path.exists() {
        fs::File::create(&index_path)?;
    }

    {
        let mut registry = write_registry(registry_lock)?;
        registry.register_drawer(
            database_name,
            schema_name,
            drawer_name,
            drawer_path.to_string_lossy().into_owned(),
        );
        registry.persist_to_root(root_directory)?;
    }

    discovery::drawer_inventory(drawer_name.to_string(), &schema_path, drawer_name)
}

pub(crate) fn register_tenant_route<F>(
    root_directory: &Path,
    registry_lock: &RwLock<CatalogRegistry>,
    tenant_id: &str,
    database_name: &str,
    location: &str,
    append_wal: F,
) -> Result<StorageInventory>
where
    F: FnOnce(&Command) -> Result<()>,
{
    catalog_validation::validate_tenant_identifier(tenant_id)?;
    catalog_validation::validate_database_name(database_name)?;
    catalog_validation::validate_catalog_location(location)?;
    let command = Command::DefineTenantRoute {
        tenant_id: tenant_id.to_string(),
        database_name: database_name.to_string(),
        location: location.to_string(),
    };
    append_wal(&command)?;

    let route_path = catalog_validation::catalog_location_path(root_directory, location);
    fs::create_dir_all(&route_path)?;

    {
        let mut registry = write_registry(registry_lock)?;
        registry.register_tenant_route(tenant_id, database_name, location);
        registry.persist_to_root(root_directory)?;
    }

    discovery::storage_inventory(tenant_id.to_string(), &route_path)
}

fn read_registry(lock: &RwLock<CatalogRegistry>) -> Result<RwLockReadGuard<'_, CatalogRegistry>> {
    lock.read()
        .map_err(|_| Error::other("Wardrobe catalog lock was poisoned during read"))
}

fn write_registry(lock: &RwLock<CatalogRegistry>) -> Result<RwLockWriteGuard<'_, CatalogRegistry>> {
    lock.write()
        .map_err(|_| Error::other("Wardrobe catalog lock was poisoned during write"))
}
