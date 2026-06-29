use super::*;

impl Drawer {
    pub fn migrate_all_records(&mut self) -> std::io::Result<VacuumReport> {
        let mut live_offsets = self
            .primary_memory_index
            .values()
            .copied()
            .collect::<Vec<_>>();
        live_offsets.sort_unstable();
        live_offsets.dedup();

        for offset in live_offsets {
            let _ = self.read_record_at_offset_with_lazy_migration(offset)?;
        }

        self.metadata_format_version = DRAWER_METADATA_FORMAT_VERSION;
        self.vacuum()
    }

    pub(super) fn read_record_at_offset_with_lazy_migration(
        &mut self,
        offset: u64,
    ) -> std::io::Result<Option<Value>> {
        let Some(record) = self
            .data_reader
            .read_record_at_offset(offset, Some(&self.field_name_map))?
        else {
            return Ok(None);
        };

        if !self.needs_format_migration() {
            return Ok(Some(self.decode_record_from_storage(record)));
        }

        let mut migrated_record = record.clone();
        if !self.migrate_legacy_record_value(&mut migrated_record) {
            return Ok(Some(self.decode_record_from_storage(record)));
        }

        self.write_migrated_record_at_offset(offset, &record, &migrated_record)?;
        Ok(Some(self.decode_record_from_storage(migrated_record)))
    }

    pub(super) fn write_migrated_record_at_offset(
        &mut self,
        offset: u64,
        old_record: &Value,
        migrated_record: &Value,
    ) -> std::io::Result<()> {
        let old_primary_key_value = old_record
            .get(&self.primary_key)
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "Cannot migrate legacy record without primary key field: {}",
                        self.primary_key
                    ),
                )
            })?;
        let new_primary_key_value = migrated_record
            .get(&self.primary_key)
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "Cannot migrate legacy record without primary key field: {}",
                        self.primary_key
                    ),
                )
            })?;

        let Some((_old_offset, old_block)) =
            self.historical_block_entry(offset, Some(old_record))?
        else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Cannot migrate legacy record because its block metadata is missing",
            ));
        };

        self.ensure_field_tokens_for_record_write(migrated_record)?;
        let stored_record = self.encode_record_for_storage(migrated_record);
        let serialized_record =
            BsonBinaryFormat::serialize_native_record(&stored_record, &self.field_name_map)?;
        let (resolved_offset, resolved_size_class) =
            if serialized_record.len() <= old_block.size_class {
                self.data_writer.overwrite_at_offset(
                    offset,
                    &serialized_record,
                    old_block.size_class,
                )?;
                (offset, old_block.size_class)
            } else {
                let target_size_class = self
                    .data_recycler
                    .calculate_aligned_size(serialized_record.len());
                let new_offset = self.write_data_payload(&serialized_record, target_size_class)?;
                self.data_writer
                    .write_tombstone_at_offset(offset, old_block.size_class)?;
                self.data_block_index.remove(&offset);
                self.data_recycler
                    .register_free_slot(old_block.size_class, offset);
                (new_offset, target_size_class)
            };
        let live_block = DataBlockIndexEntry::live(&serialized_record, resolved_size_class);

        if old_primary_key_value != new_primary_key_value {
            let primary_key_field_name = self.primary_key.clone();
            if Self::clean_legacy_identifier(old_primary_key_value) == new_primary_key_value {
                self.recycle_index_slot(&primary_key_field_name, old_primary_key_value)?;
            } else {
                self.write_index_log(
                    &primary_key_field_name,
                    old_primary_key_value,
                    Value::Array(Vec::new()),
                    None,
                )?;
            }
            self.primary_memory_index.remove(old_primary_key_value);
            self.primary_memory_index
                .remove(&Self::clean_legacy_identifier(old_primary_key_value));
        }

        self.write_index_log(
            &self.primary_key.clone(),
            new_primary_key_value,
            Value::from(resolved_offset),
            Some(live_block),
        )?;
        self.primary_memory_index
            .insert(new_primary_key_value.to_string(), resolved_offset);
        self.data_block_index.insert(resolved_offset, live_block);
        self.metadata_format_version = DRAWER_METADATA_FORMAT_VERSION;
        self.persist_metadata()
    }

    pub(super) fn needs_format_migration(&self) -> bool {
        self.metadata_format_version < DRAWER_METADATA_FORMAT_VERSION
    }

    pub(super) fn migrate_legacy_record_value(&self, record: &mut Value) -> bool {
        Self::migrate_legacy_value(record, Some(&self.primary_key))
    }

    pub(super) fn migrate_legacy_value(value: &mut Value, object_key: Option<&str>) -> bool {
        match value {
            Value::Object(map) => {
                let mut changed = false;
                for (key, child_value) in map.iter_mut() {
                    changed |= Self::migrate_legacy_value(child_value, Some(key.as_str()));
                }
                changed
            }
            Value::Array(values) => values
                .iter_mut()
                .any(|item| Self::migrate_legacy_value(item, None)),
            Value::String(string_value) => {
                let migrated_value = if object_key == Some("_id") {
                    Self::clean_legacy_identifier(string_value)
                } else if let Some((drawer_name, record_key)) =
                    Self::try_parse_legacy_pointer(string_value)
                {
                    Self::format_legacy_pointer(&drawer_name, &record_key)
                } else {
                    return false;
                };

                if migrated_value != *string_value {
                    *string_value = migrated_value;
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    pub(super) fn clean_legacy_identifier(value: &str) -> String {
        if let Some((_drawer_name, record_key)) = Self::try_parse_legacy_pointer(value) {
            return record_key;
        }

        value
            .trim_start_matches('@')
            .strip_prefix("lnk_")
            .unwrap_or_else(|| value.trim_start_matches('@'))
            .to_string()
    }

    pub(super) fn try_parse_legacy_pointer(value: &str) -> Option<(String, String)> {
        let clean_pointer = value.strip_prefix('@')?;
        let (drawer_name, record_key) = clean_pointer.split_once(':')?;
        if drawer_name.is_empty() || record_key.is_empty() || record_key.contains(':') {
            return None;
        }

        Some((
            drawer_name.to_string(),
            record_key
                .strip_prefix("lnk_")
                .unwrap_or(record_key)
                .to_string(),
        ))
    }

    pub(super) fn format_legacy_pointer(drawer_name: &str, record_key: &str) -> String {
        format!(
            "@{}:{}",
            drawer_name.trim_start_matches('@'),
            Self::clean_legacy_identifier(record_key)
        )
    }
}
