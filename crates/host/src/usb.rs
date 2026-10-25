//! Direct USB transport (Android Open Accessory): no network stack, audio goes straight over
//! a USB bulk pipe. About 1 ms less transport delay than USB tethering, but needs a driver.
//!
//! Windows needs the WinUSB driver on the "Android Accessory" interface (e.g. via Zadig) and
//! on some interface of the phone before switching (the ADB interface already has it).

use std::error::Error;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use audiorast_core::control::{Hello, Ping, Pong};
use audiorast_core::packet::{self, Header};
use audiorast_core::stream::{self, Deframer, KIND_AUDIO, KIND_HELLO, KIND_PING, KIND_PONG, KIND_STATS};
use rusb::{Context, Device, DeviceHandle, Direction, TransferType, UsbContext};

use crate::capture::{self, Capture};
use crate::status;

const GOOGLE_VID: u16 = 0x18D1;
/// Accessory-mode product IDs: accessory, +adb, +audio, +audio+adb (0x2D00..=0x2D05).
const ACCESSORY_PIDS: std::ops::RangeInclusive<u16> = 0x2D00..=0x2D05;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(1);
/// Strings the phone app's accessory filter matches (see accessory_filter.xml).
const IDENTITY: [&str; 6] = ["audiorast", "audiorast", "PC audio over USB", "1", "", "1"];

/// Streams over direct USB, reconnecting after unplugs and errors, until `quit` is set.
pub fn run(quit: &AtomicBool) {
    while !quit.load(Ordering::Relaxed) {
        match session(quit) {
            Ok(()) => status::set("Phone disconnected"),
            Err(e) => status::set(format!("Direct USB: {e}")),
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn session(quit: &AtomicBool) -> Result<(), Box<dyn Error>> {
    let ctx = Context::new()?;
    let device = match find_accessory(&ctx) {
        Some(d) => d,
        None => {
            switch_to_accessory(&ctx)?;
            wait_for_accessory(&ctx)?
        }
    };
    let handle = device.open()?;
    let (iface, ep_in, ep_out) = bulk_endpoints(&device)?;
    handle.claim_interface(iface)?;
    log::info!("USB accessory open (interface {iface}, in {ep_in:#04x}, out {ep_out:#04x})");
    stream(&handle, ep_in, ep_out, quit)
}

fn find_accessory(ctx: &Context) -> Option<Device<Context>> {
    ctx.devices().ok()?.iter().find(|d| {
        d.device_descriptor()
            .is_ok_and(|desc| desc.vendor_id() == GOOGLE_VID && ACCESSORY_PIDS.contains(&desc.product_id()))
    })
}

/// Asks every reachable Android device to re-enumerate in accessory mode (AOA protocol).
fn switch_to_accessory(ctx: &Context) -> Result<(), Box<dyn Error>> {
    let mut switched = 0;
    for device in ctx.devices()?.iter() {
        let handle = match device.open() {
            Ok(h) => h,
            Err(e) => {
                log::debug!("skip {device:?}: {e}");
                continue;
            }
        };
        let mut version = [0u8; 2];
        // GET_PROTOCOL (51): non-zero means the device supports accessory mode.
        let Ok(2) = handle.read_control(0xC0, 51, 0, 0, &mut version, CONTROL_TIMEOUT) else { continue };
        let protocol = u16::from_le_bytes(version);
        if protocol == 0 {
            continue;
        }
        for (index, s) in IDENTITY.iter().enumerate() {
            let mut bytes = s.as_bytes().to_vec();
            bytes.push(0);
            // SEND_STRING (52)
            handle.write_control(0x40, 52, 0, index as u16, &bytes, CONTROL_TIMEOUT)?;
        }
        // START (53)
        handle.write_control(0x40, 53, 0, 0, &[], CONTROL_TIMEOUT)?;
        log::info!("switched {:?} to accessory mode (AOA v{protocol})", device);
        switched += 1;
    }
    if switched == 0 {
        return Err("no Android device could be opened. Is the phone plugged in with USB debugging on \
                    (its ADB interface lets Windows talk to it)?"
            .into());
    }
    Ok(())
}

fn wait_for_accessory(ctx: &Context) -> Result<Device<Context>, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(250));
        if let Some(d) = find_accessory(ctx) {
            return Ok(d);
        }
    }
    Err("phone did not reappear in accessory mode".into())
}

