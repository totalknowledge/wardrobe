use serde_json::json;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_core::WardrobeEngine;

fn temp_storage_directory(test_name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("wardrobe_cli_{test_name}_{nanos}"))
}

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_wardrobe-cli"))
        .args(args)
        .output()
        .expect("cli binary should run")
}

#[test]
fn drawers_lists_known_drawers_without_internal_files() {
    let storage_directory = temp_storage_directory("drawers_lists_known_drawers");
    {
        let engine =
            WardrobeEngine::open(&storage_directory.to_string_lossy()).expect("engine should open");
        engine
            .upsert("gem", json!({ "_id": "@gem:lnk_fire", "element": "Fire" }))
            .expect("gem should insert");
        engine
            .upsert(
                "weapon",
                json!({ "_id": "@weapon:lnk_blade", "gem": { "_id": "@gem:lnk_fire" } }),
            )
            .expect("weapon should insert");
    }

    let output = run_cli(&[
        "--data-dir",
        storage_directory.to_str().expect("path should be utf8"),
        "drawers",
    ]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("gem"));
    assert!(stdout.contains("weapon"));
    assert!(!stdout.contains("_index"));
    assert!(!stdout.contains("_meta"));

    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn inspect_reports_drawer_companion_files() {
    let storage_directory = temp_storage_directory("inspect_reports_drawer_files");
    {
        let engine =
            WardrobeEngine::open(&storage_directory.to_string_lossy()).expect("engine should open");
        engine
            .upsert("gem", json!({ "_id": "@gem:lnk_fire", "element": "Fire" }))
            .expect("gem should insert");
    }

    let output = run_cli(&[
        "--data-dir",
        storage_directory.to_str().expect("path should be utf8"),
        "inspect",
        "gem",
    ]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Drawer: gem"));
    assert!(stdout.contains("data: present"));
    assert!(stdout.contains("index: present"));
    assert!(stdout.contains("meta: present"));

    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn records_prints_hydrated_drawer_records() {
    let storage_directory = temp_storage_directory("records_prints_records");
    {
        let engine =
            WardrobeEngine::open(&storage_directory.to_string_lossy()).expect("engine should open");
        engine
            .upsert("gem", json!({ "_id": "@gem:lnk_fire", "element": "Fire" }))
            .expect("gem should insert");
        engine
            .upsert(
                "weapon",
                json!({ "_id": "@weapon:lnk_blade", "gem": { "_id": "@gem:lnk_fire" } }),
            )
            .expect("weapon should insert");
    }

    let output = run_cli(&[
        "--data-dir",
        storage_directory.to_str().expect("path should be utf8"),
        "records",
        "weapon",
    ]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"_id\": \"@weapon:lnk_blade\""));
    assert!(stdout.contains("\"gem\""));
    assert!(stdout.contains("\"element\": \"Fire\""));

    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn diagnose_reports_ok_for_structurally_complete_drawers() {
    let storage_directory = temp_storage_directory("diagnose_reports_ok");
    {
        let engine =
            WardrobeEngine::open(&storage_directory.to_string_lossy()).expect("engine should open");
        engine
            .upsert("gem", json!({ "_id": "@gem:lnk_fire", "element": "Fire" }))
            .expect("gem should insert");
    }

    let output = run_cli(&[
        "--data-dir",
        storage_directory.to_str().expect("path should be utf8"),
        "diagnose",
    ]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Drawer count: 1"));
    assert!(stdout.contains("Status: ok"));

    let _ = std::fs::remove_dir_all(storage_directory);
}
