use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use wardrobe_core::{WardrobeClient, WardrobeEngine};

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

        let databases = engine.list_databases()?;
        println!("Wardrobe databases:");

        for database in databases {
            println!(" - {}", database.name);

            let schemas = engine.list_schemas(&database.name)?;
            for schema_name in schemas {
                println!("   schema: {}", schema_name);

                let drawers = engine.show_drawers(&database.name, &schema_name)?;
                for drawer in drawers {
                    println!(
                        "     drawer: {} ({} records)",
                        drawer.name, drawer.record_count
                    );
                }
            }
        }

        let available_databases = client.show_databases()?;
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
            let is_catalog = path.file_name().and_then(|name| name.to_str()) == Some(".catalog.drw");
            if is_drawer_file && !is_catalog {
                return Ok(true);
            }
        }

        Ok(false)
    }
}
