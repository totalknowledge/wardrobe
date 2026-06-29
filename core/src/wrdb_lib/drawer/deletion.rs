use super::*;

impl Drawer {
    pub fn delete_by_primary_key(&mut self, key: &str) -> std::io::Result<Option<Value>> {
        self.delete_primary_key_without_lifecycle_side_effects(key)
    }

    pub fn delete_by_primary_keys_set_based<I>(&mut self, keys: I) -> std::io::Result<usize>
    where
        I: IntoIterator<Item = String>,
    {
        let mut candidates = Vec::new();
        let mut seen_keys = HashSet::new();

        for key in keys {
            if !seen_keys.insert(key.clone()) {
                continue;
            }

            let Some(offset) = self.primary_memory_index.get(&key).copied() else {
                continue;
            };
            let Some(record) = self.read_logical_record_at_offset(offset)? else {
                self.primary_memory_index.remove(&key);
                continue;
            };
            candidates.push(DeleteCandidate {
                key,
                offset,
                record,
            });
        }

        self.delete_candidates_set_based(candidates)
    }

    pub fn delete_by_filter_set_based(
        &mut self,
        filter_map: &Map<String, Value>,
        drawer_namespace: Option<&str>,
    ) -> std::io::Result<usize> {
        let candidates = self.delete_candidates_matching_filter(filter_map, drawer_namespace)?;
        self.delete_candidates_set_based(candidates)
    }

    pub(super) fn delete_candidates_matching_filter(
        &mut self,
        filter_map: &Map<String, Value>,
        drawer_namespace: Option<&str>,
    ) -> std::io::Result<Vec<DeleteCandidate>> {
        if let Some(offsets) = self.indexed_candidate_offsets(filter_map)? {
            return self.delete_candidates_from_indexed_offsets(
                offsets,
                filter_map,
                drawer_namespace,
            );
        }

        let mut candidates = Vec::new();
        for record in self.find_all_records_with_migration()? {
            if !query::record_matches_filter(&record, filter_map, drawer_namespace) {
                continue;
            }

            let key = self.primary_key_for_record(&record)?;
            if let Some(offset) = self.primary_memory_index.get(&key).copied() {
                candidates.push(DeleteCandidate {
                    key,
                    offset,
                    record,
                });
            }
        }

        Ok(candidates)
    }

    pub(super) fn delete_candidates_from_indexed_offsets<I>(
        &mut self,
        offsets: I,
        filter_map: &Map<String, Value>,
        drawer_namespace: Option<&str>,
    ) -> std::io::Result<Vec<DeleteCandidate>>
    where
        I: IntoIterator<Item = u64>,
    {
        let mut offsets = offsets.into_iter().collect::<Vec<_>>();
        offsets.sort_unstable();
        offsets.dedup();

        let mut candidates = Vec::new();
        if !self.needs_format_migration() {
            let raw_records = self.data_reader.read_records_at_offsets(offsets.clone(), Some(&self.field_name_map))?;
            if raw_records.len() == offsets.len() {
                for (offset, raw_record) in offsets.into_iter().zip(raw_records) {
                    let record = self.decode_record_from_storage(raw_record);
                    if !query::record_matches_filter(&record, filter_map, drawer_namespace) {
                        continue;
                    }

                    let key = self.primary_key_for_record(&record)?;
                    if self.primary_memory_index.get(&key).copied() == Some(offset) {
                        candidates.push(DeleteCandidate {
                            key,
                            offset,
                            record,
                        });
                    }
                }
                return Ok(candidates);
            }
        }

        for offset in offsets {
            let Some(record) = self.read_record_at_offset_with_lazy_migration(offset)? else {
                continue;
            };
            if !query::record_matches_filter(&record, filter_map, drawer_namespace) {
                continue;
            }

            let key = self.primary_key_for_record(&record)?;
            if self.primary_memory_index.get(&key).copied() == Some(offset) {
                candidates.push(DeleteCandidate {
                    key,
                    offset,
                    record,
                });
            }
        }

        Ok(candidates)
    }

    pub(super) fn delete_candidates_set_based<I>(&mut self, candidates: I) -> std::io::Result<usize>
    where
        I: IntoIterator<Item = DeleteCandidate>,
    {
        let mut deleted_count = 0usize;
        let mut seen_keys = HashSet::new();
        let mut seen_offsets = HashSet::new();
        let mut deleted_records_info = Vec::new();

        for candidate in candidates {
            if self.primary_memory_index.get(&candidate.key).copied() != Some(candidate.offset) {
                continue;
            }
            if !seen_keys.insert(candidate.key.clone()) {
                continue;
            }
            if !seen_offsets.insert(candidate.offset) {
                continue;
            }

            if self
                .delete_known_primary_key_without_lifecycle_side_effects(
                    &candidate.key,
                    candidate.offset,
                    candidate.record.clone(),
                    false, // mark_metadata_dirty
                    false, // update_secondary_indexes
                )?
                .is_some()
            {
                deleted_count += 1;
                deleted_records_info.push((candidate.record, candidate.offset));
            }
        }

        if deleted_count > 0 {
            self.batch_update_secondary_indexes_for_deletions(&deleted_records_info)?;
            self.mark_metadata_dirty();
            self.flush_metadata_if_dirty()?;
        }

        Ok(deleted_count)
    }

