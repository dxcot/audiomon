//! Connects to the phone (USB tethering or Wi-Fi) and streams captured audio until the connection
//! drops, then reconnects.

use std::error::Error;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use audiorast_core::control::{Accept, Hello, Ping, Pong, Status};
use audiorast_core::packet::{self, Header};
use audiorast_core::{AUDIO_PORT, CONTROL_PORT};

use crate::capture::{self, Capture};
use crate::discover::{self, Candidate, Link};
use crate::status;

/// Streams over the network (USB tethering preferred, else Wi-Fi), reconnecting forever,
/// until `quit` is set.
pub fn run(phone: Option<Ipv4Addr>, quit: &AtomicBool) {
    while !quit.load(Ordering::Relaxed) {
        let Some((conn, ip, link)) = connect(phone) else {
            status::set("Looking for the phone: open the audiorast app on it (same Wi-Fi, or USB tethering on)");
            thread::sleep(Duration::from_secs(1));
            continue;
        };
        match stream(conn, ip, link, phone.is_none(), quit) {
            Ok(Ended::Upgrade) => {
                status::set("USB tethering available, switching to it");
                continue;
            }
            Ok(Ended::Disconnected) => status::set("Phone disconnected"),
            Err(e) => status::set(format!("Stream stopped: {e}")),
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn connect(phone: Option<Ipv4Addr>) -> Option<(TcpStream, Ipv4Addr, Link)> {
    let try_connect = |c: &Candidate| {
        let addr = SocketAddr::from((c.ip, CONTROL_PORT));
        let conn = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
        log::info!("connected to {} via {}", c.ip, c.adapter);
        Some((conn, c.ip, c.link))
    };
    if let Some(ip) = phone {
        return try_connect(&Candidate { ip, link: Link::Network, adapter: "--phone".into() });
    }
    // USB tethering first: lowest latency. Then anything on the local network.
    discover::candidates()
        .iter()
        .find_map(try_connect)
        .or_else(|| discover::lan_phones(Duration::from_millis(400)).iter().find_map(try_connect))
}

/// Why a stream ended without an error.
enum Ended {
    Disconnected,
    /// A lower-latency link (USB tethering) became available while streaming over Wi-Fi.
    Upgrade,
}

/// `allow_upgrade`: while on Wi-Fi, switch to USB tethering as soon as it appears.
fn stream(
    mut conn: TcpStream,
    ip: Ipv4Addr,
    link: Link,
    allow_upgrade: bool,
    quit: &AtomicBool,
) -> Result<Ended, Box<dyn Error>> {
    conn.set_nodelay(true)?;
    conn.set_read_timeout(Some(Duration::from_secs(3)))?;
    let capture = Capture::open()?;
    let period = capture.period_frames();
    Hello::new(capture::SAMPLE_RATE, capture::CHANNELS, period as u16).write_to(&mut conn)?;
    let accept = Accept::read_from(&mut conn)?;
    if accept.status != Status::Ok {
        return Err(format!("phone rejected the stream: {:?}", accept.status).into());
    }

    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.connect((ip, AUDIO_PORT))?;
    status::set(format!("Streaming to the phone ({ip}, {})", link.label()));

    let stop = AtomicBool::new(false);
    // Host clock for packet timestamps and clock-sync answers.
    let start = Instant::now();
    let now_us = move |at: Instant| at.duration_since(start).as_micros() as u64;
    let mut seq = 0u32;
    // One datagram per capture packet. Packets over ~180 frames (e.g. 10 ms) get IP-fragmented,
    // which is harmless on the USB link.
    let mut datagram = Vec::with_capacity(packet::HEADER_LEN + period * 8);

    let conn = &conn;
    let upgrade = AtomicBool::new(false);
    thread::scope(|s| {
        if allow_upgrade && link == Link::Network {
            // On Wi-Fi: watch for USB tethering, which has much less delay and jitter.
            s.spawn(|| {
                let mut since_check = Duration::ZERO;
                while !stop.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(100));
                    since_check += Duration::from_millis(100);
                    if since_check >= Duration::from_secs(2) {
                        since_check = Duration::ZERO;
                        if tethering_available() {
                            upgrade.store(true, Ordering::Relaxed);
                            stop.store(true, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
        // Answers clock-sync pings. The phone closing the connection ends this session.
        s.spawn(|| {
            let _ = conn.set_read_timeout(None);
            let mut io = conn;
            let mut buf = [0u8; Ping::LEN];
            while io.read_exact(&mut buf).is_ok() {
                let Ok(ping) = Ping::decode(&buf) else { break };
                let pong = Pong { phone_us: ping.phone_us, host_us: now_us(Instant::now()) };
                if io.write_all(&pong.encode()).is_err() {
                    break;
                }
            }
            stop.store(true, Ordering::Relaxed);
        });

        let result = capture.run(&stop, |p| {
            if quit.load(Ordering::Relaxed) {
                stop.store(true, Ordering::Relaxed);
            }
            // Timestamp of the packet's first frame: it was produced one packet length before
            // the packet was read.
            let duration_us = p.frames as u64 * 1_000_000 / capture::SAMPLE_RATE as u64;
            let host_ts_us = now_us(p.at).saturating_sub(duration_us);
            let header = Header { seq, frames: p.frames, host_ts_us };
            seq = seq.wrapping_add(1);
            // Silence costs one empty-payload datagram regardless of length.
            packet::encode(&header, if p.silent { &[] } else { p.samples }, &mut datagram);
            // A failed send (e.g. phone just unplugged) is noticed via the control connection.
            let _ = socket.send(&datagram);
        });
        // Unblocks the ping thread if capture ended first (e.g. audio device error).
        let _ = conn.shutdown(std::net::Shutdown::Both);
        result
    })?;
    Ok(if upgrade.load(Ordering::Relaxed) { Ended::Upgrade } else { Ended::Disconnected })
}

/// True once the phone is reachable over USB tethering.
fn tethering_available() -> bool {
    discover::candidates().iter().any(|c| {
        TcpStream::connect_timeout(&SocketAddr::from((c.ip, CONTROL_PORT)), Duration::from_millis(300)).is_ok()
    })
}
