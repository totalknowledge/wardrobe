use super::relationship::VirtualRelationship;
use crate::wrdb_lib::pointer;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::Result;

#[derive(Default)]
pub(crate) struct HydrationCache {
    records: HashMap<String, Option<Value>>,
}

#[cfg(test)]
pub(crate) fn hydrate_records<F>(
    records: &mut [Value],
    include_ids: bool,
    fetch_record: F,
) -> Result<()>
where
    F: FnMut(&str, &str) -> Result<Option<Value>>,
{
    let mut cache = HydrationCache::default();
    hydrate_records_with_cache(records, include_ids, &mut cache, fetch_record)
}

pub(crate) fn hydrate_records_with_cache<F>(
    records: &mut [Value],
    include_ids: bool,
    cache: &mut HydrationCache,
    mut fetch_record: F,
) -> Result<()>
where
    F: FnMut(&str, &str) -> Result<Option<Value>>,
{
    for record in records {
        let mut active_pointer_path = HashSet::new();
        if let Value::Object(map) = record {
            if let Some(pointer) = map.get("_id").and_then(|value| value.as_str()) {
                active_pointer_path.insert(pointer.to_string());
            }
        }
        hydrate_value_with_cache(
            record,
            include_ids,
            &mut active_pointer_path,
            cache,
            &mut fetch_record,
        )?;
    }

    Ok(())
}

#[cfg(test)]
pub(crate) fn hydrate_value<F>(
    current_value: &mut Value,
    include_ids: bool,
    active_pointer_path: &mut HashSet<String>,
    fetch_record: &mut F,
) -> Result<()>
where
    F: FnMut(&str, &str) -> Result<Option<Value>>,
{
    let mut cache = HydrationCache::default();
    hydrate_value_with_cache(
        current_value,
        include_ids,
        active_pointer_path,
        &mut cache,
        fetch_record,
    )
}

pub(crate) fn hydrate_value_with_cache<F>(
    current_value: &mut Value,
    include_ids: bool,
    active_pointer_path: &mut HashSet<String>,
    cache: &mut HydrationCache,
    fetch_record: &mut F,
) -> Result<()>
where
    F: FnMut(&str, &str) -> Result<Option<Value>>,
{
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
                        .filter(|pointer| pointer::is_pointer(pointer))
                        .map(|pointer| (field_name.clone(), pointer.to_string()))
                })
                .collect::<Vec<_>>();
            let pointer_update_fields = pointer_updates
                .iter()
                .map(|(field_name, _)| field_name.clone())
                .collect::<HashSet<_>>();

            for (field_name, pointer) in pointer_updates {
                if let Some(resolved_value) = resolve_pointer(
                    &pointer,
                    include_ids,
                    active_pointer_path,
                    cache,
                    fetch_record,
                )? {
                    if let Some(value_ref) = map.get_mut(&field_name) {
                        *value_ref = resolved_value;
                    }
                }
            }

            for (field_name, field_value) in map.iter_mut() {
                if field_name != "_id" && !pointer_update_fields.contains(field_name) {
                    hydrate_value_with_cache(
                        field_value,
                        include_ids,
                        active_pointer_path,
                        cache,
                        fetch_record,
                    )?;
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                if let Some(pointer) = value
                    .as_str()
                    .filter(|pointer| pointer::is_pointer(pointer))
                    .map(|pointer| pointer.to_string())
                {
                    if let Some(resolved_value) = resolve_pointer(
                        &pointer,
                        include_ids,
                        active_pointer_path,
                        cache,
                        fetch_record,
                    )? {
                        *value = resolved_value;
                        continue;
                    }
                }

                hydrate_value_with_cache(
                    value,
                    include_ids,
                    active_pointer_path,
                    cache,
                    fetch_record,
                )?;
            }
        }
        _ => {}
    }

    Ok(())
}

pub(crate) fn hydrate_virtual_relationships<F>(
    drawer_name: &str,
    records: &mut [Value],
    virtual_relationships: &[VirtualRelationship],
    include_ids: bool,
    mut load_children: F,
) -> Result<()>
where
    F: FnMut(&VirtualRelationship, &str, bool) -> Result<Vec<Value>>,
{
    if virtual_relationships.is_empty() {
        return Ok(());
    }

    for record in records {
        let Some(record_map) = record.as_object_mut() else {
            continue;
        };
        let Some(parent_key) = record_map.get("_id").and_then(|value| value.as_str()) else {
            continue;
        };
        let parent_pointer = pointer::format_pointer(drawer_name, parent_key);

        for relationship in virtual_relationships {
            let mut child_records = load_children(relationship, &parent_pointer, include_ids)?;
            if !include_ids {
                remove_root_ids(&mut child_records);
            }
            record_map.insert(relationship.field_name.clone(), Value::Array(child_records));
        }
    }

    Ok(())
}

fn resolve_pointer<F>(
    pointer: &str,
    include_ids: bool,
    active_pointer_path: &mut HashSet<String>,
    cache: &mut HydrationCache,
    fetch_record: &mut F,
) -> Result<Option<Value>>
where
    F: FnMut(&str, &str) -> Result<Option<Value>>,
{
    let (drawer_name, record_key) = pointer::parse_pointer(pointer)?;
    let canonical_pointer = pointer::format_pointer(&drawer_name, &record_key);

    if active_pointer_path.contains(&canonical_pointer) {
        return Ok(None);
    }

    let mut record = if let Some(cached_record) = cache.records.get(&canonical_pointer) {
        cached_record.clone()
    } else {
        let fetched_record = fetch_record(&drawer_name, &record_key)?;
        cache
            .records
            .insert(canonical_pointer.clone(), fetched_record.clone());
        fetched_record
    };

    if let Some(ref mut record_value) = record {
        active_pointer_path.insert(canonical_pointer.clone());
        hydrate_value_with_cache(
            record_value,
            include_ids,
            active_pointer_path,
            cache,
            fetch_record,
        )?;
        active_pointer_path.remove(&canonical_pointer);

        if !include_ids {
            remove_root_id(record_value);
        }
    }

    Ok(record)
}

