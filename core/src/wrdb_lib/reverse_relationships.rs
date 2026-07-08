use crate::wrdb_lib::pointer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub(crate) struct ReverseRelationshipEntry {
    pub child_drawer: String,
    pub child_id: String,
    pub child_pointer: String,
    pub field_name: String,
    #[serde(default)]
    pub explicit: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_source: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ReverseRelationshipIndex {
    #[serde(default)]
    references: BTreeMap<String, Vec<ReverseRelationshipEntry>>,
}

impl ReverseRelationshipIndex {
    pub(crate) fn clear(&mut self) {
        self.references.clear();
    }

    pub(crate) fn references_for_parent(
        &self,
        parent_pointer: &str,
    ) -> Vec<ReverseRelationshipEntry> {
        self.references
            .get(parent_pointer)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn replace_record(
        &mut self,
        child_drawer: &str,
        child_id: &str,
        record: &Value,
        relationship_constraints: &BTreeMap<String, Value>,
        delete_rules: &BTreeMap<String, Value>,
    ) {
        let child_pointer = pointer::format_pointer(child_drawer, child_id);
        self.remove_child_pointer(&child_pointer);
        self.add_record(
            child_drawer,
            child_id,
            record,
            relationship_constraints,
            delete_rules,
        );
    }

    pub(crate) fn add_record(
        &mut self,
        child_drawer: &str,
        child_id: &str,
        record: &Value,
        relationship_constraints: &BTreeMap<String, Value>,
        delete_rules: &BTreeMap<String, Value>,
    ) {
        for (parent_pointer, entry) in Self::record_relationship_entries(
            child_drawer,
            child_id,
            record,
            relationship_constraints,
            delete_rules,
        ) {
            let entries = self.references.entry(parent_pointer).or_default();
            entries.push(entry);
            entries.sort();
            entries.dedup();
        }
    }

    pub(crate) fn remove_child(&mut self, child_drawer: &str, child_id: &str) -> bool {
        let child_pointer = pointer::format_pointer(child_drawer, child_id);
        self.remove_child_pointer(&child_pointer)
    }

    fn remove_child_pointer(&mut self, child_pointer: &str) -> bool {
        let mut changed = false;
        let parent_pointers = self.references.keys().cloned().collect::<Vec<_>>();

        for parent_pointer in parent_pointers {
            let Some(entries) = self.references.get_mut(&parent_pointer) else {
                continue;
            };

            let original_len = entries.len();
            entries.retain(|entry| entry.child_pointer != child_pointer);
            changed |= entries.len() != original_len;
            if entries.is_empty() {
                self.references.remove(&parent_pointer);
            }
        }

        changed
    }

    fn record_relationship_entries(
        child_drawer: &str,
        child_id: &str,
        record: &Value,
        relationship_constraints: &BTreeMap<String, Value>,
        delete_rules: &BTreeMap<String, Value>,
    ) -> Vec<(String, ReverseRelationshipEntry)> {
        let Some(record_map) = record.as_object() else {
            return Vec::new();
        };

        let child_pointer = pointer::format_pointer(child_drawer, child_id);
        let mut entries = BTreeSet::new();

        for (field_name, value) in record_map {
            if field_name == "_id" {
                continue;
            }

            let mut parent_pointers = Vec::new();
            pointer::collect_pointer_strings(value, &mut parent_pointers);
            parent_pointers.sort();
            parent_pointers.dedup();

            if parent_pointers.is_empty() {
                continue;
            }

            let explicit = relationship_constraints.contains_key(field_name)
                || delete_rules.contains_key(field_name)
                || relationship_constraints
                    .get(field_name)
                    .and_then(|rule| rule.get("reverse"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            let delete_rule = delete_rules.get(field_name).and_then(delete_rule_action);
            let rule_source = if relationship_constraints.contains_key(field_name) {
                Some("relationship".to_string())
            } else if delete_rules.contains_key(field_name) {
                Some("delete_rule".to_string())
            } else {
                None
            };

            for parent_pointer in parent_pointers {
                entries.insert((
                    parent_pointer,
                    ReverseRelationshipEntry {
                        child_drawer: child_drawer.to_string(),
                        child_id: child_id.to_string(),
                        child_pointer: child_pointer.clone(),
                        field_name: field_name.to_string(),
                        explicit,
                        delete_rule: delete_rule.clone(),
                        rule_source: rule_source.clone(),
                    },
                ));
            }
        }

        entries.into_iter().collect()
    }
}

fn delete_rule_action(rule: &Value) -> Option<String> {
    rule.as_str()
        .or_else(|| rule.get("action").and_then(Value::as_str))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn implicit_mappings_are_recorded_for_pointer_like_values() {
        let mut index = ReverseRelationshipIndex::default();
        index.add_record(
            "book",
            "book_001",
            &json!({
                "_id": "book_001",
                "author_id": "@entity:entity_001",
                "tags": ["@tag:t1", "ordinary text"]
            }),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );

        let author_refs = index.references_for_parent("@entity:entity_001");
        assert_eq!(author_refs.len(), 1);
        assert_eq!(author_refs[0].child_pointer, "@book:book_001");
        assert_eq!(author_refs[0].field_name, "author_id");
        assert!(!author_refs[0].explicit);

        let tag_refs = index.references_for_parent("@tag:t1");
        assert_eq!(tag_refs.len(), 1);
        assert_eq!(tag_refs[0].field_name, "tags");
    }

    #[test]
    fn replace_record_removes_stale_parent_mappings() {
        let mut index = ReverseRelationshipIndex::default();
        index.replace_record(
            "book",
            "book_001",
            &json!({"_id": "book_001", "author_id": "@entity:old"}),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        index.replace_record(
            "book",
            "book_001",
            &json!({"_id": "book_001", "author_id": "@entity:new"}),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );

        assert!(index.references_for_parent("@entity:old").is_empty());
        assert_eq!(index.references_for_parent("@entity:new").len(), 1);
    }
}
