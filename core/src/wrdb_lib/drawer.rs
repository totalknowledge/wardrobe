use crate::wrdb_lib::reader::DatabaseReader;
use crate::wrdb_lib::recycler::Recycler;
use crate::wrdb_lib::storage_format::{PlainTextJsonFormat, StorageFormat};
use crate::wrdb_lib::writer::DatabaseWriter;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
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
    #[serde(default)]
    format_version: u8,
    #[serde(default)]
    primary_key: String,
    #[serde(default)]
    record_count: usize,
    #[serde(default)]
    unique_constraints: Vec<String>,
    #[serde(default)]
    relationship_constraints: BTreeMap<String, Value>,
    #[serde(default)]
    delete_rules: BTreeMap<String, Value>,
    #[serde(default)]
    cascade_delete_rules: BTreeMap<String, bool>,
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

    fn from_configuration(
        primary_key: &str,
        record_count: usize,
        unique_constraints: &[String],
        relationship_constraints: BTreeMap<String, Value>,
        delete_rules: BTreeMap<String, Value>,
        cascade_delete_rules: BTreeMap<String, bool>,
    ) -> Self {
        Self {
            format_version: DRAWER_METADATA_FORMAT_VERSION,
            primary_key: primary_key.to_string(),
            record_count,
            unique_constraints: unique_constraints.to_vec(),
            relationship_constraints,
            delete_rules,
            cascade_delete_rules,
        }
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

const DATA_BLOCK_STATUS_DEAD: u8 = 0;
const DATA_BLOCK_STATUS_LIVE: u8 = 1;

#[derive(Clone, Copy, Debug)]
struct DataBlockIndexEntry {
    payload_len: usize,
    size_class: usize,
    crc: u32,
    status: u8,
}

impl DataBlockIndexEntry {
    fn live(payload: &[u8], size_class: usize) -> Self {
        Self {
            payload_len: payload.len(),
            size_class,
            crc: crc32(payload),
            status: DATA_BLOCK_STATUS_LIVE,
        }
    }

    fn dead(self) -> Self {
        Self {
            status: DATA_BLOCK_STATUS_DEAD,
            ..self
        }
    }

    fn from_index_entry(index_entry: &Value) -> Option<(u64, Self)> {
        let offset = index_entry.get("o").and_then(|value| value.as_u64())?;
        let payload_len = index_entry.get("len").and_then(|value| value.as_u64())? as usize;
        let size_class = index_entry.get("class").and_then(|value| value.as_u64())? as usize;
        let crc = index_entry.get("crc").and_then(|value| value.as_u64())? as u32;
        let status = index_entry.get("status").and_then(|value| value.as_u64())? as u8;

        Some((
            offset,
            Self {
                payload_len,
                size_class,
                crc,
                status,
            },
        ))
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
    data_block_index: HashMap<u64, DataBlockIndexEntry>,
    relationship_constraints: BTreeMap<String, Value>,
    delete_rules: BTreeMap<String, Value>,
    cascade_delete_rules: BTreeMap<String, bool>,
    record_count: usize,
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
        let data_reader = DatabaseReader::open_drawer(&data_file_path)?;
        let index_writer = DatabaseWriter::open_drawer(&index_file_path)?;
        let mut index_reader = DatabaseReader::open_drawer(&index_file_path)?;

        let mut data_recycler = Recycler::new();
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

        let mut index_entries = Vec::new();
        index_reader.stream_with_offsets(|offset, line| {
            index_entries.push((
                offset,
                PlainTextJsonFormat::is_tombstone(line.as_bytes()),
                line.to_string(),
            ));
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
            } else if let Some(index_entry) =
                PlainTextJsonFormat::deserialize_record(line_content.as_bytes())?
            {
                if let Some((data_offset, block_entry)) =
                    DataBlockIndexEntry::from_index_entry(&index_entry)
                {
                    data_block_journal.insert(data_offset, block_entry);
                }

                if index_entry.get("status").and_then(|value| value.as_u64())
                    == Some(DATA_BLOCK_STATUS_DEAD as u64)
                {
                    if let (Some(field), Some(key), Some(data_offset)) = (
                        index_entry.get("f").and_then(|value| value.as_str()),
                        index_entry.get("k").and_then(|value| value.as_str()),
                        index_entry.get("o").and_then(|value| value.as_u64()),
                    ) {
                        if field == primary_key
                            && primary_memory_index.get(key).copied() == Some(data_offset)
                        {
                            primary_memory_index.remove(key);
                        }
                    }
                    continue;
                }

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

        let mut data_block_index = HashMap::new();
        for (data_offset, block_entry) in data_block_journal {
            if block_entry.status == DATA_BLOCK_STATUS_DEAD {
                data_recycler.register_free_slot(block_entry.size_class, data_offset);
            } else {
                data_block_index.insert(data_offset, block_entry);
            }
        }

        let record_count = primary_memory_index.len();

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
            data_block_index,
            relationship_constraints,
            delete_rules,
            cascade_delete_rules,
            record_count,
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
        let is_new_record = old_data_offset.is_none();
        let old_record = if let Some(existing_offset) = old_data_offset {
            self.data_reader.read_record_at_offset(existing_offset)?
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

        let serialized_record = PlainTextJsonFormat::serialize_record(&record)?;
        let raw_len = serialized_record.len();
        let target_size_class = self.data_recycler.calculate_aligned_size(raw_len);
        let live_block = DataBlockIndexEntry::live(&serialized_record, target_size_class);

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
            Some(live_block),
        )?;
        self.primary_memory_index
            .insert(primary_key_value.clone(), data_offset);
        self.data_block_index.insert(data_offset, live_block);

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
                    self.write_index_log(
                        &unique_field,
                        previous_value,
                        Value::Array(Vec::new()),
                        None,
                    )?;
                }
            }

            if let Some(field_value) = record.get(&unique_field).and_then(|v| v.as_str()) {
                self.write_index_log(&unique_field, field_value, Value::from(data_offset), None)?;
                if let Some(field_map) = self.secondary_memory_index.get_mut(&unique_field) {
                    let offsets = field_map.entry(field_value.to_string()).or_default();
                    if !offsets.contains(&data_offset) {
                        offsets.push(data_offset);
                    }
                }
            }
        }

        if let Some((stale_offset, old_block)) = historical_tombstone_block {
            if stale_offset != data_offset {
                self.data_writer
                    .write_tombstone_at_offset(stale_offset, old_block.size_class)?;
                self.write_data_block_status_log(
                    &primary_key_value,
                    stale_offset,
                    old_block.dead(),
                )?;
                self.data_block_index.remove(&stale_offset);
                self.data_recycler
                    .register_free_slot(old_block.size_class, stale_offset);
            }
        }

        if is_new_record {
            self.record_count += 1;
            self.persist_metadata()?;
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

    pub fn delete_by_primary_key(&mut self, key: &str) -> std::io::Result<Option<Value>> {
        let Some(stale_offset) = self.primary_memory_index.get(key).copied() else {
            return Ok(None);
        };

        let Some(deleted_record) = self.data_reader.read_record_at_offset(stale_offset)? else {
            self.primary_memory_index.remove(key);
            return Ok(None);
        };

        let Some((_stale_offset, old_block)) =
            self.historical_block_entry(stale_offset, Some(&deleted_record))?
        else {
            self.primary_memory_index.remove(key);
            return Ok(Some(deleted_record));
        };

        self.data_writer
            .write_tombstone_at_offset(stale_offset, old_block.size_class)?;
        self.write_data_block_status_log(key, stale_offset, old_block.dead())?;

        self.primary_memory_index.remove(key);
        self.data_block_index.remove(&stale_offset);
        self.data_recycler
            .register_free_slot(old_block.size_class, stale_offset);
        self.record_count = self.record_count.saturating_sub(1);

        let fields_to_clear = self.unique_constraints.clone();
        for unique_field in fields_to_clear {
            if let Some(field_value) = deleted_record
                .get(&unique_field)
                .and_then(|value| value.as_str())
            {
                if let Some(field_map) = self.secondary_memory_index.get_mut(&unique_field) {
                    if let Some(offsets) = field_map.get_mut(field_value) {
                        offsets.retain(|offset| *offset != stale_offset);
                        if offsets.is_empty() {
                            field_map.remove(field_value);
                        }
                    }
                }

                self.write_index_log(&unique_field, field_value, Value::Array(Vec::new()), None)?;
            }
        }

        self.persist_metadata()?;

        Ok(Some(deleted_record))
    }

    pub fn record_count(&self) -> usize {
        self.record_count
    }

    pub fn cascade_delete_fields(&self) -> Vec<String> {
        let mut fields = self
            .cascade_delete_rules
            .iter()
            .filter_map(|(field, should_cascade)| {
                if *should_cascade {
                    Some(field.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        for (field, rule) in &self.delete_rules {
            if Self::delete_rule_is_cascade(rule) && !fields.contains(field) {
                fields.push(field.clone());
            }
        }

        fields
    }

    fn delete_rule_is_cascade(rule: &Value) -> bool {
        if rule.as_str().is_some_and(|action| action == "Cascade") {
            return true;
        }

        rule.get("action")
            .and_then(|action| action.as_str())
            .is_some_and(|action| action == "Cascade")
    }

    fn historical_block_entry(
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
            let serialized_record = PlainTextJsonFormat::serialize_record(record)?;
            (serialized_record.len(), crc32(&serialized_record))
        } else {
            (size_class.saturating_sub(1), 0)
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

    fn estimate_data_slot_size(&self, stale_offset: u64, current_file_len: u64) -> usize {
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

    fn write_index_log(
        &mut self,
        field: &str,
        key: &str,
        offset_value: Value,
        block_entry: Option<DataBlockIndexEntry>,
    ) -> std::io::Result<()> {
        let map_key = format!("{}:{}", field, key);

        let mut index_entry = serde_json::json!({
            "f": field,
            "k": key,
            "o": offset_value
        });

        if let Some(block_entry) = block_entry {
            index_entry["len"] = Value::from(block_entry.payload_len as u64);
            index_entry["class"] = Value::from(block_entry.size_class as u64);
            index_entry["crc"] = Value::from(block_entry.crc as u64);
            index_entry["status"] = Value::from(block_entry.status as u64);
        }

        let serialized_index = PlainTextJsonFormat::serialize_record(&index_entry)?;
        let entry_raw_len = serialized_index.len();
        let target_size_class = self.index_recycler.calculate_aligned_size(entry_raw_len);

        let new_index_offset = if block_entry.is_some() {
            self.index_writer
                .append_aligned_index(&serialized_index, target_size_class)?
        } else {
            if let Some(recycled_offset) = self.index_recycler.pop_available_slot(target_size_class)
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
            }
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

    fn write_data_block_status_log(
        &mut self,
        key: &str,
        data_offset: u64,
        block_entry: DataBlockIndexEntry,
    ) -> std::io::Result<()> {
        let index_entry = serde_json::json!({
            "f": self.primary_key,
            "k": key,
            "o": data_offset,
            "len": block_entry.payload_len,
            "class": block_entry.size_class,
            "crc": block_entry.crc,
            "status": block_entry.status
        });
        let serialized_index = PlainTextJsonFormat::serialize_record(&index_entry)?;
        let entry_raw_len = serialized_index.len();
        let target_size_class = self.index_recycler.calculate_aligned_size(entry_raw_len);

        self.index_writer
            .append_aligned_index(&serialized_index, target_size_class)?;
        Ok(())
    }

    pub fn find_all_records(&mut self) -> std::io::Result<Vec<Value>> {
        let mut live_records = Vec::new();
        let mut raw_lines = Vec::new();

        self.data_reader
            .stream_with_offsets(|_offset, line_content| {
                let trimmed = line_content.trim();
                if !trimmed.is_empty() && !PlainTextJsonFormat::is_tombstone(trimmed.as_bytes()) {
                    raw_lines.push(trimmed.to_string());
                }
            })?;

        for line in raw_lines {
            if let Some(record_value) = PlainTextJsonFormat::deserialize_record(line.as_bytes())? {
                live_records.push(record_value);
            }
        }

        Ok(live_records)
    }

    fn persist_metadata(&self) -> std::io::Result<()> {
        let metadata = DrawerMetadata::from_configuration(
            &self.primary_key,
            self.record_count,
            &self.unique_constraints,
            self.relationship_constraints.clone(),
            self.delete_rules.clone(),
            self.cascade_delete_rules.clone(),
        );
        metadata.persist(&self.meta_file_path)
    }
}
