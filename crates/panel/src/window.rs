//! The layered, non-activating, topmost popup window and its hover logic.

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use chrono::Utc;
use windows::{
    core::{w, Result, PCWSTR},
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
        Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON},
            Shell::{SHQueryUserNotificationState, ShellExecuteW, QUNS_APP, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN},
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW, GetCursorPos,
                GetMessageW, GetWindowLongPtrW, KillTimer, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW,
                SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, TrackPopupMenu, TranslateMessage,
                UpdateLayeredWindow, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HMENU, HWND_TOPMOST, IDC_ARROW,
                MA_NOACTIVATE, MF_STRING, MSG, SWP_NOACTIVATE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, SW_SHOWNORMAL,
                TPM_RETURNCMD, TPM_RIGHTBUTTON, ULW_ALPHA, WM_APP, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_LBUTTONUP,
                WM_MOUSEACTIVATE, WM_NCCREATE, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
                WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
};

use crate::{
    autostart,
    data::{self, Notifier, Shared},
    monitors::{self, Monitor},
    render::{Frame, Renderer},
    settings::{self, log, log_path, settings_path, Settings},
    settings_ui,
    tracker::{Action, Input, Rect, Tracker},
    view::{self, ViewModel},
};

const WM_APP_DATA: u32 = WM_APP + 1;
/// Sent by a second `--settings` launch to the running instance.
const WM_APP_OPEN_SETTINGS: u32 = WM_APP + 2;
const CLASS_NAME: PCWSTR = w!("ClaudeUsagePanelWindow");

/// From a second process: find the running panel and ask it to open settings.
pub fn ask_running_instance_for_settings() {
    unsafe {
        match windows::Win32::UI::WindowsAndMessaging::FindWindowW(CLASS_NAME, None) {
            Ok(h) => {
                let r = PostMessageW(Some(h), WM_APP_OPEN_SETTINGS, WPARAM(0), LPARAM(0));
                log(&format!("--settings: asked running instance ({:?})", r.is_ok()));
            }
            Err(e) => log(&format!("--settings: running instance window not found: {e}")),
        }
    }
}
const TIMER_POLL: usize = 1;
const TIMER_ANIM: usize = 2;
const TIMER_CLOCK: usize = 3;
const MENU_REFRESH: usize = 1;
const MENU_EXPAND: usize = 2;
const MENU_SETTINGS_FILE: usize = 3;
const MENU_LOG: usize = 4;
const MENU_QUIT: usize = 5;
const MENU_SETTINGS: usize = 6;

struct PostNotifier(isize);
impl Notifier for PostNotifier {
    fn notify(&self) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.0 as *mut _)), WM_APP_DATA, WPARAM(0), LPARAM(0));
        }
    }
}

#[derive(Debug, Clone)]
struct Geometry {
    mon: Monitor,
    scale: f32,
    margin: i32,
    panel_w: i32,
    panel_h: i32,
    x: i32,
    shown_y: i32,
    hidden_y: i32,
    win_w: i32,
    win_h: i32,
    entry: Rect,
    exit: Rect,
}

fn geometry(settings: &Settings, mon: &Monitor, panel_h_logical: f32) -> Geometry {
    let scale = mon.scale();
    let px = |v: f32| (v * scale).round() as i32;
    let margin = px(28.0);
    let panel_w = px(settings.panel_width_px as f32);
    let panel_h = px(panel_h_logical);
    let win_w = panel_w + 2 * margin;
    let win_h = panel_h + margin;
    let cx = (mon.bounds.left + mon.bounds.right) / 2;
    let x = cx - win_w / 2;
    let shown_y = mon.bounds.top;
    let hidden_y = mon.bounds.top - win_h;
    let mw = mon.bounds.width();
    let entry_half = ((mw as f64 * settings.hot_zone_width_fraction) / 2.0) as i32;
    let entry = Rect {
        left: cx - entry_half,
        top: mon.bounds.top,
        right: cx + entry_half,
        bottom: mon.bounds.top + px(settings.hot_zone_height_px as f32).max(1),
    };
    let exit_half = entry_half.max(panel_w / 2) + (mw as f64 * settings.exit_margin_fraction) as i32;
    let exit = Rect {
        left: cx - exit_half,
        top: mon.bounds.top,
        right: cx + exit_half,
        bottom: mon.bounds.top + panel_h + px(settings.exit_margin_px as f32),
    };
    Geometry { mon: mon.clone(), scale, margin, panel_w, panel_h, x, shown_y, hidden_y, win_w, win_h, entry, exit }
}

struct Anim {
    from: i32,
    to: i32,
    start: Instant,
    dur: Duration,
}

