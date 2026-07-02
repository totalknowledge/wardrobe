use std::path::Path;
use std::process::Command;

fn main() {
    let index_html = Path::new("../dist/armoire/browser/index.html");

    if !index_html.exists() {
        let npm_command = if cfg!(windows) { "npm.cmd" } else { "npm" };

        let status = Command::new(npm_command)
            .args(["run", "build"])
            .current_dir("..")
            .status()
            .expect("failed to run frontend build");

        if !status.success() {
            panic!("frontend build failed");
        }
    }

    tauri_build::build()
}
