//! Tray icon app: streams in the background, menu for status, transport, autostart and quit.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MB_ICONINFORMATION, MB_OK, MSG, MessageBoxW, SetTimer, TranslateMessage,
};
use windows::core::w;


use crate::settings::{self, Transport};
use crate::glyph::{self, Style};
use crate::{sender, status, usb};

/// Streaming worker for one transport; replaced when the user switches transports.
struct Worker {
    quit: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    /// `previous` is stopped and waited for on the new thread, so the UI never blocks.
    fn start(transport: Transport, previous: Option<Worker>) -> Self {
        let quit = Arc::new(AtomicBool::new(false));
        let q = quit.clone();
        let handle = thread::spawn(move || {
            if let Some(mut p) = previous {
                p.stop_and_join();
            }
            match transport {
                Transport::Automatic => sender::run(None, &q),
                Transport::DirectUsb => usb::run(&q),
            }
        });
        Self { quit, handle: Some(handle) }
    }

    fn stop_and_join(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // One instance per user session: a second one would fight over the audio and USB device.
    let _instance = unsafe { CreateMutexW(None, false, w!("Local\\audiomon-tray")) }?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            MessageBoxW(
                None,
                w!("audiomon is already running.\n\nLook for the record icon near the clock (it may be behind the ^ arrow). Click it to see the status or quit."),
                w!("audiomon"),
                MB_OK | MB_ICONINFORMATION,
            );
        }
        return Ok(());
    }

    let mut transport = settings::transport();
    let mut worker = Some(Worker::start(transport, None));

    let status_item = MenuItem::new("Starting…", false, None);
    let automatic = CheckMenuItem::new("Automatic: USB tethering or Wi-Fi (recommended)", true, transport == Transport::Automatic, None);
    let direct = CheckMenuItem::new("Direct USB (advanced: needs USB debugging + WinUSB driver)", true, transport == Transport::DirectUsb, None);
    let autostart = CheckMenuItem::new("Start with Windows", true, settings::start_with_windows(), None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &status_item,
        &PredefinedMenuItem::separator(),
        &automatic,
        &direct,
        &PredefinedMenuItem::separator(),
        &autostart,
        &PredefinedMenuItem::separator(),
        &quit,
    ])?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("audiomon")
        .with_icon(record_icon(Style::Idle))
        // Left click opens the menu too: its first line is the status.
        .with_menu_on_left_click(true)
        .build()?;
    let mut streaming = false;

    // A periodic timer wakes the message loop to refresh the status text.
    unsafe { SetTimer(None, 0, 500, None) };
    let mut shown = String::new();
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let chosen = if event.id == *automatic.id() {
                Some(Transport::Automatic)
            } else if event.id == *direct.id() {
                Some(Transport::DirectUsb)
            } else if event.id == *autostart.id() {
                settings::set_start_with_windows(autostart.is_checked());
                None
            } else if event.id == *quit.id() {
                if let Some(mut w) = worker.take() {
                    w.stop_and_join();
                }
                return Ok(());
            } else {
                None
            };
            if let Some(t) = chosen {
                // Check items toggle on click; keep exactly one transport checked.
                automatic.set_checked(t == Transport::Automatic);
                direct.set_checked(t == Transport::DirectUsb);
                if t != transport {
                    transport = t;
                    settings::set_transport(t);
                    status::set("Switching…");
                    worker = Some(Worker::start(t, worker.take()));
                }
            }
        }

        let current = status::get();
        if current != shown {
            status_item.set_text(&current);
            let _ = tray.set_tooltip(Some(format!("audiomon: {current}")));
            // Navy label with gold rim while audio flows, greyed otherwise.
            let now_streaming = current.starts_with("Streaming");
            if now_streaming != streaming {
                streaming = now_streaming;
                let _ = tray.set_icon(Some(record_icon(if streaming { Style::Live } else { Style::Idle })));
            }
            shown = current;
        }
    }
    Ok(())
}

/// The record icon (see glyph.rs) at tray size.
fn record_icon(style: Style) -> Icon {
    const N: u32 = 32;
    Icon::from_rgba(glyph::record_rgba(N, style), N, N).expect("valid icon size")
}
