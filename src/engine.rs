use crate::wrdb_lib::database::Database;
use serde_json::{Map, Value};
use std::collections::HashSet;
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
        let mut records = if let Some(drawer) =
            self.database_core
                .active_drawer_or_load_from_disk(drawer_name, "_id", Vec::new())?
        {
            drawer.find_all_records()?
        } else {
            Vec::new()
        };

        for record in &mut records {
            let mut active_pointer_path = HashSet::new();
            if let Value::Object(map) = record {
                if let Some(pointer) = map.get("_id").and_then(|value| value.as_str()) {
                    active_pointer_path.insert(pointer.to_string());
                }
            }
            self.hydrate_value(record, true, &mut active_pointer_path)?;
        }

        Ok(records)
    }

    pub fn find_by_id(&mut self, pointer: &str) -> Result<Option<Value>> {
        let (drawer_name, _) = self.parse_pointer(pointer)?;

        if let Some(drawer) =
            self.database_core
                .active_drawer_or_load_from_disk(&drawer_name, "_id", Vec::new())?
        {
            if let Some(mut record) = drawer.find_by_primary_key(pointer)? {
                let mut active_pointer_path = HashSet::from([pointer.to_string()]);
                self.hydrate_value(&mut record, false, &mut active_pointer_path)?;
                if let Value::Object(ref mut map) = record {
                    map.remove("_id");
                }
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    pub fn delete_by_id(&mut self, pointer: &str) -> Result<bool> {
        let mut active_delete_path = HashSet::new();
        self.delete_by_id_inner(pointer, &mut active_delete_path)
    }

    fn delete_by_id_inner(
        &mut self,
        pointer: &str,
        active_delete_path: &mut HashSet<String>,
    ) -> Result<bool> {
        if active_delete_path.contains(pointer) {
            return Ok(false);
        }

        let (drawer_name, _) = self.parse_pointer(pointer)?;
        let Some(drawer) =
            self.database_core
                .active_drawer_or_load_from_disk(&drawer_name, "_id", Vec::new())?
        else {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{}' could not be loaded for delete", drawer_name),
            ));
        };

        let record = drawer.find_by_primary_key(pointer)?;
        let cascade_fields = drawer.cascade_delete_fields();
        let Some(record) = record else {
            return Ok(false);
        };

        active_delete_path.insert(pointer.to_string());
        let cascade_pointers = Self::collect_cascade_pointers(&record, &cascade_fields);
        for cascade_pointer in cascade_pointers {
            self.delete_by_id_inner(&cascade_pointer, active_delete_path)?;
        }

        let Some(drawer) =
            self.database_core
                .active_drawer_or_load_from_disk(&drawer_name, "_id", Vec::new())?
        else {
            active_delete_path.remove(pointer);
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{}' could not be loaded for delete", drawer_name),
            ));
        };

        let deleted_record = drawer.delete_by_primary_key(pointer)?;
        active_delete_path.remove(pointer);

        Ok(deleted_record.is_some())
    }

    fn decompose_nested_objects(&mut self, map: Map<String, Value>) -> Result<Map<String, Value>> {
        let mut continuous_map = Map::new();

        for (key, value) in map {
            let drawer_name = Self::relationship_drawer_name(&key);
            let processed_value = self.decompose_relationship_value(&drawer_name, value)?;
            continuous_map.insert(key, processed_value);
        }

        Ok(continuous_map)
    }

    fn decompose_relationship_value(&mut self, drawer_name: &str, value: Value) -> Result<Value> {
        match value {
            Value::Object(child_map) => {
                if let Some(reference_id) = Self::id_only_reference(&child_map) {
                    let normalized_pointer =
                        Self::normalize_reference_pointer(drawer_name, reference_id);
                    Ok(Value::String(normalized_pointer))
                } else {
                    let child_pointer = self.upsert(drawer_name, Value::Object(child_map))?;
                    Ok(Value::String(child_pointer))
                }
            }
            Value::Array(values) => values
                .into_iter()
                .map(|item| self.decompose_relationship_value(drawer_name, item))
                .collect::<Result<Vec<_>>>()
                .map(Value::Array),
            other => Ok(other),
        }
    }

    fn id_only_reference(map: &Map<String, Value>) -> Option<&str> {
        if map.len() == 1 {
            map.get("_id").and_then(|value| value.as_str())
        } else {
            None
        }
    }

    fn normalize_reference_pointer(drawer_name: &str, reference_id: &str) -> String {
        if Self::is_pointer(reference_id) {
            return reference_id.to_string();
        }

        let clean_id = reference_id.trim_start_matches('@');
        if clean_id.contains(":lnk_") {
            format!("@{}", clean_id)
        } else {
            format!("@{}:lnk_{}", drawer_name, clean_id)
        }
    }

    fn relationship_drawer_name(field_name: &str) -> String {
        if let Some(stem) = field_name.strip_suffix("ies") {
            return format!("{}y", stem);
        }

        if field_name.ends_with('s')
            && !field_name.ends_with("ss")
            && !field_name.ends_with("us")
            && field_name.len() > 1
        {
            return field_name[..field_name.len() - 1].to_string();
        }

        field_name.to_string()
    }

    fn collect_cascade_pointers(record: &Value, cascade_fields: &[String]) -> Vec<String> {
        let mut pointers = Vec::new();

        if let Value::Object(map) = record {
            for field in cascade_fields {
                if let Some(value) = map.get(field) {
                    Self::collect_pointer_strings(value, &mut pointers);
                }
            }
        }

        pointers
    }

    fn collect_pointer_strings(value: &Value, pointers: &mut Vec<String>) {
        match value {
            Value::String(pointer) if Self::is_pointer(pointer) => {
                pointers.push(pointer.to_string());
            }
            Value::Array(values) => {
                for value in values {
                    Self::collect_pointer_strings(value, pointers);
                }
            }
            Value::Object(map) => {
                for value in map.values() {
                    Self::collect_pointer_strings(value, pointers);
                }
            }
            _ => {}
        }
    }

    fn hydrate_value(
        &mut self,
        current_value: &mut Value,
        include_ids: bool,
        active_pointer_path: &mut HashSet<String>,
    ) -> Result<()> {
        match current_value {
            Value::Object(map) => {
                let pointer_updates = map
                    .iter()
                    .filter_map(|(field_name, field_value)| {
                        if field_name == "_id" {
                            return None;
                        }

                        field_value
                            .as_str()
                            .filter(|pointer| Self::is_pointer(pointer))
                            .map(|pointer| (field_name.clone(), pointer.to_string()))
                    })
                    .collect::<Vec<_>>();

                for (field_name, pointer) in pointer_updates {
                    if let Some(resolved_value) =
                        self.resolve_pointer(&pointer, include_ids, active_pointer_path)?
                    {
                        if let Some(value_ref) = map.get_mut(&field_name) {
                            *value_ref = resolved_value;
                        }
                    }
                }

                for (field_name, field_value) in map.iter_mut() {
                    if field_name != "_id" {
                        self.hydrate_value(field_value, include_ids, active_pointer_path)?;
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    if let Some(pointer) = value
                        .as_str()
                        .filter(|pointer| Self::is_pointer(pointer))
                        .map(|pointer| pointer.to_string())
                    {
                        if let Some(resolved_value) =
                            self.resolve_pointer(&pointer, include_ids, active_pointer_path)?
                        {
                            *value = resolved_value;
                            continue;
                        }
                    }

                    self.hydrate_value(value, include_ids, active_pointer_path)?;
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn resolve_pointer(
        &mut self,
        pointer: &str,
        include_ids: bool,
        active_pointer_path: &mut HashSet<String>,
    ) -> Result<Option<Value>> {
        if active_pointer_path.contains(pointer) {
            return Ok(None);
        }

        let (drawer_name, _) = self.parse_pointer(pointer)?;

        let mut record = if let Some(drawer) =
            self.database_core
                .active_drawer_or_load_from_disk(&drawer_name, "_id", Vec::new())?
        {
            drawer.find_by_primary_key(pointer)?
        } else {
            None
        };

        if let Some(ref mut record_value) = record {
            active_pointer_path.insert(pointer.to_string());
            self.hydrate_value(record_value, include_ids, active_pointer_path)?;
            active_pointer_path.remove(pointer);

            if !include_ids {
                if let Value::Object(map) = record_value {
                    map.remove("_id");
                }
            }
        }

        Ok(record)
    }

    fn is_pointer(value: &str) -> bool {
        value.starts_with('@') && value.contains(":lnk_")
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
