use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::drawer::Drawer;
use serde_json::{Map, Value};
use std::io::{Error, ErrorKind, Result};
use uuid::Uuid;

pub struct WardrobeEngine {
    database_core: Database,
}

impl WardrobeEngine {
    pub fn new(directory: &str) -> Result<Self> {
        let database_core = Database::initialize(directory)?;
        Ok(Self { database_core })
    }

    pub fn upsert(&mut self, drawer_name: &str, payload: Value) -> Result<String> {
        if let Value::Object(map) = payload {
            let target_primary_key = "_id";

            let record_id = match map.get(target_primary_key).and_then(|v| v.as_str()) {
                Some(existing_id) => existing_id.to_string(),
                None => format!("@{}:lnk_{}", drawer_name, Uuid::new_v4().simple()),
            };

            let processed_map = self.decompose_nested_objects(map)?;

            self.database_core
                .load_drawer(drawer_name, target_primary_key, Vec::new())?;

            if let Some(drawer) = self.database_core.use_drawer(drawer_name) {
                let mut full_record = processed_map;
                full_record.insert(
                    target_primary_key.to_string(),
                    Value::String(record_id.clone()),
                );

                match drawer.upsert_record(Value::Object(full_record))? {
                    Ok(_) => Ok(record_id),
                    Err(validation_error) => {
                        Err(Error::new(ErrorKind::InvalidData, validation_error))
                    }
                }
            } else {
                Err(Error::new(
                    ErrorKind::NotFound,
                    "Failed to acquire target drawer handle",
                ))
            }
        } else {
            Err(Error::new(
                ErrorKind::InvalidInput,
                "Payload root must be a JSON object",
            ))
        }
    }

    pub fn find_all(&mut self, drawer_name: &str) -> std::io::Result<Vec<Value>> {
        self.database_core
            .load_drawer(drawer_name, "_id", Vec::new())?;
        self.database_core.load_existing_drawers("_id")?;

        let mut registry = self.database_core.get_all_drawers();
        if let Some(drawer) = registry.get_mut(drawer_name) {
            let drawer_ptr = *drawer as *mut Drawer;
            unsafe {
                return (*drawer_ptr).find_all_records(&registry);
            }
        }
        Ok(Vec::new())
    }

    pub fn find_by_id(&mut self, pointer: &str) -> Result<Option<Value>> {
        let (drawer_name, _) = self.parse_pointer(pointer)?;

        self.database_core
            .load_drawer(&drawer_name, "_id", Vec::new())?;

        if let Some(drawer) = self.database_core.use_drawer(&drawer_name) {
            if let Some(mut record) = drawer.find_by_primary_key(pointer)? {
                if let Value::Object(ref mut map) = record {
                    map.remove("_id");
                    let resolved_map = self.deep_resolve_pointers(map.clone())?;
                    return Ok(Some(Value::Object(resolved_map)));
                }
            }
        }
        Ok(None)
    }

    fn decompose_nested_objects(&mut self, map: Map<String, Value>) -> Result<Map<String, Value>> {
        let mut continuous_map = Map::new();

        for (key, value) in map {
            if let Value::Object(child_map) = value {
                let child_pointer = self.upsert(&key, Value::Object(child_map))?;
                continuous_map.insert(key, Value::String(child_pointer));
            } else {
                continuous_map.insert(key, value);
            }
        }

        Ok(continuous_map)
    }

    fn deep_resolve_pointers(&mut self, map: Map<String, Value>) -> Result<Map<String, Value>> {
        let mut reconstructed_map = Map::new();

        for (key, value) in map {
            if let Value::String(ref pointer_candidate) = value {
                if pointer_candidate.starts_with('@') && pointer_candidate.contains(":lnk_") {
                    if let Some(resolved_child) = self.find_by_id(pointer_candidate)? {
                        reconstructed_map.insert(key, resolved_child);
                    } else {
                        reconstructed_map.insert(key, value);
                    }
                } else {
                    reconstructed_map.insert(key, value);
                }
            } else {
                reconstructed_map.insert(key, value);
            }
        }

        Ok(reconstructed_map)
    }

    fn parse_pointer(&self, pointer: &str) -> Result<(String, String)> {
        let clean_pointer = pointer.trim_start_matches('@');
        let segments: Vec<&str> = clean_pointer.split(":lnk_").collect();

        if segments.len() == 2 {
            Ok((segments[0].to_string(), segments[1].to_string()))
        } else {
            Err(Error::new(
                ErrorKind::InvalidData,
                format!("Malformed pointer reference encountered: {}", pointer),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WardrobeEngine;
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_database_directory(test_name: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("wardrobe_{}_{}", test_name, nanos));
        directory.to_string_lossy().into_owned()
    }

    #[test]
    fn find_all_loads_existing_drawer_files_after_restart() {
        let database_directory = temp_database_directory("find_all_loads_existing_drawer_files");

        {
            let mut engine =
                WardrobeEngine::new(&database_directory).expect("database should initialize");
            engine
                .upsert(
                    "weapon",
                    json!({
                        "_id": "@weapon:lnk_test_weapon",
                        "name": "Test Sword",
                        "gem": {
                            "_id": "@gem:lnk_test_gem",
                            "element": "Light",
                            "potency": 9001
                        }
                    }),
                )
                .expect("weapon should upsert");
        }

        let mut restarted_engine =
            WardrobeEngine::new(&database_directory).expect("database should reinitialize");
        let weapons = restarted_engine
            .find_all("weapon")
            .expect("weapons should load");

        assert_eq!(weapons.len(), 1);
        assert_eq!(weapons[0]["name"], "Test Sword");
        assert_eq!(weapons[0]["gem"]["element"], "Light");

        let _ = fs::remove_dir_all(database_directory);
    }
}
