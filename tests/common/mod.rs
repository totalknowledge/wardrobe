use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct TempDatabase {
    pub path: PathBuf,
}

impl TempDatabase {
    pub fn new(test_name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();

        Self {
            path: std::env::temp_dir().join(format!("wardrobe_{test_name}_{nanos}")),
        }
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
