use super::*;

impl Drawer {
    pub(super) fn read_logical_record_at_offset(
        &self,
        offset: u64,
    ) -> std::io::Result<Option<Value>> {
        self.data_reader
            .read_record_at_offset(offset, Some(&self.field_name_map))
            .map(|record| record.map(|record| self.decode_record_from_storage(record)))
    }

    pub fn find_by_primary_key(&self, key: &str) -> std::io::Result<Option<Value>> {
        if let Some(&offset) = self.primary_memory_index.get(key) {
            return self.read_logical_record_at_offset(offset);
        }
        Ok(None)
    }

    pub fn find_by_primary_key_with_migration(
        &mut self,
        key: &str,
    ) -> std::io::Result<Option<Value>> {
        if let Some(&offset) = self.primary_memory_index.get(key) {
            return self.read_record_at_offset_with_lazy_migration(offset);
        }
        Ok(None)
    }

    pub fn find_by_secondary_key(&mut self, field: &str, key: &str) -> std::io::Result<Vec<Value>> {
        if !self.index_can_satisfy_filter(field) {
            return Ok(Vec::new());
        }

        let mut index_keys = vec![
            Self::equality_filter_index_key(&Value::String(key.to_string()))
                .expect("plain string without wildcard should be indexable"),
        ];
        if let Ok(integer) = key.parse::<i64>() {
            if let Some(index_key) = Self::equality_filter_index_key(&Value::from(integer)) {
                index_keys.push(index_key);
            }
        }
        index_keys.sort();
        index_keys.dedup();

        let mut indexed_offsets = Vec::new();
        let mut used_index = false;
        for filter_value in Self::secondary_lookup_filter_values(key) {
            let mut filter_map = Map::new();
            filter_map.insert(field.to_string(), filter_value);
            if let Some(offsets) = self.indexed_candidate_offsets(&filter_map)? {
                used_index = true;
                indexed_offsets.extend(offsets);
            }
        }
        if used_index {
            indexed_offsets.sort_unstable();
            indexed_offsets.dedup();
            return self.records_at_offsets_with_migration(indexed_offsets);
        }

        let mut matching_records = Vec::new();
        for record in self.find_all_records_with_migration()? {
            if record
                .get(field)
                .and_then(Self::secondary_index_key)
                .is_some_and(|index_key| index_keys.contains(&index_key))
            {
                matching_records.push(record);
            }
        }
        Ok(matching_records)
    }

    fn secondary_lookup_filter_values(key: &str) -> Vec<Value> {
        let mut values = vec![Value::String(key.to_string())];
        if let Ok(integer) = key.parse::<i64>() {
            values.push(Value::from(integer));
        }
        values
    }

    pub(crate) fn records_at_offsets_with_migration<I>(
        &mut self,
        offsets: I,
    ) -> std::io::Result<Vec<Value>>
    where
        I: IntoIterator<Item = u64>,
    {
        let offset_vec: Vec<u64> = offsets.into_iter().collect();
        if offset_vec.is_empty() {
            return Ok(Vec::new());
        }

        if !self.needs_format_migration() {
            let raw_records = self
                .data_reader
                .read_records_at_offsets(offset_vec, Some(&self.field_name_map))?;
            return Ok(raw_records
                .into_iter()
                .map(|record| self.decode_record_from_storage(record))
                .collect());
        }

        let mut records = Vec::new();
        for offset in offset_vec {
            if let Some(record) = self.read_record_at_offset_with_lazy_migration(offset)? {
                records.push(record);
            }
        }
        Ok(records)
    }

    pub(crate) fn records_matching_filter_candidates(
        &mut self,
        filter_map: &Map<String, Value>,
        drawer_namespace: Option<&str>,
    ) -> std::io::Result<Vec<Value>> {
        let mut records = if let Some(offsets) = self.indexed_candidate_offsets(filter_map)? {
            self.records_at_offsets_with_migration(offsets)?
        } else {
            self.find_all_records_with_migration()?
        };

        records.retain(|record| query::record_matches_filter(record, filter_map, drawer_namespace));
        Ok(records)
    }

    pub(super) fn sorted_live_primary_offsets(&self) -> Vec<u64> {
        let mut live_offsets = self
            .primary_memory_index
            .values()
            .copied()
            .collect::<Vec<_>>();
        live_offsets.sort_unstable();
        live_offsets.dedup();
        live_offsets
    }

    pub(super) fn should_read_by_primary_offsets(&self) -> std::io::Result<bool> {
        if self.primary_memory_index.is_empty() || self.data_block_index.is_empty() {
            return Ok(false);
        }

        let live_bytes = self
            .data_block_index
            .values()
            .map(|block| block.size_class as u64)
            .sum::<u64>();
        if live_bytes == 0 {
            return Ok(false);
        }

        let total_bytes = self.data_writer.current_length()?;
        let dead_bytes = total_bytes.saturating_sub(live_bytes);
        Ok(dead_bytes > 65_536 && dead_bytes > live_bytes / 2)
    }

    pub(super) fn find_all_records_by_streaming_live_offsets(&self) -> std::io::Result<Vec<Value>> {
        if self.primary_memory_index.is_empty() {
            return Ok(Vec::new());
        }

        let live_offsets = self
            .primary_memory_index
            .values()
            .copied()
            .collect::<HashSet<_>>();
        let mut raw_slots = Vec::with_capacity(live_offsets.len());
        self.data_reader
            .stream_with_offsets(|offset, line_content| {
                if live_offsets.contains(&offset) {
                    raw_slots.push(line_content.to_vec());
                }
            })?;

        let mut live_records = Vec::with_capacity(raw_slots.len());
        for slot in raw_slots {
            if let Some(record_value) =
                BsonBinaryFormat::deserialize_record_with_map(&slot, Some(&self.field_name_map))?
            {
                live_records.push(self.decode_record_from_storage(record_value));
            }
        }

        Ok(live_records)
    }

    pub fn find_all_records(&self) -> std::io::Result<Vec<Value>> {
        if self.should_read_by_primary_offsets()? {
            let records = self.data_reader.read_records_at_offsets(
                self.sorted_live_primary_offsets(),
                Some(&self.field_name_map),
            )?;
            return Ok(records
                .into_iter()
                .map(|record| self.decode_record_from_storage(record))
                .collect());
        }

        self.find_all_records_by_streaming_live_offsets()
    }

    pub fn find_all_records_with_migration(&mut self) -> std::io::Result<Vec<Value>> {
        #[cfg(test)]
        {
            self.test_metrics.find_all_records_with_migration_calls += 1;
        }

        if !self.needs_format_migration() {
            return self.find_all_records();
        }

        let mut live_records = Vec::new();
        for offset in self.sorted_live_primary_offsets() {
            if let Some(record) = self.read_record_at_offset_with_lazy_migration(offset)? {
                live_records.push(record);
            }
        }

        Ok(live_records)
    }
}
