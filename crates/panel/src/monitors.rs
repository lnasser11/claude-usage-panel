//! Display enumeration and target selection, per-monitor DPI aware.
//!
//! "internal" picks the display whose output technology is INTERNAL or
//! DISPLAYPORT_EMBEDDED per QueryDisplayConfig (the laptop panel). Falls back to
//! the primary display when no internal one is active (lid closed, docked).

use windows::{
    core::BOOL,
    Win32::{
        Devices::Display::{
            DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
            DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL, QDC_ONLY_ACTIVE_PATHS,
        },
        Foundation::{LPARAM, RECT},
        Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
        },
        UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
    },
};

use crate::tracker::Rect;

#[derive(Debug, Clone)]
pub struct Monitor {
    pub handle: isize,
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub device: String,
    pub bounds: Rect,
    pub work: Rect,
    pub primary: bool,
    pub internal: bool,
    pub dpi: u32,
}

impl Monitor {
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }
}

fn to_rect(r: RECT) -> Rect {
    Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

unsafe extern "system" fn enum_proc(hmon: HMONITOR, _hdc: HDC, _rc: *mut RECT, lparam: LPARAM) -> BOOL {
    let out = &mut *(lparam.0 as *mut Vec<Monitor>);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(hmon, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
        let name_len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
        let device = String::from_utf16_lossy(&info.szDevice[..name_len]);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        out.push(Monitor {
            handle: hmon.0 as isize,
            device,
            bounds: to_rect(info.monitorInfo.rcMonitor),
            work: to_rect(info.monitorInfo.rcWork),
            primary: (info.monitorInfo.dwFlags & 1) != 0 /* MONITORINFOF_PRIMARY */,
            internal: false,
            dpi: dx.max(1),
        });
    }
    BOOL(1)
}

/// GDI device names of displays whose output technology is internal/embedded.
fn internal_device_names() -> Vec<String> {
    let mut names = Vec::new();
    unsafe {
        let (mut n_paths, mut n_modes) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes).is_err() {
            return names;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut n_paths,
            paths.as_mut_ptr(),
            &mut n_modes,
            modes.as_mut_ptr(),
            None,
        )
        .is_err()
        {
            return names;
        }
        for p in paths.iter().take(n_paths as usize) {
            let tech = p.targetInfo.outputTechnology;
            if tech != DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
                && tech != DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
            {
                continue;
            }
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) == 0 {
                let len = src.viewGdiDeviceName.iter().position(|&c| c == 0).unwrap_or(src.viewGdiDeviceName.len());
                names.push(String::from_utf16_lossy(&src.viewGdiDeviceName[..len]));
            }
        }
    }
    names
}

pub fn enumerate() -> Vec<Monitor> {
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_proc), LPARAM(&mut list as *mut Vec<Monitor> as isize));
    }
    let internal = internal_device_names();
    for m in &mut list {
        m.internal = internal.iter().any(|n| n.eq_ignore_ascii_case(&m.device));
    }
    list
}

/// Pick the target display per the `display` setting.
pub fn select<'a>(list: &'a [Monitor], setting: &str) -> Option<&'a Monitor> {
    if list.is_empty() {
        return None;
    }
    let primary = list.iter().find(|m| m.primary).or(list.first());
    match setting {
        "primary" => primary,
        "internal" => list.iter().find(|m| m.internal).or(primary),
        name => list.iter().find(|m| m.device.eq_ignore_ascii_case(name)).or(primary),
    }
}
