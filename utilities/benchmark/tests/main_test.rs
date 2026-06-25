use std::process::Command;

#[test]
fn benchmark_binary_help_flag_exits_successfully() {
    let output = Command::new(env!("CARGO_BIN_EXE_wardrobe-benchmark"))
        .arg("--help")
        .output()
        .expect("benchmark binary should run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("wardrobe-benchmark"));
    assert!(stdout.contains("--targets"));
}

#[test]
fn benchmark_binary_unknown_flag_returns_error_status() {
    let output = Command::new(env!("CARGO_BIN_EXE_wardrobe-benchmark"))
        .arg("--definitely-not-a-real-flag")
        .output()
        .expect("benchmark binary should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Unknown benchmark argument"));
}
