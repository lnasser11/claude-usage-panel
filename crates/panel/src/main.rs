//! Claude usage panel: a hidden top-center panel that slides down on hover.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod data;
mod monitors;
mod render;
mod settings;
mod settings_ui;
mod tracker;
mod view;
mod window;

use windows::{
    core::w,
    Win32::{
        Foundation::{GetLastError, ERROR_ALREADY_EXISTS},
        System::Threading::CreateMutexW,
        UI::{
            Controls::{InitCommonControlsEx, ICC_BAR_CLASSES, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX},
            HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
        },
    },
};

fn main() {
    unsafe {
        // The embedded manifest already declares per-monitor-v2 awareness; this is a
        // harmless belt-and-braces call for builds without it.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let icc = INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_BAR_CLASSES | ICC_STANDARD_CLASSES };
        let _ = InitCommonControlsEx(&icc);
        let want_settings = std::env::args().any(|a| a == "--settings");
        // Single instance. A second launch with --settings asks the running one
        // to open its settings page instead.
        let _mutex = CreateMutexW(None, false, w!("Local\\ClaudeUsagePanel"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if want_settings {
                window::ask_running_instance_for_settings();
            }
            return;
        }
    }
    let (settings, warn) = settings::load_or_create();
    if let Some(w) = warn {
        settings::log(&w);
    }
    if let Err(e) = autostart::apply(settings.run_at_login) {
        settings::log(&format!("run-at-login: {e}"));
    }
    let open_settings = std::env::args().any(|a| a == "--settings");
    if let Err(e) = window::run(settings, open_settings) {
        settings::log(&format!("fatal: {e}"));
    }
}