pub struct App {
    hwnd: HWND,
    settings: Settings,
    settings_version: u64,
    settings_mtime: Option<SystemTime>,
    last_settings_check: Instant,
    shared: Arc<Shared>,
    tracker: Tracker,
    geo: Option<Geometry>,
    renderer: Option<Renderer>,
    frame: Option<Frame>,
    anim: Option<Anim>,
    cur_y: i32,
    shown: bool,
    expanded: bool,
    last_suppress: (Instant, bool),
    vm: Option<ViewModel>,
}

impl App {
    fn current_vm(&self) -> ViewModel {
        let scan = self.shared.scan.lock().unwrap().clone();
        let limits = self.shared.limits.lock().unwrap().clone();
        view::build(&scan, &limits, &self.settings, Utc::now(), Instant::now(), self.expanded)
    }

    fn ensure_geometry(&mut self) -> bool {
        if self.geo.is_none() {
            let list = monitors::enumerate();
            match monitors::select(&list, &self.settings.display) {
                Some(mon) => {
                    let vm = self.vm.get_or_insert_with(|| ViewModel {
                        bars: vec![],
                        today: None,
                        footer: String::new(),
                        footer_warn: false,
                        detail: vec![],
                        expanded: false,
                    });
                    let h = vm.height();
                    let g = geometry(&self.settings, mon, h);
                    self.cur_y = g.hidden_y;
                    log(&format!(
                        "target display {} ({}x{} @ {} dpi, internal={}, primary={}); entry zone {:?}",
                        g.mon.device,
                        g.mon.bounds.width(),
                        g.mon.bounds.height(),
                        g.mon.dpi,
                        g.mon.internal,
                        g.mon.primary,
                        g.entry
                    ));
                    self.geo = Some(g);
                }
                None => return false,
            }
        }
        true
    }

