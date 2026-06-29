use super::*;

impl Drawer {
    pub fn vacuum(&mut self) -> std::io::Result<VacuumReport> {
        let data_bytes_before = self.data_writer.current_length()?;
        let index_bytes_before = self.index_writer.current_length()?;
        let mut live_offsets = self
            .primary_memory_index
            .iter()
            .map(|(key, offset)| (key.clone(), *offset))
            .collect::<Vec<_>>();
        live_offsets.sort_by_key(|(_, offset)| *offset);

        let mut live_records = Vec::new();
        for (_, offset) in live_offsets {
            if let Some(record) = self.read_logical_record_at_offset(offset)? {
                live_records.push(record);
            }
        }

        let mut field_map_changed = false;
        Self::ensure_reserved_id_field_mapping(&mut self.field_name_map);
        for record in &live_records {
            field_map_changed |= self.ensure_field_tokens_for_value(record);
        }
        if field_map_changed {
            self.persist_metadata()?;
        }

        let mut compact_data = Vec::new();
        let mut compact_index = Vec::new();
        let mut primary_memory_index = HashMap::new();
        let mut secondary_memory_index = HashMap::new();
        let mut index_file_offsets = HashMap::new();
        let mut data_block_index = HashMap::new();

        let mut indexed_fields = self.unique_constraints.clone();
        indexed_fields.extend(self.materialized_query_index_fields());
        indexed_fields.sort();
        indexed_fields.dedup();
        for field in &indexed_fields {
            secondary_memory_index.insert(field.clone(), BTreeMap::new());
        }

        for record in &live_records {
            let primary_key_value = record
                .get(&self.primary_key)
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Missing primary key field: {}", self.primary_key),
                    )
                })?;
            let stored_record = self.encode_record_for_storage(record);
            let serialized_record =
                BsonBinaryFormat::serialize_native_record(&stored_record, &self.field_name_map)?;
            let data_offset = Self::append_compact_payload(&mut compact_data, &serialized_record);
            let block_entry =
                DataBlockIndexEntry::live(&serialized_record, serialized_record.len());

            let primary_index_entry = Self::index_entry_value(
                &self.stored_field_name(&self.primary_key),
                primary_key_value,
                Value::from(data_offset),
                Some(block_entry),
            );
            let (index_offset, index_slot_size) = Self::append_compact_index_entry(
                &mut compact_index,
                &primary_index_entry,
                &self.field_name_map,
            )?;

            primary_memory_index.insert(primary_key_value.to_string(), data_offset);
            index_file_offsets.insert(
                format!("{}:{}", self.primary_key, primary_key_value),
                (index_offset, index_slot_size),
            );
            data_block_index.insert(data_offset, block_entry);

            for indexed_field in &indexed_fields {
                if let Some(field_value) = record
                    .get(indexed_field)
                    .and_then(Self::secondary_index_key)
                {
                    secondary_memory_index
                        .entry(indexed_field.clone())
                        .or_insert_with(BTreeMap::new)
                        .entry(field_value)
                        .or_insert_with(Vec::new)
                        .push(data_offset);
                }
            }
        }

        let mut compact_secondary_entries = secondary_memory_index
            .iter()
            .flat_map(|(field, field_map)| {
                field_map.iter().map(move |(field_value, offsets)| {
                    (field.clone(), field_value.clone(), offsets.clone())
                })
            })
            .collect::<Vec<_>>();
        compact_secondary_entries
            .sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

        for (field, field_value, offsets) in compact_secondary_entries {
            let secondary_index_entry = Self::index_entry_value(
                &self.stored_field_name(&field),
                &field_value,
                Self::offsets_index_value(&offsets),
                None,
            );
            let (index_offset, index_slot_size) = Self::append_compact_index_entry(
                &mut compact_index,
                &secondary_index_entry,
                &self.field_name_map,
            )?;

            index_file_offsets.insert(
                format!("{}:{}", field, field_value),
                (index_offset, index_slot_size),
            );
        }

        self.data_writer.rewrite_all(&compact_data)?;
        self.index_writer.rewrite_all(&compact_index)?;

        self.primary_memory_index = primary_memory_index;
        self.secondary_memory_index = secondary_memory_index;
        self.index_file_offsets = index_file_offsets;
        self.data_block_index = data_block_index;
        self.data_recycler = Recycler::new();
        self.data_recycler_cache_initialized = true;
        self.index_recycler = Recycler::new();
        self.record_count = self.primary_memory_index.len();
        self.persist_metadata()?;

        let data_bytes_after = compact_data.len() as u64;
        let index_bytes_after = compact_index.len() as u64;
        let total_before = data_bytes_before.saturating_add(index_bytes_before);
        let total_after = data_bytes_after.saturating_add(index_bytes_after);

        Ok(VacuumReport {
            records_rewritten: self.record_count,
            data_bytes_before,
            data_bytes_after,
            index_bytes_before,
            index_bytes_after,
            bytes_reclaimed: total_before.saturating_sub(total_after),
        })
    }
}
