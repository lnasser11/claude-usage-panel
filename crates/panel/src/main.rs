//! Claude usage panel: a hidden top-center panel that slides down on hover.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod data;
mod monitors;
mod render;
mod settings;
mod tracker;
mod view;
mod window;

use windows::{
    core::w,
    Win32::{
        Foundation::{GetLastError, ERROR_ALREADY_EXISTS},
        System::Threading::CreateMutexW,
        UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
    },
};

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // Single instance.
        let _mutex = CreateMutexW(None, false, w!("Local\\ClaudeUsagePanel"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
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
    if let Err(e) = window::run(settings) {
        settings::log(&format!("fatal: {e}"));
    }
}