    fn suppressed(&mut self) -> bool {
        if self.last_suppress.0.elapsed() < Duration::from_millis(250) {
            return self.last_suppress.1;
        }
        let dragging = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } < 0;
        let fullscreen = unsafe {
            match SHQueryUserNotificationState() {
                Ok(s) => s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE || s == QUNS_APP,
                Err(_) => false,
            }
        };
        let v = dragging || fullscreen;
        self.last_suppress = (Instant::now(), v);
        v
    }

    fn panel_rect(&self) -> Option<Rect> {
        let g = self.geo.as_ref()?;
        if !self.shown {
            return None;
        }
        Some(Rect { left: g.x + g.margin, top: self.cur_y.max(g.mon.bounds.top), right: g.x + g.margin + g.panel_w, bottom: self.cur_y + g.panel_h })
    }

    /// Pick up settings changed by the settings page or by hand-editing the file.
    fn check_settings(&mut self) {
        let mtime = settings::file_mtime();
        if mtime != self.settings_mtime {
            self.settings_mtime = mtime;
            if let Some(s) = settings::load() {
                if s != self.shared.settings() {
                    self.shared.update_settings(s);
                }
            }
        }
        let v = self.shared.settings_version();
        if v != self.settings_version {
            self.settings_version = v;
            let s = self.shared.settings();
            self.apply_settings(s);
        }
    }

    fn apply_settings(&mut self, s: Settings) {
        let poll_changed = s.poll_ms != self.settings.poll_ms;
        if s.run_at_login != self.settings.run_at_login {
            if let Err(e) = autostart::apply(s.run_at_login) {
                log(&format!("run-at-login: {e}"));
            }
        }
        self.tracker.dwell = Duration::from_millis(s.dwell_ms);
        self.tracker.hide_delay = Duration::from_millis(s.hide_delay_ms);
        self.settings = s;
        self.geo = None;
        self.frame = None;
        self.vm = None;
        if poll_changed {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_POLL, self.settings.poll_ms.max(15) as u32, None);
            }
        }
        if self.shown {
            self.ensure_geometry();
            if let Err(e) = self.render() {
                log(&format!("render failed: {e}"));
            }
        }
        log("settings applied");
    }

    fn poll(&mut self) {
        if self.last_settings_check.elapsed() >= Duration::from_secs(2) {
            self.last_settings_check = Instant::now();
            self.check_settings();
        }
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            return;
        }
        if !self.ensure_geometry() {
            return;
        }
        let g = self.geo.clone().unwrap();
        let near = g.exit.contains(pt.x, pt.y) || self.tracker.state != crate::tracker::State::Hidden;
        let suppressed = if near { self.suppressed() } else { false };
        let input = Input {
            cursor: (pt.x, pt.y),
            entry: g.entry,
            exit: g.exit,
            panel: self.panel_rect(),
            suppressed,
            pinned: self.expanded,
        };
        let action = self.tracker.tick(Instant::now(), &input);
        // Warm up Direct2D/DirectWrite during the dwell so the slide starts instantly.
        if matches!(self.tracker.state, crate::tracker::State::Arming { .. }) && self.renderer.is_none() {
            match Renderer::new() {
                Ok(r) => self.renderer = Some(r),
                Err(e) => log(&format!("renderer init failed: {e}")),
            }
        }
        match action {
            Action::Show => self.start_show(),
            Action::Hide => self.start_hide(),
            Action::None => {
                // Reverse an in-flight hide if the cursor came back (or vice versa).
                let target = self.anim.as_ref().map(|a| a.to);
                if self.tracker.is_visible() && self.shown && target == Some(g.hidden_y) {
                    self.start_show();
                } else if !self.tracker.is_visible() && self.shown && target != Some(g.hidden_y) && self.anim.is_none() && self.cur_y == g.shown_y {
                    self.start_hide();
                }
            }
        }
    }

    fn render(&mut self) -> Result<()> {
        let vm = self.current_vm();
        let h = vm.height();
        let mut g = match self.geo.clone() {
            Some(g) => g,
            None => return Ok(()),
        };
        if (g.panel_h as f32 - h * g.scale).abs() >= 1.0 {
            g = geometry(&self.settings, &g.mon, h);
            self.geo = Some(g.clone());
            self.frame = None;
        }
        self.vm = Some(vm.clone());
        if self.renderer.is_none() {
            self.renderer = Some(Renderer::new()?);
        }
        if self.frame.as_ref().map_or(true, |f| f.width != g.win_w || f.height != g.win_h) {
            self.frame = Some(Frame::new(g.win_w, g.win_h)?);
        }
        let frame = self.frame.as_ref().unwrap();
        self.renderer.as_mut().unwrap().draw(frame, g.scale, g.margin, g.panel_w, g.panel_h, self.settings.opacity as f32, &vm)?;
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let dst = POINT { x: g.x, y: self.cur_y };
        let size = SIZE { cx: g.win_w, cy: g.win_h };
        let src = POINT { x: 0, y: 0 };
        unsafe {
            UpdateLayeredWindow(self.hwnd, None, Some(&dst), Some(&size), Some(frame.hdc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA)?;
        }
        Ok(())
    }

    fn start_show(&mut self) {
        let g = match self.geo.clone() {
            Some(g) => g,
            None => return,
        };
        if !self.shown {
            self.cur_y = g.hidden_y;
        }
        if let Err(e) = self.render() {
            log(&format!("render failed: {e}"));
            return;
        }
        if !self.shown {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), g.x, self.cur_y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
            }
            self.shown = true;
        }
        let g = self.geo.clone().unwrap();
        self.anim = Some(Anim { from: self.cur_y, to: g.shown_y, start: Instant::now(), dur: Duration::from_millis(self.settings.animation_ms) });
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_ANIM, 10, None);
            SetTimer(Some(self.hwnd), TIMER_CLOCK, 1000, None);
        }
        self.shared.set_visible(true);
    }

    fn start_hide(&mut self) {
        let g = match self.geo.clone() {
            Some(g) => g,
            None => return,
        };
        if !self.shown {
            return;
        }
        self.anim = Some(Anim { from: self.cur_y, to: g.hidden_y, start: Instant::now(), dur: Duration::from_millis(self.settings.animation_ms) });
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_ANIM, 10, None);
            let _ = KillTimer(Some(self.hwnd), TIMER_CLOCK);
        }
        self.shared.set_visible(false);
    }

    fn anim_tick(&mut self) {
        let (g, a) = match (self.geo.clone(), self.anim.as_ref()) {
            (Some(g), Some(a)) => (g, a),
            _ => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
                }
                return;
            }
        };
        let t = if a.dur.is_zero() { 1.0 } else { (a.start.elapsed().as_secs_f32() / a.dur.as_secs_f32()).clamp(0.0, 1.0) };
        let eased = 1.0 - (1.0 - t).powi(3);
        let y = a.from + ((a.to - a.from) as f32 * eased).round() as i32;
        let to = a.to;
        self.cur_y = y;
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), g.x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
        }
        if t >= 1.0 {
            self.anim = None;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
            }
            if to == g.hidden_y {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
                self.shown = false;
                // Release GPU/font resources while hidden; they are re-created on the
                // next dwell. Keeps idle memory near the pre-first-show level.
                self.frame = None;
                self.renderer = None;
                if self.expanded {
                    self.expanded = false;
                    self.geo = None;
                    self.vm = None;
                }
            }
        }
    }

    fn toggle_expanded(&mut self) {
        self.expanded = !self.expanded;
        if self.shown {
            if let Err(e) = self.render() {
                log(&format!("render failed: {e}"));
            }
        }
    }

    fn hide_now(&mut self) {
        if self.shown {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
                let _ = KillTimer(Some(self.hwnd), TIMER_CLOCK);
            }
        }
        self.shown = false;
        self.anim = None;
        self.expanded = false;
        self.frame = None;
        self.renderer = None;
        self.tracker.reset();
        self.shared.set_visible(false);
    }

    fn on_display_change(&mut self) {
        self.hide_now();
        self.geo = None;
        self.frame = None;
        self.vm = None;
    }

    fn context_menu(&mut self) {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let menu: HMENU = match CreatePopupMenu() {
                Ok(m) => m,
                Err(_) => return,
            };
            let _ = AppendMenuW(menu, MF_STRING, MENU_REFRESH, w!("Refresh now"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_EXPAND, if self.expanded { w!("Collapse") } else { w!("Expand") });
            let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("Settings..."));
            let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS_FILE, w!("Open settings.json"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_LOG, w!("Open log"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit"));
            let _ = SetForegroundWindow(self.hwnd);
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), self.hwnd, None).0 as usize;
            let _ = DestroyMenu(menu);
            match cmd {
                MENU_REFRESH => self.shared.refresh_now(),
                MENU_EXPAND => self.toggle_expanded(),
                MENU_SETTINGS => {
                    self.hide_now();
                    settings_ui::open(self.shared.clone());
                }
                MENU_SETTINGS_FILE => open_path(&settings_path().to_string_lossy()),
                MENU_LOG => open_path(&log_path().to_string_lossy()),
                MENU_QUIT => {
                    self.shared.quit();
                    PostQuitMessage(0);
                }
                _ => {}
            }
        }
    }
}

