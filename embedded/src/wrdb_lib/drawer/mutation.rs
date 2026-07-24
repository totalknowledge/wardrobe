use super::*;

impl Drawer {
    pub fn upsert_record(&mut self, record: Value) -> std::io::Result<Result<(), String>> {
        self.upsert_record_internal(record)
            .map(|result| result.map(|_| ()))
    }

    pub fn upsert_records_atomic(
        &mut self,
        records: Vec<Value>,
    ) -> std::io::Result<Result<(), String>> {
        if let Err(validation_error) = self.validate_bulk_upsert_records(&records)? {
            return Ok(Err(validation_error));
        }

        for record in records {
            match self.upsert_record_internal(record)? {
                Ok(_) => {}
                Err(validation_error) => return Ok(Err(validation_error)),
            }
        }

        self.flush_metadata_if_dirty()?;

        Ok(Ok(()))
    }

    pub(super) fn upsert_record_internal(
        &mut self,
        mut record: Value,
    ) -> std::io::Result<Result<bool, String>> {
        let primary_key_value = match record.get(&self.primary_key).and_then(|v| v.as_str()) {
            Some(val) => val.to_string(),
            None => {
                return Ok(Err(format!(
                    "Missing primary key field: {}",
                    self.primary_key
                )));
            }
        };

        let old_data_offset = self.primary_memory_index.get(&primary_key_value).copied();
        let is_new_record = old_data_offset.is_none();
        let old_record = if let Some(existing_offset) = old_data_offset {
            self.read_logical_record_at_offset(existing_offset)?
        } else {
            None
        };

        if self.timestamps_enabled {
            if let Some(map) = record.as_object_mut() {
                let now = current_iso8601_timestamp();
                if is_new_record || old_record.is_none() {
                    map.entry("created_at".to_string())
                        .or_insert_with(|| Value::String(now.clone()));
                    map.entry("updated_at".to_string())
                        .or_insert_with(|| Value::String(now));
                } else {
                    if !map.contains_key("created_at") {
                        if let Some(created_at) = old_record.as_ref().and_then(|r| r.get("created_at")) {
                            map.insert("created_at".to_string(), created_at.clone());
                        } else {
                            map.insert("created_at".to_string(), Value::String(now.clone()));
                        }
                    }
                    map.insert("updated_at".to_string(), Value::String(now));
                }
            }
        }

        if let Err(validation_error) = self.validate_schema(&record) {
            return Ok(Err(validation_error));
        }

        if let Some(validation_error) =
            self.validate_relationship_constraints(&record, &primary_key_value)?
        {
            return Ok(Err(validation_error));
        }

        let mut historical_tombstone_block: Option<(u64, DataBlockIndexEntry)> = None;
        if let Some(stale_offset) = old_data_offset {
            historical_tombstone_block =
                self.historical_block_entry(stale_offset, old_record.as_ref())?;
        }

        for unique_field in &self.unique_constraints {
            let unique_value = query::resolve_field_path(&record, unique_field);
            if let Some(field_value) = unique_value.and_then(Self::secondary_index_key) {
                if let Some(field_map) = self.secondary_memory_index.get(unique_field) {
                    if let Some(offsets) = field_map.get(&field_value) {
                        if offsets.iter().any(|&o| Some(o) != old_data_offset) {
                            return Ok(Err(format!(
                                "Unique constraint violation: Field '{}' with value '{}' already exists",
                                unique_field,
                                unique_value.unwrap_or(&Value::Null)
                            )));
                        }
                    }
                }
            }
        }

        self.ensure_field_tokens_for_record_write(&record)?;
        let stored_record = self.encode_record_for_storage(&record);
        let serialized_record =
            BsonBinaryFormat::serialize_native_record(&stored_record, &self.field_name_map)?;
        let raw_len = serialized_record.len();
        let target_size_class = self.data_recycler.calculate_aligned_size(raw_len);
        let live_block = DataBlockIndexEntry::live(&serialized_record, target_size_class);

        let data_offset = self.write_data_payload(&serialized_record, target_size_class)?;

        let primary_key_field_name = self.primary_key.clone();
        self.write_index_log(
            &primary_key_field_name,
            &primary_key_value,
            Value::from(data_offset),
            Some(live_block),
        )?;
        self.primary_memory_index
            .insert(primary_key_value.clone(), data_offset);
        self.data_block_index.insert(data_offset, live_block);

        let unique_fields = self.unique_constraints.clone();
        for indexed_field in unique_fields {
            self.update_secondary_index_for_record_change(
                &indexed_field,
                old_record.as_ref(),
                old_data_offset,
                Some(&record),
                data_offset,
            )?;
        }

        let materialized_query_fields = self.materialized_query_index_fields();
        for indexed_field in materialized_query_fields {
            self.update_secondary_index_for_record_change(
                &indexed_field,
                old_record.as_ref(),
                old_data_offset,
                Some(&record),
                data_offset,
            )?;
        }

        if let Some((stale_offset, old_block)) = historical_tombstone_block {
            if stale_offset != data_offset {
                self.data_writer
                    .write_tombstone_at_offset(stale_offset, old_block.size_class)?;
                self.data_block_index.remove(&stale_offset);
                self.data_recycler
                    .register_free_slot(old_block.size_class, stale_offset);
            }
        }

        if is_new_record {
            self.record_count += 1;
            self.mark_metadata_dirty();
        }

        Ok(Ok(is_new_record))
    }

