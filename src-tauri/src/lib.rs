#![forbid(unsafe_code)]

use tauri::Manager;

pub mod commands;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let state = commands::AppState::initialize(app.handle()).map_err(|error| {
                std::io::Error::other(format!("{}: {}", error.code, error.message))
            })?;
            app.manage(state);
            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                // An accepted window close intermittently never finishes on
                // Windows: WebView2 teardown deadlocks the run loop and the
                // process survives with its whole msedgewebview2 tree (QA
                // installer smoke stalled this way in roughly half its runs,
                // 60 seconds past the close). Shut the pipeline down and exit
                // directly instead of trusting the destroy path: the queue is
                // crash-safe, so recovery loses nothing, and a closed window
                // must mean an exited app.
                if let Some(state) = window.app_handle().try_state::<commands::AppState>() {
                    state.prepare_exit();
                }
                std::process::exit(0);
            }
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Intern");
}
