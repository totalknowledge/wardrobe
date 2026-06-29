use super::*;
use std::ops::Bound;

#[derive(Debug, Clone)]
pub(super) enum SecondaryIndexPredicate {
    Equality(String),
    Range {
        lower: Option<(String, bool)>,
        upper: Option<(String, bool)>,
    },
}

impl Drawer {
    pub(crate) fn indexed_candidate_offsets(
        &mut self,
        filter_map: &Map<String, Value>,
    ) -> std::io::Result<Option<Vec<u64>>> {
        if filter_map.is_empty() {
            return Ok(None);
        }

        let mut materialized_for_future_query = false;
        for (field_name, expected_value) in filter_map {
            if Self::secondary_index_predicate(expected_value).is_none() {
                return Ok(None);
            }
            if !self.index_can_satisfy_filter(field_name) {
                return Ok(None);
            }
            if self
                .unique_constraints
                .iter()
                .any(|field| field == field_name)
            {
                if !self.secondary_memory_index.contains_key(field_name) {
                    let field_index = self.build_secondary_index(field_name, true)?;
                    self.secondary_memory_index
                        .insert(field_name.to_string(), field_index);
                }
                continue;
            }

            if !self.query_index_is_materialized(field_name)
                || !self.validated_secondary_indexes.contains(field_name)
            {
                if self.query_index_is_materialized(field_name)
                    && self.secondary_index_matches_authoritative(field_name)?
                {
                    self.validated_secondary_indexes
                        .insert(field_name.to_string());
                } else {
                    self.materialize_query_index(field_name)?;
                    materialized_for_future_query = true;
                }
            }
        }

        if materialized_for_future_query {
            return Ok(None);
        }

        let mut candidate_offsets: Option<Vec<u64>> = None;
        for (field_name, expected_value) in filter_map {
            let Some(field_map) = self.secondary_memory_index.get(field_name) else {
                return Ok(None);
            };
            let Some(predicate) = Self::secondary_index_predicate(expected_value) else {
                return Ok(None);
            };
            let mut offsets = Self::secondary_index_offsets_for_predicate(field_map, &predicate);
            offsets.sort_unstable();
            offsets.dedup();

            candidate_offsets = Some(match candidate_offsets {
                Some(existing_offsets) => Self::intersect_sorted_offsets(existing_offsets, offsets),
                None => offsets,
            });
        }

        Ok(candidate_offsets)
    }

