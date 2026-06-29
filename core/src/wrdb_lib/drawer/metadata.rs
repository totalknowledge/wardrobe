use super::*;

impl Drawer {
    pub(super) fn ensure_reserved_id_field_mapping(field_name_map: &mut BTreeMap<String, String>) {
        field_name_map
            .entry(RESERVED_ID_FIELD_TOKEN.to_string())
            .or_insert_with(|| RESERVED_ID_FIELD_NAME.to_string());
    }

    pub(super) fn reserve_legacy_compact_field_names(
        data_reader: &DatabaseReader,
        field_name_map: &mut BTreeMap<String, String>,
    ) -> std::io::Result<()> {
        Self::ensure_reserved_id_field_mapping(field_name_map);

        let mut raw_slots = Vec::new();
        if data_reader
            .stream_with_offsets(|_, line| {
                if !BsonBinaryFormat::is_tombstone(line) {
                    raw_slots.push(line.to_vec());
                }
            })
            .is_err()
        {
            return Ok(());
        }

        for slot in raw_slots {
            if let Ok(Some(record)) = BsonBinaryFormat::deserialize_record(&slot) {
                Self::reserve_legacy_compact_field_names_in_value(&record, field_name_map);
            }
        }

        Ok(())
    }

    pub(super) fn reserve_legacy_compact_field_names_in_value(
        value: &Value,
        field_name_map: &mut BTreeMap<String, String>,
    ) {
        match value {
            Value::Object(map) => {
                for (field_name, field_value) in map {
                    if Self::is_initial_compact_field_token(field_name)
                        && field_name != RESERVED_ID_FIELD_TOKEN
                    {
                        field_name_map
                            .entry(field_name.clone())
                            .or_insert_with(|| field_name.clone());
                    }
                    Self::reserve_legacy_compact_field_names_in_value(field_value, field_name_map);
                }
            }
            Value::Array(values) => {
                for value in values {
                    Self::reserve_legacy_compact_field_names_in_value(value, field_name_map);
                }
            }
            _ => {}
        }
    }

    pub(super) fn is_initial_compact_field_token(field_name: &str) -> bool {
        field_name.len() == 1
            && field_name
                .as_bytes()
                .first()
                .is_some_and(|byte| FIELD_TOKEN_ALPHABET.contains(byte))
    }

    pub(super) fn decode_field_name_from_map(
        field_name_map: &BTreeMap<String, String>,
        stored_field_name: &str,
    ) -> String {
        field_name_map
            .get(stored_field_name)
            .cloned()
            .unwrap_or_else(|| stored_field_name.to_string())
    }

    pub(super) fn field_token_for_logical_name(&self, logical_field_name: &str) -> Option<&str> {
        self.field_name_map
            .iter()
            .find_map(|(token, mapped_field_name)| {
                (mapped_field_name == logical_field_name).then_some(token.as_str())
            })
    }

    pub(super) fn stored_field_name(&self, logical_field_name: &str) -> String {
        self.field_token_for_logical_name(logical_field_name)
            .unwrap_or(logical_field_name)
            .to_string()
    }

    pub(super) fn ensure_field_token(&mut self, logical_field_name: &str) -> bool {
        if self
            .field_token_for_logical_name(logical_field_name)
            .is_some()
        {
            return false;
        }

        let token = if logical_field_name == RESERVED_ID_FIELD_NAME
            && !self.field_name_map.contains_key(RESERVED_ID_FIELD_TOKEN)
        {
            RESERVED_ID_FIELD_TOKEN.to_string()
        } else {
            self.next_available_field_token()
        };
        self.field_name_map
            .insert(token, logical_field_name.to_string());
        true
    }

    pub(super) fn ensure_field_tokens_for_value(&mut self, value: &Value) -> bool {
        match value {
            Value::Object(map) => {
                let mut changed = false;
                for (field_name, field_value) in map {
                    changed |= self.ensure_field_token(field_name);
                    changed |= self.ensure_field_tokens_for_value(field_value);
                }
                changed
            }
            Value::Array(values) => values.iter().fold(false, |changed, value| {
                self.ensure_field_tokens_for_value(value) || changed
            }),
            _ => false,
        }
    }