/// Interface number and bulk IN/OUT endpoint addresses of the accessory interface.
fn bulk_endpoints(device: &Device<Context>) -> Result<(u8, u8, u8), Box<dyn Error>> {
    let config = device.active_config_descriptor()?;
    for iface in config.interfaces() {
        for alt in iface.descriptors() {
            let bulk = |dir| {
                alt.endpoint_descriptors()
                    .find(|e| e.transfer_type() == TransferType::Bulk && e.direction() == dir)
                    .map(|e| e.address())
            };
            // The accessory interface is the vendor-specific one with a bulk pair (class 0xFF).
            if alt.class_code() == 0xFF
                && let (Some(i), Some(o)) = (bulk(Direction::In), bulk(Direction::Out))
            {
                return Ok((alt.interface_number(), i, o));
            }
        }
    }
    Err("accessory interface not found".into())
}

fn stream(handle: &DeviceHandle<Context>, ep_in: u8, ep_out: u8, quit: &AtomicBool) -> Result<(), Box<dyn Error>> {
    let capture = Capture::open()?;
    // Announce the new stream so the phone drops any state from a previous one.
    let mut hello = Vec::new();
    Hello::new(capture::SAMPLE_RATE, capture::CHANNELS, capture.period_frames() as u16).write_to(&mut hello)?;
    let mut frame = Vec::new();
    stream::encode(KIND_HELLO, &hello, &mut frame);
    // The write only completes once the app on the phone reads the accessory.
    loop {
        if quit.load(Ordering::Relaxed) {
            return Ok(());
        }
        match handle.write_bulk(ep_out, &frame, Duration::from_secs(1)) {
            Ok(_) => break,
            Err(rusb::Error::Timeout) => status::set("Connected by USB, waiting for the audiorast app on the phone"),
            Err(e) => return Err(e.into()),
        }
    }
    status::set("Streaming to the phone (direct USB)");
    let stop = AtomicBool::new(false);
    let start = Instant::now();
    let now_us = move |at: Instant| at.duration_since(start).as_micros() as u64;
    // Audio and pong writes come from two threads.
    let out = Mutex::new(());
    let write = |frame: &[u8]| {
        let _guard = out.lock().unwrap();
        handle.write_bulk(ep_out, frame, Duration::from_millis(100))
    };

    thread::scope(|s| {
        // Answers clock-sync pings arriving on the bulk IN pipe.
        s.spawn(|| {
            let mut buf = vec![0u8; 16 * 1024];
            let mut deframer = Deframer::default();
            let mut frame = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                match handle.read_bulk(ep_in, &mut buf, Duration::from_millis(200)) {
                    Ok(n) => deframer.push(&buf[..n]),
                    Err(rusb::Error::Timeout) => continue,
                    Err(e) => {
                        log::warn!("usb read: {e}");
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                while let Ok(Some((kind, payload))) = deframer.next_message() {
                    if kind == KIND_STATS {
                        let text = String::from_utf8_lossy(payload);
                        log::info!("phone: {text}");
                        // Lines start with "latency N ms = ..." once the clocks are synced.
                        if let Some(latency) = text.split(" =").next().filter(|l| l.starts_with("latency")) {
                            status::set(format!("Streaming to the phone (direct USB), {latency}"));
                        }
                        continue;
                    }
                    if kind != KIND_PING {
                        continue;
                    }
                    let Ok(bytes) = payload.try_into() else { continue };
                    let Ok(ping) = Ping::decode(bytes) else { continue };
                    let pong = Pong { phone_us: ping.phone_us, host_us: now_us(Instant::now()) };
                    frame.clear();
                    stream::encode(KIND_PONG, &pong.encode(), &mut frame);
                    let _ = write(&frame);
                }
            }
        });

        let mut seq = 0u32;
        let mut datagram = Vec::new();
        let mut frame = Vec::new();
        let mut dropped = 0u64;
        let result = capture.run(&stop, |p| {
            if quit.load(Ordering::Relaxed) {
                stop.store(true, Ordering::Relaxed);
            }
            let duration_us = p.frames as u64 * 1_000_000 / capture::SAMPLE_RATE as u64;
            let header = Header { seq, frames: p.frames, host_ts_us: now_us(p.at).saturating_sub(duration_us) };
            seq = seq.wrapping_add(1);
            packet::encode(&header, if p.silent { &[] } else { p.samples }, &mut datagram);
            frame.clear();
            stream::encode(KIND_AUDIO, &datagram, &mut frame);
            match write(&frame) {
                Ok(_) => {}
                // The app hasn't opened the accessory yet: drop audio rather than queue it.
                Err(rusb::Error::Timeout) => dropped += 1,
                Err(e) => {
                    log::warn!("usb write: {e}");
                    stop.store(true, Ordering::Relaxed);
                }
            }
        });
        stop.store(true, Ordering::Relaxed);
        if dropped > 0 {
            log::info!("{dropped} packets dropped while the phone was not reading");
        }
        result
    })
}
