//! Native settings window (standard Win32 controls), opened from the panel's
//! context menu. Saving writes settings.json and pushes the new settings into
//! `Shared`, which the panel applies immediately.

use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicIsize, Ordering},
        Arc,
    },
};

use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            CreateFontW, DeleteObject, GetSysColorBrush, SetBkMode, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_WINDOW,
            DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE, FW_NORMAL, FW_SEMIBOLD, HBRUSH, HDC, HFONT, OUT_DEFAULT_PRECIS,
            TRANSPARENT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::AdjustWindowRectExForDpi,
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, GetDlgCtrlID, GetWindowLongPtrW, LoadCursorW, RegisterClassW,
                SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow, BM_GETCHECK, BM_SETCHECK,
                BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL,
                CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HMENU, IDC_ARROW, SW_SHOW, WINDOW_EX_STYLE,
                WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DESTROY, WM_HSCROLL,
                WM_NCCREATE, WM_SETFONT, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
                WS_VISIBLE, WS_VSCROLL,
            },
        },
    },
};

use crate::{
    data::Shared,
    monitors::{self, Monitor},
    settings::{self, log, settings_path},
};

static WINDOW: AtomicIsize = AtomicIsize::new(0);

// Trackbar (msctls_trackbar32) messages and styles, from CommCtrl.h.
const TBM_GETPOS: u32 = 0x0400;
const TBM_SETPOS: u32 = 0x0405;
const TBM_SETRANGE: u32 = 0x0406;
const TBM_SETTICFREQ: u32 = 0x0414;
const TBM_SETPAGESIZE: u32 = 0x0415;
const TBS_AUTOTICKS: u32 = 0x0001;
/// BST_CHECKED from WinUser.h.
const BST_CHECKED: usize = 1;

const WIN_W: f32 = 560.0;
const WIN_H: f32 = 536.0;

// Control ids.
const ID_OPACITY: i32 = 101;
const ID_DWELL: i32 = 102;
const ID_HIDE: i32 = 103;
const ID_ZONE: i32 = 104;
const ID_WIDTH: i32 = 105;
const ID_ANIM: i32 = 106;
const VALUE_LABEL_OFFSET: i32 = 50;
const ID_DISPLAY: i32 = 110;
const ID_SHOW_CREDITS: i32 = 121;
const ID_SHOW_MODEL_WINDOWS: i32 = 122;
const ID_SHOW_TODAY: i32 = 123;
const ID_SHOW_BREAKDOWN: i32 = 124;
const ID_SHOW_BY_MODEL: i32 = 125;
const ID_SHOW_DAILY: i32 = 126;
const ID_RUN_AT_LOGIN: i32 = 127;
const ID_AUTO_REFRESH: i32 = 128;
const ID_SAVE: i32 = 201;
const ID_CANCEL: i32 = 202;
const ID_OPEN_FILE: i32 = 203;

struct Ui {
    shared: Arc<Shared>,
    scale: f32,
    font: HFONT,
    font_bold: HFONT,
    monitors: Vec<Monitor>,
    /// Value written to `display` for each combobox index.
    display_values: Vec<String>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn app(hwnd: HWND) -> Option<&'static mut Ui> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui;
    if p.is_null() {
        None
    } else {
        Some(&mut *p)
    }
}

fn make_font(size_pt: f32, scale: f32, weight: i32) -> HFONT {
    let height = -((size_pt * scale * 96.0 / 72.0).round() as i32);
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
            w!("Segoe UI"),
        )
    }
}

/// Create one child control at logical coordinates.
unsafe fn make(parent: HWND, class: PCWSTR, text: &str, style: u32, x: f32, y: f32, w: f32, h: f32, id: i32, font: HFONT, scale: f32) -> HWND {
    let px = |v: f32| (v * scale).round() as i32;
    let t = wide(text);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class,
        PCWSTR(t.as_ptr()),
        WINDOW_STYLE(style) | WS_CHILD | WS_VISIBLE,
        px(x),
        px(y),
        px(w),
        px(h),
        Some(parent),
        Some(HMENU(id as isize as *mut c_void)),
        None,
        None,
    )
    .unwrap_or_default();
    SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
    hwnd
}

