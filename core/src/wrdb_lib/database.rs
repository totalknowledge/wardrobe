use crate::wrdb_lib::core::writer::DatabaseWriter;
use crate::wrdb_lib::drawer::Drawer;
use crate::wrdb_lib::pointer;
use crate::wrdb_lib::reverse_relationships::{
    REVERSE_RELATIONSHIP_INDEX_FILE_NAME, ReverseRelationshipEntry, ReverseRelationshipIndex,
};
use crate::wrdb_lib::wal::{DurabilityPolicy, WalJournal};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

const DEFAULT_WAL_SIZE_THRESHOLD: u64 = 1_048_576;
const DEFAULT_WAL_OPS_THRESHOLD: u64 = 1000;

struct CachedDrawer {
    drawer: Arc<RwLock<Drawer>>,
    last_access_tick: AtomicU64,
}

impl CachedDrawer {
    fn new(drawer: Drawer, access_tick: u64) -> Self {
        Self {
            drawer: Arc::new(RwLock::new(drawer)),
            last_access_tick: AtomicU64::new(access_tick),
        }
    }

    fn touch(&self, access_tick: u64) {
        self.last_access_tick.store(access_tick, Ordering::Relaxed);
    }

    fn last_access_tick(&self) -> u64 {
        self.last_access_tick.load(Ordering::Relaxed)
    }
}

pub struct Database {
    storage_directory: PathBuf,
    active_drawers: HashMap<String, CachedDrawer>,
    reverse_relationship_index: ReverseRelationshipIndex,
    reverse_relationship_writer: DatabaseWriter,
    reverse_relationship_index_available: bool,
    reverse_relationship_index_dirty: bool,
    mutated_drawers: Mutex<HashSet<String>>,
    max_cached_drawers: Option<usize>,
    access_clock: AtomicU64,
    wal_bytes_since_checkpoint: AtomicU64,
    wal_ops_since_checkpoint: AtomicU64,
    wal_size_threshold_bytes: u64,
    wal_ops_threshold_count: u64,
    durability_policy: DurabilityPolicy,
    pub(crate) wal_journal: WalJournal,
}

impl Database {
    pub fn record_wal_activity(&self, bytes: u64, ops: u64) {
        self.wal_bytes_since_checkpoint
            .fetch_add(bytes, Ordering::Relaxed);
        self.wal_ops_since_checkpoint
            .fetch_add(ops, Ordering::Relaxed);
    }

    pub fn get_wal_counters(&self) -> (u64, u64) {
        (
            self.wal_bytes_since_checkpoint.load(Ordering::Relaxed),
            self.wal_ops_since_checkpoint.load(Ordering::Relaxed),
        )
    }

    pub fn reset_wal_counters(&self) {
        self.wal_bytes_since_checkpoint.store(0, Ordering::Relaxed);
        self.wal_ops_since_checkpoint.store(0, Ordering::Relaxed);
    }

    pub fn wal_thresholds(&self) -> (u64, u64) {
        (self.wal_size_threshold_bytes, self.wal_ops_threshold_count)
    }

    pub fn durability_policy(&self) -> DurabilityPolicy {
        self.durability_policy.clone()
    }

    pub fn default_wal_thresholds() -> (u64, u64) {
        (DEFAULT_WAL_SIZE_THRESHOLD, DEFAULT_WAL_OPS_THRESHOLD)
    }

    pub fn initialize<P: AsRef<Path>>(directory_path: P) -> std::io::Result<Self> {
        Self::initialize_with_cache_limit(directory_path, None)
    }

    pub fn initialize_with_cache_limit<P: AsRef<Path>>(
        directory_path: P,
        max_cached_drawers: Option<usize>,
    ) -> std::io::Result<Self> {
        Self::initialize_with_cache_limit_and_wal_thresholds(
            directory_path,
            max_cached_drawers,
            DEFAULT_WAL_SIZE_THRESHOLD,
            DEFAULT_WAL_OPS_THRESHOLD,
        )
    }

    pub fn initialize_with_wal_thresholds<P: AsRef<Path>>(
        directory_path: P,
        wal_size_threshold_bytes: u64,
        wal_ops_threshold_count: u64,
    ) -> std::io::Result<Self> {
        Self::initialize_with_cache_limit_and_wal_thresholds(
            directory_path,
            None,
            wal_size_threshold_bytes,
            wal_ops_threshold_count,
        )
    }

