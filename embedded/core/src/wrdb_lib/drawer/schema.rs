use super::*;

impl Drawer {
    pub fn manage_schema_rule(
        &mut self,
        action: &str,
        kind: &str,
        field_name: &str,
        payload: Value,
    ) -> std::io::Result<Value> {
        let normalized_action = action.to_ascii_lowercase();
        let normalized_kind = Self::normalize_schema_kind(kind)?;
        let effective_field = if normalized_kind == "timestamp" && field_name.trim().is_empty() {
            "timestamps"
        } else {
            Self::validate_schema_field(field_name)?;
            field_name
        };

        match normalized_action.as_str() {
            "add" => self.add_schema_rule(&normalized_kind, effective_field, payload.clone())?,
            "remove" => self.remove_schema_rule(&normalized_kind, effective_field, &payload)?,
            "rebuild" => self.rebuild_schema_rule(&normalized_kind, effective_field)?,
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("Unknown schema action: {action}"),
                ));
            }
        }

        self.persist_metadata()?;
        Ok(serde_json::json!({
            "drawer": self.name,
            "action": normalized_action,
            "type": normalized_kind,
            "field": effective_field,
            "payload": payload
        }))
    }

    pub(super) fn add_schema_rule(
        &mut self,
        kind: &str,
        field_name: &str,
        payload: Value,
    ) -> std::io::Result<()> {
        match kind {
            "index" => {
                self.add_secondary_index(field_name)?;
                self.record_schema_extension("indexes", field_name, payload);
                Ok(())
            }
            "key" => {
                let key_type = payload
                    .get("key_type")
                    .and_then(Value::as_str)
                    .unwrap_or("secondary")
                    .to_ascii_lowercase();
                if key_type == "primary" {
                    if field_name != self.primary_key {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            format!(
                                "primary key is fixed to '{}' and cannot be changed to '{}'",
                                self.primary_key, field_name
                            ),
                        ));
                    }
                } else {
                    self.add_unique_constraint(field_name)?;
                }
                self.record_schema_extension("keys", field_name, payload);
                Ok(())
            }
            "constraint" => {
                let constraint = Self::constraint_type(&payload)?;
                if Self::is_unique_constraint(constraint) {
                    self.add_unique_constraint(field_name)?;
                }
                if Self::is_required_constraint(constraint) {
                    self.add_required_field(field_name);
                }
                self.record_schema_extension("constraints", field_name, payload);
                Ok(())
            }
            "trigger" => {
                self.record_schema_extension("triggers", field_name, payload);
                Ok(())
            }
            "relationship" => {
                self.relationship_constraints
                    .insert(field_name.to_string(), payload);
                Ok(())
            }
            "cascade-delete" => {
                self.cascade_delete_rules
                    .insert(field_name.to_string(), true);
                self.delete_rules.insert(
                    field_name.to_string(),
                    serde_json::json!({ "action": "Cascade" }),
                );
                Ok(())
            }
            "timestamp" => {
                self.timestamps_enabled = true;
                self.record_schema_extension("timestamps", field_name, payload);
                Ok(())
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Unknown schema type: {kind}"),
            )),
        }
    }

    pub(super) fn remove_schema_rule(
        &mut self,
        kind: &str,
        field_name: &str,
        payload: &Value,
    ) -> std::io::Result<()> {
        match kind {
            "index" => {
                self.remove_schema_extension("indexes", field_name);
                self.remove_query_index(field_name)?;
                Ok(())
            }
            "key" => {
                let key_type = payload
                    .get("key_type")
                    .and_then(Value::as_str)
                    .unwrap_or("secondary")
                    .to_ascii_lowercase();
                if key_type != "primary" {
                    self.clear_unique_constraint(field_name)?;
                }
                self.remove_schema_extension("keys", field_name);
                Ok(())
            }
            "constraint" => {
                if let Ok(constraint) = Self::constraint_type(payload) {
                    if Self::is_unique_constraint(constraint) {
                        self.clear_unique_constraint(field_name)?;
                    }
                    if Self::is_required_constraint(constraint) {
                        self.remove_required_field(field_name);
                    }
                } else {
                    self.clear_unique_constraint(field_name)?;
                    self.remove_required_field(field_name);
                }
                self.remove_schema_extension("constraints", field_name);
                Ok(())
            }
            "trigger" => {
                self.remove_schema_extension("triggers", field_name);
                Ok(())
            }
            "relationship" => {
                self.relationship_constraints.remove(field_name);
                Ok(())
            }
            "cascade-delete" => {
                self.cascade_delete_rules.remove(field_name);
                self.delete_rules.remove(field_name);
                Ok(())
            }
            "timestamp" => {
                self.timestamps_enabled = false;
                self.remove_schema_extension("timestamps", field_name);
                Ok(())
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Unknown schema type: {kind}"),
            )),
        }
    }

    pub(super) fn add_unique_constraint(&mut self, field_name: &str) -> std::io::Result<()> {
        if self
            .unique_constraints
            .iter()
            .any(|constraint| constraint == field_name)
        {
            return Ok(());
        }

        let field_index = self.build_secondary_index(field_name, true)?;
        self.write_secondary_index_snapshot(field_name, &field_index)?;

        self.unique_constraints.push(field_name.to_string());
        self.secondary_memory_index
            .insert(field_name.to_string(), field_index);
        self.validated_secondary_indexes
            .insert(field_name.to_string());
        Ok(())
    }

    pub(super) fn add_secondary_index(&mut self, field_name: &str) -> std::io::Result<()> {
        if !self
            .unique_constraints
            .iter()
            .any(|constraint| constraint == field_name)
        {
            self.secondary_memory_index.remove(field_name);
        }
        self.materialized_secondary_indexes.remove(field_name);
        self.validated_secondary_indexes.remove(field_name);
        Ok(())
    }

    pub(super) fn rebuild_schema_rule(
        &mut self,
        kind: &str,
        field_name: &str,
    ) -> std::io::Result<()> {
        match kind {
            "index" => {
                if !self.schema_has_index(field_name)
                    && !self
                        .unique_constraints
                        .iter()
                        .any(|constraint| constraint == field_name)
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("Index '{field_name}' is not declared"),
                    ));
                }

                if self
                    .unique_constraints
                    .iter()
                    .any(|constraint| constraint == field_name)
                {
                    let field_index = self.build_secondary_index(field_name, true)?;
                    self.write_secondary_index_snapshot(field_name, &field_index)?;
                    self.secondary_memory_index
                        .insert(field_name.to_string(), field_index);
                    self.validated_secondary_indexes
                        .insert(field_name.to_string());
                } else {
                    self.materialize_query_index(field_name)?;
                }
                Ok(())
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Cannot rebuild schema type: {kind}"),
            )),
        }
    }

    pub(super) fn add_required_field(&mut self, field_name: &str) {
        let schema = self.ensure_schema_object();
        let required = schema
            .entry("required".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !required.is_array() {
            *required = Value::Array(Vec::new());
        }

        let Some(required_fields) = required.as_array_mut() else {
            return;
        };
        if !required_fields
            .iter()
            .any(|value| value.as_str() == Some(field_name))
        {
            required_fields.push(Value::String(field_name.to_string()));
        }
    }

    pub(super) fn remove_required_field(&mut self, field_name: &str) {
        let Some(schema) = self.schema.as_mut().and_then(Value::as_object_mut) else {
            return;
        };
        let Some(required_fields) = schema.get_mut("required").and_then(Value::as_array_mut) else {
            return;
        };

        required_fields.retain(|value| value.as_str() != Some(field_name));
    }

    pub(super) fn record_schema_extension(
        &mut self,
        bucket: &str,
        field_name: &str,
        payload: Value,
    ) {
        let schema = self.ensure_schema_object();
        let extension = schema
            .entry("x-wardrobe-cli".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !extension.is_object() {
            *extension = Value::Object(Map::new());
        }

        let Some(extension_map) = extension.as_object_mut() else {
            return;
        };
        let bucket_value = extension_map
            .entry(bucket.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !bucket_value.is_object() {
            *bucket_value = Value::Object(Map::new());
        }

        if let Some(bucket_map) = bucket_value.as_object_mut() {
            bucket_map.insert(field_name.to_string(), payload);
        }
    }

    pub(super) fn remove_schema_extension(&mut self, bucket: &str, field_name: &str) {
        let Some(schema) = self.schema.as_mut().and_then(Value::as_object_mut) else {
            return;
        };
        let Some(extension_map) = schema
            .get_mut("x-wardrobe-cli")
            .and_then(Value::as_object_mut)
        else {
            return;
        };
        let Some(bucket_map) = extension_map.get_mut(bucket).and_then(Value::as_object_mut) else {
            return;
        };

        bucket_map.remove(field_name);
    }

    pub(super) fn schema_has_index(&self, field_name: &str) -> bool {
        Self::schema_extension_fields(self.schema.as_ref(), "indexes")
            .iter()
            .any(|field| field == field_name)
    }

    pub(crate) fn mark_hidden_field(&mut self, field_name: &str) -> std::io::Result<()> {
        if self.hidden_fields.insert(field_name.to_string()) {
            self.persist_metadata()?;
        }

        Ok(())
    }

    pub(crate) fn hidden_output_fields(&self) -> Vec<String> {
        let mut fields = self
            .hidden_fields
            .iter()
            .filter(|field_name| !self.schema_declares_field(field_name))
            .cloned()
            .collect::<Vec<_>>();
        fields.sort();
        fields
    }

    pub(crate) fn remove_hidden_fields_from_value(value: &mut Value, hidden_fields: &[String]) {
        match value {
            Value::Object(map) => {
                for field_name in hidden_fields {
                    map.remove(field_name);
                }
                for field_value in map.values_mut() {
                    Self::remove_hidden_fields_from_value(field_value, hidden_fields);
                }
            }
            Value::Array(values) => {
                for value in values {
                    Self::remove_hidden_fields_from_value(value, hidden_fields);
                }
            }
            _ => {}
        }
    }

    pub(super) fn schema_declares_field(&self, field_name: &str) -> bool {
        self.schema
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|schema| schema.get("properties"))
            .and_then(Value::as_object)
            .is_some_and(|properties| properties.contains_key(field_name))
    }

    pub(super) fn schema_extension_fields(schema: Option<&Value>, bucket: &str) -> Vec<String> {
        schema
            .and_then(Value::as_object)
            .and_then(|schema_map| schema_map.get("x-wardrobe-cli"))
            .and_then(Value::as_object)
            .and_then(|extension_map| extension_map.get(bucket))
            .and_then(Value::as_object)
            .map(|bucket_map| bucket_map.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub(super) fn ensure_schema_object(&mut self) -> &mut Map<String, Value> {
        if !self.schema.as_ref().is_some_and(Value::is_object) {
            self.schema = Some(Value::Object(Map::new()));
        }

        self.schema
            .as_mut()
            .and_then(Value::as_object_mut)
            .expect("schema object must exist")
    }

    pub(super) fn normalize_schema_kind(kind: &str) -> std::io::Result<String> {
        match kind.to_ascii_lowercase().as_str() {
            "index" | "indexes" => Ok("index".to_string()),
            "key" | "keys" => Ok("key".to_string()),
            "constraint" | "constraints" => Ok("constraint".to_string()),
            "trigger" | "triggers" => Ok("trigger".to_string()),
            "relationship" | "relationships" => Ok("relationship".to_string()),
            "cascade-delete" | "cascade_delete" | "cascade" | "delete-rule" | "delete-rules" => {
                Ok("cascade-delete".to_string())
            }
            "timestamp" | "timestamps" => Ok("timestamp".to_string()),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Unknown schema type: {kind}"),
            )),
        }
    }

    pub(super) fn validate_schema_field(field_name: &str) -> std::io::Result<()> {
        if field_name.trim().is_empty()
            || field_name
                .split('.')
                .any(|segment| segment.trim().is_empty())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "schema command target field cannot be empty",
            ));
        }

        Ok(())
    }

    pub(super) fn constraint_type(payload: &Value) -> std::io::Result<&str> {
        payload
            .get("constraint")
            .or_else(|| payload.get("constraint_type"))
            .or_else(|| payload.get("type"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "constraint command requires a constraint type",
                )
            })
    }

    pub(super) fn is_unique_constraint(constraint: &str) -> bool {
        constraint.eq_ignore_ascii_case("unique")
    }

    pub(super) fn is_required_constraint(constraint: &str) -> bool {
        matches!(
            constraint.to_ascii_lowercase().as_str(),
            "non-null" | "non_null" | "nonnull" | "required"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_helper_functions_and_error_paths() {
        assert!(Drawer::validate_schema_field("").is_err());
        assert!(Drawer::validate_schema_field("  ").is_err());
        assert!(Drawer::validate_schema_field("a..b").is_err());
        assert!(Drawer::validate_schema_field("field_name").is_ok());

        assert!(Drawer::normalize_schema_kind("index").is_ok());
        assert!(Drawer::normalize_schema_kind("key").is_ok());
        assert!(Drawer::normalize_schema_kind("constraint").is_ok());
        assert!(Drawer::normalize_schema_kind("trigger").is_ok());
        assert!(Drawer::normalize_schema_kind("relationship").is_ok());
        assert!(Drawer::normalize_schema_kind("cascade-delete").is_ok());
        assert!(Drawer::normalize_schema_kind("timestamp").is_ok());
        assert!(Drawer::normalize_schema_kind("unknown_kind").is_err());

        assert!(Drawer::is_unique_constraint("unique"));
        assert!(Drawer::is_unique_constraint("UNIQUE"));
        assert!(!Drawer::is_unique_constraint("required"));

        assert!(Drawer::is_required_constraint("required"));
        assert!(Drawer::is_required_constraint("non-null"));
        assert!(Drawer::is_required_constraint("non_null"));
        assert!(Drawer::is_required_constraint("nonnull"));
        assert!(!Drawer::is_required_constraint("unique"));

        assert!(Drawer::constraint_type(&json!({"constraint": "unique"})).is_ok());
        assert!(Drawer::constraint_type(&json!({})).is_err());
    }
}
