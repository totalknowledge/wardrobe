use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::{SystemTime, UNIX_EPOCH};

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

fn missing_relative_database_name(test_name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();

    format!("armoire_missing_{test_name}_{nanos}")
}

#[test]
fn wardrobe_create_source_location_command_initializes_database_directory() {
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

    let _ = std::fs::remove_dir_all(created_path);
}

#[test]
fn wardrobe_test_database_access_command_accepts_initialized_directory() {
    let path = temp_database_path("test_database_access_command");
    let source_location = block_on(
        armoire_lib::commands::wardrobe::wardrobe_create_source_location(
            path.to_string_lossy().into_owned(),
        ),
    )
    .expect("source location should initialize");

    block_on(armoire_lib::commands::wardrobe::wardrobe_test_database_access(
        source_location.clone(),
    ))
    .expect("test connection command should succeed");

    let _ = std::fs::remove_dir_all(source_location);
}

#[test]
fn wardrobe_test_database_access_command_reports_missing_directory() {
    let path = missing_relative_database_name("database_access_command");

    let error = block_on(armoire_lib::commands::wardrobe::wardrobe_test_database_access(
        path,
    ))
    .expect_err("missing directory should fail");

    assert!(error.contains("was not found"));
}
