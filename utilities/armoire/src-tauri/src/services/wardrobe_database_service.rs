#[cfg(test)]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use wardrobe_embedded::{
    CreateRequest, OperationFilter, OperationOptions, ReadResult, StatusRequest, StorageInventory,
    WardrobeClient, WardrobeEngine,
};

struct ActiveConnection {
    _database_directory: String,
    client: WardrobeClient,
}

static ACTIVE_CONNECTION: Mutex<Option<ActiveConnection>> = Mutex::new(None);

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

        Self::connect_source_location(&database_directory_string)?;

        Ok(database_directory_string)
    }

    pub fn connect_source_location(database_directory: &str) -> io::Result<()> {
        Self::connect_source_location_with_name(database_directory, None)
    }

    pub fn connect_source_location_with_name(
        database_directory: &str,
        name: Option<&str>,
    ) -> io::Result<()> {
        let database_directory = Self::resolve_database_directory(database_directory)?;
        let database_directory_string = database_directory.to_string_lossy().into_owned();

        println!(
            "Connecting to Wardrobe source location at: {}",
            database_directory_string
        );

        let client = WardrobeClient::open(&database_directory_string)?;

        let mut lock = ACTIVE_CONNECTION
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "ACTIVE_CONNECTION lock poisoned"))?;
        *lock = Some(ActiveConnection {
            _database_directory: database_directory_string.clone(),
            client,
        });

        let conn_type = if database_directory_string.starts_with("wardrobe://")
            || database_directory_string.contains("://")
        {
            "connection"
        } else {
            "location"
        };
        let _ = Self::save_connection(&database_directory_string, conn_type, name);

        Ok(())
    }

    fn with_active_client<F, T>(f: F) -> io::Result<T>
    where
        F: FnOnce(&WardrobeClient) -> io::Result<T>,
    {
        let lock = ACTIVE_CONNECTION
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "ACTIVE_CONNECTION lock poisoned"))?;
        if let Some(active) = lock.as_ref() {
            f(&active.client)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "No open Wardrobe database connection. Connect to or create a source location first.",
            ))
        }
    }

    pub fn show_wardrobes() -> io::Result<Vec<StorageInventory>> {
        Self::with_active_client(|client| status_databases(client))
    }

    pub fn create_new_wardrobe(database_name: &str) -> io::Result<()> {
        Self::with_active_client(|client| {
            client.create(CreateRequest::database(database_name))?;
            Ok(())
        })
    }

    pub fn show_bays(database_name: &str) -> io::Result<Vec<String>> {
        Self::with_active_client(|client| status_schemas(client, database_name))
    }

    pub fn create_new_bay(database_name: &str, schema_name: &str) -> io::Result<()> {
        Self::with_active_client(|client| {
            // Ensure parent database registry entry is created/registered in the catalog
            let _ = client.create(CreateRequest::database(database_name));
            client.create(CreateRequest::schema(database_name, schema_name))?;
            Ok(())
        })
    }

    pub fn show_drawers(
        database_name: &str,
        schema_name: &str,
    ) -> io::Result<Vec<StorageInventory>> {
        Self::with_active_client(|client| status_drawers(client, database_name, schema_name))
    }

    pub fn create_new_drawer(
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> io::Result<()> {
        Self::with_active_client(|client| {
            // Ensure parent database and schema registry entries are created/registered in the catalog
            let _ = client.create(CreateRequest::database(database_name));
            let _ = client.create(CreateRequest::schema(database_name, schema_name));
            client.create(CreateRequest::drawer(
                database_name,
                schema_name,
                drawer_name,
            ))?;
            Ok(())
        })
    }
    pub fn read_records(
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
    ) -> io::Result<Vec<serde_json::Value>> {
        Self::with_active_client(|client| {
            let path = if schema_name.is_empty() {
                format!("{database_name}/{drawer_name}")
            } else {
                format!("{database_name}/{schema_name}/{drawer_name}")
            };
            let filter = OperationFilter::drawer(path);
            let options = OperationOptions::default().hydrate(true);
            match client.read(filter, options) {
                Ok(ReadResult::Records(records)) => Ok(records),
                Ok(ReadResult::Page(page)) => Ok(page.records),
                Ok(ReadResult::Record(Some(record))) => Ok(vec![record]),
                Ok(_) => Ok(vec![]),
                Err(error) => Err(io::Error::new(io::ErrorKind::Other, error.to_string())),
            }
        })
    }
    pub fn create_record(
        database_name: &str,
        schema_name: &str,
        drawer_name: &str,
        payload: serde_json::Value,
    ) -> io::Result<()> {
        Self::with_active_client(|client| {
            let path = if schema_name.is_empty() {
                format!("{database_name}/{drawer_name}")
            } else {
                format!("{database_name}/{schema_name}/{drawer_name}")
            };
            let filter = OperationFilter::drawer(path);
            let options = OperationOptions::default();
            client
                .upsert(payload, filter, options)
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;
            Ok(())
        })
    }

    pub fn test_connection(database_directory: &str) -> io::Result<()> {
        let database_directory = Self::resolve_database_directory(database_directory)?;
        let database_directory = database_directory.to_string_lossy().into_owned();
        println!("Testing Wardrobe database at: {}", database_directory);

        if database_directory.starts_with("wardrobe://") || database_directory.contains("://") {
            let client = WardrobeClient::open(&database_directory)?;
            let available_databases = status_databases(&client)?;
            println!(
                "Remote client can see {} database(s).",
                available_databases.len()
            );
            return Ok(());
        }

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
        if database_directory.starts_with("wardrobe://") || database_directory.contains("://") {
            return Ok(PathBuf::from(database_directory));
        }
        let requested_path = Path::new(database_directory);
        if requested_path.is_absolute() {
            return Ok(requested_path.to_path_buf());
        }

        let current_dir = std::env::current_dir()?;
        let mut resolved = current_dir.join(requested_path);

        if !resolved.exists() && current_dir.ends_with("src-tauri") {
            if let Some(parent) = current_dir.parent() {
                let parent_resolved = parent.join(requested_path);
                if parent_resolved.exists() {
                    resolved = parent_resolved;
                }
            }
        }

        if resolved.exists() && resolved.is_dir() {
            Ok(resolved.canonicalize()?)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "Wardrobe database directory '{}' was not found relative to the working directory '{}' or parent project folder",
                    database_directory,
                    current_dir.display()
                )
            ))
        }
    }

    fn resolve_source_location(database_directory: &str) -> PathBuf {
        let requested_path = Path::new(database_directory);
        if requested_path.is_absolute() {
            return requested_path.to_path_buf();
        }

        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        if current_dir.ends_with("src-tauri") {
            if let Some(parent) = current_dir.parent() {
                return parent.join(requested_path);
            }
        }
        current_dir.join(requested_path)
    }

    #[cfg(test)]
    fn contains_wardrobe_storage(directory: &Path) -> io::Result<bool> {
        Self::contains_wardrobe_storage_at_depth(directory, 0)
    }

    #[cfg(test)]
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

    pub fn get_saved_connections() -> io::Result<Vec<serde_json::Value>> {
        let engine = Self::get_or_create_metadata_engine()?;
        let filter = OperationFilter::drawer("armoire/config/connections");
        let options = OperationOptions::default().hydrate(true);
        match engine.read(filter, options) {
            Ok(ReadResult::Records(records)) => Ok(records),
            Ok(ReadResult::Page(page)) => Ok(page.records),
            Ok(ReadResult::Record(Some(record))) => Ok(vec![record]),
            Ok(_) => Ok(vec![]),
            Err(error) => Err(io::Error::new(io::ErrorKind::Other, error.to_string())),
        }
    }

    pub fn save_connection(target: &str, conn_type: &str, name: Option<&str>) -> io::Result<()> {
        let engine = Self::get_or_create_metadata_engine()?;
        let normalized_id = target
            .replace("/", "_")
            .replace("\\", "_")
            .replace(":", "_")
            .replace(".", "_");
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let filter =
            OperationFilter::pointer(format!("armoire/config/connections/{}", normalized_id));
        let options = OperationOptions::default().hydrate(true);
        let existing_record = match engine.read(filter, options) {
            Ok(ReadResult::Record(Some(record))) => Some(record),
            _ => None,
        };
        let existing_name = existing_record
            .as_ref()
            .and_then(|record| record.get("name").or_else(|| record.get("alias")))
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned);
        let display_name = name
            .and_then(|value| {
                let trimmed = value.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_owned())
            })
            .or(existing_name);
        let mut payload = serde_json::json!({
            "_id": normalized_id,
            "target": target,
            "type": conn_type,
            "last_connected_at": timestamp
        });
        if let Some(display_name) = display_name {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "name".to_string(),
                    serde_json::Value::String(display_name.clone()),
                );
                obj.insert("alias".to_string(), serde_json::Value::String(display_name));
            }
        }
        let filter = OperationFilter::drawer("armoire/config/connections");
        let options = OperationOptions::default();
        engine
            .upsert(payload, filter, options)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        Ok(())
    }

    pub fn remove_connection(id_or_target: &str) -> io::Result<()> {
        let engine = Self::get_or_create_metadata_engine()?;
        let options = OperationOptions::default();

        // 1. Try deleting by raw ID/pointer directly
        let filter1 =
            OperationFilter::pointer(format!("armoire/config/connections/{}", id_or_target));
        let _ = engine.delete(filter1, options.clone());

        // 2. Try deleting by normalized ID just in case
        let normalized_id = id_or_target
            .replace("/", "_")
            .replace("\\", "_")
            .replace(":", "_")
            .replace(".", "_");
        let filter2 =
            OperationFilter::pointer(format!("armoire/config/connections/{}", normalized_id));
        let _ = engine.delete(filter2, options.clone());

        // 3. Delete any records that were saved before ID normalization stabilized.
        let target_filter = OperationFilter::query_in(
            "armoire/config/connections",
            serde_json::json!({ "target": id_or_target }),
        );
        let _ = engine.delete(target_filter, OperationOptions::new().multi(true));

        let id_filter = OperationFilter::query_in(
            "armoire/config/connections",
            serde_json::json!({ "_id": normalized_id }),
        );
        let _ = engine.delete(id_filter, OperationOptions::new().multi(true));

        let records = match engine.read(
            OperationFilter::drawer("armoire/config/connections"),
            OperationOptions::default().hydrate(true),
        ) {
            Ok(ReadResult::Records(records)) => records,
            Ok(ReadResult::Page(page)) => page.records,
            Ok(ReadResult::Record(Some(record))) => vec![record],
            _ => vec![],
        };
        for record in records {
            let record_target = record.get("target").and_then(|value| value.as_str());
            let record_id = record.get("_id").and_then(|value| value.as_str());
            if record_target == Some(id_or_target)
                || record_target == Some(normalized_id.as_str())
                || record_id == Some(id_or_target)
                || record_id == Some(normalized_id.as_str())
            {
                if let Some(record_id) = record_id {
                    let filter = OperationFilter::pointer(format!(
                        "armoire/config/connections/{}",
                        record_id
                    ));
                    let _ = engine.delete(filter, OperationOptions::default());
                }
            }
        }

        Ok(())
    }

    pub fn update_connection_alias(target: &str, alias: &str) -> io::Result<()> {
        let engine = Self::get_or_create_metadata_engine()?;
        let normalized_id = target
            .replace("/", "_")
            .replace("\\", "_")
            .replace(":", "_")
            .replace(".", "_");

        let filter =
            OperationFilter::pointer(format!("armoire/config/connections/{}", normalized_id));
        let options = OperationOptions::default().hydrate(true);

        let read_res = engine.read(filter, options);
        let mut existing_record = match read_res {
            Ok(ReadResult::Record(Some(record))) => record,
            _ => {
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let conn_type = if target.starts_with("wardrobe://") || target.contains("://") {
                    "connection"
                } else {
                    "location"
                };
                serde_json::json!({
                    "_id": normalized_id,
                    "target": target,
                    "type": conn_type,
                    "last_connected_at": timestamp
                })
            }
        };

        if let Some(obj) = existing_record.as_object_mut() {
            obj.insert(
                "_id".to_string(),
                serde_json::Value::String(normalized_id.clone()),
            );
            obj.insert(
                "alias".to_string(),
                serde_json::Value::String(alias.to_string()),
            );
            obj.insert(
                "name".to_string(),
                serde_json::Value::String(alias.to_string()),
            );
        }

        let filter = OperationFilter::drawer("armoire/config/connections");
        let options = OperationOptions::default();
        engine
            .upsert(existing_record, filter, options)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        Ok(())
    }

    pub fn delete_connection_files(target: &str, id: &str) -> io::Result<()> {
        if target.starts_with("wardrobe://") || target.contains("://") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot delete remote connection files",
            ));
        }
        let resolved_path = Self::resolve_database_directory(target)?;
        if resolved_path.exists() && resolved_path.is_dir() {
            std::fs::remove_dir_all(&resolved_path)?;
        }
        Self::remove_connection(id)?;
        Self::remove_connection(target)?;
        Ok(())
    }

    fn get_armoire_metadata_path() -> PathBuf {
        if let Ok(override_path) = std::env::var("ARMOIRE_METADATA_DIR") {
            return PathBuf::from(override_path);
        }
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| ".".to_string());
        Path::new(&home).join(".armoire").join("data")
    }

    fn get_or_create_metadata_engine() -> io::Result<WardrobeEngine> {
        let metadata_dir = Self::get_armoire_metadata_path();
        std::fs::create_dir_all(&metadata_dir)?;
        let metadata_dir = metadata_dir.canonicalize()?;
        let dir_str = metadata_dir.to_string_lossy().into_owned();

        let engine = WardrobeEngine::open(&dir_str)?;

        let _ = engine.create(CreateRequest::database("armoire"));
        let _ = engine.create(CreateRequest::schema("armoire", "config"));
        let _ = engine.create(CreateRequest::drawer("armoire", "config", "connections"));

        Ok(engine)
    }
}

