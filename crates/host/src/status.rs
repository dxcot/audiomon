//! One-line connection status, printed for the command line and shown in the tray.

use std::sync::Mutex;

static STATUS: Mutex<String> = Mutex::new(String::new());

pub fn set(msg: impl Into<String>) {
    let msg = msg.into();
    let mut current = STATUS.lock().unwrap();
    if *current != msg {
        println!("{msg}");
        log::info!("status: {msg}");
        *current = msg;
    }
}

pub fn get() -> String {
    STATUS.lock().unwrap().clone()
}
