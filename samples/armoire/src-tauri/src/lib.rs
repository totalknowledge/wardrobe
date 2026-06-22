pub mod commands;
pub mod services;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    build_app()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

pub fn build_app() -> tauri::Builder<tauri::Wry> {
    tauri::Builder::default()
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::wardrobe::wardrobe_test_database_access,
            commands::wardrobe::wardrobe_create_source_location
        ])
}
