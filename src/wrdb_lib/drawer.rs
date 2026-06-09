use crate::wrdb_lib::reader::DatabaseReader;
use crate::wrdb_lib::recycler::Recycler;
use crate::wrdb_lib::writer::DatabaseWriter;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const DRAWER_METADATA_FORMAT_VERSION: u8 = 1;

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;

    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let least_significant_bit = crc & 1;
            crc >>= 1;
            if least_significant_bit != 0 {
                crc ^= 0xedb8_8320;
            }
        }
    }

    !crc
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DrawerMetadata {
    format_version: u8,
    primary_key: String,
    unique_constraints: Vec<String>,
    record_status_map: HashMap<String, String>,
    payload_lengths: HashMap<String, usize>,
    allocated_size_classes: HashMap<String, usize>,
    integrity_checksums: HashMap<String, u32>,
}

impl DrawerMetadata {
    fn load(path: &Path) -> std::io::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }

        let contents = std::fs::read_to_string(path)?;
        if contents.trim().is_empty() {
            return Ok(None);
        }

        match serde_json::from_str(&contents) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(_) => Ok(None),
        }
    }

    fn rebuild_from_files(
        data_file_path: &Path,
        index_file_path: &Path,
        primary_key: &str,
        unique_constraints: &[String],
    ) -> std::io::Result<Self> {
        let mut metadata = Self {
            format_version: DRAWER_METADATA_FORMAT_VERSION,
            primary_key: primary_key.to_string(),
            unique_constraints: unique_constraints.to_vec(),
            ..Self::default()
        };

        let mut data_reader = DatabaseReader::open_drawer(data_file_path)?;
        let mut data_entries = Vec::new();
        data_reader.stream_with_offsets(|offset, line| {
            data_entries.push((offset, line.starts_with("!!DEAD!!"), line.to_string()));
        })?;

        let data_len = std::fs::metadata(data_file_path)?.len();
        for (index, (offset, is_dead, line)) in data_entries.iter().enumerate() {
            let next_offset = data_entries
                .get(index + 1)
                .map(|entry| entry.0)
                .unwrap_or(data_len);
            let allocated_size_class = (next_offset - *offset) as usize;
            metadata.register_entry("data", *offset, allocated_size_class, line, *is_dead);
        }

        let mut index_reader = DatabaseReader::open_drawer(index_file_path)?;
        let mut index_entries = Vec::new();
        index_reader.stream_with_offsets(|offset, line| {
            index_entries.push((offset, line.starts_with("!!DEAD!!"), line.to_string()));
        })?;

        let index_len = std::fs::metadata(index_file_path)?.len();
        for (index, (offset, is_dead, line)) in index_entries.iter().enumerate() {
            let next_offset = index_entries
                .get(index + 1)
                .map(|entry| entry.0)
                .unwrap_or(index_len);
            let allocated_size_class = (next_offset - *offset) as usize;
            metadata.register_entry("index", *offset, allocated_size_class, line, *is_dead);
        }

        Ok(metadata)
    }

    fn register_entry(
        &mut self,
        entry_kind: &str,
        offset: u64,
        allocated_size_class: usize,
        line: &str,
        is_dead: bool,
    ) {
        let metadata_key = format!("{}:{}", entry_kind, offset);
        let normalized_payload = line.trim_end().to_string();

        self.record_status_map.insert(
            metadata_key.clone(),
            if is_dead { "dead" } else { "live" }.to_string(),
        );
        self.payload_lengths
            .insert(metadata_key.clone(), normalized_payload.len());
        self.allocated_size_classes
            .insert(metadata_key.clone(), allocated_size_class);
        self.integrity_checksums
            .insert(metadata_key, crc32(normalized_payload.as_bytes()));
    }

    fn persist(&self, path: &Path) -> std::io::Result<()> {
        let serialized = serde_json::to_vec_pretty(self)?;
        let temporary_path = path.with_extension("drw.tmp");
        std::fs::write(&temporary_path, serialized)?;

        if path.exists() {
            std::fs::remove_file(path)?;
        }

        std::fs::rename(temporary_path, path)?;
        Ok(())
    }
}

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
    data_file_path: PathBuf,
    index_file_path: PathBuf,
    meta_file_path: PathBuf,
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
        let meta_file_path = base_path.join(format!("{}_meta.drw", name));

        let data_writer = DatabaseWriter::open_drawer(&data_file_path)?;
        let mut data_reader = DatabaseReader::open_drawer(&data_file_path)?;
        let index_writer = DatabaseWriter::open_drawer(&index_file_path)?;
        let mut index_reader = DatabaseReader::open_drawer(&index_file_path)?;

        let mut data_recycler = Recycler::new();
        let mut index_recycler = Recycler::new();

        let mut primary_memory_index = HashMap::new();
        let mut secondary_memory_index = HashMap::new();
        let mut index_file_offsets = HashMap::new();
        let existing_metadata = DrawerMetadata::load(&meta_file_path)?;
        let mut inferred_unique_constraints = if unique_constraints.is_empty() {
            existing_metadata
                .as_ref()
                .map(|metadata| metadata.unique_constraints.clone())
                .unwrap_or_default()
        } else {
            unique_constraints
        };

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

        let drawer = Self {
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
            data_file_path,
            index_file_path,
            meta_file_path,
        };

        drawer.persist_metadata()?;

        Ok(drawer)
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

        self.persist_metadata()?;

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

    fn persist_metadata(&self) -> std::io::Result<()> {
        let metadata = DrawerMetadata::rebuild_from_files(
            &self.data_file_path,
            &self.index_file_path,
            &self.primary_key,
            &self.unique_constraints,
        )?;
        metadata.persist(&self.meta_file_path)
    }
}
