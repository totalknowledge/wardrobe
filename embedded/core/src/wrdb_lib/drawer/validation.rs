use super::*;

impl Drawer {
    pub fn cascade_delete_fields(&self) -> Vec<String> {
        let mut fields = self
            .cascade_delete_rules
            .iter()
            .filter_map(|(field, should_cascade)| {
                if *should_cascade {
                    Some(field.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        for (field, rule) in &self.delete_rules {
            if Self::delete_rule_is_cascade(rule) && !fields.contains(field) {
                fields.push(field.clone());
            }
        }

        fields
    }

    pub fn relationship_constraints(&self) -> BTreeMap<String, Value> {
        self.relationship_constraints.clone()
    }

    pub fn register_relationship_constraint(
        &mut self,
        field_name: &str,
        rule: Value,
    ) -> std::io::Result<()> {
        if self.relationship_constraints.contains_key(field_name) {
            return Ok(());
        }

        self.relationship_constraints
            .insert(field_name.to_string(), rule);
        self.persist_metadata()
    }

    pub(super) fn validate_relationship_constraints(
        &self,
        record: &Value,
        primary_key_value: &str,
    ) -> std::io::Result<Option<String>> {
        let relationship_constraints = self.relationship_constraints.clone();

        for (field_name, rule) in relationship_constraints {
            let Some(relationship_type) = Self::relationship_type(&rule) else {
                continue;
            };

            match relationship_type {
                "1:1" => {
                    if let Some(field_value) = record.get(&field_name) {
                        if let Some(validation_error) =
                            Self::validate_reference_field(&field_name, field_value, &rule)
                        {
                            return Ok(Some(validation_error));
                        }

                        if let Some(pointer) = field_value.as_str() {
                            if let Some(validation_error) = self.validate_one_to_one_unique(
                                &field_name,
                                pointer,
                                primary_key_value,
                            )? {
                                return Ok(Some(validation_error));
                            }
                        }
                    }
                }
                "M:1" => {
                    if let Some(field_value) = record.get(&field_name) {
                        if let Some(validation_error) =
                            Self::validate_reference_field(&field_name, field_value, &rule)
                        {
                            return Ok(Some(validation_error));
                        }
                    }
                }
                "M:M" => {
                    if let Some(field_value) = record.get(&field_name) {
                        if let Some(validation_error) =
                            Self::validate_many_to_many_field(&field_name, field_value, &rule)
                        {
                            return Ok(Some(validation_error));
                        }
                    }
                }
                "1:M" => {}
                _ => {}
            }
        }

        Ok(None)
    }

    pub(super) fn validate_one_to_one_unique(
        &self,
        field_name: &str,
        pointer: &str,
        primary_key_value: &str,
    ) -> std::io::Result<Option<String>> {
        for existing_record in self.find_all_records()? {
            if existing_record
                .get(&self.primary_key)
                .and_then(|value| value.as_str())
                == Some(primary_key_value)
            {
                continue;
            }

            if existing_record
                .get(field_name)
                .and_then(|value| value.as_str())
                == Some(pointer)
            {
                return Ok(Some(format!(
                    "1:1 relationship constraint violation: Field '{}' with value '{}' already exists",
                    field_name, pointer
                )));
            }
        }

        Ok(None)
    }

    pub(super) fn validate_reference_field(
        field_name: &str,
        value: &Value,
        rule: &Value,
    ) -> Option<String> {
        let Some(pointer) = value.as_str() else {
            return Some(format!(
                "Relationship constraint violation: Field '{}' must be a pointer string",
                field_name
            ));
        };

        Self::validate_pointer_target(field_name, pointer, rule)
    }

    pub(super) fn validate_many_to_many_field(
        field_name: &str,
        value: &Value,
        rule: &Value,
    ) -> Option<String> {
        let Some(values) = value.as_array() else {
            return Some(format!(
                "M:M relationship constraint violation: Field '{}' must be an array of pointer strings",
                field_name
            ));
        };

        for value in values {
            let Some(pointer) = value.as_str() else {
                return Some(format!(
                    "M:M relationship constraint violation: Field '{}' must contain only pointer strings",
                    field_name
                ));
            };

            if let Some(validation_error) = Self::validate_pointer_target(field_name, pointer, rule)
            {
                return Some(validation_error);
            }
        }

        None
    }

    pub(super) fn validate_pointer_target(
        field_name: &str,
        pointer: &str,
        rule: &Value,
    ) -> Option<String> {
        let Some(pointer_drawer) = Self::pointer_drawer_name(pointer) else {
            return Some(format!(
                "Relationship constraint violation: Field '{}' contains malformed pointer '{}'",
                field_name, pointer
            ));
        };

        if let Some(target_drawer) = Self::relationship_target_drawer(rule) {
            if !Self::pointer_matches_target_drawer(pointer_drawer, target_drawer) {
                return Some(format!(
                    "Relationship constraint violation: Field '{}' expected target drawer '{}' but found '{}'",
                    field_name, target_drawer, pointer_drawer
                ));
            }
        }

        None
    }

    pub(super) fn relationship_type(rule: &Value) -> Option<&str> {
        rule.get("type").and_then(|value| value.as_str())
    }

    pub(super) fn relationship_target_drawer(rule: &Value) -> Option<&str> {
        rule.get("target_drawer").and_then(|value| value.as_str())
    }

    pub(super) fn pointer_drawer_name(pointer: &str) -> Option<&str> {
        let clean_pointer = pointer.strip_prefix('@')?;
        let (drawer_name, record_key) = clean_pointer.split_once(':')?;
        let record_key = record_key.strip_prefix("lnk_").unwrap_or(record_key);

        if drawer_name.is_empty() || record_key.is_empty() || record_key.contains(':') {
            return None;
        }

        Some(drawer_name)
    }

    pub(super) fn pointer_matches_target_drawer(pointer_drawer: &str, target_drawer: &str) -> bool {
        pointer_drawer == target_drawer
            || pointer_drawer
                .strip_suffix(target_drawer)
                .is_some_and(|prefix| prefix.ends_with('_'))
    }

    pub(super) fn validate_schema(&self, record: &Value) -> Result<(), String> {
        let Some(schema) = self.schema.as_ref() else {
            return Ok(());
        };

        let hidden_fields = self.hidden_output_fields();
        if hidden_fields.is_empty() {
            return Self::validate_value_against_schema(record, schema, "$");
        }

        let mut client_record = record.clone();
        Self::remove_hidden_fields_from_value(&mut client_record, &hidden_fields);
        Self::validate_value_against_schema(&client_record, schema, "$")
    }

    pub(super) fn validate_value_against_schema(
        value: &Value,
        schema: &Value,
        path: &str,
    ) -> Result<(), String> {
        let Some(schema_map) = schema.as_object() else {
            return Ok(());
        };

        if let Some(allowed_values) = schema_map
            .get("enum")
            .and_then(|enum_value| enum_value.as_array())
        {
            if !allowed_values
                .iter()
                .any(|allowed_value| allowed_value == value)
            {
                return Err(format!("{path} must match one of the declared enum values"));
            }
        }

        if let Some(type_rule) = schema_map.get("type") {
            Self::validate_type_rule(value, type_rule, path)?;
        }

        if let Some(required_fields) = schema_map.get("required") {
            Self::validate_required_fields(value, required_fields, path)?;
        }

        if let Some(properties) = schema_map
            .get("properties")
            .and_then(|properties| properties.as_object())
        {
            if let Some(object) = value.as_object() {
                for (field_name, field_schema) in properties {
                    if let Some(field_value) = object.get(field_name) {
                        let field_path = format!("{path}.{field_name}");
                        Self::validate_value_against_schema(
                            field_value,
                            field_schema,
                            &field_path,
                        )?;
                    }
                }

                if schema_map
                    .get("additionalProperties")
                    .and_then(|rule| rule.as_bool())
                    == Some(false)
                {
                    for field_name in object.keys() {
                        if !properties.contains_key(field_name) {
                            return Err(format!("{path}.{field_name} is not allowed by schema"));
                        }
                    }
                }
            }
        }

        Self::validate_string_bounds(value, schema_map, path)?;
        Self::validate_numeric_bounds(value, schema_map, path)?;

        Ok(())
    }

    pub(super) fn validate_type_rule(
        value: &Value,
        type_rule: &Value,
        path: &str,
    ) -> Result<(), String> {
        if let Some(type_name) = type_rule.as_str() {
            if Self::value_matches_type(value, type_name) {
                return Ok(());
            }

            return Err(format!("{path} must be of type {type_name}"));
        }

        if let Some(type_names) = type_rule.as_array() {
            let matches_any_type = type_names
                .iter()
                .filter_map(|type_name| type_name.as_str())
                .any(|type_name| Self::value_matches_type(value, type_name));

            if matches_any_type {
                return Ok(());
            }

            return Err(format!(
                "{path} must match one of the declared schema types"
            ));
        }

        Ok(())
    }

    pub(super) fn validate_required_fields(
        value: &Value,
        required_fields: &Value,
        path: &str,
    ) -> Result<(), String> {
        let Some(_) = value.as_object() else {
            return Ok(());
        };
        let Some(required_fields) = required_fields.as_array() else {
            return Ok(());
        };

        for field in required_fields {
            let Some(field_name) = field.as_str() else {
                continue;
            };

            let resolved = query::resolve_field_path(value, field_name);
            if resolved.is_none() || resolved == Some(&Value::Null) {
                return Err(format!("{path}.{field_name} is required by schema"));
            }
        }

        Ok(())
    }

    pub(super) fn validate_string_bounds(
        value: &Value,
        schema: &serde_json::Map<String, Value>,
        path: &str,
    ) -> Result<(), String> {
        let Some(value) = value.as_str() else {
            return Ok(());
        };

        if let Some(min_length) = schema.get("minLength").and_then(|length| length.as_u64()) {
            if value.chars().count() < min_length as usize {
                return Err(format!("{path} must have at least {min_length} characters"));
            }
        }

        if let Some(max_length) = schema.get("maxLength").and_then(|length| length.as_u64()) {
            if value.chars().count() > max_length as usize {
                return Err(format!("{path} must have at most {max_length} characters"));
            }
        }

        Ok(())
    }

    pub(super) fn validate_numeric_bounds(
        value: &Value,
        schema: &serde_json::Map<String, Value>,
        path: &str,
    ) -> Result<(), String> {
        let Some(value) = value.as_f64() else {
            return Ok(());
        };

        if let Some(minimum) = schema.get("minimum").and_then(|minimum| minimum.as_f64()) {
            if value < minimum {
                return Err(format!("{path} must be greater than or equal to {minimum}"));
            }
        }

        if let Some(maximum) = schema.get("maximum").and_then(|maximum| maximum.as_f64()) {
            if value > maximum {
                return Err(format!("{path} must be less than or equal to {maximum}"));
            }
        }

        Ok(())
    }

    pub(super) fn value_matches_type(value: &Value, type_name: &str) -> bool {
        match type_name {
            "array" => value.is_array(),
            "boolean" => value.is_boolean(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "null" => value.is_null(),
            "number" => value.is_number(),
            "object" => value.is_object(),
            "string" => value.is_string(),
            _ => false,
        }
    }
}