fn remove_root_ids(records: &mut [Value]) {
    for record in records {
        remove_root_id(record);
    }
}

fn remove_root_id(record: &mut Value) {
    if let Value::Object(map) = record {
        map.remove("_id");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn test_store() -> HashMap<(String, String), Value> {
        HashMap::from([
            (
                ("gem".to_string(), "fire".to_string()),
                json!({"_id": "@gem:fire", "element": "Fire"}),
            ),
            (
                ("gem".to_string(), "water".to_string()),
                json!({"_id": "@gem:water", "element": "Water"}),
            ),
            (
                ("weapon".to_string(), "blade".to_string()),
                json!({"_id": "@weapon:blade", "gem": "@gem:fire"}),
            ),
        ])
    }

    #[test]
    fn hydrate_records_expands_object_pointers() {
        let store = test_store();
        let mut records = vec![json!({"_id": "@weapon:blade", "gem": "@gem:fire"})];

        hydrate_records(&mut records, true, |drawer_name, record_key| {
            Ok(store
                .get(&(drawer_name.to_string(), record_key.to_string()))
                .cloned())
        })
        .expect("hydration should succeed");

        assert_eq!(records[0]["gem"]["element"], "Fire");
        assert_eq!(records[0]["gem"]["_id"], "@gem:fire");
    }

    #[test]
    fn hydrate_records_reuses_repeated_pointer_fetches() {
        let store = test_store();
        let mut fetch_count = 0usize;
        let mut records = vec![
            json!({"_id": "@weapon:blade", "gem": "@gem:fire"}),
            json!({"_id": "@weapon:axe", "gem": "@gem:fire"}),
        ];

        hydrate_records(&mut records, true, |drawer_name, record_key| {
            if drawer_name == "gem" && record_key == "fire" {
                fetch_count += 1;
            }
            Ok(store
                .get(&(drawer_name.to_string(), record_key.to_string()))
                .cloned())
        })
        .expect("hydration should succeed");

        assert_eq!(fetch_count, 1);
        assert_eq!(records[0]["gem"]["element"], "Fire");
        assert_eq!(records[1]["gem"]["element"], "Fire");
    }

    #[test]
    fn hydrate_records_expands_array_pointers_and_removes_ids_when_requested() {
        let store = test_store();
        let mut records =
            vec![json!({"_id": "@weapon:blade", "gems": ["@gem:fire", "@gem:water"]})];

        hydrate_records(&mut records, false, |drawer_name, record_key| {
            Ok(store
                .get(&(drawer_name.to_string(), record_key.to_string()))
                .cloned())
        })
        .expect("hydration should succeed");

        assert_eq!(records[0]["gems"][0]["element"], "Fire");
        assert!(records[0]["gems"][0].get("_id").is_none());
        assert_eq!(records[0]["gems"][1]["element"], "Water");
    }

    #[test]
    fn hydrate_value_recurses_into_nested_objects() {
        let store = test_store();
        let mut record = json!({"nested": {"gem": "@gem:fire"}});
        let mut active_path = HashSet::new();

        hydrate_value(
            &mut record,
            true,
            &mut active_path,
            &mut |drawer_name, record_key| {
                Ok(store
                    .get(&(drawer_name.to_string(), record_key.to_string()))
                    .cloned())
            },
        )
        .expect("hydration should succeed");

        assert_eq!(record["nested"]["gem"]["element"], "Fire");
    }

    #[test]
    fn hydrate_value_skips_active_pointer_cycles() {
        let store = HashMap::from([(
            ("node".to_string(), "root".to_string()),
            json!({"_id": "@node:root", "self": "@node:root"}),
        )]);
        let mut record = json!({"_id": "@node:root", "self": "@node:root"});
        let mut active_path = HashSet::from(["@node:root".to_string()]);

        hydrate_value(
            &mut record,
            true,
            &mut active_path,
            &mut |drawer_name, record_key| {
                Ok(store
                    .get(&(drawer_name.to_string(), record_key.to_string()))
                    .cloned())
            },
        )
        .expect("hydration should succeed");

        assert_eq!(record["self"], "@node:root");
    }

    #[test]
    fn hydrate_virtual_relationships_attaches_child_arrays() {
        let relationships = vec![VirtualRelationship {
            field_name: "weapons".to_string(),
            target_drawer: "weapon".to_string(),
            mapped_by: "owner".to_string(),
        }];
        let mut records = vec![json!({"_id": "alice", "name": "Alice"})];

        hydrate_virtual_relationships(
            "character",
            &mut records,
            &relationships,
            false,
            |relationship, parent_pointer, _include_ids| {
                assert_eq!(relationship.target_drawer, "weapon");
                assert_eq!(parent_pointer, "@character:alice");
                Ok(vec![json!({"_id": "@weapon:blade", "name": "Blade"})])
            },
        )
        .expect("virtual hydration should succeed");

        assert_eq!(records[0]["weapons"][0]["name"], "Blade");
        assert!(records[0]["weapons"][0].get("_id").is_none());
    }
}
