use crate::wrdb_lib::catalog_validation;
use crate::wrdb_lib::drawer::{
    INDEX_FIELD_KEY, INDEX_OFFSET_KEY, INDEX_STATUS_KEY, INDEX_VALUE_KEY,
};
use crate::wrdb_lib::reader::DatabaseReader;
use crate::wrdb_lib::registry::{CatalogEntry, CatalogRegistry};
use crate::wrdb_lib::storage::StorageInventory;
use crate::wrdb_lib::storage_format::{BsonBinaryFormat, StorageFormat};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Error, ErrorKind, Result};
use std::path::{Component, Path, PathBuf};

pub(crate) fn show_tenants(
    root_directory: &Path,
    registry: &CatalogRegistry,
) -> Result<Vec<String>> {
    if !registry.is_empty() {
        let mut tenants = BTreeSet::new();
        let explicit_tenants = registry.tenant_ids();
        let has_explicit_tenants = !explicit_tenants.is_empty();
        tenants.extend(explicit_tenants);

        for database_name in registry.database_names() {
            if let Some((tenant_name, _)) = database_name.split_once('/') {
                tenants.insert(tenant_name.to_string());
            } else if !has_explicit_tenants {
                tenants.insert(database_name);
            }
        }
        return Ok(tenants.into_iter().collect());
    }

    discover_tenants(root_directory)
}

pub(crate) fn show_databases(
    root_directory: &Path,
    registry: &CatalogRegistry,
) -> Result<Vec<StorageInventory>> {
    if !registry.is_empty() {
        return registry
            .database_names()
            .into_iter()
            .map(|name| {
                let path = catalog_validation::database_path_from_name(root_directory, &name)?;
                if path.exists() {
                    storage_inventory(name, &path)
                } else {
                    Ok(StorageInventory {
                        name,
                        record_count: 0,
                        disk_size_bytes: 0,
                        register_file_count: 0,
                    })
                }
            })
            .collect();
    }

    discover_databases(root_directory)
}

pub(crate) fn show_schemas(
    root_directory: &Path,
    registry: &CatalogRegistry,
    database_name: &str,
) -> Result<Vec<String>> {
    if !registry.is_empty() {
        return Ok(registry.schema_names(database_name));
    }

    let database_path = catalog_validation::database_path_from_name(root_directory, database_name)?;
    discover_schemas(&database_path)
}

pub(crate) fn show_drawers(
    root_directory: &Path,
    registry: &CatalogRegistry,
    database_name: &str,
    schema_name: &str,
) -> Result<Vec<StorageInventory>> {
    if !registry.is_empty() {
        return registry
            .drawer_entries(database_name, schema_name)
            .iter()
            .map(catalog_drawer_inventory)
            .collect();
    }

    let database_path = catalog_validation::database_path_from_name(root_directory, database_name)?;
    discover_drawers(&database_path, schema_name)
}

pub(crate) fn storage_inventory(name: String, path: &Path) -> Result<StorageInventory> {
    let mut record_count = 0;
    let mut disk_size_bytes = 0;
    let mut register_file_count = 0;
    accumulate_storage_inventory(
        path,
        &mut record_count,
        &mut disk_size_bytes,
        &mut register_file_count,
    )?;

    Ok(StorageInventory {
        name,
        record_count,
        disk_size_bytes,
        register_file_count,
    })
}

pub(crate) fn drawer_inventory(
    name: String,
    directory: &Path,
    physical_drawer_name: &str,
) -> Result<StorageInventory> {
    let data_path = directory.join(format!("{physical_drawer_name}.drw"));
    let index_path = directory.join(format!("{physical_drawer_name}_index.drw"));
    let meta_path = directory.join(format!("{physical_drawer_name}_meta.drw"));
    let companion_paths = [data_path, index_path.clone(), meta_path.clone()];

    let mut disk_size_bytes = 0;
    let mut register_file_count = 0;
    for path in companion_paths {
        if path.exists() {
            disk_size_bytes += fs::metadata(path)?.len();
            register_file_count += 1;
        }
    }

    let record_count = drawer_record_count_from_index(&index_path)?
        .or_else(|| metadata_record_count(&meta_path).ok())
        .unwrap_or_default();

    Ok(StorageInventory {
        name,
        record_count,
        disk_size_bytes,
        register_file_count,
    })
}

fn catalog_drawer_inventory(entry: &CatalogEntry) -> Result<StorageInventory> {
    let location = PathBuf::from(&entry.location);
    let (directory, physical_drawer_name) = if location
        .extension()
        .and_then(|extension| extension.to_str())
        == Some("drw")
    {
        let directory = location
            .parent()
            .map(|path| path.to_path_buf())
            .unwrap_or_else(PathBuf::new);
        let physical_drawer_name = location
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(entry.drawer.as_str())
            .to_string();
        (directory, physical_drawer_name)
    } else {
        (location, entry.drawer.clone())
    };

    if !directory.exists() {
        return Ok(StorageInventory {
            name: entry.drawer.clone(),
            record_count: 0,
            disk_size_bytes: 0,
            register_file_count: 0,
        });
    }

    drawer_inventory(entry.drawer.clone(), &directory, &physical_drawer_name)
}

