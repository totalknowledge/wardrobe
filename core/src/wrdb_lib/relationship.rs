use crate::wrdb_lib::pointer;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RelationTarget {
    Inferred(String),
    Static(String),
    Polymorphic,
    SelfReference,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VirtualRelationship {
    pub(crate) field_name: String,
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
            let target_drawer = relationship_target_drawer(relationship_rule)?;
            let mapped_by = relationship_mapped_by(relationship_rule)?;

            Some(InverseDeleteRule {
                field_name,
                action,
                target_drawer: target_drawer.to_string(),
                mapped_by: mapped_by.to_string(),
            })
        })
        .collect()
}

pub(crate) fn inline_relationship_aliases(
    map: &Map<String, Value>,
    relationship_constraints: &BTreeMap<String, Value>,
) -> Vec<(String, Value)> {
    map.iter()
        .filter(|(field_name, _)| field_name.as_str() != "_id")
        .filter(|(field_name, _)| !relationship_constraints.contains_key(field_name.as_str()))
        .filter_map(|(field_name, value)| {
            let drawer_names = pointer::inline_pointer_drawer_names(value);
            if drawer_names.is_empty() {
                return None;
            }

            Some((
                field_name.clone(),
                serde_json::json!({
                    "type": "polymorphic",
                    "target_drawers": drawer_names
                }),
            ))
        })
        .collect()
}

pub(crate) fn relation_target_for_field(
    field_name: &str,
    current_drawer_name: &str,
    relationship_constraints: &BTreeMap<String, Value>,
) -> RelationTarget {
    let Some(rule) = relationship_constraints.get(field_name) else {
        return RelationTarget::Inferred(relationship_drawer_name(field_name));
    };

    if relationship_constraint_type(rule)
        .is_some_and(|relationship_type| relationship_type.eq_ignore_ascii_case("polymorphic"))
    {
        return RelationTarget::Polymorphic;
    }

    let Some(target_drawer) = relationship_target_drawer(rule) else {
        return RelationTarget::Inferred(relationship_drawer_name(field_name));
    };

    if target_drawer == current_drawer_name
        || current_drawer_name
            .strip_suffix(target_drawer)
            .is_some_and(|prefix| prefix.ends_with('_'))
    {
        RelationTarget::SelfReference
    } else {
        RelationTarget::Static(target_drawer.to_string())
    }
}

pub(crate) fn drawer_name_for_relation_target(
    target: &RelationTarget,
    field_name: &str,
    current_drawer_name: &str,
) -> String {
    match target {
        RelationTarget::Inferred(drawer_name) => drawer_name.clone(),
        RelationTarget::Static(drawer_name) => drawer_name.clone(),
        RelationTarget::SelfReference => current_drawer_name.to_string(),
        RelationTarget::Polymorphic => relationship_drawer_name(field_name),
    }
}

pub(crate) fn should_normalize_plain_string(target: &RelationTarget) -> bool {
    matches!(
        target,
        RelationTarget::Static(_) | RelationTarget::SelfReference
    )
}

pub(crate) fn virtual_relationships(
    relationship_constraints: BTreeMap<String, Value>,
) -> Vec<VirtualRelationship> {
    relationship_constraints
        .into_iter()
        .filter_map(|(field_name, rule)| {
            if relationship_constraint_type(&rule) != Some("1:M") {
                return None;
            }

            let target_drawer = relationship_target_drawer(&rule)?.to_string();
            let mapped_by = relationship_mapped_by(&rule)?.to_string();
            Some(VirtualRelationship {
                field_name,
                target_drawer,
                mapped_by,
            })
        })
        .collect()
}

pub(crate) fn relationship_constraint_type(rule: &Value) -> Option<&str> {
    rule.get("type").and_then(|value| value.as_str())
}

pub(crate) fn relationship_target_drawer(rule: &Value) -> Option<&str> {
    rule.get("target_drawer").and_then(|value| value.as_str())
}

pub(crate) fn relationship_mapped_by(rule: &Value) -> Option<&str> {
    rule.get("mapped_by").and_then(|value| value.as_str())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn relation_target_infers_pluralized_drawer_names() {
        let constraints = BTreeMap::new();

        assert_eq!(
            relation_target_for_field("gems", "weapon", &constraints),
            RelationTarget::Inferred("gem".to_string())
        );
        assert_eq!(
            relation_target_for_field("parties", "character", &constraints),
            RelationTarget::Inferred("party".to_string())
        );
    }

    #[test]
    fn relation_target_resolves_static_polymorphic_and_self_references() {
        let constraints = BTreeMap::from([
            (
                "owner".to_string(),
                json!({"type": "M:1", "target_drawer": "character"}),
            ),
            ("links".to_string(), json!({"type": "polymorphic"})),
            (
                "parent".to_string(),
                json!({"type": "M:1", "target_drawer": "category"}),
            ),
        ]);

        assert_eq!(
            relation_target_for_field("owner", "weapon", &constraints),
            RelationTarget::Static("character".to_string())
        );
        assert_eq!(
            relation_target_for_field("links", "weapon", &constraints),
            RelationTarget::Polymorphic
        );
        assert_eq!(
            relation_target_for_field("parent", "tenant_category", &constraints),
            RelationTarget::SelfReference
        );
    }

    #[test]
    fn inline_aliases_detect_inline_pointer_drawers() {
        let map = Map::from_iter([(
            "attachments".to_string(),
            json!([
                {"_id": "@image:cover"},
                {"_id": "@document:manual"}
            ]),
        )]);
        let aliases = inline_relationship_aliases(&map, &BTreeMap::new());

        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].0, "attachments");
        assert_eq!(aliases[0].1["type"], "polymorphic");
        assert_eq!(aliases[0].1["target_drawers"], json!(["document", "image"]));
    }

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
    fn virtual_relationships_returns_one_to_many_metadata() {
        let constraints = BTreeMap::from([
            (
                "weapons".to_string(),
                json!({"type": "1:M", "target_drawer": "weapon", "mapped_by": "owner"}),
            ),
            (
                "ignored".to_string(),
                json!({"type": "M:1", "target_drawer": "team"}),
            ),
        ]);

        let relationships = virtual_relationships(constraints);

        assert_eq!(relationships.len(), 1);
        assert_eq!(relationships[0].field_name, "weapons");
        assert_eq!(relationships[0].target_drawer, "weapon");
        assert_eq!(relationships[0].mapped_by, "owner");
    }
}