    pub(super) fn ensure_field_tokens_for_record_write(
        &mut self,
        record: &Value,
    ) -> std::io::Result<()> {
        Self::ensure_reserved_id_field_mapping(&mut self.field_name_map);
        if self.ensure_field_tokens_for_value(record) {
            self.persist_metadata()?;
        }
        Ok(())
    }

    pub(super) fn ensure_field_token_for_field_write(
        &mut self,
        field_name: &str,
    ) -> std::io::Result<()> {
        Self::ensure_reserved_id_field_mapping(&mut self.field_name_map);
        if self.ensure_field_token(field_name) {
            self.persist_metadata()?;
        }
        Ok(())
    }

    pub(super) fn next_available_field_token(&self) -> String {
        let base = FIELD_TOKEN_ALPHABET.len();
        let mut width = 1usize;

        loop {
            let capacity = base.pow(width as u32);
            for ordinal in 0..capacity {
                let token = Self::field_token_for_ordinal(ordinal, width);
                if !self.field_name_map.contains_key(&token) {
                    return token;
                }
            }
            width += 1;
        }
    }

    pub(super) fn field_token_for_ordinal(mut ordinal: usize, width: usize) -> String {
        let base = FIELD_TOKEN_ALPHABET.len();
        let mut token = vec![FIELD_TOKEN_ALPHABET[0]; width];

        for position in (0..width).rev() {
            token[position] = FIELD_TOKEN_ALPHABET[ordinal % base];
            ordinal /= base;
        }

        String::from_utf8(token).expect("field token alphabet must be valid utf-8")
    }

    pub(super) fn encode_record_for_storage(&self, record: &Value) -> Value {
        Self::encode_value_for_storage(record, &self.field_name_map)
    }

