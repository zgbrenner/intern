#![forbid(unsafe_code)]

use tauri::Manager;

pub mod commands;
pub mod intake;
pub mod microsoft_intake;
pub mod model;
pub mod secrets;
pub mod tray;

pub fn run() {
    tauri::Builder::default()
        // One Intern per machine, and registered first as the plugin
        // requires. Autostart at sign-in followed by a click on the shortcut
        // otherwise runs two local models, two intake watchers, and two trays
        // against one queue database.
        .plugin(tauri_plugin_single_instance::init(
            commands::second_instance_launched,
        ))
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
        // Updates remain user-initiated and signature-verified. Microsoft
        // upload verification and hosted inference are separate opt-in network
        // integrations; see their explicit permissions/privacy notices.
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
            app.resources_table()
                .add(ShutdownGuard(app.handle().clone()));
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
            microsoft_intake::microsoft_intake_status,
            microsoft_intake::microsoft_sign_in_start,
            microsoft_intake::microsoft_sign_in_poll,
            microsoft_intake::microsoft_disconnect,
            microsoft_intake::microsoft_bind_intake,
            microsoft_intake::microsoft_open_sign_in,
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
            commands::hosted_model_status,
            commands::hosted_model_set_key,
            commands::hosted_model_clear_key,
            commands::hosted_model_test,
            commands::house_rules_list,
            commands::house_rule_forget,
            commands::house_rule_use,
        ])
        .build(tauri::generate_context!())
        .expect("error while running Intern")
        // Every ordinary way out of the app arrives here, and the pipeline is
        // still whole at this point, so llama-server and the parser worker are
        // stopped deliberately rather than left to the kernel. The job object
        // in intern-engine remains the backstop for the exits that never reach
        // this callback: a panic, a crash, and the updater's installer.
        .run(|app, event| {
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                commands::shutdown_runtime(app);
            }
        });
}

/// Parked in the app's resource table for the sake of its `Drop`.
///
/// The updater's install step is the one exit nothing else here sees: it runs
/// the plugin's before-exit hook, hands the installer to the shell, and leaves
/// through `std::process::exit`. That hook is Tauri's `cleanup_before_exit`,
/// which clears this table - so dropping from it is the notice we get, and it
/// arrives while there is still time to release the binaries NSIS must
/// replace.
struct ShutdownGuard(tauri::AppHandle);

impl tauri::Resource for ShutdownGuard {}

impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        commands::shutdown_runtime(&self.0);
    }
}