    pub(super) fn batch_update_secondary_indexes_for_deletions(
        &mut self,
        deleted_records: &[(Value, u64)],
    ) -> std::io::Result<()> {
        if deleted_records.is_empty() {
            return Ok(());
        }

        let mut fields_to_update = self.unique_constraints.clone();
        fields_to_update.extend(self.materialized_query_index_fields());
        fields_to_update.sort();
        fields_to_update.dedup();

        for indexed_field in fields_to_update {
            if !self.secondary_memory_index.contains_key(&indexed_field) {
                continue;
            }

            let mut deletions_by_value: HashMap<String, Vec<u64>> = HashMap::new();
            for (record, offset) in deleted_records {
                if let Some(field_value) = record
                    .get(&indexed_field)
                    .and_then(Self::secondary_index_key)
                {
                    deletions_by_value.entry(field_value).or_default().push(*offset);
                }
            }

            for (field_value, stale_offsets) in deletions_by_value {
                let mut remaining_offsets = Vec::new();
                let mut old_existed = false;

                if let Some(field_map) = self.secondary_memory_index.get_mut(&indexed_field) {
                    if let Some(offsets) = field_map.get_mut(&field_value) {
                        old_existed = true;
                        offsets.retain(|offset| !stale_offsets.contains(offset));
                        remaining_offsets = offsets.clone();
                        if offsets.is_empty() {
                            field_map.remove(&field_value);
                        }
                    }
                }

                if old_existed {
                    if remaining_offsets.is_empty() {
                        self.tombstone_index_slot(&indexed_field, &field_value)?;
                    } else {
                        self.write_index_log(
                            &indexed_field,
                            &field_value,
                            Self::offsets_index_value(&remaining_offsets),
                            None,
                        )?;
                    }
                }
            }
        }

        Ok(())
    }

    pub(super) fn delete_primary_key_without_lifecycle_side_effects(
        &mut self,
        key: &str,
    ) -> std::io::Result<Option<Value>> {
        let Some(stale_offset) = self.primary_memory_index.get(key).copied() else {
            return Ok(None);
        };

        let Some(deleted_record) = self.read_logical_record_at_offset(stale_offset)? else {
            self.primary_memory_index.remove(key);
            return Ok(None);
        };

        self.delete_known_primary_key_without_lifecycle_side_effects(
            key,
            stale_offset,
            deleted_record,
            true, // mark_metadata_dirty
            true, // update_secondary_indexes
        )
    }

    pub(super) fn delete_known_primary_key_without_lifecycle_side_effects(
        &mut self,
        key: &str,
        stale_offset: u64,
        deleted_record: Value,
        mark_metadata_dirty: bool,
        update_secondary_indexes: bool,
    ) -> std::io::Result<Option<Value>> {
        let Some((_stale_offset, old_block)) =
            self.historical_block_entry(stale_offset, Some(&deleted_record))?
        else {
            self.primary_memory_index.remove(key);
            return Ok(Some(deleted_record));
        };

        self.data_writer
            .write_tombstone_at_offset(stale_offset, old_block.size_class)?;
        let primary_key_field_name = self.primary_key.clone();
        self.tombstone_index_slot(&primary_key_field_name, key)?;

        self.primary_memory_index.remove(key);
        self.data_block_index.remove(&stale_offset);
        self.data_recycler
            .register_free_slot(old_block.size_class, stale_offset);
        self.record_count = self.record_count.saturating_sub(1);
        if mark_metadata_dirty {
            self.mark_metadata_dirty();
        }

        if update_secondary_indexes {
            let fields_to_clear = self.unique_constraints.clone();
            for indexed_field in fields_to_clear {
                if let Some(field_value) = deleted_record
                    .get(&indexed_field)
                    .and_then(Self::secondary_index_key)
                {
                    if let Some(field_map) = self.secondary_memory_index.get_mut(&indexed_field) {
                        if let Some(offsets) = field_map.get_mut(&field_value) {
                            offsets.retain(|offset| *offset != stale_offset);
                            if offsets.is_empty() {
                                field_map.remove(&field_value);
                            }
                        }
                    }

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
            for indexed_field in self.materialized_query_index_fields() {
                self.update_secondary_index_for_record_change(
                    &indexed_field,
                    Some(&deleted_record),
                    Some(stale_offset),
                    None,
                    stale_offset,
                )?;
            }
        }
        Ok(Some(deleted_record))
    }

    pub fn delete_rules(&self) -> BTreeMap<String, Value> {
        self.delete_rules.clone()
    }

    pub(super) fn delete_rule_is_cascade(rule: &Value) -> bool {
        if rule.as_str().is_some_and(|action| action == "Cascade") {
            return true;
        }

        rule.get("action")
            .and_then(|action| action.as_str())
            .is_some_and(|action| action == "Cascade")
    }
}
