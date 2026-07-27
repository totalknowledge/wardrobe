use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{SystemTime, UNIX_EPOCH};

static COMMAND_TEST_LOCK: Mutex<()> = Mutex::new(());

struct NoopWaker;

impl Wake for NoopWaker {
    fn wake(self: Arc<Self>) {}
}

fn block_on<TFuture>(future: TFuture) -> TFuture::Output
where
    TFuture: Future,
{
    let waker = Waker::from(Arc::new(NoopWaker));
    let mut context = Context::from_waker(&waker);
    let mut future = Pin::from(Box::new(future));

    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

fn temp_database_path(test_name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();

    std::env::temp_dir().join(format!("armoire_{test_name}_{nanos}"))
}

fn isolated_test(test_name: &str) -> (MutexGuard<'static, ()>, std::path::PathBuf) {
    let guard = COMMAND_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let metadata_path = temp_database_path(&format!("{test_name}_metadata"));
    std::fs::create_dir_all(&metadata_path).expect("metadata directory should create");
    std::env::set_var("ARMOIRE_METADATA_DIR", &metadata_path);
    (guard, metadata_path)
}

fn clean_up(paths: &[&std::path::Path]) {
    std::env::remove_var("ARMOIRE_METADATA_DIR");
    for path in paths {
        let _ = std::fs::remove_dir_all(path);
    }
}

fn missing_relative_database_name(test_name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();

    format!("armoire_missing_{test_name}_{nanos}")
}

#[test]
fn wardrobe_create_source_location_command_initializes_database_directory() {
    let (_guard, metadata_path) = isolated_test("create_source_location_command");
    let path = temp_database_path("create_source_location_command");
    let result = block_on(
        armoire_lib::commands::wardrobe::wardrobe_create_source_location(
            path.to_string_lossy().into_owned(),
        ),
    )
    .expect("command should create source location");

    let created_path = std::path::PathBuf::from(result);
    assert!(created_path.exists());
    assert!(created_path.is_dir());

    clean_up(&[created_path.as_path(), metadata_path.as_path()]);
}

#[test]
fn wardrobe_test_database_access_command_accepts_initialized_directory() {
    let (_guard, metadata_path) = isolated_test("test_database_access_command");
    let path = temp_database_path("test_database_access_command");
    let source_location = block_on(
        armoire_lib::commands::wardrobe::wardrobe_create_source_location(
            path.to_string_lossy().into_owned(),
        ),
    )
    .expect("source location should initialize");

    block_on(
        armoire_lib::commands::wardrobe::wardrobe_test_database_access(source_location.clone()),
    )
    .expect("test connection command should succeed");

    clean_up(&[
        std::path::Path::new(&source_location),
        metadata_path.as_path(),
    ]);
}

#[test]
fn wardrobe_test_database_access_command_reports_missing_directory() {
    let (_guard, metadata_path) = isolated_test("missing_database_access_command");
    let path = missing_relative_database_name("database_access_command");

    let error = block_on(armoire_lib::commands::wardrobe::wardrobe_test_database_access(path))
        .expect_err("missing directory should fail");

    assert!(error.contains("was not found"));
    clean_up(&[metadata_path.as_path()]);
}

#[test]
fn wardrobe_commands_cover_database_and_connection_lifecycle() {
    use armoire_lib::commands::wardrobe;

    let (_guard, metadata_path) = isolated_test("command_lifecycle");
    let database_path = temp_database_path("command_lifecycle");
    let target = database_path.to_string_lossy().into_owned();

    let created_path = block_on(wardrobe::wardrobe_create_source_location(target.clone()))
        .expect("source location should create");
    block_on(wardrobe::wardrobe_connect_source_location(
        created_path.clone(),
        Some("Primary".to_string()),
    ))
    .expect("source location should connect");

    assert!(block_on(wardrobe::wardrobe_show_wardrobes())
        .expect("wardrobes should load")
        .is_empty());
    block_on(wardrobe::wardrobe_create_new_wardrobe("closet".to_string()))
        .expect("wardrobe should create");
    let wardrobes = block_on(wardrobe::wardrobe_show_wardrobes()).expect("wardrobes should reload");
    assert_eq!(wardrobes.len(), 1);
    assert_eq!(wardrobes[0].name, "closet");

    assert!(block_on(wardrobe::wardrobe_show_bays("closet".to_string()))
        .expect("bays should load")
        .is_empty());
    block_on(wardrobe::wardrobe_create_new_bay(
        "closet".to_string(),
        "shelf".to_string(),
    ))
    .expect("bay should create");
    assert_eq!(
        block_on(wardrobe::wardrobe_show_bays("closet".to_string())).expect("bays should reload"),
        vec!["shelf"]
    );

    assert!(block_on(wardrobe::wardrobe_show_drawers(
        "closet".to_string(),
        "shelf".to_string(),
    ))
    .expect("drawers should load")
    .is_empty());
    block_on(wardrobe::wardrobe_create_new_drawer(
        "closet".to_string(),
        "shelf".to_string(),
        "shirts".to_string(),
    ))
    .expect("drawer should create");
    let drawers = block_on(wardrobe::wardrobe_show_drawers(
        "closet".to_string(),
        "shelf".to_string(),
    ))
    .expect("drawers should reload");
    assert_eq!(drawers.len(), 1);
    assert_eq!(drawers[0].name, "shirts");

    assert!(block_on(wardrobe::wardrobe_read_records(
        "closet".to_string(),
        "shelf".to_string(),
        "shirts".to_string(),
    ))
    .expect("records should load")
    .is_empty());
    block_on(wardrobe::wardrobe_create_record(
        "closet".to_string(),
        "shelf".to_string(),
        "shirts".to_string(),
        serde_json::json!({"color": "blue"}),
    ))
    .expect("record should create");
    let records = block_on(wardrobe::wardrobe_read_records(
        "closet".to_string(),
        "shelf".to_string(),
        "shirts".to_string(),
    ))
    .expect("records should reload");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["color"], "blue");

    let connections =
        block_on(wardrobe::armoire_get_saved_connections()).expect("connections should load");
    let connection = connections
        .iter()
        .find(|connection| connection["target"] == created_path)
        .expect("created connection should be saved");
    let connection_id = connection["_id"]
        .as_str()
        .expect("connection should have an id")
        .to_string();

    block_on(wardrobe::armoire_update_connection_alias(
        created_path.clone(),
        "Renamed".to_string(),
    ))
    .expect("connection alias should update");
    let connections =
        block_on(wardrobe::armoire_get_saved_connections()).expect("connections should reload");
    assert!(connections
        .iter()
        .any(|connection| connection["alias"] == "Renamed"));

    block_on(wardrobe::armoire_remove_connection(connection_id.clone()))
        .expect("connection should be removed");
    block_on(wardrobe::wardrobe_connect_source_location(
        created_path.clone(),
        None,
    ))
    .expect("source location should reconnect");
    block_on(wardrobe::armoire_delete_connection_files(
        created_path.clone(),
        connection_id,
    ))
    .expect("connection files should delete");
    assert!(!std::path::Path::new(&created_path).exists());

    let error = block_on(wardrobe::armoire_delete_connection_files(
        "wardrobe://localhost:24842".to_string(),
        "remote".to_string(),
    ))
    .expect_err("remote connection files should not be deleted");
    assert!(error.contains("Cannot delete remote connection files"));

    clean_up(&[database_path.as_path(), metadata_path.as_path()]);
}