fn discover_tenants(root_directory: &Path) -> Result<Vec<String>> {
    let mut tenants = BTreeSet::new();

    if !root_directory.exists() {
        return Ok(Vec::new());
    }

    collect_drawer_prefix_tenants(root_directory, &mut tenants)?;

    for top_level in child_directories(root_directory)? {
        let Some(top_level_name) = file_name_to_string(&top_level) else {
            continue;
        };

        if directory_has_drawer_files(&top_level)? {
            tenants.insert(top_level_name.clone());
            collect_drawer_prefix_tenants(&top_level, &mut tenants)?;
        }

        let mut has_coordinate_storage = false;
        for second_level in child_directories(&top_level)? {
            if directory_has_drawer_files(&second_level)? {
                if let Some(schema_name) = file_name_to_string(&second_level) {
                    tenants.insert(schema_name);
                }
                collect_drawer_prefix_tenants(&second_level, &mut tenants)?;
            }

            for third_level in child_directories(&second_level)? {
                if directory_has_drawer_files(&third_level)? {
                    has_coordinate_storage = true;
                    collect_drawer_prefix_tenants(&third_level, &mut tenants)?;
                }
            }
        }

        if has_coordinate_storage {
            tenants.insert(top_level_name);
        }
    }

    Ok(tenants.into_iter().collect())
}

fn discover_databases(root_directory: &Path) -> Result<Vec<StorageInventory>> {
    let mut database_paths = BTreeMap::new();

    if !root_directory.exists() {
        return Ok(Vec::new());
    }

    if directory_has_drawer_files(root_directory)? {
        database_paths.insert(
            database_inventory_name(root_directory, root_directory),
            root_directory.to_path_buf(),
        );
    }

    for top_level in child_directories(root_directory)? {
        if directory_has_database_layout(&top_level)? {
            database_paths.insert(
                database_inventory_name(root_directory, &top_level),
                top_level,
            );
            continue;
        }

        for second_level in child_directories(&top_level)? {
            if directory_has_database_layout(&second_level)? {
                database_paths.insert(
                    database_inventory_name(root_directory, &second_level),
                    second_level,
                );
            }
        }
    }

    database_paths
        .into_iter()
        .map(|(name, path)| storage_inventory(name, &path))
        .collect()
}

fn discover_schemas(database_path: &Path) -> Result<Vec<String>> {
    let mut schemas = BTreeSet::new();

    if !database_path.exists() {
        return Ok(Vec::new());
    }

    for child in child_directories(database_path)? {
        if directory_has_drawer_files(&child)? {
            if let Some(schema_name) = file_name_to_string(&child) {
                schemas.insert(schema_name);
            }
        }
    }

    collect_flat_schema_prefixes(database_path, &mut schemas)?;

    Ok(schemas.into_iter().collect())
}

fn discover_drawers(database_path: &Path, schema_name: &str) -> Result<Vec<StorageInventory>> {
    catalog_validation::validate_storage_coordinate_component("schema", schema_name)?;
    let mut drawer_paths = BTreeMap::new();

    let nested_schema_path = database_path.join(schema_name);
    if nested_schema_path.exists() {
        collect_drawers_in_directory(&nested_schema_path, None, &mut drawer_paths)?;
    }

    collect_drawers_in_directory(database_path, Some(schema_name), &mut drawer_paths)?;

    drawer_paths
        .into_iter()
        .map(|(name, (directory, physical_drawer_name))| {
            drawer_inventory(name, &directory, &physical_drawer_name)
        })
        .collect()
}

fn collect_drawers_in_directory(
    directory: &Path,
    flat_schema_name: Option<&str>,
    drawer_paths: &mut BTreeMap<String, (PathBuf, String)>,
) -> Result<()> {
    if !directory.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let Some(physical_drawer_name) = drawer_name_from_file_path(&entry.path()) else {
            continue;
        };

        let logical_drawer_name = if let Some(schema_name) = flat_schema_name {
            let Some((prefix, drawer_name)) = physical_drawer_name.split_once('.') else {
                continue;
            };

            if prefix != schema_name || drawer_name.is_empty() {
                continue;
            }

            drawer_name.to_string()
        } else {
            physical_drawer_name.clone()
        };

        drawer_paths.insert(
            logical_drawer_name,
            (directory.to_path_buf(), physical_drawer_name),
        );
    }

    Ok(())
}

fn directory_has_database_layout(directory: &Path) -> Result<bool> {
    if directory_has_drawer_files(directory)? {
        return Ok(true);
    }

    for child in child_directories(directory)? {
        if directory_has_drawer_files(&child)? {
            return Ok(true);
        }
    }

    Ok(false)
}