    pub(super) fn encode_value_for_storage(
        value: &Value,
        field_name_map: &BTreeMap<String, String>,
    ) -> Value {
        match value {
            Value::Object(map) => {
                let encoded = map
                    .iter()
                    .map(|(field_name, field_value)| {
                        let stored_field_name = field_name_map
                            .iter()
                            .find_map(|(token, logical_name)| {
                                (logical_name == field_name).then_some(token.clone())
                            })
                            .unwrap_or_else(|| field_name.clone());
                        (
                            stored_field_name,
                            Self::encode_value_for_storage(field_value, field_name_map),
                        )
                    })
                    .collect::<Map<String, Value>>();
                Value::Object(encoded)
            }
            Value::Array(values) => Value::Array(
                values
                    .iter()
                    .map(|value| Self::encode_value_for_storage(value, field_name_map))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    pub(super) fn decode_record_from_storage(&self, record: Value) -> Value {
        Self::decode_value_from_storage(record, &self.field_name_map)
    }

    pub(super) fn decode_value_from_storage(
        value: Value,
        field_name_map: &BTreeMap<String, String>,
    ) -> Value {
        match value {
            Value::Object(map) => {
                let decoded = map
                    .into_iter()
                    .map(|(stored_field_name, field_value)| {
                        (
                            Self::decode_field_name_from_map(field_name_map, &stored_field_name),
                            Self::decode_value_from_storage(field_value, field_name_map),
                        )
                    })
                    .collect::<Map<String, Value>>();
                Value::Object(decoded)
            }
            Value::Array(values) => Value::Array(
                values
                    .into_iter()
                    .map(|value| Self::decode_value_from_storage(value, field_name_map))
                    .collect(),
            ),
            other => other,
        }
    }

    fn secondary_index_key_from_loaded_entry(
        data_reader: &DatabaseReader,
        field_name_map: &BTreeMap<String, String>,
        field: &str,
        stored_key: &str,
        offset_value: &Value,
    ) -> std::io::Result<String> {
        if Self::secondary_index_key_is_encoded(stored_key) {
            return Ok(stored_key.to_string());
        }

        let candidate_offset = offset_value
            .as_u64()
            .or_else(|| offset_value.as_array()?.iter().find_map(Value::as_u64));
        if let Some(offset) = candidate_offset {
            if let Some(record) = data_reader.read_record_at_offset(offset, Some(field_name_map))? {
                let decoded = Self::decode_value_from_storage(record, field_name_map);
                if let Some(index_key) = decoded.get(field).and_then(Self::secondary_index_key) {
                    return Ok(index_key);
                }
            }
        }

        Ok(
            Self::secondary_index_key(&Value::String(stored_key.to_string()))
                .unwrap_or_else(|| stored_key.to_string()),
        )
    }

    pub fn open<P: AsRef<Path>>(
        directory: P,
        name: &str,
        primary_key: &str,
        unique_constraints: Vec<String>,
    ) -> std::io::Result<Self> {
        let base_path = directory.as_ref().to_path_buf();

        let data_file_path = base_path.join(format!("{}.drw", name));
        let index_file_path = base_path.join(format!("{}_index.drw", name));
        let meta_file_path = base_path.join(format!("{}_meta.drw", name));

        let data_writer = DatabaseWriter::open_drawer(&data_file_path)?;
        let data_reader = DatabaseReader::open_drawer(&data_file_path)?;
        let mut index_writer = DatabaseWriter::open_drawer(&index_file_path)?;
        let index_reader = DatabaseReader::open_drawer(&index_file_path)?;
        let metadata_writer = DatabaseWriter::open_drawer(&meta_file_path)?;

        let data_recycler = Recycler::new();
        let mut index_recycler = Recycler::new();

        let mut primary_memory_index = HashMap::new();
        let mut secondary_memory_index = HashMap::new();
        let mut index_file_offsets = HashMap::new();
        let mut data_block_journal = HashMap::new();
        let existing_metadata = DrawerMetadata::load(&meta_file_path)?;
        let relationship_constraints = existing_metadata
            .as_ref()
            .map(|metadata| metadata.relationship_constraints.clone())
            .unwrap_or_default();
        let delete_rules = existing_metadata
            .as_ref()
            .map(|metadata| metadata.delete_rules.clone())
            .unwrap_or_default();
        let cascade_delete_rules = existing_metadata
            .as_ref()
            .map(|metadata| metadata.cascade_delete_rules.clone())
            .unwrap_or_default();
        let schema = existing_metadata
            .as_ref()
            .and_then(|metadata| metadata.schema.clone());
        let secondary_index_generation = existing_metadata
            .as_ref()
            .map(|metadata| metadata.secondary_index_generation)
            .unwrap_or_default();
        let materialized_secondary_indexes = existing_metadata
            .as_ref()
            .map(|metadata| metadata.materialized_secondary_indexes.clone())
            .unwrap_or_default();
        let mut field_name_map = existing_metadata
            .as_ref()
            .map(|metadata| metadata.field_name_map.clone())
            .unwrap_or_default();
        let field_name_map_was_empty = field_name_map.is_empty();
        Self::ensure_reserved_id_field_mapping(&mut field_name_map);
        if field_name_map_was_empty {
            Self::reserve_legacy_compact_field_names(&data_reader, &mut field_name_map)?;
        }
        let metadata_format_version = existing_metadata
            .as_ref()
            .map(|metadata| metadata.format_version)
            .unwrap_or(DRAWER_METADATA_FORMAT_VERSION);
        let drawer_needs_format_migration =
            existing_metadata.is_some() && metadata_format_version < DRAWER_METADATA_FORMAT_VERSION;
        let inferred_unique_constraints = if unique_constraints.is_empty() {
            existing_metadata
                .as_ref()
                .map(|metadata| metadata.unique_constraints.clone())
                .unwrap_or_default()
        } else {
            unique_constraints
        };

        for field in &inferred_unique_constraints {
            secondary_memory_index.insert(field.clone(), BTreeMap::new());
        }
        for field in Self::schema_extension_fields(schema.as_ref(), "indexes") {
            if materialized_secondary_indexes
                .get(&field)
                .is_some_and(|generation| *generation == secondary_index_generation)
            {
                secondary_memory_index.insert(field.clone(), BTreeMap::new());
            }
        }

        let mut index_entries = Vec::new();
        index_reader.stream_with_offsets(|offset, line| {
            let is_dead =
                BsonBinaryFormat::is_tombstone(line) || NativeBinaryIndexFormat::is_tombstone(line);
            index_entries.push((offset, is_dead, line.to_vec()));
        })?;

        let total_index_file_len = index_writer.current_length()?;
        for i in 0..index_entries.len() {
            let (current_offset, is_dead, ref line_content) = index_entries[i];
            let next_offset = if i + 1 < index_entries.len() {
                index_entries[i + 1].0
            } else {
                total_index_file_len
            };
            let actual_slot_size = (next_offset - current_offset) as usize;

            if is_dead {
                index_recycler.register_free_slot(actual_slot_size, current_offset);
            } else {
                let index_entry_opt = if BsonBinaryFormat::is_binary_frame(line_content) {
                    BsonBinaryFormat::deserialize_record(line_content)?
                } else if NativeBinaryIndexFormat::is_binary_frame(line_content) {
                    NativeBinaryIndexFormat::deserialize_index_entry(line_content, &field_name_map)?
                } else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Unknown index frame magic",
                    ));
                };

                if let Some(index_entry) = index_entry_opt {
                    if let Some((data_offset, block_entry)) =
                        DataBlockIndexEntry::from_index_entry(&index_entry)
                    {
                        data_block_journal.insert(data_offset, block_entry);
                    }

                    if index_entry
                        .get(INDEX_STATUS_KEY)
                        .and_then(|value| value.as_u64())
                        == Some(DATA_BLOCK_STATUS_DEAD as u64)
                    {
                        if let (Some(stored_field), Some(key), Some(data_offset)) = (
                            index_entry
                                .get(INDEX_FIELD_KEY)
                                .and_then(|value| value.as_str()),
                            index_entry
                                .get(INDEX_VALUE_KEY)
                                .and_then(|value| value.as_str()),
                            index_entry
                                .get(INDEX_OFFSET_KEY)
                                .and_then(|value| value.as_u64()),
                        ) {
                            let field =
                                Self::decode_field_name_from_map(&field_name_map, stored_field);
                            if field == primary_key
                                && primary_memory_index.get(key).copied() == Some(data_offset)
                            {
                                primary_memory_index.remove(key);
                            }
                        }
                        index_writer.write_tombstone_at_offset(current_offset, actual_slot_size)?;
                        continue;
                    }

                    if let (Some(stored_field), Some(key), Some(data_offset_val)) = (
                        index_entry.get(INDEX_FIELD_KEY).and_then(|v| v.as_str()),
                        index_entry.get(INDEX_VALUE_KEY).and_then(|v| v.as_str()),
                        index_entry.get(INDEX_OFFSET_KEY),
                    ) {
                        let field = Self::decode_field_name_from_map(&field_name_map, stored_field);
                        let loaded_key = if field == primary_key {
                            key.to_string()
                        } else {
                            Self::secondary_index_key_from_loaded_entry(
                                &data_reader,
                                &field_name_map,
                                &field,
                                key,
                                data_offset_val,
                            )?
                        };
                        let map_key = format!("{}:{}", field, loaded_key);
                        if let Some((stale_index_offset, stale_slot_size)) =
                            index_file_offsets.insert(map_key, (current_offset, actual_slot_size))
                        {
                            index_writer
                                .write_tombstone_at_offset(stale_index_offset, stale_slot_size)?;
                        }

                        if field == primary_key {
                            if let Some(data_offset) = data_offset_val.as_u64() {
                                let index_key = if drawer_needs_format_migration {
                                    Self::clean_legacy_identifier(key)
                                } else {
                                    key.to_string()
                                };
                                primary_memory_index.insert(index_key, data_offset);
                            } else if data_offset_val
                                .as_array()
                                .is_some_and(|offset_array| offset_array.is_empty())
                            {
                                primary_memory_index.remove(key);
                                primary_memory_index.remove(&Self::clean_legacy_identifier(key));
                            }
                        } else if let Some(field_map) =
                            secondary_memory_index.get_mut(field.as_str())
                        {
                            if let Some(data_offset) = data_offset_val.as_u64() {
                                field_map.insert(loaded_key, vec![data_offset]);
                            } else if let Some(offset_array) = data_offset_val.as_array() {
                                if offset_array.is_empty() {
                                    field_map.remove(&loaded_key);
                                } else {
                                    let offsets: Vec<u64> =
                                        offset_array.iter().filter_map(|v| v.as_u64()).collect();
                                    field_map.insert(loaded_key, offsets);
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut data_block_index = HashMap::new();
        for (data_offset, block_entry) in data_block_journal {
            if block_entry.status != DATA_BLOCK_STATUS_DEAD {
                data_block_index.insert(data_offset, block_entry);
            }
        }

        let record_count = primary_memory_index.len();

        let mut drawer = Self {
            name: name.to_string(),
            primary_key: primary_key.to_string(),
            unique_constraints: inferred_unique_constraints,
            data_writer,
            data_reader,
            index_writer,
            metadata_writer,
            data_recycler,
            data_recycler_cache_initialized: false,
            index_recycler,
            primary_memory_index,
            secondary_memory_index,
            validated_secondary_indexes: HashSet::new(),
            materialized_secondary_indexes,
            secondary_index_generation,
            index_file_offsets,
            data_block_index,
            relationship_constraints,
            delete_rules,
            cascade_delete_rules,
            schema,
            record_count,
            metadata_dirty: false,
            metadata_format_version,
            field_name_map,
            #[cfg(test)]
            data_file_path,
            index_file_path,
            meta_file_path,
            #[cfg(test)]
            test_metrics: DrawerTestMetrics::default(),
        };

        if !drawer_needs_format_migration {
            drawer.persist_metadata()?;
        }

        Ok(drawer)
    }

    pub fn record_count(&self) -> usize {
        self.record_count
    }

    pub(super) fn mark_metadata_dirty(&mut self) {
        self.metadata_dirty = true;
    }

    #[cfg(test)]
    pub(super) fn reset_test_metrics(&mut self) {
        self.test_metrics = DrawerTestMetrics::default();
    }

    pub(crate) fn flush_metadata_if_dirty(&mut self) -> std::io::Result<()> {
        if self.metadata_dirty {
            self.persist_metadata()?;
        }
        Ok(())
    }

    pub(super) fn persist_metadata(&mut self) -> std::io::Result<()> {
        #[cfg(test)]
        {
            self.test_metrics.persist_metadata_calls += 1;
        }

        let metadata = DrawerMetadata::from_configuration(
            &self.primary_key,
            self.record_count,
            &self.unique_constraints,
            self.relationship_constraints.clone(),
            self.delete_rules.clone(),
            self.cascade_delete_rules.clone(),
            self.schema.clone(),
            self.secondary_index_generation,
            self.materialized_secondary_indexes.clone(),
            self.field_name_map.clone(),
        );
        let serialized = serde_json::to_vec_pretty(&metadata)?;
        self.metadata_writer.rewrite_all(&serialized)?;
        self.metadata_dirty = false;
        Ok(())
    }

    pub fn checkpoint(&mut self) -> std::io::Result<()> {
        self.commit()
    }

    pub(crate) fn commit(&mut self) -> std::io::Result<()> {
        TransactionCoordinator::harden_writer(&mut self.data_writer)?;
        TransactionCoordinator::harden_writer(&mut self.index_writer)?;
        self.flush_metadata_if_dirty()?;
        Ok(())
    }
}