unsafe fn ctl(parent: HWND, id: i32) -> HWND {
    windows::Win32::UI::WindowsAndMessaging::GetDlgItem(Some(parent), id).unwrap_or_default()
}

unsafe fn track_pos(parent: HWND, id: i32) -> i32 {
    SendMessageW(ctl(parent, id), TBM_GETPOS, None, None).0 as i32
}

unsafe fn checked(parent: HWND, id: i32) -> bool {
    SendMessageW(ctl(parent, id), BM_GETCHECK, None, None).0 == BST_CHECKED as isize
}

fn value_text(id: i32, pos: i32) -> String {
    match id {
        ID_OPACITY => format!("{pos}%"),
        ID_DWELL | ID_HIDE | ID_ANIM => format!("{pos} ms"),
        ID_ZONE => format!("{pos}% of width"),
        ID_WIDTH => format!("{pos} px"),
        _ => pos.to_string(),
    }
}

unsafe fn trackbar_row(ui: &Ui, parent: HWND, y: f32, label: &str, id: i32, min: i32, max: i32, step: i32, pos: i32) {
    make(parent, w!("STATIC"), label, 0, 20.0, y + 6.0, 150.0, 22.0, 0, ui.font, ui.scale);
    let tb = make(parent, w!("msctls_trackbar32"), "", TBS_AUTOTICKS | WS_TABSTOP.0, 175.0, y, 250.0, 30.0, id, ui.font, ui.scale);
    SendMessageW(tb, TBM_SETRANGE, Some(WPARAM(1)), Some(LPARAM(((max as u32) << 16 | (min as u32 & 0xFFFF)) as isize)));
    SendMessageW(tb, TBM_SETPAGESIZE, None, Some(LPARAM(step as isize)));
    SendMessageW(tb, TBM_SETTICFREQ, Some(WPARAM(((max - min) / 10).max(1) as usize)), None);
    SendMessageW(tb, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(pos as isize)));
    make(parent, w!("STATIC"), &value_text(id, pos), 0, 435.0, y + 6.0, 110.0, 22.0, id + VALUE_LABEL_OFFSET, ui.font, ui.scale);
}

unsafe fn checkbox(ui: &Ui, parent: HWND, x: f32, y: f32, label: &str, id: i32, on: bool) {
    let h = make(parent, w!("BUTTON"), label, BS_AUTOCHECKBOX as u32 | WS_TABSTOP.0, x, y, 250.0, 24.0, id, ui.font, ui.scale);
    SendMessageW(h, BM_SETCHECK, Some(WPARAM(if on { BST_CHECKED } else { 0 })), None);
}