    pub(super) fn build_secondary_index(
        &self,
        field_name: &str,
        enforce_unique: bool,
    ) -> std::io::Result<SecondaryIndex> {
        let mut field_index: SecondaryIndex = BTreeMap::new();
        for (primary_key, offset) in &self.primary_memory_index {
            let Some(record) = self.read_logical_record_at_offset(*offset)? else {
                continue;
            };
            let Some(field_value) = record.get(field_name).and_then(Self::secondary_index_key)
            else {
                continue;
            };

            if enforce_unique && field_index.contains_key(&field_value) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "Cannot add unique constraint for '{}' because value '{}' already exists",
                        field_name, field_value
                    ),
                ));
            }

            if record.get(&self.primary_key).and_then(Value::as_str) != Some(primary_key.as_str()) {
                continue;
            }

            field_index.entry(field_value).or_default().push(*offset);
        }

        Ok(field_index)
    }

    pub(super) fn write_secondary_index_snapshot(
        &mut self,
        field_name: &str,
        field_index: &SecondaryIndex,
    ) -> std::io::Result<()> {
        self.ensure_field_token_for_field_write(field_name)?;
        let field_prefix = format!("{field_name}:");
        let stale_field_values: Vec<String> = self
            .index_file_offsets
            .keys()
            .filter_map(|map_key| map_key.strip_prefix(&field_prefix))
            .filter(|field_value| !field_index.contains_key(*field_value))
            .map(ToOwned::to_owned)
            .collect();

        for field_value in stale_field_values {
            self.tombstone_index_slot(field_name, &field_value)?;
        }

        for (field_value, offsets) in field_index {
            self.write_index_log(
                field_name,
                field_value,
                Self::offsets_index_value(offsets),
                None,
            )?;
        }

        Ok(())
    }

    pub(super) fn index_can_satisfy_filter(&self, field_name: &str) -> bool {
        self.unique_constraints
            .iter()
            .any(|constraint| constraint == field_name)
            || self.schema_has_index(field_name)
    }

    pub(super) fn query_index_is_materialized(&self, field_name: &str) -> bool {
        self.materialized_secondary_indexes
            .get(field_name)
            .is_some_and(|generation| *generation == self.secondary_index_generation)
            && self.secondary_memory_index.contains_key(field_name)
    }

    pub(super) fn materialize_query_index(&mut self, field_name: &str) -> std::io::Result<()> {
        if !self.schema_has_index(field_name) {
            return Ok(());
        }

        let field_index = self.build_secondary_index(field_name, false)?;
        self.write_secondary_index_snapshot(field_name, &field_index)?;
        self.secondary_memory_index
            .insert(field_name.to_string(), field_index);
        self.materialized_secondary_indexes
            .insert(field_name.to_string(), self.secondary_index_generation);
        self.validated_secondary_indexes
            .insert(field_name.to_string());
        self.persist_metadata()
    }

    pub(super) fn secondary_index_matches_authoritative(
        &self,
        field_name: &str,
    ) -> std::io::Result<bool> {
        let expected_index = self.build_secondary_index(field_name, false)?;
        let Some(actual_index) = self.secondary_memory_index.get(field_name) else {
            return Ok(false);
        };

        Ok(Self::normalized_secondary_index(actual_index)
            == Self::normalized_secondary_index(&expected_index))
    }

    pub(super) fn normalized_secondary_index(
        field_index: &SecondaryIndex,
    ) -> BTreeMap<String, Vec<u64>> {
        field_index
            .iter()
            .map(|(field_value, offsets)| {
                let mut offsets = offsets.clone();
                offsets.sort_unstable();
                offsets.dedup();
                (field_value.clone(), offsets)
            })
            .collect()
    }

    pub(super) fn materialized_query_index_fields(&self) -> Vec<String> {
        self.materialized_secondary_indexes
            .keys()
            .filter(|field| {
                self.secondary_memory_index.contains_key(*field)
                    && !self
                        .unique_constraints
                        .iter()
                        .any(|unique| unique == *field)
            })
            .cloned()
            .collect()
    }

    pub(super) fn clear_unique_constraint(&mut self, field_name: &str) -> std::io::Result<()> {
        self.unique_constraints
            .retain(|constraint| constraint != field_name);

        if self.schema_has_index(field_name) {
            self.materialized_secondary_indexes
                .insert(field_name.to_string(), self.secondary_index_generation);
            return Ok(());
        }

        self.clear_secondary_index_entries(field_name)
    }

    pub(super) fn remove_query_index(&mut self, field_name: &str) -> std::io::Result<()> {
        if self
            .unique_constraints
            .iter()
            .any(|constraint| constraint == field_name)
        {
            return Ok(());
        }

        self.clear_secondary_index_entries(field_name)
    }

    pub(super) fn clear_secondary_index_entries(
        &mut self,
        field_name: &str,
    ) -> std::io::Result<()> {
        self.materialized_secondary_indexes.remove(field_name);
        self.validated_secondary_indexes.remove(field_name);
        if let Some(field_map) = self.secondary_memory_index.remove(field_name) {
            for field_value in field_map.keys() {
                self.tombstone_index_slot(field_name, field_value)?;
            }
        }

        Ok(())
    }

    pub(super) fn write_index_log(
        &mut self,
        field: &str,
        key: &str,
        offset_value: Value,
        block_entry: Option<DataBlockIndexEntry>,
    ) -> std::io::Result<()> {
        let map_key = format!("{}:{}", field, key);

        let index_entry = Self::index_entry_value(
            &self.stored_field_name(field),
            key,
            offset_value,
            block_entry,
        );

        let serialized_index =
            NativeBinaryIndexFormat::serialize_index_entry(&index_entry, &self.field_name_map)?;
        let entry_raw_len = serialized_index.len();
        let target_size_class = self.index_recycler.calculate_aligned_size(entry_raw_len);

        let new_index_offset = self.write_index_payload(&serialized_index, target_size_class)?;

        if let Some((old_index_offset, old_size_class)) =
            self.index_file_offsets.get(&map_key).copied()
        {
            if old_index_offset != new_index_offset {
                self.index_writer
                    .write_tombstone_at_offset(old_index_offset, old_size_class)?;
            }
        }

        self.index_file_offsets
            .insert(map_key, (new_index_offset, target_size_class));
        Ok(())
    }

    pub(super) fn write_index_payload(
        &mut self,
        serialized_index: &[u8],
        target_size_class: usize,
    ) -> std::io::Result<u64> {
        if let Some(recycled_offset) = self.index_recycler.pop_available_slot(target_size_class) {
            self.index_writer.overwrite_at_offset(
                recycled_offset,
                serialized_index,
                target_size_class,
            )?;
            Ok(recycled_offset)
        } else {
            self.index_writer
                .append_aligned_index(serialized_index, target_size_class)
        }
    }

    pub(super) fn recycle_index_slot(&mut self, field: &str, key: &str) -> std::io::Result<()> {
        self.tombstone_index_slot(field, key).map(|_| ())
    }

    pub(super) fn tombstone_index_slot(
        &mut self,
        field: &str,
        key: &str,
    ) -> std::io::Result<Option<(u64, usize)>> {
        let map_key = format!("{}:{}", field, key);
        if let Some((index_offset, size_class)) = self.index_file_offsets.remove(&map_key) {
            self.index_writer
                .write_tombstone_at_offset(index_offset, size_class)?;
            Ok(Some((index_offset, size_class)))
        } else {
            Ok(None)
        }
    }

    pub(super) fn secondary_index_key(value: &Value) -> Option<String> {
        match value {
            Value::Null => Some("0:".to_string()),
            Value::Bool(false) => Some("1:0".to_string()),
            Value::Bool(true) => Some("1:1".to_string()),
            Value::Number(number) => number
                .as_f64()
                .map(|number| format!("2:{:016x}", Self::sortable_f64_bits(number))),
            Value::String(value) => Some(format!("3:{value}")),
            _ => None,
        }
    }

    pub(super) fn secondary_index_key_is_encoded(key: &str) -> bool {
        key == "0:"
            || key == "1:0"
            || key == "1:1"
            || (key.len() == 18
                && key.starts_with("2:")
                && key[2..].chars().all(|ch| ch.is_ascii_hexdigit()))
            || key.starts_with("3:")
    }

    pub(super) fn primary_key_for_record(&self, record: &Value) -> std::io::Result<String> {
        record
            .get(&self.primary_key)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "record is missing a string {} for set-based delete",
                        self.primary_key
                    ),
                )
            })
    }

    pub(super) fn equality_filter_index_key(value: &Value) -> Option<String> {
        if let Value::String(value) = value {
            if value.contains('%') {
                return None;
            }
        }
        Self::secondary_index_key(value)
    }

    pub(super) fn secondary_index_predicate(value: &Value) -> Option<SecondaryIndexPredicate> {
        if let Value::Object(map) = value {
            if map.keys().any(|key| key.starts_with('$')) {
                return Self::range_filter_index_predicate(map);
            }
        }

        Self::equality_filter_index_key(value).map(SecondaryIndexPredicate::Equality)
    }

    pub(super) fn range_filter_index_predicate(
        map: &Map<String, Value>,
    ) -> Option<SecondaryIndexPredicate> {
        let mut lower: Option<(String, bool)> = None;
        let mut upper: Option<(String, bool)> = None;

        for (operator, value) in map {
            let key = Self::secondary_index_key(value)?;
            match operator.as_str() {
                "$gt" => lower = Some(Self::stronger_lower_bound(lower, (key, false))),
                "$gte" => lower = Some(Self::stronger_lower_bound(lower, (key, true))),
                "$lt" => upper = Some(Self::stronger_upper_bound(upper, (key, false))),
                "$lte" => upper = Some(Self::stronger_upper_bound(upper, (key, true))),
                "$eq" => return Some(SecondaryIndexPredicate::Equality(key)),
                _ => return None,
            }
        }

        if lower.is_none() && upper.is_none() {
            return None;
        }

        Some(SecondaryIndexPredicate::Range { lower, upper })
    }

    fn stronger_lower_bound(
        existing: Option<(String, bool)>,
        candidate: (String, bool),
    ) -> (String, bool) {
        match existing {
            Some(existing) if existing.0 > candidate.0 => existing,
            Some(existing) if existing.0 == candidate.0 => (existing.0, existing.1 && candidate.1),
            _ => candidate,
        }
    }

    fn stronger_upper_bound(
        existing: Option<(String, bool)>,
        candidate: (String, bool),
    ) -> (String, bool) {
        match existing {
            Some(existing) if existing.0 < candidate.0 => existing,
            Some(existing) if existing.0 == candidate.0 => (existing.0, existing.1 && candidate.1),
            _ => candidate,
        }
    }

    pub(super) fn secondary_index_offsets_for_predicate(
        field_map: &SecondaryIndex,
        predicate: &SecondaryIndexPredicate,
    ) -> Vec<u64> {
        match predicate {
            SecondaryIndexPredicate::Equality(index_key) => {
                field_map.get(index_key).cloned().unwrap_or_default()
            }
            SecondaryIndexPredicate::Range { lower, upper } => {
                if let (Some((lower_key, lower_inclusive)), Some((upper_key, upper_inclusive))) =
                    (lower, upper)
                {
                    if lower_key > upper_key
                        || (lower_key == upper_key && (!lower_inclusive || !upper_inclusive))
                    {
                        return Vec::new();
                    }
                }
                let lower_bound = match lower {
                    Some((key, true)) => Bound::Included(key.as_str()),
                    Some((key, false)) => Bound::Excluded(key.as_str()),
                    None => Bound::Unbounded,
                };
                let upper_bound = match upper {
                    Some((key, true)) => Bound::Included(key.as_str()),
                    Some((key, false)) => Bound::Excluded(key.as_str()),
                    None => Bound::Unbounded,
                };
                field_map
                    .range::<str, _>((lower_bound, upper_bound))
                    .flat_map(|(_, offsets)| offsets.iter().copied())
                    .collect()
            }
        }
    }

    fn sortable_f64_bits(value: f64) -> u64 {
        let bits = value.to_bits();
        if bits & (1 << 63) == 0 {
            bits ^ (1 << 63)
        } else {
            !bits
        }
    }

    pub(super) fn offsets_index_value(offsets: &[u64]) -> Value {
        Value::Array(offsets.iter().copied().map(Value::from).collect())
    }

    pub(super) fn intersect_sorted_offsets(left: Vec<u64>, right: Vec<u64>) -> Vec<u64> {
        let mut intersection = Vec::new();
        let mut left_index = 0usize;
        let mut right_index = 0usize;

        while left_index < left.len() && right_index < right.len() {
            match left[left_index].cmp(&right[right_index]) {
                std::cmp::Ordering::Equal => {
                    intersection.push(left[left_index]);
                    left_index += 1;
                    right_index += 1;
                }
                std::cmp::Ordering::Less => left_index += 1,
                std::cmp::Ordering::Greater => right_index += 1,
            }
        }

        intersection
    }

    pub(super) fn index_entry_value(
        field: &str,
        key: &str,
        offset_value: Value,
        block_entry: Option<DataBlockIndexEntry>,
    ) -> Value {
        let mut index_entry = Map::new();
        index_entry.insert(
            INDEX_FIELD_KEY.to_string(),
            Value::String(field.to_string()),
        );
        index_entry.insert(INDEX_VALUE_KEY.to_string(), Value::String(key.to_string()));
        index_entry.insert(INDEX_OFFSET_KEY.to_string(), offset_value);

        if let Some(block_entry) = block_entry {
            index_entry.insert(
                INDEX_LENGTH_KEY.to_string(),
                Value::from(block_entry.payload_len as u64),
            );
            index_entry.insert(
                INDEX_SIZE_CLASS_KEY.to_string(),
                Value::from(block_entry.size_class as u64),
            );
            index_entry.insert(
                INDEX_CRC_KEY.to_string(),
                Value::from(block_entry.crc as u64),
            );
            index_entry.insert(
                INDEX_STATUS_KEY.to_string(),
                Value::from(block_entry.status as u64),
            );
        }

        Value::Object(index_entry)
    }
}
