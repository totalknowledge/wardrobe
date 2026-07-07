use super::relationship::{self, RelationTarget};
use crate::wrdb_lib::pointer;
use crate::wrdb_lib::routing::ExecutionContext;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::io::Result;

pub(crate) fn register_inline_relationship_aliases<F>(
    map: &Map<String, Value>,
    relationship_constraints: &mut BTreeMap<String, Value>,
    mut register_alias: F,
) -> Result<()>
where
    F: FnMut(&str, Value) -> Result<()>,
{
    let aliases = relationship::inline_relationship_aliases(map, relationship_constraints);

    for (field_name, rule) in aliases {
        register_alias(&field_name, rule.clone())?;
        relationship_constraints.insert(field_name, rule);
    }

    Ok(())
}

pub(crate) fn decompose_nested_objects<F>(
    map: Map<String, Value>,
    current_drawer_name: &str,
    parent_pointer: &str,
    relationship_constraints: &BTreeMap<String, Value>,
    context: ExecutionContext<'_>,
    mut upsert_child: F,
) -> Result<Map<String, Value>>
where
    F: FnMut(&str, Value, ExecutionContext<'_>, Option<&str>) -> Result<String>,
{
    let mut continuous_map = Map::new();

    for (key, value) in map {
        let relation_target = relationship::relation_target_for_field(
            &key,
            current_drawer_name,
            relationship_constraints,
        );
        let mut drawer_name = relationship::drawer_name_for_relation_target(
            &relation_target,
            &key,
            current_drawer_name,
        );
        if let Some(idx) = current_drawer_name.rfind('/') {
            let namespace = &current_drawer_name[..idx];
            if !drawer_name.starts_with(namespace) {
                drawer_name = format!("{namespace}/{drawer_name}");
            }
        }
        let implicit_parent_field = relationship_constraints
            .get(&key)
            .and_then(relationship::implicit_parent_field_for_rule);
        let processed_value = decompose_relationship_value(
            &drawer_name,
            value,
            relation_target,
            parent_pointer,
            implicit_parent_field,
            context,
            &mut upsert_child,
        )?;
        continuous_map.insert(key, processed_value);
    }

    Ok(continuous_map)
}

fn decompose_relationship_value<F>(
    drawer_name: &str,
    value: Value,
    relation_target: RelationTarget,
    parent_pointer: &str,
    implicit_parent_field: Option<&str>,
    context: ExecutionContext<'_>,
    upsert_child: &mut F,
) -> Result<Value>
where
    F: FnMut(&str, Value, ExecutionContext<'_>, Option<&str>) -> Result<String>,
{
    match value {
        Value::Object(mut child_map) => {
            if let Some(reference_id) = id_only_reference(&child_map) {
                let normalized_pointer = pointer::normalize_reference_pointer_for_namespace(
                    drawer_name,
                    reference_id,
                    context.drawer_namespace,
                );
                if let Some(mapped_by) = implicit_parent_field {
                    let mut child_link_map = Map::new();
                    child_link_map.insert("_id".to_string(), Value::String(normalized_pointer));
                    child_link_map.insert(
                        mapped_by.to_string(),
                        Value::String(parent_pointer.to_string()),
                    );
                    let child_pointer = upsert_child(
                        drawer_name,
                        Value::Object(child_link_map),
                        context,
                        implicit_parent_field,
                    )?;
                    return Ok(Value::String(child_pointer));
                }
                Ok(Value::String(normalized_pointer))
            } else {
                if let Some(mapped_by) = implicit_parent_field {
                    child_map.insert(
                        mapped_by.to_string(),
                        Value::String(parent_pointer.to_string()),
                    );
                }
                let child_pointer = upsert_child(
                    drawer_name,
                    Value::Object(child_map),
                    context,
                    implicit_parent_field,
                )?;
                Ok(Value::String(child_pointer))
            }
        }
        Value::Array(values) => values
            .into_iter()
            .map(|item| {
                decompose_relationship_value(
                    drawer_name,
                    item,
                    relation_target.clone(),
                    parent_pointer,
                    implicit_parent_field,
                    context,
                    upsert_child,
                )
            })
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::String(pointer) if pointer::is_pointer(&pointer) => Ok(Value::String(
            pointer::normalize_reference_pointer_for_namespace(
                drawer_name,
                &pointer,
                context.drawer_namespace,
            ),
        )),
        Value::String(reference_id)
            if relationship::should_normalize_plain_string(&relation_target) =>
        {
            Ok(Value::String(
                pointer::normalize_reference_pointer_for_namespace(
                    drawer_name,
                    &reference_id,
                    context.drawer_namespace,
                ),
            ))
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io;

    #[test]
    fn register_inline_aliases_persists_new_polymorphic_rules() {
        let map = Map::from_iter([(
            "attachments".to_string(),
            json!([{"_id": "@image:cover"}, {"_id": "@document:manual"}]),
        )]);
        let mut constraints = BTreeMap::new();
        let mut registered = Vec::new();

        register_inline_relationship_aliases(&map, &mut constraints, |field_name, rule| {
            registered.push((field_name.to_string(), rule));
            Ok(())
        })
        .expect("aliases should register");

        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].0, "attachments");
        assert_eq!(constraints["attachments"]["type"], "polymorphic");
    }

    #[test]
    fn decompose_id_only_object_into_inferred_pointer_without_upsert() {
        let map = Map::from_iter([("gems".to_string(), json!({"_id": "fire"}))]);
        let mut upsert_count = 0;

        let decomposed = decompose_nested_objects(
            map,
            "weapon",
            "@weapon:blade",
            &BTreeMap::new(),
            ExecutionContext::root(),
            |_, _, _, _| {
                upsert_count += 1;
                Ok("@gem:new".to_string())
            },
        )
        .expect("decomposition should succeed");

        assert_eq!(decomposed["gems"], "@gem:fire");
        assert_eq!(upsert_count, 0);
    }

    #[test]
    fn decompose_full_nested_object_via_child_upsert_callback() {
        let map = Map::from_iter([("gem".to_string(), json!({"name": "Ruby"}))]);
        let mut upserted_drawers = Vec::new();

        let decomposed = decompose_nested_objects(
            map,
            "weapon",
            "@weapon:blade",
            &BTreeMap::new(),
            ExecutionContext::root(),
            |drawer_name, value, _, _| {
                upserted_drawers.push((drawer_name.to_string(), value));
                Ok(format!("@{drawer_name}:generated"))
            },
        )
        .expect("decomposition should succeed");

        assert_eq!(decomposed["gem"], "@gem:generated");
        assert_eq!(upserted_drawers.len(), 1);
        assert_eq!(upserted_drawers[0].0, "gem");
        assert_eq!(upserted_drawers[0].1["name"], "Ruby");
    }

    #[test]
    fn decompose_arrays_recursively_preserves_pointer_order() {
        let map = Map::from_iter([(
            "gems".to_string(),
            json!([
                {"_id": "fire"},
                {"name": "Water"},
                "@gem:air"
            ]),
        )]);

        let decomposed = decompose_nested_objects(
            map,
            "weapon",
            "@weapon:blade",
            &BTreeMap::new(),
            ExecutionContext::root(),
            |drawer_name, _, _, _| Ok(format!("@{drawer_name}:generated")),
        )
        .expect("decomposition should succeed");

        assert_eq!(
            decomposed["gems"],
            json!(["@gem:fire", "@gem:generated", "@gem:air"])
        );
    }

    #[test]
    fn static_relationships_normalize_plain_strings_and_scoped_drawers() {
        let map = Map::from_iter([("owner".to_string(), json!("alice"))]);
        let constraints = BTreeMap::from([(
            "owner".to_string(),
            json!({"type": "M:1", "target_drawer": "character"}),
        )]);

        let decomposed = decompose_nested_objects(
            map,
            "tenant_weapon",
            "@tenant_weapon:blade",
            &constraints,
            ExecutionContext {
                drawer_namespace: Some("tenant"),
            },
            |_, _, _, _| Err(io::Error::other("unexpected upsert")),
        )
        .expect("decomposition should succeed");

        assert_eq!(decomposed["owner"], "@tenant_character:alice");
    }

    #[test]
    fn decompose_nested_object_propagates_slash_namespace() {
        let map = Map::from_iter([("gem".to_string(), json!({"name": "Ruby"}))]);
        let mut upserted_drawers = Vec::new();

        let decomposed = decompose_nested_objects(
            map,
            "db/schema/weapon",
            "@db/schema/weapon:blade",
            &BTreeMap::new(),
            ExecutionContext::root(),
            |drawer_name, value, _, _| {
                upserted_drawers.push((drawer_name.to_string(), value));
                Ok(format!("@{drawer_name}:generated"))
            },
        )
        .expect("decomposition should succeed");

        assert_eq!(decomposed["gem"], "@db/schema/gem:generated");
        assert_eq!(upserted_drawers.len(), 1);
        assert_eq!(upserted_drawers[0].0, "db/schema/gem");
    }
}
