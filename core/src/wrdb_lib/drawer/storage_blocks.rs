use super::*;

impl Drawer {
    pub(super) fn historical_block_entry(
        &self,
        stale_offset: u64,
        old_record: Option<&Value>,
    ) -> std::io::Result<Option<(u64, DataBlockIndexEntry)>> {
        if let Some(block_entry) = self.data_block_index.get(&stale_offset).copied() {
            return Ok(Some((stale_offset, block_entry)));
        }

        let current_file_len = self.data_writer.current_length()?;
        if stale_offset >= current_file_len {
            return Ok(None);
        }

        let size_class = self.estimate_data_slot_size(stale_offset, current_file_len);
        let (payload_len, crc) = if let Some(record) = old_record {
            let stored_record = self.encode_record_for_storage(record);
            let serialized_record = BsonBinaryFormat::serialize_record(&stored_record)?;
            (serialized_record.len(), crc32(&serialized_record))
        } else {
            (size_class, 0)
        };

        Ok(Some((
            stale_offset,
            DataBlockIndexEntry {
                payload_len,
                size_class,
                crc,
                status: DATA_BLOCK_STATUS_LIVE,
            },
        )))
    }

    pub(super) fn estimate_data_slot_size(
        &self,
        stale_offset: u64,
        current_file_len: u64,
    ) -> usize {
        let mut record_offsets: Vec<u64> = self.primary_memory_index.values().copied().collect();
        record_offsets.sort_unstable();

        if let Some(current_pos) = record_offsets
            .iter()
            .position(|&offset| offset == stale_offset)
        {
            if current_pos + 1 < record_offsets.len() {
                let next_offset = record_offsets[current_pos + 1];
                if next_offset > stale_offset && next_offset <= current_file_len {
                    return (next_offset - stale_offset) as usize;
                }
            }
        }

        (current_file_len - stale_offset) as usize
    }

    pub(super) fn write_data_payload(
        &mut self,
        serialized_record: &[u8],
        target_size_class: usize,
    ) -> std::io::Result<u64> {
        self.ensure_data_recycler_cache()?;

        if let Some(recycled_offset) = self.data_recycler.pop_available_slot(target_size_class) {
            self.data_writer.overwrite_at_offset(
                recycled_offset,
                serialized_record,
                target_size_class,
            )?;
            Ok(recycled_offset)
        } else {
            self.data_writer
                .append_record(serialized_record, target_size_class)
        }
    }

    pub(super) fn ensure_data_recycler_cache(&mut self) -> std::io::Result<()> {
        if self.data_recycler_cache_initialized {
            return Ok(());
        }

        if self.index_writer.current_length()? == 0 {
            self.data_recycler_cache_initialized = true;
            return Ok(());
        }

        let index_reader = DatabaseReader::open_drawer(&self.index_file_path)?;
        let mut data_block_journal = HashMap::new();
        let mut index_lines = Vec::new();
        let mut registered_data_slots = HashSet::new();

        index_reader.stream_with_offsets(|_offset, line| {
            if !BsonBinaryFormat::is_tombstone(line) {
                index_lines.push(line.to_vec());
            }
        })?;

        for line in index_lines {
            if let Some(index_entry) = BsonBinaryFormat::deserialize_record(&line)? {
                if let Some((data_offset, block_entry)) =
                    DataBlockIndexEntry::from_index_entry(&index_entry)
                {
                    data_block_journal.insert(data_offset, block_entry);
                }
            }
        }

        for (data_offset, block_entry) in data_block_journal {
            if block_entry.status == DATA_BLOCK_STATUS_DEAD {
                registered_data_slots.insert((block_entry.size_class, data_offset));
                self.data_recycler
                    .register_free_slot(block_entry.size_class, data_offset);
            }
        }

        let mut data_slots = Vec::new();
        self.data_reader.stream_with_offsets(|offset, line| {
            data_slots.push((offset, BsonBinaryFormat::is_tombstone(line)));
        })?;

        let total_data_file_len = self.data_writer.current_length()?;
        for i in 0..data_slots.len() {
            let (current_offset, is_dead) = data_slots[i];
            if !is_dead {
                continue;
            }

            let next_offset = if i + 1 < data_slots.len() {
                data_slots[i + 1].0
            } else {
                total_data_file_len
            };
            let slot_size = (next_offset - current_offset) as usize;
            if registered_data_slots.insert((slot_size, current_offset)) {
                self.data_recycler
                    .register_free_slot(slot_size, current_offset);
            }
        }

        self.data_recycler_cache_initialized = true;
        Ok(())
    }

    pub(super) fn append_compact_payload(target: &mut Vec<u8>, payload: &[u8]) -> u64 {
        let starting_offset = target.len() as u64;
        target.extend_from_slice(payload);
        starting_offset
    }

    pub(super) fn append_compact_index_entry(
        target: &mut Vec<u8>,
        index_entry: &Value,
    ) -> std::io::Result<(u64, usize)> {
        let starting_offset = target.len() as u64;
        let serialized_index = BsonBinaryFormat::serialize_record(index_entry)?;
        target.extend_from_slice(&serialized_index);

        Ok((starting_offset, serialized_index.len()))
    }
}
