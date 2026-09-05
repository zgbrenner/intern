#![forbid(unsafe_code)]

use tauri::Manager;

pub mod commands;
pub mod intake;
pub mod tray;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Opens the published user guide in the system browser. A webview
        // <a target="_blank"> has nowhere to go inside Tauri, and the scope in
        // capabilities/default.json admits only the guide's own origin.
        .plugin(tauri_plugin_opener::init())
        // Autostart entries launch Intern with "--minimized" so a sign-in
        // launch can go straight to the tray (when background mode allows it)
        // instead of opening a window nobody asked for. macOS keeps the
        // default LaunchAgent mechanism.
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .arg("--minimized")
                .build(),
        )
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
            let settings = state.settings_snapshot();
            app.manage(state);
            tray::sync_tray(app.handle(), settings.run_in_background);
            let minimized_launch = std::env::args().any(|argument| argument == "--minimized");
            if tray::window_starts_hidden(
                settings.start_minimized,
                settings.run_in_background,
                minimized_launch,
            ) && let Some(window) = app.get_webview_window("main")
            {
                let _ = window.hide();
            }
            Ok(())
        })
        // Close-to-tray. When background mode is on the close request is
        // prevented and the window merely hidden - no teardown of any kind
        // begins, so the deliberate close-time exit behavior for the normal
        // case is left completely alone: when background mode is off (or the
        // settings cannot be read) nothing here touches the event and the
        // window closes exactly as it always has.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && window.state::<commands::AppState>().hide_window_on_close()
            {
                api.prevent_close();
                let _ = window.hide();
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
            commands::history_list,
            commands::history_export,
            commands::queue_discard_waiting,
            commands::intake_status,
            commands::intake_scan_now,
            commands::folder_classify,
            commands::cloud_roots,
            commands::descriptions_status,
            commands::descriptions_backfill,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Intern");
}
