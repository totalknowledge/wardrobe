use crate::wrdb_lib::drawer::Drawer;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

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
    max_cached_drawers: Option<usize>,
    access_clock: AtomicU64,
    wal_bytes_since_checkpoint: AtomicU64,
    wal_ops_since_checkpoint: AtomicU64,
    wal_size_threshold_bytes: u64,
    wal_ops_threshold_count: u64,
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
    pub fn initialize<P: AsRef<Path>>(directory_path: P) -> std::io::Result<Self> {
        Self::initialize_with_cache_limit(directory_path, None)
    }

    pub fn initialize_with_cache_limit<P: AsRef<Path>>(
        directory_path: P,
        max_cached_drawers: Option<usize>,
    ) -> std::io::Result<Self> {
        if max_cached_drawers == Some(0) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Drawer cache limit must be greater than zero",
            ));
        }

        let storage_directory = directory_path.as_ref().to_path_buf();
        if !storage_directory.exists() {
            std::fs::create_dir_all(&storage_directory)?;
        }

        Ok(Self {
            storage_directory,
            active_drawers: HashMap::new(),
            max_cached_drawers,
            access_clock: AtomicU64::new(0),
            wal_bytes_since_checkpoint: AtomicU64::new(0),
            wal_ops_since_checkpoint: AtomicU64::new(0),
            wal_size_threshold_bytes: DEFAULT_WAL_SIZE_THRESHOLD,
            wal_ops_threshold_count: DEFAULT_WAL_OPS_THRESHOLD,
        })
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
