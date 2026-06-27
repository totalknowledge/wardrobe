use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use wardrobe_core::{
    StatusRequest, StatusResult, StorageInventory, WardrobeClient, WardrobeEngine,
};

pub struct WardrobeDatabaseService;

impl WardrobeDatabaseService {
    pub fn create_source_location(database_directory: &str) -> io::Result<String> {
        let database_directory = Self::resolve_source_location(database_directory);

        std::fs::create_dir_all(&database_directory)?;
        let database_directory = database_directory.canonicalize()?;
        let database_directory_string = database_directory.to_string_lossy().into_owned();

        println!(
            "Creating Wardrobe source location at: {}",
            database_directory_string
        );

        let _engine = WardrobeEngine::open(&database_directory_string)?;

        println!(
            "Wardrobe source location initialized at: {}",
            database_directory_string
        );

        Ok(database_directory_string)
    }

    pub fn test_connection(database_directory: &str) -> io::Result<()> {
        let database_directory = Self::resolve_database_directory(database_directory)?;
        let database_directory = database_directory.to_string_lossy().into_owned();
        println!("Testing Wardrobe database at: {}", database_directory);

        let engine = WardrobeEngine::open(&database_directory)?;
        let client = WardrobeClient::open(&database_directory)?;

        let databases = status_databases(&engine)?;
        println!("Wardrobe databases:");

        for database in databases {
            println!(" - {}", database.name);

            let schemas = status_schemas(&engine, &database.name)?;
            for schema_name in schemas {
                println!("   schema: {}", schema_name);

                let drawers = status_drawers(&engine, &database.name, &schema_name)?;
                for drawer in drawers {
                    println!(
                        "     drawer: {} ({} records)",
                        drawer.name, drawer.record_count
                    );
                }
            }
        }

        let available_databases = status_databases(&client)?;
        println!("Client can see {} database(s).", available_databases.len());

        Ok(())
    }

    fn resolve_database_directory(database_directory: &str) -> io::Result<PathBuf> {
        let requested_path = Path::new(database_directory);
        if requested_path.is_absolute() {
            return Ok(requested_path.to_path_buf());
        }

        let mut candidates = Vec::new();

        if let Ok(current_directory) = std::env::current_dir() {
            candidates.push(current_directory.join(requested_path));
        }

        let manifest_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        candidates.push(manifest_directory.join(requested_path));

        for ancestor in manifest_directory.ancestors() {
            candidates.push(ancestor.join(requested_path));
        }

        for candidate in &candidates {
            if candidate.exists() && Self::contains_wardrobe_storage(candidate)? {
                return candidate.canonicalize();
            }
        }

        for candidate in candidates {
            if candidate.exists() && candidate.is_dir() {
                return candidate.canonicalize();
            }
        }

        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "Wardrobe database directory '{}' was not found from the Tauri runtime or project roots",
                database_directory
            ),
        ))
    }

    fn resolve_source_location(database_directory: &str) -> PathBuf {
        let requested_path = Path::new(database_directory);
        if requested_path.is_absolute() {
            return requested_path.to_path_buf();
        }

        if let Ok(current_directory) = std::env::current_dir() {
            let candidate = current_directory.join(requested_path);
            if candidate.exists() {
                return candidate;
            }
        }

        let manifest_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for ancestor in manifest_directory.ancestors() {
            let candidate = ancestor.join(requested_path);
            if candidate.exists() {
                return candidate;
            }
        }

        manifest_directory
            .ancestors()
            .last()
            .map(|root| root.join(requested_path))
            .unwrap_or_else(|| manifest_directory.join(requested_path))
    }

    fn contains_wardrobe_storage(directory: &Path) -> io::Result<bool> {
        Self::contains_wardrobe_storage_at_depth(directory, 0)
    }

    fn contains_wardrobe_storage_at_depth(directory: &Path, depth: usize) -> io::Result<bool> {
        if depth > 4 || !directory.exists() {
            return Ok(false);
        }

        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                if Self::contains_wardrobe_storage_at_depth(&path, depth + 1)? {
                    return Ok(true);
                }
                continue;
            }

            let is_drawer_file =
                path.extension().and_then(|extension| extension.to_str()) == Some("drw");
            let is_catalog =
                path.file_name().and_then(|name| name.to_str()) == Some(".catalog.drw");
            if is_drawer_file && !is_catalog {
                return Ok(true);
            }
        }

        Ok(false)
    }
}

trait StatusSource {
    fn status_databases(&self) -> io::Result<StatusResult>;
    fn status_schemas(&self, database_name: &str) -> io::Result<StatusResult>;
    fn status_drawers(&self, database_name: &str, schema_name: &str) -> io::Result<StatusResult>;
}

impl StatusSource for WardrobeEngine {
    fn status_databases(&self) -> io::Result<StatusResult> {
        self.status(StatusRequest::databases())
    }

    fn status_schemas(&self, database_name: &str) -> io::Result<StatusResult> {
        self.status(StatusRequest::schemas(database_name))
    }

