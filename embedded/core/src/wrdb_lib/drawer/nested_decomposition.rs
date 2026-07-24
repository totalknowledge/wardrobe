use super::relationship::{self, RelationTarget};
use crate::wrdb_lib::pointer;
use crate::wrdb_lib::routing::ExecutionContext;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::io::{Error, ErrorKind, Result};

pub(crate) fn validate_declared_relationship_targets(
    map: &Map<String, Value>,
    current_drawer_name: &str,
    relationship_constraints: &BTreeMap<String, Value>,
    context: ExecutionContext<'_>,
) -> Result<()> {
    for (field_name, rule) in relationship_constraints {
        let Some(value) = map.get(field_name) else {
            continue;
        };
        let allowed_drawers = allowed_target_drawers(rule, current_drawer_name, context);
        if allowed_drawers.is_empty() {
            continue;
        }

        for explicit_drawer in pointer::inline_pointer_drawer_names(value) {
            let resolved_drawer =
                resolve_drawer_name(&explicit_drawer, current_drawer_name, context);
            if !allowed_drawers.contains(&resolved_drawer) {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "relationship field '{field_name}' expected target drawer '{}' but pointer targets '{explicit_drawer}'",
                        allowed_drawers.join("' or '")
                    ),
                ));
            }
        }
    }

    Ok(())
}

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
        let drawer_name = relationship::drawer_name_for_relation_target(
            &relation_target,
            &key,
            current_drawer_name,
        );
        let drawer_name = resolve_drawer_name(&drawer_name, current_drawer_name, context);
        let implicit_parent_field = relationship_constraints
            .get(&key)
            .and_then(relationship::implicit_parent_field_for_rule);
        let processed_value = decompose_relationship_value(
            &drawer_name,
            current_drawer_name,
            value,
            relation_target,
            parent_pointer,
            implicit_parent_field,
            context,
            true,
            &mut upsert_child,
        )?;
        continuous_map.insert(key, processed_value);
    }

    Ok(continuous_map)
}

