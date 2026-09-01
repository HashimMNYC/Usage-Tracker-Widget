#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::time::{SystemTime, UNIX_EPOCH};

use usage_widget::{
    providers::claude::{capture_mode_from_args, run_claude_capture},
    shell::{gui_start_error_message, run_gui},
    state_store::{default_state_path, JsonStateStore},
};
use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

fn main() {
    let args = std::env::args_os().collect::<Vec<_>>();
    if capture_mode_from_args(&args) {
        let Ok(path) = default_state_path() else {
            eprintln!("USAGE: LOCAL STATE ERROR");
            std::process::exit(2);
        };
        let store = JsonStateStore::new(path);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or(0);
        let exit = run_claude_capture(
            std::io::stdin().lock(),
            std::io::stdout().lock(),
            std::io::stderr().lock(),
            &store,
            now,
        );
        std::process::exit(exit);
    }

    if let Err(error) = run_gui() {
        if let Some(message) = gui_start_error_message(error) {
            show_gui_start_error(message);
        }
        std::process::exit(1);
    }
}

fn show_gui_start_error(message: &str) {
    let message = message.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let title = "Usage Widget"
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
