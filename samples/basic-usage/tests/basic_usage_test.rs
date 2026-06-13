use std::env;
use std::fs;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_core::WardrobeClient;

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
fn sample_runs_extended_lifecycle_and_cleans_related_records() {
    let _guard = cwd_lock().lock().expect("cwd lock should not be poisoned");
    let previous_directory = env::current_dir().expect("cwd should be readable");
    let working_directory = temp_working_directory("sample_runs_extended_lifecycle");
    fs::create_dir_all(&working_directory).expect("temp dir should create");

    env::set_current_dir(&working_directory).expect("cwd should change");
    let output = Command::new(env!("CARGO_BIN_EXE_basic-usage"))
        .output()
        .expect("binary should run");
    env::set_current_dir(previous_directory).expect("cwd should restore");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(working_directory.join("wardrobe/basic-usage/public/user.drw").is_file());
    assert!(working_directory.join("wardrobe/basic-usage/public/gem.drw").is_file());
    assert!(working_directory.join("wardrobe/basic-usage/public/weapon.drw").is_file());
    assert!(stdout.contains("Phase 1: Metadata & Inventory Discovery"));
    assert!(stdout.contains("Phase 2: Relational Data Population"));
    assert!(stdout.contains("Phase 3: Filter Query Execution"));
    assert!(stdout.contains("Phase 4: Relation Verification"));
    assert!(stdout.contains("Phase 5: Targeted Lifecycle Cleanup"));
    assert!(stdout.contains("Phase 6: Scoped Maintenance & Stress Test"));
    assert!(stdout.contains("Phase 7: State Reconciliation"));
    assert!(stdout.contains("Drawers in basic-usage/public:"));
    assert!(stdout.contains("Filtered gems for tag match: 1"));
    assert!(stdout.contains("Relation check:"));
    assert!(stdout.contains("Deleted 3 gems linked to user"));

    let root_client = WardrobeClient::open(
        working_directory
            .join("wardrobe")
            .to_str()
            .expect("unicode path"),
    )
    .expect("root client should initialize");
    let drawers = root_client
        .show_drawers("basic-usage", "public")
        .expect("drawer metadata should be readable");
    assert!(drawers.iter().any(|drawer| drawer.name == "user"));
    assert!(drawers.iter().any(|drawer| drawer.name == "gem"));

    let scoped_client = WardrobeClient::open(
        working_directory
            .join("wardrobe/basic-usage/public")
            .to_str()
            .expect("unicode path"),
    )
    .expect("scoped client should initialize");

    let gems = scoped_client
        .find_by_filter(
            "gem",
            serde_json::json!({
                "user_id": "@user:user_001"
            }),
            None,
        )
        .expect("filtered gem lookup should succeed");
    assert!(gems.is_empty());

    let user = scoped_client
        .find_by_id("@user:user_001")
        .expect("user lookup should succeed");
    assert!(user.is_none());
}

#[test]
fn sample_fails_when_storage_root_is_blocked() {
    let _guard = cwd_lock().lock().expect("cwd lock should not be poisoned");
    let previous_directory = env::current_dir().expect("cwd should be readable");
    let working_directory = temp_working_directory("sample_fails_when_storage_root_blocked");
    fs::create_dir_all(&working_directory).expect("temp dir should create");

    let wardrobe_file = working_directory.join("wardrobe");
    fs::write(&wardrobe_file, b"blocked").expect("file should create");

    env::set_current_dir(&working_directory).expect("cwd should change");
    let output = Command::new(env!("CARGO_BIN_EXE_basic-usage"))
        .output()
        .expect("binary should run");
    env::set_current_dir(previous_directory).expect("cwd should restore");

    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());

    let _ = fs::remove_file(wardrobe_file);
    let _ = fs::remove_dir_all(working_directory);
}
