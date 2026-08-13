#![forbid(unsafe_code)]

use tauri::Manager;

pub mod commands;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // The only network call Intern makes besides the one-off model
        // download, and it happens only when someone presses the button in
        // Settings. There is no background poll and no timer: a document tool
        // that reaches out on its own is a document tool you have to take on
        // trust. Updates are verified against the public key in tauri.conf.json
        // before anything is installed.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let state = commands::AppState::initialize(app.handle()).map_err(|error| {
                std::io::Error::other(format!("{}: {}", error.code, error.message))
            })?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::queue_list,
            commands::queue_add_files,
            commands::queue_add_folder,
            commands::queue_pause,
            commands::queue_resume,
            commands::queue_cancel,
            commands::queue_retry,
            commands::queue_remove,
            commands::proposal_approve,
            commands::proposal_keep_original,
            commands::operation_undo,
            commands::settings_get,
            commands::settings_save,
            commands::setup_get,
            commands::setup_start,
            commands::setup_cancel,
            commands::setup_choose_existing,
            commands::history_clear,
            commands::queue_discard_waiting,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Intern");
}