fn open_path(path: &str) {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

unsafe fn app_from(hwnd: HWND) -> Option<&'static mut App> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    if p.is_null() {
        None
    } else {
        Some(&mut *p)
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            if let Some(app) = app_from(hwnd) {
                app.hwnd = hwnd;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER => {
            if let Some(app) = app_from(hwnd) {
                match wparam.0 {
                    TIMER_POLL => app.poll(),
                    TIMER_ANIM => app.anim_tick(),
                    TIMER_CLOCK => {
                        if app.shown {
                            if let Err(e) = app.render() {
                                log(&format!("render failed: {e}"));
                            }
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_APP_DATA => {
            if let Some(app) = app_from(hwnd) {
                if app.shown {
                    if let Err(e) = app.render() {
                        log(&format!("render failed: {e}"));
                    }
                }
            }
            LRESULT(0)
        }
        WM_APP_OPEN_SETTINGS => {
            log("open settings requested");
            if let Some(app) = app_from(hwnd) {
                app.hide_now();
                settings_ui::open(app.shared.clone());
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(app) = app_from(hwnd) {
                app.toggle_expanded();
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            if let Some(app) = app_from(hwnd) {
                app.context_menu();
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
            if let Some(app) = app_from(hwnd) {
                app.on_display_change();
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub fn run(settings: Settings, open_settings: bool) -> Result<()> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let class_name = CLASS_NAME;
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let shared = Arc::new(Shared::new(settings.clone()));
        let mut app = Box::new(App {
            hwnd: HWND::default(),
            tracker: Tracker::new(Duration::from_millis(settings.dwell_ms), Duration::from_millis(settings.hide_delay_ms)),
            settings_version: shared.settings_version(),
            settings_mtime: settings::file_mtime(),
            last_settings_check: Instant::now(),
            settings: settings.clone(),
            shared: shared.clone(),
            geo: None,
            renderer: None,
            frame: None,
            anim: None,
            cur_y: 0,
            shown: false,
            expanded: false,
            last_suppress: (Instant::now() - Duration::from_secs(10), false),
            vm: None,
        });
        let app_ptr: *mut App = &mut *app;

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_name,
            w!("Claude usage"),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(hinstance.into()),
            Some(app_ptr as *const _),
        )?;
        app.hwnd = hwnd;

        data::spawn(shared.clone(), Box::new(PostNotifier(hwnd.0 as isize)));
        SetTimer(Some(hwnd), TIMER_POLL, settings.poll_ms.max(15) as u32, None);
        log("panel started");
        if open_settings {
            let _ = PostMessageW(Some(hwnd), WM_APP_OPEN_SETTINGS, WPARAM(0), LPARAM(0));
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        shared.quit();
        log("panel exiting");
        Ok(())
    }
}
