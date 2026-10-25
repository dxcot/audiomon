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

const BLUE: [u8; 3] = [0x2E, 0x86, 0xDE];
const GREY: [u8; 3] = [0x8A, 0x8F, 0x98];

use crate::settings::{self, Transport};
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
    let _instance = unsafe { CreateMutexW(None, false, w!("Local\\audiorast-tray")) }?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            MessageBoxW(
                None,
                w!("audiorast is already running.\n\nLook for the speaker icon near the clock (it may be behind the ^ arrow). Click it to see the status or quit."),
                w!("audiorast"),
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
        .with_tooltip("audiorast")
        .with_icon(speaker_icon(GREY))
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
            let _ = tray.set_tooltip(Some(format!("audiorast: {current}")));
            // Blue while audio is flowing, grey otherwise.
            let now_streaming = current.starts_with("Streaming");
            if now_streaming != streaming {
                streaming = now_streaming;
                let _ = tray.set_icon(Some(speaker_icon(if streaming { BLUE } else { GREY })));
            }
            shown = current;
        }
    }
    Ok(())
}

/// 32×32 speaker glyph with sound waves, drawn in code to avoid shipping image files.
fn speaker_icon([red, green, blue]: [u8; 3]) -> Icon {
    const N: usize = 32;
    let mut rgba = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let dy = (fy - 16.0).abs();
            // Speaker body: a small box and a cone widening to the right.
            let body = (3.0..9.0).contains(&fx) && dy < 5.0;
            let cone = (9.0..17.0).contains(&fx) && dy < 5.0 + (fx - 9.0);
            // Two sound-wave arcs centred on the cone.
            let r = ((fx - 12.0).powi(2) + (fy - 16.0).powi(2)).sqrt();
            let wave = fx > 18.0 && ((r - 10.5).abs() < 1.3 || (r - 15.5).abs() < 1.3) && dy < r * 0.75;
            if body || cone || wave {
                let i = (y * N + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[red, green, blue, 0xFF]);
            }
        }
    }
    Icon::from_rgba(rgba, N as u32, N as u32).expect("valid icon size")
}
