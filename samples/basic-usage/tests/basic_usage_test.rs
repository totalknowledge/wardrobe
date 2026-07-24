use std::env;
use std::fs;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use wardrobe_embedded::{
    OperationFilter, OperationOptions, ReadResult, StatusRequest, StorageInventory, WardrobeClient,
};

fn read_records(client: &WardrobeClient, filter: OperationFilter) -> Vec<serde_json::Value> {
    match client
        .read(filter, None::<OperationOptions>)
        .expect("read should succeed")
    {
        ReadResult::Records(records) => records,
        ReadResult::Page(page) => page.records,
        other => panic!("expected records, got {other:?}"),
    }
}

fn read_record(client: &WardrobeClient, filter: OperationFilter) -> Option<serde_json::Value> {
    match client
        .read(filter, None::<OperationOptions>)
        .expect("read should succeed")
    {
        ReadResult::Record(record) => record,
        other => panic!("expected record, got {other:?}"),
    }
}

fn status_drawers(
    client: &WardrobeClient,
    database_name: &str,
    schema_name: &str,
) -> Vec<StorageInventory> {
    client
        .status(StatusRequest::drawers(database_name, schema_name))
        .expect("status should succeed")
}

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
    assert!(
        working_directory
            .join("wardrobe/publishing-house/public/publisher.drw")
            .is_file()
    );
    assert!(
        working_directory
            .join("wardrobe/publishing-house/public/person.drw")
            .is_file()
    );
    assert!(
        working_directory
            .join("wardrobe/publishing-house/public/book.drw")
            .is_file()
    );
    assert!(stdout.contains("Phase 1: Metadata & Inventory Discovery"));
    assert!(stdout.contains("Phase 2: Relational Data Population"));
    assert!(stdout.contains("Phase 3: Filter Query Execution"));
    assert!(stdout.contains("Phase 4: Relation Verification"));
    assert!(stdout.contains("Phase 5: Maintenance & Stress Test Cycle"));
    assert!(stdout.contains("Phase 6: Detailed Engine Inspection"));
    assert!(stdout.contains("Phase 7: Final State Reconciliation & Integrity"));
    assert!(stdout.contains("Drawers in publishing-house/public:"));
    assert!(stdout.contains("Found 1 matching personnel records:"));
    assert!(stdout.contains("Book lookup check: true"));
    assert!(stdout.contains("Stress test cycle completed (5 temporary book upserts/deletes)."));

    let root_client = WardrobeClient::open(
        working_directory
            .join("wardrobe")
            .to_str()
            .expect("unicode path"),
    )
    .expect("root client should initialize");
    let drawers = status_drawers(&root_client, "publishing-house", "public");
    assert!(drawers.iter().any(|drawer| drawer.name == "publisher"));
    assert!(drawers.iter().any(|drawer| drawer.name == "person"));
    assert!(drawers.iter().any(|drawer| drawer.name == "book"));

    let scoped_client = WardrobeClient::open(
        working_directory
            .join("wardrobe/publishing-house/public")
            .to_str()
            .expect("unicode path"),
    )
    .expect("scoped client should initialize");

    let temporary_books = read_records(
        &scoped_client,
        OperationFilter::query_in(
            "book",
            serde_json::json!({
                "title": "Temporary Draft"
            }),
        ),
    );
    assert!(temporary_books.is_empty());

    let publisher = read_record(
        &scoped_client,
        OperationFilter::pointer("@publisher:pub_001"),
    );
    assert!(publisher.is_some());

    let author = read_record(
        &scoped_client,
        OperationFilter::pointer("@person:author_001"),
    );
    assert!(author.is_some());

    let book = read_record(&scoped_client, OperationFilter::pointer("@book:book_001"));
    assert!(book.is_some());
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
