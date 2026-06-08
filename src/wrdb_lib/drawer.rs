use crate::wrdb_lib::reader::DatabaseReader;
use crate::wrdb_lib::recycler::Recycler;
use crate::wrdb_lib::writer::DatabaseWriter;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

pub struct Drawer {
    pub name: String,
    pub primary_key: String,
    pub unique_constraints: Vec<String>,

    data_writer: DatabaseWriter,
    data_reader: DatabaseReader,
    index_writer: DatabaseWriter,

    data_recycler: Recycler,
    index_recycler: Recycler,

    primary_memory_index: HashMap<String, u64>,
    secondary_memory_index: HashMap<String, HashMap<String, Vec<u64>>>,
    index_file_offsets: HashMap<String, (u64, usize)>,
}

impl Drawer {
    pub fn open<P: AsRef<Path>>(
        directory: P,
        name: &str,
        primary_key: &str,
        unique_constraints: Vec<String>,
    ) -> std::io::Result<Self> {
        let base_path = directory.as_ref().to_path_buf();

        let data_file_path = base_path.join(format!("{}.drw", name));
        let index_file_path = base_path.join(format!("{}_index.drw", name));

        let data_writer = DatabaseWriter::open_drawer(&data_file_path)?;
        let mut data_reader = DatabaseReader::open_drawer(&data_file_path)?;
        let index_writer = DatabaseWriter::open_drawer(&index_file_path)?;
        let mut index_reader = DatabaseReader::open_drawer(&index_file_path)?;

        let mut data_recycler = Recycler::new();
        let mut index_recycler = Recycler::new();

        let mut primary_memory_index = HashMap::new();
        let mut secondary_memory_index = HashMap::new();
        let mut index_file_offsets = HashMap::new();
        let mut inferred_unique_constraints = unique_constraints;

        for field in &inferred_unique_constraints {
            secondary_memory_index.insert(field.clone(), HashMap::new());
        }

        let mut data_entries = Vec::new();
        data_reader.stream_with_offsets(|offset, line| {
            data_entries.push((offset, line.starts_with("!!DEAD!!")));
        })?;

        let total_data_file_len = data_writer.current_length()?;
        for i in 0..data_entries.len() {
            let (current_offset, is_dead) = data_entries[i];
            if is_dead {
                let next_offset = if i + 1 < data_entries.len() {
                    data_entries[i + 1].0
                } else {
                    total_data_file_len
                };
                let actual_slot_size = (next_offset - current_offset) as usize;
                data_recycler.register_free_slot(actual_slot_size, current_offset);
            }
        }

        let mut index_entries = Vec::new();
        index_reader.stream_with_offsets(|offset, line| {
            index_entries.push((offset, line.starts_with("!!DEAD!!"), line.to_string()));
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
            } else if let Ok(index_entry) = serde_json::from_str::<Value>(line_content) {
                if let (Some(field), Some(key), Some(data_offset_val)) = (
                    index_entry.get("f").and_then(|v| v.as_str()),
                    index_entry.get("k").and_then(|v| v.as_str()),
                    index_entry.get("o"),
                ) {
                    let map_key = format!("{}:{}", field, key);
                    index_file_offsets.insert(map_key, (current_offset, actual_slot_size));

                    if field == primary_key {
                        if let Some(data_offset) = data_offset_val.as_u64() {
                            primary_memory_index.insert(key.to_string(), data_offset);
                        }
                    } else {
                        if !inferred_unique_constraints.contains(&field.to_string()) {
                            inferred_unique_constraints.push(field.to_string());
                        }

                        let field_map = secondary_memory_index
                            .entry(field.to_string())
                            .or_insert_with(HashMap::new);

                        if let Some(data_offset) = data_offset_val.as_u64() {
                            field_map.insert(key.to_string(), vec![data_offset]);
                        } else if let Some(offset_array) = data_offset_val.as_array() {
                            if offset_array.is_empty() {
                                field_map.remove(key);
                            } else {
                                let offsets: Vec<u64> =
                                    offset_array.iter().filter_map(|v| v.as_u64()).collect();
                                field_map.insert(key.to_string(), offsets);
                            }
                        }
                    }
                }
            }
        }

        Ok(Self {
            name: name.to_string(),
            primary_key: primary_key.to_string(),
            unique_constraints: inferred_unique_constraints,
            data_writer,
            data_reader,
            index_writer,
            data_recycler,
            index_recycler,
            primary_memory_index,
            secondary_memory_index,
            index_file_offsets,
        })
    }

