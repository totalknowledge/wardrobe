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
        record: Value,
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

        if let Err(validation_error) = self.validate_schema(&record) {
            return Ok(Err(validation_error));
        }

        let old_data_offset = self.primary_memory_index.get(&primary_key_value).copied();
        if let Some(validation_error) =
            self.validate_relationship_constraints(&record, &primary_key_value)?
        {
            return Ok(Err(validation_error));
        }

        let is_new_record = old_data_offset.is_none();
        let old_record = if let Some(existing_offset) = old_data_offset {
            self.read_logical_record_at_offset(existing_offset)?
        } else {
            None
        };

        let mut historical_tombstone_block: Option<(u64, DataBlockIndexEntry)> = None;
        if let Some(stale_offset) = old_data_offset {
            historical_tombstone_block =
                self.historical_block_entry(stale_offset, old_record.as_ref())?;
        }

        for unique_field in &self.unique_constraints {
            if let Some(field_value) = record.get(unique_field).and_then(|v| v.as_str()) {
                if let Some(field_map) = self.secondary_memory_index.get(unique_field) {
                    if let Some(offsets) = field_map.get(field_value) {
                        if offsets.iter().any(|&o| Some(o) != old_data_offset) {
                            return Ok(Err(format!(
                                "Unique constraint violation: Field '{}' with value '{}' already exists",
                                unique_field, field_value
                            )));
                        }
                    }
                }
            }
        }

        self.ensure_field_tokens_for_record_write(&record)?;
        let stored_record = self.encode_record_for_storage(&record);
        let serialized_record = BsonBinaryFormat::serialize_record(&stored_record)?;
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
            let old_field_value = old_record
                .as_ref()
                .and_then(|value| value.get(&indexed_field))
                .and_then(Self::secondary_index_key);
            let new_field_value = record
                .get(&indexed_field)
                .and_then(Self::secondary_index_key);
            let mut keys_to_write = Vec::new();

            {
                let field_map = self
                    .secondary_memory_index
                    .entry(indexed_field.clone())
                    .or_insert_with(HashMap::new);

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
                    if !offsets.contains(&data_offset) {
                        offsets.push(data_offset);
                    }
                    keys_to_write.push(field_value.to_string());
                }
            }

            keys_to_write.sort();
            keys_to_write.dedup();

            for field_value in keys_to_write {
                let offsets = self
                    .secondary_memory_index
                    .get(&indexed_field)
                    .and_then(|field_map| field_map.get(&field_value))
                    .cloned()
                    .unwrap_or_default();
                self.write_index_log(
                    &indexed_field,
                    &field_value,
                    Self::offsets_index_value(&offsets),
                    None,
                )?;
            }
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

        self.invalidate_materialized_query_indexes()?;

        Ok(Ok(is_new_record))
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
                let Some(field_value) = record.get(unique_field).and_then(Value::as_str) else {
                    continue;
                };

                if let Some(field_map) = self.secondary_memory_index.get(unique_field) {
                    if let Some(offsets) = field_map.get(field_value) {
                        if offsets.iter().any(|&o| Some(o) != old_data_offset) {
                            return Ok(Err(format!(
                                "Unique constraint violation: Field '{}' with value '{}' already exists",
                                unique_field, field_value
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
