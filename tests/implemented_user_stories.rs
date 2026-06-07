use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe::{Database, WardrobeEngine};

struct TempDatabase {
    path: PathBuf,
}

impl TempDatabase {
    fn new(test_name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();

        Self {
            path: std::env::temp_dir().join(format!("wardrobe_{test_name}_{nanos}")),
        }
    }

    fn as_str(&self) -> &str {
        self.path
            .to_str()
            .expect("temporary path should be valid unicode")
    }

    fn join<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        self.path.join(path)
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn us_001_database_initialization_creates_storage_directory() {
    let database = TempDatabase::new("us_001");

    assert!(!database.path.exists());
    Database::initialize(&database.path).expect("database should initialize");

    assert!(database.path.is_dir());
}

#[test]
fn us_002_opening_a_drawer_creates_data_and_index_files() {
    let database = TempDatabase::new("us_002");
    let mut database_core =
        Database::initialize(&database.path).expect("database should initialize");

    database_core
        .load_drawer("gem", "_id", Vec::new())
        .expect("drawer should load");

    assert!(database.join("gem.drw").is_file());
    assert!(database.join("gem_index.drw").is_file());
}

#[test]
fn us_003_004_005_upsert_generates_ids_and_supports_primary_lookup() {
    let database = TempDatabase::new("us_003_004_005");
    let mut engine = WardrobeEngine::new(database.as_str()).expect("engine should initialize");

    let generated_id = engine
        .upsert(
            "gem",
            json!({
                "element": "Fire",
                "potency": 5000
            }),
        )
        .expect("record should upsert");

    assert!(generated_id.starts_with("@gem:lnk_"));

    let found = engine
        .find_by_id(&generated_id)
        .expect("primary lookup should succeed")
        .expect("record should be found");

    assert_eq!(found["element"].as_str(), Some("Fire"));
    assert_eq!(found["potency"].as_u64(), Some(5000));
}

#[test]
fn us_006_tombstoned_slots_are_recycled_by_size_class() {
    let database = TempDatabase::new("us_006");
    let mut engine = WardrobeEngine::new(database.as_str()).expect("engine should initialize");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_recycle_a",
                "element": "Air",
                "potency": 1
            }),
        )
        .expect("first record should upsert");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_recycle_a",
                "element": "Thunderstorm",
                "potency": 999999
            }),
        )
        .expect("replacement record should upsert");

    let tombstoned_file = fs::read_to_string(database.join("gem.drw"))
        .expect("data file should be readable after replacement");
    assert!(tombstoned_file.contains("!!DEAD!!"));

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_recycle_b",
                "element": "Ice",
                "potency": 2
            }),
        )
        .expect("same-size record should reuse the tombstoned slot");

    let recycled_file =
        fs::read_to_string(database.join("gem.drw")).expect("data file should be readable");
    let first_line = recycled_file.lines().next().unwrap_or_default();

    assert!(first_line.contains("@gem:lnk_recycle_b"));
    assert!(!first_line.starts_with("!!DEAD!!"));
}

#[test]
fn us_007_find_all_returns_only_live_records() {
    let database = TempDatabase::new("us_007");
    let mut engine = WardrobeEngine::new(database.as_str()).expect("engine should initialize");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_live_only",
                "element": "Air",
                "potency": 1
            }),
        )
        .expect("first record should upsert");

    engine
        .upsert(
            "gem",
            json!({
                "_id": "@gem:lnk_live_only",
                "element": "Earthquake",
                "potency": 200
            }),
        )
        .expect("updated record should upsert");

    let gems = engine.find_all("gem").expect("find_all should succeed");

    assert_eq!(gems.len(), 1);
    assert_eq!(gems[0]["element"].as_str(), Some("Earthquake"));
}

#[test]
fn us_008_009_010_pointers_nested_objects_and_hydration_work_together() {
    let database = TempDatabase::new("us_008_009_010");
    let mut engine = WardrobeEngine::new(database.as_str()).expect("engine should initialize");

    engine
        .upsert(
            "character",
            json!({
                "_id": "@character:lnk_nested_character",
                "name": "Ada",
                "weapon": {
                    "_id": "@weapon:lnk_nested_weapon",
                    "name": "Hammer",
                    "damage": 77,
                    "gem": {
                        "_id": "@gem:lnk_nested_gem",
                        "element": "Light",
                        "potency": 9001
                    }
                }
            }),
        )
        .expect("complex character should upsert");

    let characters = engine
        .find_all("character")
        .expect("characters should be found");

    assert_eq!(characters.len(), 1);
    assert_eq!(characters[0]["name"].as_str(), Some("Ada"));
    assert_eq!(characters[0]["weapon"]["name"].as_str(), Some("Hammer"));
    assert_eq!(
        characters[0]["weapon"]["gem"]["element"].as_str(),
        Some("Light")
    );

    let weapon_file =
        fs::read_to_string(database.join("weapon.drw")).expect("weapon drawer should exist");
    let character_file =
        fs::read_to_string(database.join("character.drw")).expect("character drawer should exist");

    assert!(weapon_file.contains("@gem:lnk_nested_gem"));
    assert!(character_file.contains("@weapon:lnk_nested_weapon"));
}

#[test]
fn us_011_demo_domain_files_include_expected_drawers() {
    let demo_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("wardrobe");

    for file_name in [
        "gem.drw",
        "gem_index.drw",
        "weapon.drw",
        "weapon_index.drw",
        "character.drw",
        "character_index.drw",
    ] {
        assert!(
            demo_directory.join(file_name).is_file(),
            "expected demo file {file_name}"
        );
    }

    let gems = fs::read_to_string(demo_directory.join("gem.drw")).expect("gems should be readable");
    let weapons =
        fs::read_to_string(demo_directory.join("weapon.drw")).expect("weapons should be readable");
    let characters = fs::read_to_string(demo_directory.join("character.drw"))
        .expect("characters should be readable");

    assert!(gems.contains("\"element\":\"Fire\""));
    assert!(weapons.contains("\"name\":\"Axe\""));
    assert!(characters.contains("\"name\":\"Gorthor\""));
}

#[test]
fn us_012_indexes_rebuild_from_disk_after_restart() {
    let database = TempDatabase::new("us_012");
    let record_id = "@gem:lnk_restart_gem";

    {
        let mut engine = WardrobeEngine::new(database.as_str()).expect("engine should initialize");
        engine
            .upsert(
                "gem",
                json!({
                    "_id": record_id,
                    "element": "Water",
                    "potency": 7300
                }),
            )
            .expect("record should upsert");
    }

    let mut restarted_engine =
        WardrobeEngine::new(database.as_str()).expect("engine should reinitialize");
    let found = restarted_engine
        .find_by_id(record_id)
        .expect("lookup should use rebuilt index")
        .expect("record should be found after restart");

    assert_eq!(found["element"].as_str(), Some("Water"));
    assert_eq!(found["potency"].as_u64(), Some(7300));
}
