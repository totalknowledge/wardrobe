use super::relationship;
use crate::wrdb_lib::pointer;
use crate::wrdb_lib::routing;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Error, ErrorKind, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeleteAction {
    Cascade,
    Restrict,
    SetNull,
}

#[derive(Clone, Debug)]
pub(crate) struct InverseDeleteRule {
    pub(crate) field_name: String,
    pub(crate) action: DeleteAction,
    pub(crate) target_drawer: String,
    pub(crate) mapped_by: String,
}

pub(crate) fn inverse_delete_rules(
    delete_rules: BTreeMap<String, Value>,
    relationship_constraints: BTreeMap<String, Value>,
) -> Vec<InverseDeleteRule> {
    delete_rules
        .into_iter()
        .filter_map(|(field_name, rule)| {
            let action = delete_rule_action(&rule)?;
            let relationship_rule = relationship_constraints.get(&field_name)?;
            let target_drawer = relationship::relationship_target_drawer(relationship_rule)?;
            let mapped_by = relationship::relationship_mapped_by(relationship_rule)?;

            Some(InverseDeleteRule {
                field_name,
                action,
                target_drawer: target_drawer.to_string(),
                mapped_by: mapped_by.to_string(),
            })
        })
        .collect()
}

pub(crate) fn evaluate_restrict_delete_rules<F>(
    parent_pointer: &str,
    inverse_delete_rules: &[InverseDeleteRule],
    drawer_namespace: Option<&str>,
    mut records_matching_parent_pointer: F,
) -> Result<()>
where
    F: FnMut(&str, &str, &str) -> Result<Vec<Value>>,
{
    let restricted_pointers = collect_inverse_delete_rule_pointers(
        parent_pointer,
        inverse_delete_rules,
        DeleteAction::Restrict,
        drawer_namespace,
        &mut records_matching_parent_pointer,
    )?;

    if let Some(blocking_pointer) = restricted_pointers.first() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "Delete restricted: '{}' is still referenced by '{}' through a Restrict rule",
                parent_pointer, blocking_pointer
            ),
        ));
    }

    Ok(())
}

pub(crate) fn collect_inverse_delete_rule_pointers<F>(
    parent_pointer: &str,
    inverse_delete_rules: &[InverseDeleteRule],
    action: DeleteAction,
    drawer_namespace: Option<&str>,
    mut records_matching_parent_pointer: F,
) -> Result<Vec<String>>
where
    F: FnMut(&str, &str, &str) -> Result<Vec<Value>>,
{
    let mut pointers = Vec::new();

    for rule in inverse_delete_rules
        .iter()
        .filter(|rule| rule.action == action)
    {
        let child_records =
            records_matching_parent_pointer(&rule.target_drawer, &rule.mapped_by, parent_pointer)?;

        let physical_target_drawer =
            routing::scoped_drawer_name(&rule.target_drawer, drawer_namespace);
        for record in child_records {
            if let Some(child_key) = record.get("_id").and_then(|value| value.as_str()) {
                pointers.push(pointer::format_pointer(&physical_target_drawer, child_key));
            }
        }
    }

    Ok(pointers)
}

pub(crate) fn apply_set_null_delete_rules<F, G>(
    parent_pointer: &str,
    inverse_delete_rules: &[InverseDeleteRule],
    drawer_namespace: Option<&str>,
    mut records_matching_parent_pointer: F,
    mut upsert_child_record: G,
) -> Result<()>
where
    F: FnMut(&str, &str, &str) -> Result<Vec<Value>>,
    G: FnMut(&str, &str, Value) -> Result<()>,
{
    for rule in inverse_delete_rules
        .iter()
        .filter(|rule| rule.action == DeleteAction::SetNull)
    {
        let mut child_records =
            records_matching_parent_pointer(&rule.target_drawer, &rule.mapped_by, parent_pointer)?;

        for child_record in &mut child_records {
            clear_parent_pointer_field(child_record, &rule.mapped_by, parent_pointer);
        }

        let physical_target_drawer =
            routing::scoped_drawer_name(&rule.target_drawer, drawer_namespace);
        for child_record in child_records {
            upsert_child_record(&physical_target_drawer, &rule.field_name, child_record)?;
        }
    }

    Ok(())
}

pub(crate) fn collect_cascade_pointers(record: &Value, cascade_fields: &[String]) -> Vec<String> {
    let mut pointers = Vec::new();

    if let Value::Object(map) = record {
        for field in cascade_fields {
            if let Some(value) = map.get(field) {
                pointer::collect_pointer_strings(value, &mut pointers);
            }
        }
    }

    pointers
}

pub(crate) fn value_contains_pointer(value: &Value, pointer: &str) -> bool {
    match value {
        Value::String(value) => value == pointer,
        Value::Array(values) => values
            .iter()
            .any(|value| value_contains_pointer(value, pointer)),
        Value::Object(map) => map
            .values()
            .any(|value| value_contains_pointer(value, pointer)),
        _ => false,
    }
}

