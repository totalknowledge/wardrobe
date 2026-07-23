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
            commands::wardrobe::wardrobe_create_source_location,
            commands::wardrobe::wardrobe_connect_source_location,
            commands::wardrobe::wardrobe_show_wardrobes,
            commands::wardrobe::wardrobe_create_new_wardrobe,
            commands::wardrobe::wardrobe_show_bays,
            commands::wardrobe::wardrobe_create_new_bay,
            commands::wardrobe::wardrobe_show_drawers,
            commands::wardrobe::wardrobe_create_new_drawer,
            commands::wardrobe::wardrobe_read_records,
            commands::wardrobe::wardrobe_create_record,
            commands::wardrobe::armoire_get_saved_connections,
            commands::wardrobe::armoire_remove_connection,
            commands::wardrobe::armoire_update_connection_alias,
            commands::wardrobe::armoire_delete_connection_files
        ])
}