unsafe fn build_controls(ui: &mut Ui, hwnd: HWND) {
    let s = ui.shared.settings();
    let mut y = 16.0;
    trackbar_row(ui, hwnd, y, "Opacity", ID_OPACITY, 30, 100, 5, (s.opacity * 100.0).round() as i32);
    y += 36.0;
    trackbar_row(ui, hwnd, y, "Show after (dwell)", ID_DWELL, 0, 1000, 50, s.dwell_ms as i32);
    y += 36.0;
    trackbar_row(ui, hwnd, y, "Hide after leaving", ID_HIDE, 0, 2000, 50, s.hide_delay_ms as i32);
    y += 36.0;
    trackbar_row(ui, hwnd, y, "Hot zone width", ID_ZONE, 4, 60, 2, (s.hot_zone_width_fraction * 100.0).round() as i32);
    y += 36.0;
    trackbar_row(ui, hwnd, y, "Panel width", ID_WIDTH, 260, 600, 20, s.panel_width_px as i32);
    y += 36.0;
    trackbar_row(ui, hwnd, y, "Slide animation", ID_ANIM, 0, 500, 20, s.animation_ms as i32);
    y += 40.0;

    make(hwnd, w!("STATIC"), "Display", 0, 20.0, y + 4.0, 150.0, 22.0, 0, ui.font, ui.scale);
    let combo = make(hwnd, w!("COMBOBOX"), "", CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0 | WS_TABSTOP.0, 175.0, y, 370.0, 200.0, ID_DISPLAY, ui.font, ui.scale);
    ui.display_values.clear();
    let mut items: Vec<(String, String)> = vec![
        ("Laptop display (internal)".into(), "internal".into()),
        ("Primary display".into(), "primary".into()),
    ];
    for m in &ui.monitors {
        let mut name = format!("{}  {}x{}", m.device, m.bounds.width(), m.bounds.height());
        if m.internal {
            name.push_str("  (internal)");
        }
        if m.primary {
            name.push_str("  (primary)");
        }
        items.push((name, m.device.clone()));
    }
    let mut selected = 0;
    for (i, (label, value)) in items.iter().enumerate() {
        let t = wide(label);
        SendMessageW(combo, CB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
        if value.eq_ignore_ascii_case(&s.display) {
            selected = i;
        }
        ui.display_values.push(value.clone());
    }
    SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(selected)), None);
    y += 44.0;

    make(hwnd, w!("STATIC"), "Show on the panel", 0, 20.0, y, 300.0, 22.0, 0, ui.font_bold, ui.scale);
    y += 26.0;
    checkbox(ui, hwnd, 20.0, y, "Usage credits (billed)", ID_SHOW_CREDITS, s.show_credits);
    checkbox(ui, hwnd, 290.0, y, "Per-model weekly windows", ID_SHOW_MODEL_WINDOWS, s.show_model_windows);
    y += 28.0;
    checkbox(ui, hwnd, 20.0, y, "Today's tokens and cost", ID_SHOW_TODAY, s.show_today);
    checkbox(ui, hwnd, 290.0, y, "Weekly window by surface (expanded)", ID_SHOW_BREAKDOWN, s.show_breakdown);
    y += 28.0;
    checkbox(ui, hwnd, 20.0, y, "By model, last 7 days (expanded)", ID_SHOW_BY_MODEL, s.show_by_model);
    checkbox(ui, hwnd, 290.0, y, "Last 7 days (expanded)", ID_SHOW_DAILY, s.show_daily);
    y += 40.0;

    make(hwnd, w!("STATIC"), "Behaviour", 0, 20.0, y, 300.0, 22.0, 0, ui.font_bold, ui.scale);
    y += 26.0;
    checkbox(ui, hwnd, 20.0, y, "Start with Windows", ID_RUN_AT_LOGIN, s.run_at_login);
    checkbox(ui, hwnd, 290.0, y, "Renew Claude Code token automatically", ID_AUTO_REFRESH, s.auto_refresh_token);
    y += 44.0;

    make(hwnd, w!("BUTTON"), "Open settings.json", WS_TABSTOP.0, 20.0, y, 150.0, 30.0, ID_OPEN_FILE, ui.font, ui.scale);
    make(hwnd, w!("BUTTON"), "Cancel", WS_TABSTOP.0, 340.0, y, 95.0, 30.0, ID_CANCEL, ui.font, ui.scale);
    make(hwnd, w!("BUTTON"), "Save", BS_DEFPUSHBUTTON as u32 | WS_TABSTOP.0, 445.0, y, 95.0, 30.0, ID_SAVE, ui.font, ui.scale);
}

