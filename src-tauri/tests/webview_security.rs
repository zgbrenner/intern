//! What the webview is allowed to do.
//!
//! The window renders a bundled page and talks to Rust; it has no reason to
//! load a script, a stylesheet, or a frame from anywhere else, and saying so
//! is what stops one injected string from becoming a page that can call every
//! command Intern exposes.

use std::path::Path;

fn security() -> serde_json::Value {
    let config =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json"))
            .expect("the Tauri configuration is readable");
    serde_json::from_str::<serde_json::Value>(&config).expect("the Tauri configuration is JSON")
        ["app"]["security"]
        .clone()
}

#[test]
fn csp_is_configured() {
    let csp = security()["csp"]
        .as_str()
        .expect("the webview must have a content security policy")
        .to_owned();
    for directive in [
        // Nothing loads from anywhere but the bundle.
        "default-src 'self'",
        // The one that matters: no injected or remote script runs.
        "script-src 'self'",
        "object-src 'none'",
        // The window is not framed, and frames nothing.
        "frame-src 'none'",
        "frame-ancestors 'none'",
        // Tauri's own channel, which the commands arrive on.
        "ipc:",
    ] {
        assert!(
            csp.contains(directive),
            "the policy must carry {directive}: {csp}"
        );
    }
}
