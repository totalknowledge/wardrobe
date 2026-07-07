use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_storage_directory(test_name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("wardrobe_cli_main_{test_name}_{nanos}"))
}

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_wardrobe"))
        .args(args)
        .output()
        .expect("cli binary should run")
}

#[test]
fn binary_help_flag_exits_successfully() {
    let output = run_cli(&["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("wardrobe:"));
    assert!(stdout.contains("-h, --help"));
    assert!(stdout.contains("-v, --version"));
    assert!(
        stdout.contains("The first argument to the CLI is always the target connection context.")
    );
    assert!(stdout.contains("STRUCTURAL COMMANDS"));
    assert!(stdout.contains("DOCUMENT MUTATIONS & QUERIES (RUDIC)"));
    assert!(stdout.contains("SCHEMA ENGINE & RELATIONSHIP MANAGEMENT"));
    assert!(stdout.contains("BACKUP & DISASTER RECOVERY"));
    assert!(stdout.contains("SERVER ACCESS CONTROL & USER ADMINISTRATION"));
    assert!(stdout.contains("CORE ARCHITECTURAL RULES"));
    assert!(stdout.contains("Example: create drawer my_wardrobe/my_bay/user"));
    assert!(stdout.contains("Example: backup my_wardrobe/my_bay ./backups/bay_snapshot.wrb"));
    assert!(stdout.contains("create user <json_user_payload>"));
    assert!(stdout.contains("drop user <username>"));
    assert!(stdout.contains("grant permission <username> <permission_scope>"));
    assert!(stdout.contains("revoke permission <username> <permission_scope>"));
    assert!(!stdout.contains("records <path>"));
    assert!(!stdout.contains("clean <path>"));
    assert!(!stdout.contains("show <type>"));
}

#[test]
fn binary_short_help_flag_exits_successfully() {
    let long_output = run_cli(&["--help"]);
    let short_output = run_cli(&["-h"]);
    assert!(short_output.status.success());
    assert_eq!(short_output.stdout, long_output.stdout);
}

#[test]
fn binary_version_flags_exit_successfully() {
    let long_output = run_cli(&["--version"]);
    assert!(long_output.status.success());
    let stdout = String::from_utf8_lossy(&long_output.stdout);
    assert!(stdout.contains("wardrobe"));
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));

    let short_output = run_cli(&["-v"]);
    assert!(short_output.status.success());
    assert_eq!(short_output.stdout, long_output.stdout);
}

#[test]
fn binary_missing_connection_string_returns_invalid_input_error() {
    let output = run_cli(&["--target"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--target/--data-dir requires"));
}

#[test]
fn binary_unknown_command_returns_error_status() {
    let tmp = temp_storage_directory("binary_unknown_command");
    let _ = fs::create_dir_all(&tmp);
    let target = tmp.to_string_lossy().to_string();

    let output = run_cli(&[&target, "non-existent-command"]);
    assert!(!output.status.success());

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn binary_missing_command_arguments_validation_guards() {
    let tmp = temp_storage_directory("binary_guards");
    let _ = fs::create_dir_all(&tmp);
    let target = tmp.to_string_lossy().to_string();

    let inspect_out = run_cli(&["--target", &target, "inspect"]);
    assert!(!inspect_out.status.success());

    let read_out = run_cli(&["--target", &target, "read"]);
    assert!(!read_out.status.success());

    let upsert_out = run_cli(&["--target", &target, "upsert"]);
    assert!(!upsert_out.status.success());

    let status_bays_out = run_cli(&["--target", &target, "status", "bays"]);
    assert!(!status_bays_out.status.success());

    let status_drawers_out = run_cli(&["--target", &target, "status", "drawers"]);
    assert!(!status_drawers_out.status.success());

    let compact_out = run_cli(&["--target", &target, "compact"]);
    assert!(!compact_out.status.success());

    let create_bay_out = run_cli(&["--target", &target, "create", "bay"]);
    assert!(!create_bay_out.status.success());

    let create_user_out = run_cli(&["--target", &target, "create", "user"]);
    assert!(!create_user_out.status.success());

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn binary_piped_stdin_stream_execution() {
    let tmp = temp_storage_directory("binary_stdin");
    let _ = fs::create_dir_all(&tmp);

    let mut child = Command::new(env!("CARGO_BIN_EXE_wardrobe"))
        .args(&["--target", &tmp.to_string_lossy()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");

    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(b"status drawer-names\n").expect("write");
    }

    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success());
    let _ = fs::remove_dir_all(tmp);
}
