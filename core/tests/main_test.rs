use std::env;
use std::fs;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_core::WardrobeEngine;

fn cwd_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn temp_working_directory(test_name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("wardrobe_{test_name}_{nanos}"))
}

#[test]
fn main_creates_demo_storage_in_current_directory() {
    let _guard = cwd_lock().lock().expect("cwd lock should not be poisoned");
    let previous_directory = env::current_dir().expect("cwd should be readable");
    let working_directory = temp_working_directory("main_creates_demo_storage");
    fs::create_dir_all(&working_directory).expect("temp dir should create");

    env::set_current_dir(&working_directory).expect("cwd should change");
    let output = Command::new(env!("CARGO_BIN_EXE_wardrobe"))
        .output()
        .expect("binary should run");
    env::set_current_dir(previous_directory).expect("cwd should restore");

    assert!(output.status.success());
    assert!(working_directory.join("wardrobe").is_dir());
    assert!(working_directory.join("wardrobe/gem.drw").is_file());
    assert!(working_directory.join("wardrobe/weapon.drw").is_file());
    assert!(working_directory.join("wardrobe/character.drw").is_file());
}

#[test]
fn main_prints_existing_weapon_and_character_drawers() {
    let _guard = cwd_lock().lock().expect("cwd lock should not be poisoned");
    let previous_directory = env::current_dir().expect("cwd should be readable");
    let working_directory = temp_working_directory("main_prints_existing_drawers");
    fs::create_dir_all(&working_directory).expect("temp dir should create");
    let storage_directory = working_directory.join("wardrobe");

    {
        let mut engine = WardrobeEngine::new(storage_directory.to_str().expect("unicode path"))
            .expect("engine should initialize");
        engine
            .upsert(
                "weapon",
                serde_json::json!({
                    "_id": "@weapon:lnk_main_weapon",
                    "name": "Halberd"
                }),
            )
            .expect("weapon should upsert");
        engine
            .upsert(
                "character",
                serde_json::json!({
                    "_id": "@character:lnk_main_character",
                    "name": "Mira"
                }),
            )
            .expect("character should upsert");
    }

    env::set_current_dir(&working_directory).expect("cwd should change");
    let output = Command::new(env!("CARGO_BIN_EXE_wardrobe"))
        .output()
        .expect("binary should run");
    env::set_current_dir(previous_directory).expect("cwd should restore");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("Axe"));
    assert!(stdout.contains("Halberd"));
    assert!(stdout.contains("Gorthor"));
    assert!(stdout.contains("Mira"));
}

#[test]
fn main_handles_startup_write_failure_with_empty_reads() {
    let _guard = cwd_lock().lock().expect("cwd lock should not be poisoned");
    let previous_directory = env::current_dir().expect("cwd should be readable");
    let working_directory = temp_working_directory("main_handles_startup_write_failure");
    fs::create_dir_all(&working_directory).expect("temp dir should create");

    let wardrobe_file = working_directory.join("wardrobe");
    fs::write(&wardrobe_file, b"blocked").expect("file should create");

    env::set_current_dir(&working_directory).expect("cwd should change");
    let output = Command::new(env!("CARGO_BIN_EXE_wardrobe"))
        .output()
        .expect("binary should run");
    env::set_current_dir(previous_directory).expect("cwd should restore");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("(Gems drawer is currently empty)"));

    let _ = fs::remove_file(wardrobe_file);
    let _ = fs::remove_dir_all(working_directory);
}