trait StatusSource {
    fn status_databases(&self) -> io::Result<Vec<StorageInventory>>;
    fn status_schemas(&self, database_name: &str) -> io::Result<Vec<String>>;
    fn status_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> io::Result<Vec<StorageInventory>>;
}

impl StatusSource for WardrobeEngine {
    fn status_databases(&self) -> io::Result<Vec<StorageInventory>> {
        self.status(StatusRequest::databases())
    }

    fn status_schemas(&self, database_name: &str) -> io::Result<Vec<String>> {
        self.status(StatusRequest::schemas(database_name))
    }

    fn status_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> io::Result<Vec<StorageInventory>> {
        self.status(StatusRequest::drawers(database_name, schema_name))
    }
}

impl StatusSource for WardrobeClient {
    fn status_databases(&self) -> io::Result<Vec<StorageInventory>> {
        self.status(StatusRequest::databases())
    }

    fn status_schemas(&self, database_name: &str) -> io::Result<Vec<String>> {
        self.status(StatusRequest::schemas(database_name))
    }

    fn status_drawers(
        &self,
        database_name: &str,
        schema_name: &str,
    ) -> io::Result<Vec<StorageInventory>> {
        self.status(StatusRequest::drawers(database_name, schema_name))
    }
}

fn status_databases(source: &impl StatusSource) -> io::Result<Vec<StorageInventory>> {
    source.status_databases()
}