fn decompose_relationship_value<F>(
    drawer_name: &str,
    current_drawer_name: &str,
    value: Value,
    relation_target: RelationTarget,
    parent_pointer: &str,
    implicit_parent_field: Option<&str>,
    context: ExecutionContext<'_>,
    direct_relationship_value: bool,
    upsert_child: &mut F,
) -> Result<Value>
where
    F: FnMut(&str, Value, ExecutionContext<'_>, Option<&str>) -> Result<String>,
{
    match value {
        Value::Object(mut child_map) => {
            let relationship_identity =
                child_map
                    .get("_id")
                    .and_then(Value::as_str)
                    .and_then(|reference_id| {
                        relationship_identity(
                            reference_id,
                            drawer_name,
                            current_drawer_name,
                            &relation_target,
                            context,
                            direct_relationship_value,
                        )
                    });

            if let Some((target_drawer, normalized_pointer)) = relationship_identity {
                if child_map.len() == 1 {
                    return Ok(Value::String(normalized_pointer));
                }

                child_map.insert("_id".to_string(), Value::String(normalized_pointer));
                if let Some(mapped_by) = implicit_parent_field {
                    child_map.insert(
                        mapped_by.to_string(),
                        Value::String(parent_pointer.to_string()),
                    );
                }
                let child_pointer = upsert_child(
                    &target_drawer,
                    Value::Object(child_map),
                    context,
                    implicit_parent_field,
                )?;
                Ok(Value::String(child_pointer))
            } else {
                let mut inline_map = Map::new();
                for (key, value) in child_map {
                    let processed_value = decompose_relationship_value(
                        drawer_name,
                        current_drawer_name,
                        value,
                        relation_target.clone(),
                        parent_pointer,
                        implicit_parent_field,
                        context,
                        false,
                        upsert_child,
                    )?;
                    inline_map.insert(key, processed_value);
                }
                Ok(Value::Object(inline_map))
            }
        }
        Value::Array(values) => values
            .into_iter()
            .map(|item| {
                decompose_relationship_value(
                    drawer_name,
                    current_drawer_name,
                    item,
                    relation_target.clone(),
                    parent_pointer,
                    implicit_parent_field,
                    context,
                    direct_relationship_value,
                    upsert_child,
                )
            })
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::String(reference) if pointer::is_pointer(&reference) => {
            let (pointer_drawer, pointer_key) =
                pointer::try_parse_pointer(&reference).ok_or_else(|| {
                    Error::new(ErrorKind::InvalidInput, "invalid relationship pointer")
                })?;
            let target_drawer = resolve_drawer_name(&pointer_drawer, current_drawer_name, context);
            let normalized_pointer = pointer::format_pointer(&target_drawer, &pointer_key);
            Ok(Value::String(normalized_pointer))
        }
        Value::String(reference_id)
            if direct_relationship_value
                && relationship::should_normalize_plain_string(&relation_target) =>
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

fn relationship_identity(
    reference_id: &str,
    default_drawer_name: &str,
    current_drawer_name: &str,
    relation_target: &RelationTarget,
    context: ExecutionContext<'_>,
    direct_relationship_value: bool,
) -> Option<(String, String)> {
    let (target_drawer, normalized_pointer) =
        if let Some((pointer_drawer, pointer_key)) = pointer::try_parse_pointer(reference_id) {
            let target_drawer = resolve_drawer_name(&pointer_drawer, current_drawer_name, context);
            let normalized_pointer = pointer::format_pointer(&target_drawer, &pointer_key);
            (target_drawer, normalized_pointer)
        } else if direct_relationship_value
            && relationship::should_normalize_plain_string(relation_target)
        {
            let target_drawer = default_drawer_name.to_string();
            let normalized_pointer = pointer::normalize_reference_pointer_for_namespace(
                &target_drawer,
                reference_id,
                context.drawer_namespace,
            );
            (target_drawer, normalized_pointer)
        } else {
            return None;
        };
    Some((target_drawer, normalized_pointer))
}

fn allowed_target_drawers(
    rule: &Value,
    current_drawer_name: &str,
    context: ExecutionContext<'_>,
) -> Vec<String> {
    let mut targets = relationship::relationship_target_drawer(rule)
        .into_iter()
        .map(|target| resolve_drawer_name(target, current_drawer_name, context))
        .collect::<Vec<_>>();
    if let Some(polymorphic_targets) = rule.get("target_drawers").and_then(Value::as_array) {
        targets.extend(
            polymorphic_targets
                .iter()
                .filter_map(Value::as_str)
                .map(|target| resolve_drawer_name(target, current_drawer_name, context)),
        );
    }
    targets.sort();
    targets.dedup();
    targets
}

fn resolve_drawer_name(
    drawer_name: &str,
    current_drawer_name: &str,
    context: ExecutionContext<'_>,
) -> String {
    let slash_scoped = if drawer_name.contains('/') {
        drawer_name.to_string()
    } else if let Some(idx) = current_drawer_name.rfind('/') {
        format!("{}/{}", &current_drawer_name[..idx], drawer_name)
    } else {
        drawer_name.to_string()
    };
    pointer::scoped_drawer_name(&slash_scoped, context.drawer_namespace)
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

        assert_eq!(decomposed["gems"], json!({"_id": "fire"}));
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

        assert_eq!(decomposed["gem"], json!({"name": "Ruby"}));
        assert!(upserted_drawers.is_empty());
    }

    #[test]
    fn decompose_arrays_recursively_preserves_pointer_order() {
        let map = Map::from_iter([(
            "gems".to_string(),
            json!([
                {"_id": "fire"},
                {"_id": "@gem:water", "name": "Water"},
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
            json!([{"_id": "fire"}, "@gem:generated", "@gem:air"])
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
        let map = Map::from_iter([(
            "gem".to_string(),
            json!({"_id": "@gem:ruby", "name": "Ruby"}),
        )]);
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

    #[test]
    fn map_entries_are_classified_independently_and_keys_are_preserved() {
        let map = Map::from_iter([(
            "item_map".to_string(),
            json!({
                "main_hand": {"_id": "@item:sword"},
                "off_hand": {"_id": "@item:shield", "name": "Shield"},
                "notes": {"quality": "rare"}
            }),
        )]);
        let mut upserted = Vec::new();

        let decomposed = decompose_nested_objects(
            map,
            "character",
            "@character:hero",
            &BTreeMap::new(),
            ExecutionContext::root(),
            |drawer_name, value, _, _| {
                upserted.push((drawer_name.to_string(), value));
                Ok("@item:shield".to_string())
            },
        )
        .expect("decomposition should succeed");

        assert_eq!(
            decomposed["item_map"],
            json!({
                "main_hand": "@item:sword",
                "off_hand": "@item:shield",
                "notes": {"quality": "rare"}
            })
        );
        assert_eq!(upserted.len(), 1);
        assert_eq!(upserted[0].0, "item");
        assert_eq!(upserted[0].1["name"], "Shield");
    }

    #[test]
    fn declared_relationship_rejects_conflicting_pointer_target() {
        let map = Map::from_iter([(
            "item_map".to_string(),
            json!({"main_hand": {"_id": "@spell:fireball"}}),
        )]);
        let constraints = BTreeMap::from([(
            "item_map".to_string(),
            json!({"type": "M:1", "target_drawer": "item"}),
        )]);

        let error = validate_declared_relationship_targets(
            &map,
            "character",
            &constraints,
            ExecutionContext::root(),
        )
        .expect_err("conflicting pointer should fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("item_map"));
        assert!(error.to_string().contains("spell"));
    }
}
