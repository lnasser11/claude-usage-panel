//! Run-at-login via HKCU\Software\Microsoft\Windows\CurrentVersion\Run.

use windows::{
    core::{w, PCWSTR},
    Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_SET_VALUE, REG_SZ,
    },
};

const VALUE_NAME: PCWSTR = w!("ClaudeUsagePanel");
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

fn open_run_key() -> Option<HKEY> {
    let mut key = HKEY::default();
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, Some(0), KEY_SET_VALUE, &mut key) };
    if r.is_ok() {
        Some(key)
    } else {
        None
    }
}

/// Register (or remove) the current executable in the Run key.
pub fn apply(enabled: bool) -> Result<(), String> {
    let key = open_run_key().ok_or("cannot open HKCU Run key")?;
    let result = if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let cmd = format!("\"{}\"", exe.display());
        let wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
        let r = unsafe { RegSetValueExW(key, VALUE_NAME, Some(0), REG_SZ, Some(bytes)) };
        if r.is_ok() {
            Ok(())
        } else {
            Err(format!("RegSetValueExW failed: {:?}", r))
        }
    } else {
        // Deleting a value that does not exist is fine.
        let _ = unsafe { RegDeleteValueW(key, VALUE_NAME) };
        Ok(())
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}
