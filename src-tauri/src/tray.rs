//! The system tray for background mode, and the pure decisions behind it.
//!
//! The tray exists only while `runInBackground` is on: it is created when the
//! setting turns on (or at startup) and removed when it turns off, so a user
//! who never asked for a background app never grows a tray icon. Creation can
//! genuinely fail at runtime — a Linux desktop without a status-notifier host,
//! a session with no tray at all — and that failure is logged and swallowed:
//! a user without a tray still has the window, and must never lose the app to
//! a panic over an icon.

use tauri::{
    AppHandle, Manager,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

/// The single tray icon's stable id, so settings changes can find it again.
pub const TRAY_ID: &str = "intern-tray";

/// Whether closing the main window should hide it to the tray.
///
/// Only when `runInBackground` is on. When it is off this must return false so
/// the close request falls through to the existing exit path completely
/// untouched — the window closes and the process ends exactly as before.
pub fn close_hides_to_tray(run_in_background: bool) -> bool {
    run_in_background
}

/// Whether the main window should start hidden (tray only).
///
/// `startMinimized` (or a `--minimized` autostart launch) only takes effect
/// when `runInBackground` is also on. Without the tray there would be no way
/// to bring the window back: a minimized start would strand the app running
/// invisibly, so without `runInBackground` the setting is deliberately inert.
pub fn window_starts_hidden(
    start_minimized: bool,
    run_in_background: bool,
    minimized_launch: bool,
) -> bool {
    run_in_background && (start_minimized || minimized_launch)
}

/// The tray's tooltip: "Intern" alone when nothing waits on a person, and
/// otherwise what does, so a glance at the tray answers "is there anything
/// for me?" without opening the window.
pub fn tray_tooltip(needs_review: usize, ready: usize) -> String {
    let mut parts = Vec::new();
    if needs_review > 0 {
        let verb = if needs_review == 1 { "needs" } else { "need" };
        parts.push(format!("{needs_review} {verb} review"));
    }
    if ready > 0 {
        parts.push(format!("{ready} ready to rename"));
    }
    if parts.is_empty() {
        "Intern".to_owned()
    } else {
        format!("Intern – {}", parts.join(", "))
    }
}

/// Refreshes the tooltip when there is a tray to carry it. Without one
/// (background mode off, or no tray on this desktop) there is nothing to do,
/// and a tooltip the platform refuses is no reason to fail a queue listing.
pub fn update_tooltip(app: &AppHandle, needs_review: usize, ready: usize) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tray_tooltip(needs_review, ready)));
    }
}

/// Brings the tray presence in line with the setting: create the icon when
/// background mode is on and it does not exist yet, remove it when the mode
/// turns off. Idempotent, and never fails the caller — see the module note.
pub fn sync_tray(app: &AppHandle, run_in_background: bool) {
    if !run_in_background {
        let _ = app.remove_tray_by_id(TRAY_ID);
        return;
    }
    if app.tray_by_id(TRAY_ID).is_none()
        && let Err(error) = create_tray(app)
    {
        eprintln!("intern: system tray is unavailable, continuing without it: {error}");
    }
}

/// Builds the tray icon itself. Kept thin and runtime-only: everything worth
/// unit testing lives in the pure helpers above.
fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItemBuilder::with_id("open", "Open Intern").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit Intern").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&open)
        .separator()
        .item(&quit)
        .build()?;
    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Intern")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            // The explicit quit path: the same deliberate shutdown-and-exit the
            // window close runs when background mode is off.
            "quit" => crate::commands::shutdown_and_exit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// Reopens the hidden (or merely buried) main window and focuses it.
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::{close_hides_to_tray, tray_tooltip, window_starts_hidden};

    #[test]
    fn the_tooltip_says_what_waits_on_a_person_and_nothing_else() {
        assert_eq!(tray_tooltip(0, 0), "Intern");
        assert_eq!(tray_tooltip(1, 0), "Intern – 1 needs review");
        assert_eq!(tray_tooltip(3, 0), "Intern – 3 need review");
        assert_eq!(tray_tooltip(0, 2), "Intern – 2 ready to rename");
        assert_eq!(
            tray_tooltip(3, 2),
            "Intern – 3 need review, 2 ready to rename"
        );
    }

    #[test]
    fn closing_the_window_hides_to_tray_only_in_background_mode() {
        assert!(close_hides_to_tray(true));
        // Off means the request must fall through to the existing exit path.
        assert!(!close_hides_to_tray(false));
    }

    #[test]
    fn the_window_starts_hidden_only_when_the_tray_is_there_to_reopen_it() {
        assert!(window_starts_hidden(true, true, false));
        assert!(window_starts_hidden(false, true, true));
        assert!(window_starts_hidden(true, true, true));
        // Without background mode a hidden start would strand the app: the
        // setting and the autostart flag are both deliberately inert.
        assert!(!window_starts_hidden(true, false, false));
        assert!(!window_starts_hidden(false, false, true));
        assert!(!window_starts_hidden(false, true, false));
        assert!(!window_starts_hidden(false, false, false));
    }
}
