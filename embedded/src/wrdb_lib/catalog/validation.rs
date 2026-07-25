use std::io::{Error, ErrorKind, Result};
use std::path::{Component, Path, PathBuf};

pub(crate) fn validate_database_name(database_name: &str) -> Result<()> {
    let trimmed = database_name.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.contains('\\')
        || trimmed
            .split('/')
            .any(|segment| is_reserved_or_empty_segment(segment))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Invalid database name: {database_name}"),
        ));
    }

    Ok(())
}

pub(crate) fn validate_schema_name(schema_name: &str) -> Result<()> {
    validate_catalog_token(schema_name, "schema")
}

pub(crate) fn validate_drawer_name(drawer_name: &str) -> Result<()> {
    validate_catalog_token(drawer_name, "drawer")
}

pub(crate) fn validate_tenant_identifier(tenant_id: &str) -> Result<()> {
    validate_catalog_token(tenant_id, "tenant")
}

pub(crate) fn validate_catalog_token(value: &str, label: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || is_reserved_or_empty_segment(trimmed)
        || trimmed.ends_with("_index")
        || trimmed.ends_with("_meta")
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Invalid {label} name: {value}"),
        ));
    }

    Ok(())
}

pub(crate) fn validate_catalog_location(location: &str) -> Result<()> {
    let trimmed = location.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.contains('\\')
        || trimmed
            .split('/')
            .any(|segment| is_reserved_or_empty_segment(segment))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Invalid catalog route location: {location}"),
        ));
    }

    Ok(())
}

pub(crate) fn validate_storage_coordinate_component(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Storage coordinate {label} cannot be empty"),
        ));
    }

    let mut components = Path::new(value).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Storage coordinate {label} must be a single path segment"),
        ));
    }

    Ok(())
}

pub(crate) fn catalog_location_path(root_directory: &Path, location: &str) -> PathBuf {
    root_directory.join(location)
}

pub(crate) fn database_path_from_name(
    root_directory: &Path,
    database_name: &str,
) -> Result<PathBuf> {
    if database_name.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Database name cannot be empty",
        ));
    }

    let mut database_path = root_directory.to_path_buf();
    for component in Path::new(database_name).components() {
        match component {
            Component::Normal(value) => database_path.push(value),
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Database name must contain only normal path segments",
                ));
            }
        }
    }

    Ok(database_path)
}

fn is_reserved_or_empty_segment(segment: &str) -> bool {
    segment.is_empty() || segment == "." || segment == ".." || segment == ".catalog"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_validations() {
        assert!(validate_database_name("mydb").is_ok());
        assert!(validate_database_name("").is_err());
        assert!(validate_database_name("/mydb").is_err());
        assert!(validate_database_name("my\\db").is_err());
        assert!(validate_database_name("my/./db").is_err());
        assert!(validate_database_name("my/.catalog/db").is_err());

        assert!(validate_schema_name("public").is_ok());
        assert!(validate_schema_name("my_index").is_err());
        assert!(validate_schema_name("my_meta").is_err());
        assert!(validate_schema_name("foo/bar").is_err());

        assert!(validate_drawer_name("users").is_ok());
        assert!(validate_tenant_identifier("tenant1").is_ok());

        assert!(validate_catalog_location("loc1/loc2").is_ok());
        assert!(validate_catalog_location("").is_err());
        assert!(validate_catalog_location("/loc").is_err());

        assert!(validate_storage_coordinate_component("drawer", "valid").is_ok());
        assert!(validate_storage_coordinate_component("drawer", "").is_err());
        assert!(validate_storage_coordinate_component("drawer", "a/b").is_err());

        assert_eq!(catalog_location_path(Path::new("/root"), "loc"), PathBuf::from("/root/loc"));
        assert_eq!(database_path_from_name(Path::new("/root"), "db1").unwrap(), PathBuf::from("/root/db1"));
        assert!(database_path_from_name(Path::new("/root"), "").is_err());
        assert!(database_path_from_name(Path::new("/root"), "..").is_err());
    }
}