unsafe fn save(ui: &Ui, hwnd: HWND) {
    let mut s = ui.shared.settings();
    s.opacity = track_pos(hwnd, ID_OPACITY) as f64 / 100.0;
    s.dwell_ms = track_pos(hwnd, ID_DWELL) as u64;
    s.hide_delay_ms = track_pos(hwnd, ID_HIDE) as u64;
    s.hot_zone_width_fraction = track_pos(hwnd, ID_ZONE) as f64 / 100.0;
    s.panel_width_px = track_pos(hwnd, ID_WIDTH) as u32;
    s.animation_ms = track_pos(hwnd, ID_ANIM) as u64;
    let sel = SendMessageW(ctl(hwnd, ID_DISPLAY), CB_GETCURSEL, None, None).0;
    if sel >= 0 {
        if let Some(v) = ui.display_values.get(sel as usize) {
            s.display = v.clone();
        }
    }
    s.show_credits = checked(hwnd, ID_SHOW_CREDITS);
    s.show_model_windows = checked(hwnd, ID_SHOW_MODEL_WINDOWS);
    s.show_today = checked(hwnd, ID_SHOW_TODAY);
    s.show_breakdown = checked(hwnd, ID_SHOW_BREAKDOWN);
    s.show_by_model = checked(hwnd, ID_SHOW_BY_MODEL);
    s.show_daily = checked(hwnd, ID_SHOW_DAILY);
    s.run_at_login = checked(hwnd, ID_RUN_AT_LOGIN);
    s.auto_refresh_token = checked(hwnd, ID_AUTO_REFRESH);
    let s = s.sanitized();
    if let Err(e) = settings::save(&s) {
        log(&format!("settings save failed: {e}"));
    }
    ui.shared.update_settings(s);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CREATE => {
            if let Some(ui) = app(hwnd) {
                build_controls(ui, hwnd);
            }
            LRESULT(0)
        }
        WM_HSCROLL => {
            let tb = HWND(lparam.0 as *mut c_void);
            if !tb.is_invalid() {
                let id = GetDlgCtrlID(tb);
                if (ID_OPACITY..=ID_ANIM).contains(&id) {
                    let pos = SendMessageW(tb, TBM_GETPOS, None, None).0 as i32;
                    let t = wide(&value_text(id, pos));
                    let _ = SetWindowTextW(ctl(hwnd, id + VALUE_LABEL_OFFSET), PCWSTR(t.as_ptr()));
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            match id {
                ID_SAVE => {
                    if let Some(ui) = app(hwnd) {
                        save(ui, hwnd);
                    }
                    let _ = DestroyWindow(hwnd);
                }
                ID_CANCEL => {
                    let _ = DestroyWindow(hwnd);
                }
                ID_OPEN_FILE => {
                    let p = settings_path().to_string_lossy().to_string();
                    let t = wide(&p);
                    windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), PCWSTR(t.as_ptr()), None, None, windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let hdc = HDC(wparam.0 as *mut c_void);
            SetBkMode(hdc, TRANSPARENT);
            let brush: HBRUSH = GetSysColorBrush(COLOR_WINDOW);
            LRESULT(brush.0 as isize)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            WINDOW.store(0, Ordering::SeqCst);
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui;
            if !p.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let ui = Box::from_raw(p);
                let _ = DeleteObject(ui.font.into());
                let _ = DeleteObject(ui.font_bold.into());
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Open the settings window, or bring the existing one to the front.
pub fn open(shared: Arc<Shared>) {
    let existing = WINDOW.load(Ordering::SeqCst);
    if existing != 0 {
        unsafe {
            let _ = SetForegroundWindow(HWND(existing as *mut c_void));
        }
        return;
    }
    unsafe {
        let hinstance = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(_) => return,
        };
        let class_name = w!("ClaudeUsagePanelSettings");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as isize as *mut c_void),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc); // fails harmlessly when already registered

        let settings = shared.settings();
        let list = monitors::enumerate();
        let target = monitors::select(&list, &settings.display).cloned();
        let (dpi, work) = match &target {
            Some(m) => (m.dpi, m.work),
            None => (96, crate::tracker::Rect { left: 0, top: 0, right: 1280, bottom: 720 }),
        };
        let scale = dpi as f32 / 96.0;
        let font = make_font(9.5, scale, FW_NORMAL.0 as i32);
        let font_bold = make_font(9.5, scale, FW_SEMIBOLD.0 as i32);
        let ui = Box::new(Ui { shared, scale, font, font_bold, monitors: list, display_values: Vec::new() });

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rect = RECT { left: 0, top: 0, right: (WIN_W * scale) as i32, bottom: (WIN_H * scale) as i32 };
        let _ = AdjustWindowRectExForDpi(&mut rect, style, false, WINDOW_EX_STYLE(0), dpi);
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        let x = work.left + (work.width() - w) / 2;
        let y = work.top + (work.height() - h) / 2;

        let ui_ptr = Box::into_raw(ui);
        match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("Claude usage panel - Settings"),
            style,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hinstance.into()),
            Some(ui_ptr as *const c_void),
        ) {
            Ok(hwnd) => {
                WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
                log(&format!("settings window opened at {x},{y} ({w}x{h})"));
            }
            Err(e) => {
                log(&format!("settings window failed: {e}"));
                drop(Box::from_raw(ui_ptr));
            }
        }
    }
}
