//! `log` backend writing to Android logcat (tag "audiomon").

use std::ffi::{CString, c_char, c_int};

const TAG: &[u8] = b"audiomon\0";

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

struct Logcat;

impl log::Log for Logcat {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let prio = match r.level() {
            log::Level::Error => 6,
            log::Level::Warn => 5,
            log::Level::Info => 4,
            log::Level::Debug => 3,
            log::Level::Trace => 2,
        };
        if let Ok(text) = CString::new(r.args().to_string()) {
            unsafe { __android_log_write(prio, TAG.as_ptr().cast(), text.as_ptr()) };
        }
    }

    fn flush(&self) {}
}

pub fn init() {
    if log::set_logger(&Logcat).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}
