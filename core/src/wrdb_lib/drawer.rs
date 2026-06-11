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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schema: Option<Value>,
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
        schema: Option<Value>,
    ) -> Self {
        Self {
            format_version: DRAWER_METADATA_FORMAT_VERSION,
            primary_key: primary_key.to_string(),
            record_count,
            unique_constraints: unique_constraints.to_vec(),
            relationship_constraints,
            delete_rules,
            cascade_delete_rules,
            schema,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VacuumReport {
    pub records_rewritten: usize,
    pub data_bytes_before: u64,
    pub data_bytes_after: u64,
    pub index_bytes_before: u64,
    pub index_bytes_after: u64,
    pub bytes_reclaimed: u64,
}

pub struct Drawer {
    pub name: String,
    pub primary_key: String,
    pub unique_constraints: Vec<String>,

    data_writer: DatabaseWriter,
    data_reader: DatabaseReader,
    index_writer: DatabaseWriter,

    data_recycler: Recycler,
    data_recycler_cache_initialized: bool,
    index_recycler: Recycler,

    primary_memory_index: HashMap<String, u64>,
    secondary_memory_index: HashMap<String, HashMap<String, Vec<u64>>>,
    index_file_offsets: HashMap<String, (u64, usize)>,
    data_block_index: HashMap<u64, DataBlockIndexEntry>,
    relationship_constraints: BTreeMap<String, Value>,
    delete_rules: BTreeMap<String, Value>,
    cascade_delete_rules: BTreeMap<String, bool>,
    schema: Option<Value>,
    record_count: usize,
    metadata_format_version: u8,
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
        let data_reader = DatabaseReader::open_drawer(&data_file_path)?;
        let index_writer = DatabaseWriter::open_drawer(&index_file_path)?;
        let index_reader = DatabaseReader::open_drawer(&index_file_path)?;

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
        let metadata_format_version = existing_metadata
            .as_ref()
            .map(|metadata| metadata.format_version)
            .unwrap_or(DRAWER_METADATA_FORMAT_VERSION);
        let drawer_needs_format_migration =
            existing_metadata.is_some() && metadata_format_version < DRAWER_METADATA_FORMAT_VERSION;
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
            if block_entry.status != DATA_BLOCK_STATUS_DEAD {
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
            data_recycler_cache_initialized: false,
            index_recycler,
            primary_memory_index,
            secondary_memory_index,
            index_file_offsets,
            data_block_index,
            relationship_constraints,
            delete_rules,
            cascade_delete_rules,
            schema,
            record_count,
            metadata_format_version,
            index_file_path,
            meta_file_path,
        };

        if !drawer_needs_format_migration {
            drawer.persist_metadata()?;
        }

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

        if let Err(validation_error) = self.validate_schema(&record) {
            return Ok(Err(validation_error));
        }

        let old_data_offset = self.primary_memory_index.get(&primary_key_value).copied();
        if let Some(validation_error) =
            self.validate_relationship_constraints(&record, &primary_key_value)?
        {
            return Ok(Err(validation_error));
        }

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

        let data_offset = self.write_data_payload(&serialized_record, target_size_class)?;

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

    pub fn find_by_primary_key(&self, key: &str) -> std::io::Result<Option<Value>> {
        if let Some(&offset) = self.primary_memory_index.get(key) {
            return self.data_reader.read_record_at_offset(offset);
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

    pub fn find_by_secondary_key(&self, field: &str, key: &str) -> std::io::Result<Vec<Value>> {
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
            if let Some(record) = self.data_reader.read_record_at_offset(offset)? {
                live_records.push(record);
            }
        }

        let mut compact_data = Vec::new();
        let mut compact_index = Vec::new();
        let mut primary_memory_index = HashMap::new();
        let mut secondary_memory_index = HashMap::new();
        let mut index_file_offsets = HashMap::new();
        let mut data_block_index = HashMap::new();

        for field in &self.unique_constraints {
            secondary_memory_index.insert(field.clone(), HashMap::new());
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
            let serialized_record = PlainTextJsonFormat::serialize_record(record)?;
            let data_offset = Self::append_compact_payload(&mut compact_data, &serialized_record);
            let block_entry =
                DataBlockIndexEntry::live(&serialized_record, serialized_record.len() + 1);

            let primary_index_entry = Self::index_entry_value(
                &self.primary_key,
                primary_key_value,
                Value::from(data_offset),
                Some(block_entry),
            );
            let (index_offset, index_slot_size) =
                Self::append_compact_index_entry(&mut compact_index, &primary_index_entry)?;

            primary_memory_index.insert(primary_key_value.to_string(), data_offset);
            index_file_offsets.insert(
                format!("{}:{}", self.primary_key, primary_key_value),
                (index_offset, index_slot_size),
            );
            data_block_index.insert(data_offset, block_entry);

            for unique_field in &self.unique_constraints {
                if let Some(field_value) = record.get(unique_field).and_then(|value| value.as_str())
                {
                    let secondary_index_entry = Self::index_entry_value(
                        unique_field,
                        field_value,
                        Value::from(data_offset),
                        None,
                    );
                    let (index_offset, index_slot_size) = Self::append_compact_index_entry(
                        &mut compact_index,
                        &secondary_index_entry,
                    )?;

                    index_file_offsets.insert(
                        format!("{}:{}", unique_field, field_value),
                        (index_offset, index_slot_size),
                    );
                    secondary_memory_index
                        .entry(unique_field.clone())
                        .or_insert_with(HashMap::new)
                        .entry(field_value.to_string())
                        .or_insert_with(Vec::new)
                        .push(data_offset);
                }
            }
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

    pub fn relationship_constraints(&self) -> BTreeMap<String, Value> {
        self.relationship_constraints.clone()
    }

    pub fn register_relationship_constraint(
        &mut self,
        field_name: &str,
        rule: Value,
    ) -> std::io::Result<()> {
        if self.relationship_constraints.contains_key(field_name) {
            return Ok(());
        }

        self.relationship_constraints
            .insert(field_name.to_string(), rule);
        self.persist_metadata()
    }

    pub fn delete_rules(&self) -> BTreeMap<String, Value> {
        self.delete_rules.clone()
    }

    fn validate_relationship_constraints(
        &self,
        record: &Value,
        primary_key_value: &str,
    ) -> std::io::Result<Option<String>> {
        let relationship_constraints = self.relationship_constraints.clone();

        for (field_name, rule) in relationship_constraints {
            let Some(relationship_type) = Self::relationship_type(&rule) else {
                continue;
            };

            match relationship_type {
                "1:1" => {
                    if let Some(field_value) = record.get(&field_name) {
                        if let Some(validation_error) =
                            Self::validate_reference_field(&field_name, field_value, &rule)
                        {
                            return Ok(Some(validation_error));
                        }

                        if let Some(pointer) = field_value.as_str() {
                            if let Some(validation_error) = self.validate_one_to_one_unique(
                                &field_name,
                                pointer,
                                primary_key_value,
                            )? {
                                return Ok(Some(validation_error));
                            }
                        }
                    }
                }
                "M:1" => {
                    if let Some(field_value) = record.get(&field_name) {
                        if let Some(validation_error) =
                            Self::validate_reference_field(&field_name, field_value, &rule)
                        {
                            return Ok(Some(validation_error));
                        }
                    }
                }
                "M:M" => {
                    if let Some(field_value) = record.get(&field_name) {
                        if let Some(validation_error) =
                            Self::validate_many_to_many_field(&field_name, field_value, &rule)
                        {
                            return Ok(Some(validation_error));
                        }
                    }
                }
                "1:M" => {}
                _ => {}
            }
        }

        Ok(None)
    }

    fn validate_one_to_one_unique(
        &self,
        field_name: &str,
        pointer: &str,
        primary_key_value: &str,
    ) -> std::io::Result<Option<String>> {
        for existing_record in self.find_all_records()? {
            if existing_record
                .get(&self.primary_key)
                .and_then(|value| value.as_str())
                == Some(primary_key_value)
            {
                continue;
            }

            if existing_record
                .get(field_name)
                .and_then(|value| value.as_str())
                == Some(pointer)
            {
                return Ok(Some(format!(
                    "1:1 relationship constraint violation: Field '{}' with value '{}' already exists",
                    field_name, pointer
                )));
            }
        }

        Ok(None)
    }

    fn validate_reference_field(field_name: &str, value: &Value, rule: &Value) -> Option<String> {
        let Some(pointer) = value.as_str() else {
            return Some(format!(
                "Relationship constraint violation: Field '{}' must be a pointer string",
                field_name
            ));
        };

        Self::validate_pointer_target(field_name, pointer, rule)
    }

    fn validate_many_to_many_field(
        field_name: &str,
        value: &Value,
        rule: &Value,
    ) -> Option<String> {
        let Some(values) = value.as_array() else {
            return Some(format!(
                "M:M relationship constraint violation: Field '{}' must be an array of pointer strings",
                field_name
            ));
        };

        for value in values {
            let Some(pointer) = value.as_str() else {
                return Some(format!(
                    "M:M relationship constraint violation: Field '{}' must contain only pointer strings",
                    field_name
                ));
            };

            if let Some(validation_error) = Self::validate_pointer_target(field_name, pointer, rule)
            {
                return Some(validation_error);
            }
        }

        None
    }

    fn validate_pointer_target(field_name: &str, pointer: &str, rule: &Value) -> Option<String> {
        let Some(pointer_drawer) = Self::pointer_drawer_name(pointer) else {
            return Some(format!(
                "Relationship constraint violation: Field '{}' contains malformed pointer '{}'",
                field_name, pointer
            ));
        };

        if let Some(target_drawer) = Self::relationship_target_drawer(rule) {
            if !Self::pointer_matches_target_drawer(pointer_drawer, target_drawer) {
                return Some(format!(
                    "Relationship constraint violation: Field '{}' expected target drawer '{}' but found '{}'",
                    field_name, target_drawer, pointer_drawer
                ));
            }
        }

        None
    }

    fn relationship_type(rule: &Value) -> Option<&str> {
        rule.get("type").and_then(|value| value.as_str())
    }

    fn relationship_target_drawer(rule: &Value) -> Option<&str> {
        rule.get("target_drawer").and_then(|value| value.as_str())
    }

    fn pointer_drawer_name(pointer: &str) -> Option<&str> {
        let clean_pointer = pointer.strip_prefix('@')?;
        let (drawer_name, record_key) = clean_pointer.split_once(':')?;
        let record_key = record_key.strip_prefix("lnk_").unwrap_or(record_key);

        if drawer_name.is_empty() || record_key.is_empty() || record_key.contains(':') {
            return None;
        }

        Some(drawer_name)
    }

    fn pointer_matches_target_drawer(pointer_drawer: &str, target_drawer: &str) -> bool {
        pointer_drawer == target_drawer
            || pointer_drawer
                .strip_suffix(target_drawer)
                .is_some_and(|prefix| prefix.ends_with('_'))
    }

    fn validate_schema(&self, record: &Value) -> Result<(), String> {
        let Some(schema) = self.schema.as_ref() else {
            return Ok(());
        };

        Self::validate_value_against_schema(record, schema, "$")
    }

    fn validate_value_against_schema(
        value: &Value,
        schema: &Value,
        path: &str,
    ) -> Result<(), String> {
        let Some(schema_map) = schema.as_object() else {
            return Ok(());
        };

        if let Some(allowed_values) = schema_map
            .get("enum")
            .and_then(|enum_value| enum_value.as_array())
        {
            if !allowed_values
                .iter()
                .any(|allowed_value| allowed_value == value)
            {
                return Err(format!("{path} must match one of the declared enum values"));
            }
        }

        if let Some(type_rule) = schema_map.get("type") {
            Self::validate_type_rule(value, type_rule, path)?;
        }

        if let Some(required_fields) = schema_map.get("required") {
            Self::validate_required_fields(value, required_fields, path)?;
        }

        if let Some(properties) = schema_map
            .get("properties")
            .and_then(|properties| properties.as_object())
        {
            if let Some(object) = value.as_object() {
                for (field_name, field_schema) in properties {
                    if let Some(field_value) = object.get(field_name) {
                        let field_path = format!("{path}.{field_name}");
                        Self::validate_value_against_schema(
                            field_value,
                            field_schema,
                            &field_path,
                        )?;
                    }
                }

                if schema_map
                    .get("additionalProperties")
                    .and_then(|rule| rule.as_bool())
                    == Some(false)
                {
                    for field_name in object.keys() {
                        if !properties.contains_key(field_name) {
                            return Err(format!("{path}.{field_name} is not allowed by schema"));
                        }
                    }
                }
            }
        }

        Self::validate_string_bounds(value, schema_map, path)?;
        Self::validate_numeric_bounds(value, schema_map, path)?;

        Ok(())
    }

    fn validate_type_rule(value: &Value, type_rule: &Value, path: &str) -> Result<(), String> {
        if let Some(type_name) = type_rule.as_str() {
            if Self::value_matches_type(value, type_name) {
                return Ok(());
            }

            return Err(format!("{path} must be of type {type_name}"));
        }

        if let Some(type_names) = type_rule.as_array() {
            let matches_any_type = type_names
                .iter()
                .filter_map(|type_name| type_name.as_str())
                .any(|type_name| Self::value_matches_type(value, type_name));

            if matches_any_type {
                return Ok(());
            }

            return Err(format!(
                "{path} must match one of the declared schema types"
            ));
        }

        Ok(())
    }

    fn validate_required_fields(
        value: &Value,
        required_fields: &Value,
        path: &str,
    ) -> Result<(), String> {
        let Some(object) = value.as_object() else {
            return Ok(());
        };
        let Some(required_fields) = required_fields.as_array() else {
            return Ok(());
        };

        for field in required_fields {
            let Some(field_name) = field.as_str() else {
                continue;
            };

            if !object.contains_key(field_name) {
                return Err(format!("{path}.{field_name} is required by schema"));
            }
        }

        Ok(())
    }

    fn validate_string_bounds(
        value: &Value,
        schema: &serde_json::Map<String, Value>,
        path: &str,
    ) -> Result<(), String> {
        let Some(value) = value.as_str() else {
            return Ok(());
        };

        if let Some(min_length) = schema.get("minLength").and_then(|length| length.as_u64()) {
            if value.chars().count() < min_length as usize {
                return Err(format!("{path} must have at least {min_length} characters"));
            }
        }

        if let Some(max_length) = schema.get("maxLength").and_then(|length| length.as_u64()) {
            if value.chars().count() > max_length as usize {
                return Err(format!("{path} must have at most {max_length} characters"));
            }
        }

        Ok(())
    }

    fn validate_numeric_bounds(
        value: &Value,
        schema: &serde_json::Map<String, Value>,
        path: &str,
    ) -> Result<(), String> {
        let Some(value) = value.as_f64() else {
            return Ok(());
        };

        if let Some(minimum) = schema.get("minimum").and_then(|minimum| minimum.as_f64()) {
            if value < minimum {
                return Err(format!("{path} must be greater than or equal to {minimum}"));
            }
        }

        if let Some(maximum) = schema.get("maximum").and_then(|maximum| maximum.as_f64()) {
            if value > maximum {
                return Err(format!("{path} must be less than or equal to {maximum}"));
            }
        }

        Ok(())
    }

    fn value_matches_type(value: &Value, type_name: &str) -> bool {
        match type_name {
            "array" => value.is_array(),
            "boolean" => value.is_boolean(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "null" => value.is_null(),
            "number" => value.is_number(),
            "object" => value.is_object(),
            "string" => value.is_string(),
            _ => false,
        }
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

    fn read_record_at_offset_with_lazy_migration(
        &mut self,
        offset: u64,
    ) -> std::io::Result<Option<Value>> {
        let Some(record) = self.data_reader.read_record_at_offset(offset)? else {
            return Ok(None);
        };

        if !self.needs_format_migration() {
            return Ok(Some(record));
        }

        let mut migrated_record = record.clone();
        if !self.migrate_legacy_record_value(&mut migrated_record) {
            return Ok(Some(record));
        }

        self.write_migrated_record_at_offset(offset, &record, &migrated_record)?;
        Ok(Some(migrated_record))
    }

    fn write_migrated_record_at_offset(
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

        let serialized_record = PlainTextJsonFormat::serialize_record(migrated_record)?;
        if serialized_record.len() + 1 > old_block.size_class {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Cannot lazily migrate legacy record because the upgraded payload no longer fits its allocated block",
            ));
        }

        let live_block = DataBlockIndexEntry::live(&serialized_record, old_block.size_class);
        self.data_writer
            .overwrite_at_offset(offset, &serialized_record, old_block.size_class)?;

        if old_primary_key_value != new_primary_key_value {
            self.write_index_log(
                &self.primary_key.clone(),
                old_primary_key_value,
                Value::Array(Vec::new()),
                None,
            )?;
            self.primary_memory_index.remove(old_primary_key_value);
            self.primary_memory_index
                .remove(&Self::clean_legacy_identifier(old_primary_key_value));
        }

        self.write_index_log(
            &self.primary_key.clone(),
            new_primary_key_value,
            Value::from(offset),
            Some(live_block),
        )?;
        self.primary_memory_index
            .insert(new_primary_key_value.to_string(), offset);
        self.data_block_index.insert(offset, live_block);
        self.metadata_format_version = DRAWER_METADATA_FORMAT_VERSION;
        self.persist_metadata()
    }

    fn needs_format_migration(&self) -> bool {
        self.metadata_format_version < DRAWER_METADATA_FORMAT_VERSION
    }

    fn migrate_legacy_record_value(&self, record: &mut Value) -> bool {
        Self::migrate_legacy_value(record, Some(&self.primary_key))
    }

    fn migrate_legacy_value(value: &mut Value, object_key: Option<&str>) -> bool {
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

    fn clean_legacy_identifier(value: &str) -> String {
        if let Some((_drawer_name, record_key)) = Self::try_parse_legacy_pointer(value) {
            return record_key;
        }

        value
            .trim_start_matches('@')
            .strip_prefix("lnk_")
            .unwrap_or_else(|| value.trim_start_matches('@'))
            .to_string()
    }

    fn try_parse_legacy_pointer(value: &str) -> Option<(String, String)> {
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

    fn format_legacy_pointer(drawer_name: &str, record_key: &str) -> String {
        format!(
            "@{}:{}",
            drawer_name.trim_start_matches('@'),
            Self::clean_legacy_identifier(record_key)
        )
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

    fn write_data_payload(
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

    fn ensure_data_recycler_cache(&mut self) -> std::io::Result<()> {
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

        index_reader.stream_with_offsets(|_offset, line| {
            if !PlainTextJsonFormat::is_tombstone(line.as_bytes()) {
                index_lines.push(line.to_string());
            }
        })?;

        for line in index_lines {
            if let Some(index_entry) = PlainTextJsonFormat::deserialize_record(line.as_bytes())? {
                if let Some((data_offset, block_entry)) =
                    DataBlockIndexEntry::from_index_entry(&index_entry)
                {
                    data_block_journal.insert(data_offset, block_entry);
                }
            }
        }

        for (data_offset, block_entry) in data_block_journal {
            if block_entry.status == DATA_BLOCK_STATUS_DEAD {
                self.data_recycler
                    .register_free_slot(block_entry.size_class, data_offset);
            }
        }

        self.data_recycler_cache_initialized = true;
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

    fn index_entry_value(
        field: &str,
        key: &str,
        offset_value: Value,
        block_entry: Option<DataBlockIndexEntry>,
    ) -> Value {
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

        index_entry
    }

    fn append_compact_payload(target: &mut Vec<u8>, payload: &[u8]) -> u64 {
        let starting_offset = target.len() as u64;
        target.extend_from_slice(payload);
        target.push(b'\n');
        starting_offset
    }

    fn append_compact_index_entry(
        target: &mut Vec<u8>,
        index_entry: &Value,
    ) -> std::io::Result<(u64, usize)> {
        let starting_offset = target.len() as u64;
        let serialized_index = PlainTextJsonFormat::serialize_record(index_entry)?;
        target.extend_from_slice(&serialized_index);
        target.push(b'\n');

        Ok((starting_offset, serialized_index.len() + 1))
    }

    pub fn find_all_records(&self) -> std::io::Result<Vec<Value>> {
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

    pub fn find_all_records_with_migration(&mut self) -> std::io::Result<Vec<Value>> {
        if !self.needs_format_migration() {
            return self.find_all_records();
        }

        let mut live_offsets = self
            .primary_memory_index
            .values()
            .copied()
            .collect::<Vec<_>>();
        live_offsets.sort_unstable();
        live_offsets.dedup();

        let mut live_records = Vec::new();
        for offset in live_offsets {
            if let Some(record) = self.read_record_at_offset_with_lazy_migration(offset)? {
                live_records.push(record);
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
            self.schema.clone(),
        );
        metadata.persist(&self.meta_file_path)
    }
}