    pub fn upsert_record(&mut self, record: Value) -> std::io::Result<Result<(), String>> {
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
        let old_record = if let Some(existing_offset) = old_data_offset {
            self.data_reader.read_record_at_offset(existing_offset)?
        } else {
            None
        };

        let mut historical_tombstone_meta: Option<(u64, usize)> = None;
        if let Some(stale_offset) = old_data_offset {
            let current_file_len = self.data_writer.current_length()?;
            if stale_offset < current_file_len {
                let mut record_offsets: Vec<u64> =
                    self.primary_memory_index.values().copied().collect();
                record_offsets.sort_unstable();

                let mut exact_disk_len = 0;
                if let Some(current_pos) = record_offsets.iter().position(|&o| o == stale_offset) {
                    if current_pos + 1 < record_offsets.len() {
                        let next_offset = record_offsets[current_pos + 1];
                        if next_offset > stale_offset && next_offset <= current_file_len {
                            exact_disk_len = (next_offset - stale_offset) as usize;
                        }
                    }
                }

                if exact_disk_len == 0 {
                    exact_disk_len = (current_file_len - stale_offset) as usize;
                }

                historical_tombstone_meta = Some((stale_offset, exact_disk_len));
            }
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

        let serialized_record = serde_json::to_string(&record)?;
        let raw_len = serialized_record.len();
        let target_size_class = self.data_recycler.calculate_aligned_size(raw_len);

        let data_offset = if let Some(recycled_offset) =
            self.data_recycler.pop_available_slot(target_size_class)
        {
            self.data_writer.overwrite_at_offset(
                recycled_offset,
                &serialized_record,
                target_size_class,
            )?;
            recycled_offset
        } else {
            self.data_writer
                .append_record(&serialized_record, target_size_class)?
        };

        let primary_key_field_name = self.primary_key.clone();
        self.write_index_log(
            &primary_key_field_name,
            &primary_key_value,
            Value::from(data_offset),
        )?;
        self.primary_memory_index
            .insert(primary_key_value, data_offset);

        let fields_to_index = self.unique_constraints.clone();
        for unique_field in fields_to_index {
            let old_field_value = old_record
                .as_ref()
                .and_then(|value| value.get(&unique_field))
                .and_then(|value| value.as_str());

            if let (Some(previous_value), Some(previous_offset)) =
                (old_field_value, old_data_offset)
            {
                if let Some(field_map) = self.secondary_memory_index.get_mut(&unique_field) {
                    if let Some(offsets) = field_map.get_mut(previous_value) {
                        offsets.retain(|offset| *offset != previous_offset);
                        if offsets.is_empty() {
                            field_map.remove(previous_value);
                        }
                    }
                }

                if record.get(&unique_field).and_then(|value| value.as_str())
                    != Some(previous_value)
                {
                    self.write_index_log(&unique_field, previous_value, Value::Array(Vec::new()))?;
                }
            }

            if let Some(field_value) = record.get(&unique_field).and_then(|v| v.as_str()) {
                self.write_index_log(&unique_field, field_value, Value::from(data_offset))?;
                if let Some(field_map) = self.secondary_memory_index.get_mut(&unique_field) {
                    let offsets = field_map.entry(field_value.to_string()).or_default();
                    if !offsets.contains(&data_offset) {
                        offsets.push(data_offset);
                    }
                }
            }
        }

        if let Some((stale_offset, old_size_class)) = historical_tombstone_meta {
            if stale_offset != data_offset {
                self.data_writer
                    .write_tombstone_at_offset(stale_offset, old_size_class)?;
                self.data_recycler
                    .register_free_slot(old_size_class, stale_offset);
            }
        }

        Ok(Ok(()))
    }

    pub fn find_by_primary_key(&mut self, key: &str) -> std::io::Result<Option<Value>> {
        if let Some(&offset) = self.primary_memory_index.get(key) {
            return self.data_reader.read_record_at_offset(offset);
        }
        Ok(None)
    }

    pub fn find_by_secondary_key(&mut self, field: &str, key: &str) -> std::io::Result<Vec<Value>> {
        let mut matching_records = Vec::new();
        if let Some(field_map) = self.secondary_memory_index.get(field) {
            if let Some(offsets) = field_map.get(key) {
                for &offset in offsets {
                    if let Some(record) = self.data_reader.read_record_at_offset(offset)? {
                        matching_records.push(record);
                    }
                }
            }
        }
        Ok(matching_records)
    }

    fn write_index_log(
        &mut self,
        field: &str,
        key: &str,
        offset_value: Value,
    ) -> std::io::Result<()> {
        let map_key = format!("{}:{}", field, key);

        let index_entry = serde_json::json!({
            "f": field,
            "k": key,
            "o": offset_value
        });

        let serialized_index = serde_json::to_string(&index_entry)?;
        let entry_raw_len = serialized_index.len();
        let target_size_class = self.index_recycler.calculate_aligned_size(entry_raw_len);

        let new_index_offset = if let Some(recycled_offset) =
            self.index_recycler.pop_available_slot(target_size_class)
        {
            self.index_writer.overwrite_at_offset(
                recycled_offset,
                &serialized_index,
                target_size_class,
            )?;
            recycled_offset
        } else {
            self.index_writer
                .append_aligned_index(&serialized_index, target_size_class)?
        };

        if let Some((old_index_offset, old_size_class)) =
            self.index_file_offsets.get(&map_key).copied()
        {
            if old_index_offset != new_index_offset {
                self.index_writer
                    .write_tombstone_at_offset(old_index_offset, old_size_class)?;
                self.index_recycler
                    .register_free_slot(old_size_class, old_index_offset);
            }
        }

        self.index_file_offsets
            .insert(map_key, (new_index_offset, target_size_class));
        Ok(())
    }

    pub fn find_all_records(&mut self) -> std::io::Result<Vec<Value>> {
        let mut live_records = Vec::new();
        let mut raw_lines = Vec::new();

        self.data_reader
            .stream_with_offsets(|_offset, line_content| {
                let trimmed = line_content.trim();
                if !trimmed.is_empty() && !trimmed.starts_with("!!DEAD!!") {
                    raw_lines.push(trimmed.to_string());
                }
            })?;

        for line in raw_lines {
            if let Ok(record_value) = serde_json::from_str::<Value>(&line) {
                live_records.push(record_value);
            }
        }

        Ok(live_records)
    }
}
