use crate::wrdb_lib::drawer::Drawer;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Database {
    storage_directory: PathBuf,
    active_drawers: HashMap<String, Drawer>,
}

impl Database {
    pub fn initialize<P: AsRef<Path>>(directory_path: P) -> std::io::Result<Self> {
        let storage_directory = directory_path.as_ref().to_path_buf();
        if !storage_directory.exists() {
            std::fs::create_dir_all(&storage_directory)?;
        }

        Ok(Self {
            storage_directory,
            active_drawers: HashMap::new(),
        })
    }

    pub fn load_drawer(
        &mut self,
        drawer_name: &str,
        primary_key: &str,
        unique_constraints: Vec<String>,
    ) -> std::io::Result<()> {
        if !self.active_drawers.contains_key(drawer_name) {
            let initiated_drawer = Drawer::open(
                &self.storage_directory,
                drawer_name,
                primary_key,
                unique_constraints,
            )?;
            self.active_drawers
                .insert(drawer_name.to_string(), initiated_drawer);
        }
        Ok(())
    }

    fn drawer_data_file_path(&self, drawer_name: &str) -> PathBuf {
        self.storage_directory.join(format!("{}.drw", drawer_name))
    }

    fn drawer_index_file_path(&self, drawer_name: &str) -> PathBuf {
        self.storage_directory
            .join(format!("{}_index.drw", drawer_name))
    }

    pub fn active_drawer_or_load_from_disk(
        &mut self,
        drawer_name: &str,
        primary_key: &str,
        unique_constraints: Vec<String>,
    ) -> std::io::Result<Option<&mut Drawer>> {
        if !self.active_drawers.contains_key(drawer_name) {
            let data_file = self.drawer_data_file_path(drawer_name);
            let index_file = self.drawer_index_file_path(drawer_name);

            if !(data_file.exists() && index_file.exists()) {
                return Ok(None);
            }

            self.load_drawer(drawer_name, primary_key, unique_constraints)?;
        }

        Ok(self.active_drawers.get_mut(drawer_name))
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

            if !drawer_name.ends_with("_index") {
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

    pub fn use_drawer(&mut self, drawer_name: &str) -> Option<&mut Drawer> {
        self.active_drawers.get_mut(drawer_name)
    }

    pub fn close_drawer(&mut self, drawer_name: &str) {
        self.active_drawers.remove(drawer_name);
    }

    pub fn get_all_drawers(&mut self) -> HashMap<String, &mut Drawer> {
        let mut registry = HashMap::new();
        for (drawer_name, drawer_instance) in self.active_drawers.iter_mut() {
            registry.insert(drawer_name.clone(), drawer_instance);
        }
        registry
    }
}