fn drawer_record_count_from_index(index_path: &Path) -> Result<Option<usize>> {
    if !index_path.exists() {
        return Ok(None);
    }

    let mut live_primary_keys = BTreeSet::new();
    let mut saw_primary_key_rows = false;
    let reader = DatabaseReader::open_drawer(index_path)?;

    let mut index_entries = Vec::new();
    reader.stream_with_offsets(|_offset, slot| {
        if !BsonBinaryFormat::is_tombstone(slot) {
            index_entries.push(slot.to_vec());
        }
    })?;

    for slot in index_entries {
        let Some(index_entry) = BsonBinaryFormat::deserialize_record(&slot)? else {
            continue;
        };

        if index_entry.get(INDEX_FIELD_KEY).and_then(Value::as_str) != Some("_id") {
            continue;
        }

        let Some(primary_key) = index_entry.get(INDEX_VALUE_KEY).and_then(Value::as_str) else {
            continue;
        };
        saw_primary_key_rows = true;

        let is_deleted = index_entry
            .get(INDEX_STATUS_KEY)
            .and_then(Value::as_u64)
            .is_some_and(|status| status == 0)
            || index_entry
                .get(INDEX_OFFSET_KEY)
                .and_then(Value::as_array)
                .is_some_and(|offsets| offsets.is_empty());

        if is_deleted {
            live_primary_keys.remove(primary_key);
        } else if index_entry
            .get(INDEX_OFFSET_KEY)
            .and_then(Value::as_u64)
            .is_some()
        {
            live_primary_keys.insert(primary_key.to_string());
        }
    }

    if saw_primary_key_rows {
        Ok(Some(live_primary_keys.len()))
    } else {
        Ok(None)
    }
}

fn accumulate_storage_inventory(
    path: &Path,
    record_count: &mut usize,
    disk_size_bytes: &mut u64,
    register_file_count: &mut usize,
) -> Result<()> {
    if path.is_file() {
        *disk_size_bytes += fs::metadata(path)?.len();
        *register_file_count += 1;
        if is_metadata_file(path) {
            *record_count += metadata_record_count(path)?;
        }
        return Ok(());
    }

    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            accumulate_storage_inventory(
                &entry?.path(),
                record_count,
                disk_size_bytes,
                register_file_count,
            )?;
        }
    }

    Ok(())
}

fn metadata_record_count(path: &Path) -> Result<usize> {
    let contents = fs::read_to_string(path)?;
    let metadata: Value = serde_json::from_str(&contents).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!(
                "Failed to parse drawer metadata at {}: {error}",
                path.display()
            ),
        )
    })?;

    Ok(metadata
        .get("record_count")
        .and_then(Value::as_u64)
        .unwrap_or_default() as usize)
}

fn database_inventory_name(root_directory: &Path, database_path: &Path) -> String {
    match database_path.strip_prefix(root_directory) {
        Ok(relative_path) if relative_path.components().next().is_some() => relative_path
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => value.to_str().map(ToOwned::to_owned),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/"),
        _ => root_directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(".")
            .to_string(),
    }
}

fn child_directories(directory: &Path) -> Result<Vec<PathBuf>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }

    let mut directories = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            directories.push(path);
        }
    }
    directories.sort();
    Ok(directories)
}

fn directory_has_drawer_files(directory: &Path) -> Result<bool> {
    if !directory.exists() {
        return Ok(false);
    }

    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if drawer_name_from_file_path(&entry.path()).is_some() {
            return Ok(true);
        }
    }

    Ok(false)
}

fn collect_drawer_prefix_tenants(directory: &Path, tenants: &mut BTreeSet<String>) -> Result<()> {
    if !directory.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let Some(drawer_name) = drawer_name_from_file_path(&entry.path()) else {
            continue;
        };

        if let Some((tenant_prefix, drawer_name)) = drawer_name.rsplit_once('_') {
            if !tenant_prefix.is_empty() && !drawer_name.is_empty() {
                tenants.insert(tenant_prefix.to_string());
            }
        }
    }

    Ok(())
}

fn collect_flat_schema_prefixes(directory: &Path, schemas: &mut BTreeSet<String>) -> Result<()> {
    if !directory.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let Some(drawer_name) = drawer_name_from_file_path(&entry.path()) else {
            continue;
        };

        if let Some((schema_name, drawer_name)) = drawer_name.split_once('.') {
            if !schema_name.is_empty() && !drawer_name.is_empty() {
                schemas.insert(schema_name.to_string());
            }
        }
    }

    Ok(())
}

fn is_metadata_file(path: &Path) -> bool {
    path.is_file()
        && path.extension().and_then(|extension| extension.to_str()) == Some("drw")
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.ends_with("_meta"))
}

fn drawer_name_from_file_path(path: &Path) -> Option<String> {
    if !path.is_file() || path.extension().and_then(|extension| extension.to_str()) != Some("drw") {
        return None;
    }

    let stem = path.file_stem()?.to_str()?;
    if stem == ".catalog" || stem.ends_with("_index") || stem.ends_with("_meta") {
        return None;
    }

    Some(stem.to_string())
}

fn file_name_to_string(path: &Path) -> Option<String> {
    path.file_name()?.to_str().map(ToOwned::to_owned)
}