fn clear_parent_pointer_field(record: &mut Value, field_name: &str, pointer: &str) -> bool {
    let Value::Object(map) = record else {
        return false;
    };

    let Some(field_value) = map.get_mut(field_name) else {
        return false;
    };

    let mut remove_field = false;
    let changed = match field_value {
        Value::String(value) if value == pointer => {
            remove_field = true;
            true
        }
        Value::Array(values) => {
            let original_len = values.len();
            values.retain(|value| !value_contains_pointer(value, pointer));
            if values.is_empty() {
                remove_field = true;
            }
            values.len() != original_len
        }
        _ => false,
    };

    if remove_field {
        map.remove(field_name);
    }

    changed
}

fn delete_rule_action(rule: &Value) -> Option<DeleteAction> {
    let action = rule
        .as_str()
        .or_else(|| rule.get("action").and_then(|action| action.as_str()))?;

    if action.eq_ignore_ascii_case("Cascade") {
        Some(DeleteAction::Cascade)
    } else if action.eq_ignore_ascii_case("Restrict") {
        Some(DeleteAction::Restrict)
    } else if action.eq_ignore_ascii_case("SetNull") {
        Some(DeleteAction::SetNull)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inverse_delete_rules_keep_only_complete_relationship_metadata() {
        let delete_rules = BTreeMap::from([
            ("children".to_string(), json!("Cascade")),
            ("orphans".to_string(), json!({"action": "SetNull"})),
            ("ignored".to_string(), json!("Restrict")),
        ]);
        let relationship_constraints = BTreeMap::from([
            (
                "children".to_string(),
                json!({"target_drawer": "weapon", "mapped_by": "owner"}),
            ),
            (
                "orphans".to_string(),
                json!({"target_drawer": "pet", "mapped_by": "owner"}),
            ),
        ]);

        let rules = inverse_delete_rules(delete_rules, relationship_constraints);

        assert_eq!(rules.len(), 2);
        assert!(matches!(rules[0].action, DeleteAction::Cascade));
        assert_eq!(rules[0].target_drawer, "weapon");
        assert!(matches!(rules[1].action, DeleteAction::SetNull));
    }

    #[test]
    fn restrict_rules_return_blocking_pointer_error() {
        let rules = vec![InverseDeleteRule {
            field_name: "weapons".to_string(),
            action: DeleteAction::Restrict,
            target_drawer: "weapon".to_string(),
            mapped_by: "owner".to_string(),
        }];

        let error = evaluate_restrict_delete_rules(
            "@character:alice",
            &rules,
            None,
            |_target_drawer, _mapped_by, _parent_pointer| {
                Ok(vec![json!({"_id": "blade", "owner": "@character:alice"})])
            },
        )
        .expect_err("restrict rule should fail");

        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(error.to_string().contains("Delete restricted"));
    }

    #[test]
    fn cascade_rules_collect_scoped_child_pointers() {
        let rules = vec![InverseDeleteRule {
            field_name: "weapons".to_string(),
            action: DeleteAction::Cascade,
            target_drawer: "weapon".to_string(),
            mapped_by: "owner".to_string(),
        }];

        let pointers = collect_inverse_delete_rule_pointers(
            "@tenant_character:alice",
            &rules,
            DeleteAction::Cascade,
            Some("tenant"),
            |_target_drawer, _mapped_by, _parent_pointer| {
                Ok(vec![json!({"_id": "blade"}), json!({"_id": "bow"})])
            },
        )
        .expect("cascade pointers should collect");

        assert_eq!(pointers, vec!["@tenant_weapon:blade", "@tenant_weapon:bow"]);
    }

    #[test]
    fn set_null_rules_clear_child_pointer_fields_and_upsert_children() {
        let rules = vec![InverseDeleteRule {
            field_name: "weapons".to_string(),
            action: DeleteAction::SetNull,
            target_drawer: "weapon".to_string(),
            mapped_by: "owner".to_string(),
        }];
        let mut upserted = Vec::new();

        apply_set_null_delete_rules(
            "@character:alice",
            &rules,
            None,
            |_target_drawer, _mapped_by, _parent_pointer| {
                Ok(vec![
                    json!({"_id": "blade", "owner": "@character:alice"}),
                    json!({"_id": "bow", "owner": ["@character:alice", "@character:bob"]}),
                ])
            },
            |target_drawer, field_name, record| {
                upserted.push((target_drawer.to_string(), field_name.to_string(), record));
                Ok(())
            },
        )
        .expect("set-null rules should apply");

        assert_eq!(upserted.len(), 2);
        assert!(upserted[0].2.get("owner").is_none());
        assert_eq!(upserted[1].2["owner"], json!(["@character:bob"]));
    }

    #[test]
    fn cascade_fields_collect_nested_pointer_values() {
        let record = json!({
            "weapon": "@weapon:blade",
            "ignored": "@gem:fire",
            "nested": [{"gem": "@gem:water"}]
        });
        let fields = vec!["weapon".to_string(), "nested".to_string()];

        let pointers = collect_cascade_pointers(&record, &fields);

        assert_eq!(pointers, vec!["@weapon:blade", "@gem:water"]);
    }
}
