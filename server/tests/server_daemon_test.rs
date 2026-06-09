use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_storage_directory(test_name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("wardrobe_server_{test_name}_{nanos}"))
}

#[test]
fn daemon_check_initializes_storage_directory_and_exits() {
    let storage_directory = temp_storage_directory("check_initializes_storage");

    let output = Command::new(env!("CARGO_BIN_EXE_wardrobe-server"))
        .arg("--data-dir")
        .arg(&storage_directory)
        .arg("--check")
        .output()
        .expect("server binary should run");

    assert!(output.status.success());
    assert!(storage_directory.is_dir());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Wardrobe daemon initialized"));
    assert!(stdout.contains("Wardrobe daemon check completed"));

    let _ = std::fs::remove_dir_all(storage_directory);
}

#[test]
fn daemon_check_rejects_missing_data_dir_argument() {
    let output = Command::new(env!("CARGO_BIN_EXE_wardrobe-server"))
        .arg("--data-dir")
        .output()
        .expect("server binary should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--data-dir requires a directory path"));
}
