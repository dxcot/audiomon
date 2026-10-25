//! Persistent settings in the current user's registry: transport choice and autostart.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::{HSTRING, PCWSTR, w};

const SETTINGS_KEY: PCWSTR = w!("Software\\audiorast");
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("audiorast");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// USB tethering if it is on, else Wi-Fi / local network. Works without installing anything.
    Automatic,
    /// Android Open Accessory over a raw USB pipe. Needs the WinUSB driver.
    DirectUsb,
}

pub fn transport() -> Transport {
    match read(SETTINGS_KEY, w!("Transport")).as_deref() {
        Some("usb") => Transport::DirectUsb,
        _ => Transport::Automatic,
    }
}

pub fn set_transport(t: Transport) {
    let value = match t {
        Transport::Automatic => "auto",
        Transport::DirectUsb => "usb",
    };
    write(SETTINGS_KEY, w!("Transport"), value);
}

pub fn start_with_windows() -> bool {
    read(RUN_KEY, RUN_VALUE).is_some()
}

pub fn set_start_with_windows(enabled: bool) {
    if enabled {
        if let Ok(exe) = std::env::current_exe() {
            write(RUN_KEY, RUN_VALUE, &format!("\"{}\"", exe.display()));
        }
    } else {
        unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
}

fn read(key: PCWSTR, value: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1); // size includes the terminating NUL
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn write(key: PCWSTR, value: PCWSTR, data: &str) {
    let data = HSTRING::from(data);
    let bytes = (data.len() + 1) * 2;
    let r = unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, key, value, REG_SZ.0, Some(data.as_ptr().cast()), bytes as u32)
    };
    if r != ERROR_SUCCESS {
        log::warn!("could not save setting: {r:?}");
    }
}