fn status_schemas(source: &impl StatusSource, database_name: &str) -> io::Result<Vec<String>> {
    source.status_schemas(database_name)
}

fn status_drawers(
    source: &impl StatusSource,
    database_name: &str,
    schema_name: &str,
) -> io::Result<Vec<StorageInventory>> {
    source.status_drawers(database_name, schema_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    static SERVICE_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn service_test_lock() -> std::sync::MutexGuard<'static, ()> {
        SERVICE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct FakeStatusSource {
        databases: Vec<StorageInventory>,
        schemas: Vec<String>,
        drawers: Vec<StorageInventory>,
    }

    impl StatusSource for FakeStatusSource {
        fn status_databases(&self) -> io::Result<Vec<StorageInventory>> {
            Ok(self.databases.clone())
        }

        fn status_schemas(&self, _database_name: &str) -> io::Result<Vec<String>> {
            Ok(self.schemas.clone())
        }

        fn status_drawers(
            &self,
            _database_name: &str,
            _schema_name: &str,
        ) -> io::Result<Vec<StorageInventory>> {
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
        let _guard = service_test_lock();
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
    fn status_helpers_return_direct_arrays() {
        let _guard = service_test_lock();
        let inventory = StorageInventory {
            name: "wardrobe".to_string(),
            record_count: 1,
            disk_size_bytes: 10,
            register_file_count: 1,
        };
        let source = FakeStatusSource {
            databases: vec![inventory.clone()],
            schemas: vec!["public".to_string()],
            drawers: vec![inventory],
        };

        assert_eq!(status_databases(&source).unwrap()[0].name, "wardrobe");
        assert_eq!(status_schemas(&source, "wardrobe").unwrap(), vec!["public"]);
        assert_eq!(
            status_drawers(&source, "wardrobe", "public").unwrap()[0].record_count,
            1
        );
    }

    #[test]
    fn wardrobe_database_service_lifecycle_works() {
        let _guard = service_test_lock();
        let root = temp_path("service_lifecycle");

        assert!(WardrobeDatabaseService::show_wardrobes().is_err());

        let path =
            WardrobeDatabaseService::create_source_location(&root.to_string_lossy()).unwrap();
        assert_eq!(PathBuf::from(path), root.canonicalize().unwrap());

        let db_list = WardrobeDatabaseService::show_wardrobes().unwrap();
        assert_eq!(db_list.len(), 0);

        WardrobeDatabaseService::create_new_wardrobe("db1").unwrap();
        let db_list = WardrobeDatabaseService::show_wardrobes().unwrap();
        assert_eq!(db_list.len(), 1);
        assert_eq!(db_list[0].name, "db1");

        let bay_list = WardrobeDatabaseService::show_bays("db1").unwrap();
        assert_eq!(bay_list.len(), 0);

        WardrobeDatabaseService::create_new_bay("db1", "bay1").unwrap();
        let bay_list = WardrobeDatabaseService::show_bays("db1").unwrap();
        assert_eq!(bay_list.len(), 1);
        assert_eq!(bay_list[0], "bay1");

        let drawer_list = WardrobeDatabaseService::show_drawers("db1", "bay1").unwrap();
        assert_eq!(drawer_list.len(), 0);

        WardrobeDatabaseService::create_new_drawer("db1", "bay1", "drawer1").unwrap();
        let drawer_list = WardrobeDatabaseService::show_drawers("db1", "bay1").unwrap();
        assert_eq!(drawer_list.len(), 1);
        assert_eq!(drawer_list[0].name, "drawer1");

        let mut lock = ACTIVE_CONNECTION.lock().unwrap();
        *lock = None;
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_persistence_connection_lifecycle_works() {
        let _guard = service_test_lock();
        let target = "test_connection_path";
        let metadata_dir = temp_path("metadata_persistence_store");
        std::fs::create_dir_all(&metadata_dir).expect("metadata directory should create");
        std::env::set_var("ARMOIRE_METADATA_DIR", &metadata_dir);
        let _ = WardrobeDatabaseService::remove_connection(target);

        let saved = WardrobeDatabaseService::get_saved_connections().unwrap();
        let initial_count = saved.len();

        WardrobeDatabaseService::save_connection(target, "location", None).unwrap();
        let saved = WardrobeDatabaseService::get_saved_connections().unwrap();
        assert_eq!(saved.len(), initial_count + 1);

        let target_record = saved
            .iter()
            .find(|item| item.get("target").and_then(|t| t.as_str()) == Some(target));
        assert!(target_record.is_some());

        WardrobeDatabaseService::update_connection_alias(target, "My Test Alias").unwrap();
        let saved = WardrobeDatabaseService::get_saved_connections().unwrap();
        let target_record = saved
            .iter()
            .find(|item| item.get("target").and_then(|t| t.as_str()) == Some(target))
            .unwrap();
        assert_eq!(
            target_record.get("alias").and_then(|a| a.as_str()),
            Some("My Test Alias")
        );

        WardrobeDatabaseService::remove_connection(target).unwrap();
        let saved = WardrobeDatabaseService::get_saved_connections().unwrap();
        assert_eq!(saved.len(), initial_count);

        std::env::remove_var("ARMOIRE_METADATA_DIR");
        let _ = fs::remove_dir_all(metadata_dir);
    }
}