    fn status_drawers(&self, database_name: &str, schema_name: &str) -> io::Result<StatusResult> {
        self.status(StatusRequest::drawers(database_name, schema_name))
    }
}

impl StatusSource for WardrobeClient {
    fn status_databases(&self) -> io::Result<StatusResult> {
        self.status(StatusRequest::databases())
    }

    fn status_schemas(&self, database_name: &str) -> io::Result<StatusResult> {
        self.status(StatusRequest::schemas(database_name))
    }

    fn status_drawers(&self, database_name: &str, schema_name: &str) -> io::Result<StatusResult> {
        self.status(StatusRequest::drawers(database_name, schema_name))
    }
}

fn status_databases(source: &impl StatusSource) -> io::Result<Vec<StorageInventory>> {
    match source.status_databases()? {
        StatusResult::Databases(databases) => Ok(databases),
        other => Err(unexpected_status_result("databases", other)),
    }
}

fn status_schemas(source: &impl StatusSource, database_name: &str) -> io::Result<Vec<String>> {
    match source.status_schemas(database_name)? {
        StatusResult::Schemas(schemas) => Ok(schemas),
        other => Err(unexpected_status_result("schemas", other)),
    }
}

fn status_drawers(
    source: &impl StatusSource,
    database_name: &str,
    schema_name: &str,
) -> io::Result<Vec<StorageInventory>> {
    match source.status_drawers(database_name, schema_name)? {
        StatusResult::Drawers(drawers) => Ok(drawers),
        other => Err(unexpected_status_result("drawers", other)),
    }
}

fn unexpected_status_result(expected: &str, actual: StatusResult) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("expected {expected}, got {actual:?}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct FakeStatusSource {
        databases: StatusResult,
        schemas: StatusResult,
        drawers: StatusResult,
    }

    impl StatusSource for FakeStatusSource {
        fn status_databases(&self) -> io::Result<StatusResult> {
            Ok(self.databases.clone())
        }

        fn status_schemas(&self, _database_name: &str) -> io::Result<StatusResult> {
            Ok(self.schemas.clone())
        }

        fn status_drawers(
            &self,
            _database_name: &str,
            _schema_name: &str,
        ) -> io::Result<StatusResult> {
            Ok(self.drawers.clone())
        }
    }

    fn temp_path(test_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("armoire_service_unit_{test_name}_{nanos}"))
    }

    #[test]
    fn path_resolution_and_storage_detection_cover_edge_cases() {
        let root = temp_path("storage_detection");
        let nested = root.join("a").join("b").join("c").join("d");
        fs::create_dir_all(&nested).expect("nested directory should create");

        assert!(!WardrobeDatabaseService::contains_wardrobe_storage(&root).unwrap());

        fs::write(nested.join(".catalog.drw"), b"catalog").expect("catalog should write");
        assert!(!WardrobeDatabaseService::contains_wardrobe_storage(&root).unwrap());

        fs::write(nested.join("gem.drw"), b"drawer").expect("drawer should write");
        assert!(WardrobeDatabaseService::contains_wardrobe_storage(&root).unwrap());

        let absolute = WardrobeDatabaseService::resolve_source_location(&root.to_string_lossy());
        assert_eq!(absolute, root);
        assert!(
            WardrobeDatabaseService::resolve_database_directory(&root.to_string_lossy()).is_ok()
        );

        let missing = temp_path("missing_resolution");
        assert_eq!(
            WardrobeDatabaseService::resolve_database_directory(&missing.to_string_lossy())
                .expect("absolute paths are accepted as-is"),
            missing
        );
        let relative_missing = format!(
            "armoire_service_missing_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        );
        assert!(WardrobeDatabaseService::resolve_database_directory(&relative_missing).is_err());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_helpers_accept_expected_shapes_and_reject_mismatches() {
        let inventory = StorageInventory {
            name: "wardrobe".to_string(),
            record_count: 1,
            disk_size_bytes: 10,
            register_file_count: 1,
        };
        let source = FakeStatusSource {
            databases: StatusResult::Databases(vec![inventory.clone()]),
            schemas: StatusResult::Schemas(vec!["public".to_string()]),
            drawers: StatusResult::Drawers(vec![inventory]),
        };

        assert_eq!(status_databases(&source).unwrap()[0].name, "wardrobe");
        assert_eq!(status_schemas(&source, "wardrobe").unwrap(), vec!["public"]);
        assert_eq!(
            status_drawers(&source, "wardrobe", "public").unwrap()[0].record_count,
            1
        );

        let mismatched = FakeStatusSource {
            databases: StatusResult::Tenants(Vec::new()),
            schemas: StatusResult::Tenants(Vec::new()),
            drawers: StatusResult::Tenants(Vec::new()),
        };
        assert_eq!(
            status_databases(&mismatched).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            status_schemas(&mismatched, "wardrobe").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            status_drawers(&mismatched, "wardrobe", "public")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(
            unexpected_status_result("drawers", StatusResult::Tenants(Vec::new()))
                .to_string()
                .contains("expected drawers")
        );
    }
}
