#[test]
fn main_binary_target_is_available_to_integration_tests() {
    let binary_path = env!("CARGO_BIN_EXE_armoire");

    assert!(!binary_path.is_empty());
    assert!(
        binary_path.ends_with("armoire") || binary_path.ends_with("armoire.exe"),
        "unexpected binary path: {binary_path}"
    );
}