    pub fn initialize_with_cache_limit_and_wal_thresholds<P: AsRef<Path>>(
        directory_path: P,
        max_cached_drawers: Option<usize>,
        wal_size_threshold_bytes: u64,
        wal_ops_threshold_count: u64,
    ) -> std::io::Result<Self> {
        Self::initialize_with_cache_limit_wal_thresholds_and_durability(
            directory_path,
            max_cached_drawers,
            wal_size_threshold_bytes,
            wal_ops_threshold_count,
            DurabilityPolicy::Strict,
        )
    }

    pub fn initialize_with_cache_limit_wal_thresholds_and_durability<P: AsRef<Path>>(
        directory_path: P,
        max_cached_drawers: Option<usize>,
        wal_size_threshold_bytes: u64,
        wal_ops_threshold_count: u64,
        durability_policy: DurabilityPolicy,
    ) -> std::io::Result<Self> {
        if max_cached_drawers == Some(0) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Drawer cache limit must be greater than zero",
            ));
        }
        if wal_size_threshold_bytes == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "WAL size checkpoint threshold must be greater than zero",
            ));
        }
        if wal_ops_threshold_count == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "WAL operation checkpoint threshold must be greater than zero",
            ));
        }

        let storage_directory = directory_path.as_ref().to_path_buf();
        if !storage_directory.exists() {
            std::fs::create_dir_all(&storage_directory)?;
        }

        let reverse_relationship_index_path =
            storage_directory.join(REVERSE_RELATIONSHIP_INDEX_FILE_NAME);
        let reverse_relationship_index_available = reverse_relationship_index_path.exists();
        let reverse_relationship_index =
            ReverseRelationshipIndex::load(&reverse_relationship_index_path)?;
        let reverse_relationship_writer =
            DatabaseWriter::open_drawer(&reverse_relationship_index_path)?;

        let wal_journal =
            WalJournal::at_database_path_with_policy(&storage_directory, durability_policy.clone());

        let mut database = Self {
            storage_directory,
            active_drawers: HashMap::new(),
            reverse_relationship_index,
            reverse_relationship_writer,
            reverse_relationship_index_available,
            reverse_relationship_index_dirty: false,
            mutated_drawers: Mutex::new(HashSet::new()),
            max_cached_drawers,
            access_clock: AtomicU64::new(0),
            wal_bytes_since_checkpoint: AtomicU64::new(0),
            wal_ops_since_checkpoint: AtomicU64::new(0),
            wal_size_threshold_bytes,
            wal_ops_threshold_count,
            durability_policy,
            wal_journal,
        };

        if !database.reverse_relationship_index_available {
            database.rebuild_reverse_relationship_index_from_disk()?;
        }

        Ok(database)
    }

    pub fn load_drawer(
        &mut self,
        drawer_name: &str,
        primary_key: &str,
        unique_constraints: Vec<String>,
    ) -> std::io::Result<()> {
        if self.active_drawers.contains_key(drawer_name) {
            self.touch_drawer(drawer_name);
            return Ok(());
        }

        let initiated_drawer = Drawer::open(
            &self.storage_directory,
            drawer_name,
            primary_key,
            unique_constraints,
        )?;
        let access_tick = self.next_access_tick();
        self.active_drawers.insert(
            drawer_name.to_string(),
            CachedDrawer::new(initiated_drawer, access_tick),
        );
        self.evict_lru_drawers(Some(drawer_name));

        Ok(())
    }

    fn drawer_data_file_path(&self, drawer_name: &str) -> PathBuf {
        self.storage_directory.join(format!("{}.drw", drawer_name))
    }

    fn drawer_index_file_path(&self, drawer_name: &str) -> PathBuf {
        self.storage_directory
            .join(format!("{}_index.drw", drawer_name))
    }

    pub fn storage_directory_path(&self) -> PathBuf {
        self.storage_directory.clone()
    }

    pub(crate) fn reverse_relationship_index_available(&self) -> bool {
        self.reverse_relationship_index_available
    }

    pub(crate) fn reverse_relationships_for_parent(
        &self,
        parent_pointer: &str,
    ) -> Vec<ReverseRelationshipEntry> {
        self.reverse_relationship_index
            .references_for_parent(parent_pointer)
    }

    pub(crate) fn replace_reverse_relationships_for_record(
        &mut self,
        child_drawer: &str,
        child_id: &str,
        record: &Value,
        relationship_constraints: &BTreeMap<String, Value>,
        delete_rules: &BTreeMap<String, Value>,
    ) -> std::io::Result<()> {
        self.reverse_relationship_index.replace_record(
            child_drawer,
            child_id,
            record,
            relationship_constraints,
            delete_rules,
        );
        self.reverse_relationship_index_dirty = true;
        Ok(())
    }

    pub(crate) fn replace_reverse_relationships_for_records(
        &mut self,
        child_drawer: &str,
        records: &[(String, Value)],
        relationship_constraints: &BTreeMap<String, Value>,
        delete_rules: &BTreeMap<String, Value>,
    ) -> std::io::Result<()> {
        for (child_id, record) in records {
            self.reverse_relationship_index.replace_record(
                child_drawer,
                child_id,
                record,
                relationship_constraints,
                delete_rules,
            );
        }
        self.reverse_relationship_index_dirty = true;
        Ok(())
    }

    pub(crate) fn remove_reverse_relationships_for_record_keys(
        &mut self,
        child_drawer: &str,
        child_ids: &[String],
    ) -> std::io::Result<()> {
        let mut changed = false;
        for child_id in child_ids {
            changed |= self
                .reverse_relationship_index
                .remove_child(child_drawer, child_id);
        }

        if changed {
            self.reverse_relationship_index_dirty = true;
        }

        Ok(())
    }

    pub(crate) fn persist_reverse_relationship_index(&mut self) -> std::io::Result<()> {
        if self.reverse_relationship_index_dirty {
            let bytes = serde_json::to_vec_pretty(&self.reverse_relationship_index)?;
            self.reverse_relationship_writer.rewrite_all(&bytes)?;
            self.reverse_relationship_index_dirty = false;
        }
        self.reverse_relationship_index_available = true;
        Ok(())
    }

    fn rebuild_reverse_relationship_index_from_disk(&mut self) -> std::io::Result<()> {
        self.reverse_relationship_index.clear();

        for drawer_name in self.drawer_names_on_disk()? {
            let mut drawer =
                match Drawer::open(&self.storage_directory, &drawer_name, "_id", Vec::new()) {
                    Ok(drawer) => drawer,
                    Err(error) if error.kind() == ErrorKind::InvalidData => {
                        self.reverse_relationship_index.clear();
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                };
            let relationship_constraints = drawer.relationship_constraints();
            let delete_rules = drawer.delete_rules();

            let records = match drawer.find_all_records_with_migration() {
                Ok(records) => records,
                Err(error) if error.kind() == ErrorKind::InvalidData => {
                    self.reverse_relationship_index.clear();
                    return Ok(());
                }
                Err(error) => return Err(error),
            };

            for record in records {
                let Some(record_key) = record
                    .get("_id")
                    .and_then(Value::as_str)
                    .map(pointer::clean_primary_key_token)
                else {
                    continue;
                };

                self.reverse_relationship_index.add_record(
                    &drawer_name,
                    &record_key,
                    &record,
                    &relationship_constraints,
                    &delete_rules,
                );
            }
        }

        self.reverse_relationship_index_dirty = true;
        self.persist_reverse_relationship_index()
    }

    pub(crate) fn mark_drawer_mutated(&self, name: &str) {
        if let Ok(mut guard) = self.mutated_drawers.lock() {
            guard.insert(name.to_string());
        }
    }

    pub(crate) fn take_mutated_drawers(&self) -> HashSet<String> {
        if let Ok(mut guard) = self.mutated_drawers.lock() {
            std::mem::take(&mut *guard)
        } else {
            HashSet::new()
        }
    }

    pub(crate) fn flush_all_drawers_metadata(&self) -> std::io::Result<()> {
        let drawers = self.get_all_drawers();
        for (_name, drawer) in drawers {
            let mut guard = drawer
                .write()
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "lock poisoned"))?;
            guard.flush_metadata_if_dirty()?;
        }
        Ok(())
    }

    pub fn get_drawer(&self, name: &str) -> Option<Arc<RwLock<Drawer>>> {
        self.active_drawers
            .get(name)
            .map(|cached| cached.drawer.clone())
    }

    fn drawer_names_on_disk(&self) -> std::io::Result<Vec<String>> {
        let mut drawer_names = Vec::new();

        for entry_result in std::fs::read_dir(&self.storage_directory)? {
            let entry = entry_result?;
            let path = entry.path();

            if path.extension().and_then(|extension| extension.to_str()) != Some("drw") {
                continue;
            }

            let Some(drawer_name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };

            if drawer_name != ".catalog"
                && !drawer_name.ends_with("_index")
                && !drawer_name.ends_with("_meta")
            {
                drawer_names.push(drawer_name.to_string());
            }
        }

        drawer_names.sort();
        Ok(drawer_names)
    }

    pub fn active_drawer_or_load_from_disk(
        &mut self,
        drawer_name: &str,
        primary_key: &str,
        unique_constraints: Vec<String>,
    ) -> std::io::Result<Option<Arc<RwLock<Drawer>>>> {
        if self.active_drawers.contains_key(drawer_name) {
            return Ok(self.use_drawer(drawer_name));
        }

        if !self.active_drawers.contains_key(drawer_name) {
            let data_file = self.drawer_data_file_path(drawer_name);
            let index_file = self.drawer_index_file_path(drawer_name);

            if !(data_file.exists() && index_file.exists()) {
                return Ok(None);
            }

            self.load_drawer(drawer_name, primary_key, unique_constraints)?;
        }

        Ok(self.use_drawer(drawer_name))
    }

    pub fn load_existing_drawers(
        &mut self,
        primary_key: &str,
        default_constraints: HashMap<String, Vec<String>>,
    ) -> std::io::Result<()> {
        let mut drawer_names = Vec::new();

        for entry_result in std::fs::read_dir(&self.storage_directory)? {
            let entry = entry_result?;
            let path = entry.path();

            if path.extension().and_then(|extension| extension.to_str()) != Some("drw") {
                continue;
            }

            let Some(drawer_name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };

            if drawer_name != ".catalog"
                && !drawer_name.ends_with("_index")
                && !drawer_name.ends_with("_meta")
            {
                drawer_names.push(drawer_name.to_string());
            }
        }

        for drawer_name in drawer_names {
            let constraints = default_constraints
                .get(&drawer_name)
                .cloned()
                .unwrap_or_default();
            self.load_drawer(&drawer_name, primary_key, constraints)?;
        }

        Ok(())
    }

    pub fn use_drawer(&self, drawer_name: &str) -> Option<Arc<RwLock<Drawer>>> {
        let drawer = self.active_drawers.get(drawer_name)?;
        drawer.touch(self.next_access_tick());
        Some(drawer.drawer.clone())
    }

    pub fn close_drawer(&mut self, drawer_name: &str) {
        self.active_drawers.remove(drawer_name);
    }

    pub fn cached_drawer_count(&self) -> usize {
        self.active_drawers.len()
    }

    pub fn get_all_drawers(&self) -> HashMap<String, Arc<RwLock<Drawer>>> {
        let mut registry = HashMap::new();
        for (drawer_name, drawer_instance) in &self.active_drawers {
            registry.insert(drawer_name.clone(), drawer_instance.drawer.clone());
        }
        registry
    }

    fn next_access_tick(&self) -> u64 {
        self.access_clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn touch_drawer(&self, drawer_name: &str) {
        if let Some(drawer) = self.active_drawers.get(drawer_name) {
            drawer.touch(self.next_access_tick());
        }
    }

    fn evict_lru_drawers(&mut self, protected_drawer: Option<&str>) {
        let Some(max_cached_drawers) = self.max_cached_drawers else {
            return;
        };

        while self.active_drawers.len() > max_cached_drawers {
            let candidate_name = self
                .active_drawers
                .iter()
                .filter(|(drawer_name, cached_drawer)| {
                    Some(drawer_name.as_str()) != protected_drawer
                        && Arc::strong_count(&cached_drawer.drawer) == 1
                })
                .min_by_key(|(_, cached_drawer)| cached_drawer.last_access_tick())
                .map(|(drawer_name, _)| drawer_name.clone());

            let Some(candidate_name) = candidate_name else {
                break;
            };

            self.active_drawers.remove(&candidate_name);
        }
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = self.persist_reverse_relationship_index();
    }
}