    pub(super) fn update_secondary_index_for_record_change(
        &mut self,
        indexed_field: &str,
        old_record: Option<&Value>,
        old_data_offset: Option<u64>,
        new_record: Option<&Value>,
        new_data_offset: u64,
    ) -> std::io::Result<()> {
        if !self.secondary_memory_index.contains_key(indexed_field) {
            return Ok(());
        }

        let old_field_value = old_record
            .and_then(|value| query::resolve_field_path(value, indexed_field))
            .and_then(Self::secondary_index_key);
        let new_field_value = new_record
            .and_then(|value| query::resolve_field_path(value, indexed_field))
            .and_then(Self::secondary_index_key);
        let mut keys_to_write = Vec::new();

        {
            let field_map = self
                .secondary_memory_index
                .entry(indexed_field.to_string())
                .or_insert_with(BTreeMap::new);

            if let (Some(previous_value), Some(previous_offset)) =
                (old_field_value.as_deref(), old_data_offset)
            {
                if let Some(offsets) = field_map.get_mut(previous_value) {
                    offsets.retain(|offset| *offset != previous_offset);
                    if offsets.is_empty() {
                        field_map.remove(previous_value);
                    }
                }
                keys_to_write.push(previous_value.to_string());
            }

            if let Some(field_value) = new_field_value.as_deref() {
                let offsets = field_map.entry(field_value.to_string()).or_default();
                if !offsets.contains(&new_data_offset) {
                    offsets.push(new_data_offset);
                }
                keys_to_write.push(field_value.to_string());
            }
        }

        keys_to_write.sort();
        keys_to_write.dedup();

        for field_value in keys_to_write {
            let offsets = self
                .secondary_memory_index
                .get(indexed_field)
                .and_then(|field_map| field_map.get(&field_value))
                .cloned()
                .unwrap_or_default();
            self.write_index_log(
                indexed_field,
                &field_value,
                Self::offsets_index_value(&offsets),
                None,
            )?;
        }
        Ok(())
    }

    pub(super) fn validate_bulk_upsert_records(
        &self,
        records: &[Value],
    ) -> std::io::Result<Result<(), String>> {
        let mut batch_unique_values = HashMap::new();

        for record in records {
            let primary_key_value = match record.get(&self.primary_key).and_then(|v| v.as_str()) {
                Some(val) => val.to_string(),
                None => {
                    return Ok(Err(format!(
                        "Missing primary key field: {}",
                        self.primary_key
                    )));
                }
            };

            if let Err(validation_error) = self.validate_schema(record) {
                return Ok(Err(validation_error));
            }

            if let Some(validation_error) =
                self.validate_relationship_constraints(record, &primary_key_value)?
            {
                return Ok(Err(validation_error));
            }

            let old_data_offset = self.primary_memory_index.get(&primary_key_value).copied();
            for unique_field in &self.unique_constraints {
                let unique_value = query::resolve_field_path(record, unique_field);
                let Some(field_value) = unique_value.and_then(Self::secondary_index_key) else {
                    continue;
                };

                if let Some(field_map) = self.secondary_memory_index.get(unique_field) {
                    if let Some(offsets) = field_map.get(&field_value) {
                        if offsets.iter().any(|&o| Some(o) != old_data_offset) {
                            return Ok(Err(format!(
                                "Unique constraint violation: Field '{}' with value '{}' already exists",
                                unique_field,
                                unique_value.unwrap_or(&Value::Null)
                            )));
                        }
                    }
                }

                let batch_key = format!("{unique_field}\u{1f}{field_value}");
                if let Some(previous_primary_key) =
                    batch_unique_values.insert(batch_key, primary_key_value.clone())
                {
                    if previous_primary_key != primary_key_value {
                        return Ok(Err(format!(
                            "Unique constraint violation: Field '{}' with value '{}' already exists",
                            unique_field, field_value
                        )));
                    }
                }
            }
        }

        Ok(Ok(()))
    }
}

pub(crate) fn current_iso8601_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();

    let days = (secs / 86400) as i64;
    let rem_secs = (secs % 86400) as u32;
    let hours = rem_secs / 3600;
    let minutes = (rem_secs % 3600) / 60;
    let seconds = rem_secs % 60;

    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z")
}
