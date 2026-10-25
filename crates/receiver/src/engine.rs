//! Receiver engine: waits for a host on the control port, then plays its UDP audio stream.
//! Alternatively plays audio from a USB accessory pipe ([`Engine::attach_accessory`]).
//!
//! Threads:
//! - control: accepts one host at a time, runs the handshake, owns the session lifetime;
//! - network (or `ar-usb` for the accessory): receive → [`Depacketizer`] → ring buffer;
//! - AAudio callback (real-time): ring buffer → [`Playout`] → device.

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use audiomon_core::control::{Accept, ClockSync, Hello, Ping, Pong, Status};
use audiomon_core::depacketizer::{Depacketizer, DepacketizerStats};
use audiomon_core::jitter::DelayHistogram;
use audiomon_core::packet::Header;
use audiomon_core::playout::{Playout, PlayoutConfig, PlayoutStats};
use audiomon_core::stream::{self as stream_frame, Deframer, KIND_AUDIO, KIND_HELLO, KIND_PING, KIND_PONG, KIND_STATS};
use audiomon_core::{
    AUDIO_PORT, CONTROL_PORT, DISCOVERY_PORT, DISCOVERY_REPLY, DISCOVERY_REQUEST, PROTO_VERSION, SampleSink, SampleSource,
};

use crate::aaudio::{self, OutputStream, monotonic_ns};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
/// Ring buffer between network and audio threads: 1 s of audio.
const RING_SAMPLES: usize = SAMPLE_RATE as usize * CHANNELS as usize;
/// AAudio device buffer at start, in bursts. It grows by one burst on every xrun.
const DEVICE_BUFFER_BURSTS: i32 = 1;
/// Upper bound for the self-tuned device buffer.
const MAX_DEVICE_BUFFER_BURSTS: i32 = 6;
const PING_INTERVAL: Duration = Duration::from_millis(500);
/// No data or pong from the PC for this long means it stopped (or the cable is out).
const IDLE_TIMEOUT: Duration = Duration::from_secs(3);
/// Window over which arrival jitter (peak-to-peak) is measured.
const JITTER_WINDOW: Duration = Duration::from_secs(2);
/// Sentinel for "clock offset not known yet".
const NO_OFFSET: i64 = i64::MIN;
/// Fraction of packets the playout margin must cover; tuned in the core simulation tests.
const JITTER_QUANTILE: f64 = 0.9995;
/// Packets (~30 s at 10 ms) that shape the lateness histogram.
const JITTER_MEMORY_PACKETS: u32 = 3000;
/// Arrivals right after connecting are chaotic (clock sync converging, startup bursts) and
/// would inflate the histogram for minutes; they are not fed to it.
const JITTER_WARMUP: Duration = Duration::from_secs(5);
/// Packet size assumed for accessory streams, which have no handshake (Windows' 10 ms period).
const ACCESSORY_PACKET_FRAMES: usize = 480;

/// Where the latency goes, measured per packet with synced clocks. All in ms.
#[derive(Debug, Default, Clone, Copy)]
pub struct Timing {
    /// Age of a packet's first frame when it arrives: PC capture + USB transfer.
    pub capture_and_link_ms: f64,
    /// Peak-to-peak variation of arrival times (as seen by this app) over the last window.
    pub arrival_jitter_ms: f64,
    /// The same, using the kernel's receive timestamps: jitter of the PC + USB link alone.
    pub link_jitter_ms: f64,
    /// Longest delay between the kernel receiving a packet and this app reading it, last window.
    pub wake_max_ms: f64,
    /// Audio queued on the phone ahead of a newly arrived packet.
    pub buffer_ms: f64,
    pub clock_rtt_ms: f64,
}

#[derive(Debug, Default, Clone)]
pub struct EngineStatus {
    pub state: String,
    /// How the PC is connected, for display.
    pub transport: &'static str,
    pub host: Option<SocketAddr>,
    pub device: Option<aaudio::Info>,
    pub output_latency_ms: Option<f64>,
    pub xruns: i32,
    pub net: DepacketizerStats,
    pub playout: PlayoutStats,
    /// `None` until the clocks are synced.
    pub timing: Option<Timing>,
}

struct Shared {
    stop: AtomicBool,
    status: Mutex<EngineStatus>,
    /// Written by the audio callback with `try_lock`, so it never blocks there.
    playout: Mutex<PlayoutStats>,
    /// Host clock minus phone clock (µs), or [`NO_OFFSET`].
    clock_offset_us: AtomicI64,
    /// Jitter the playout should cover (ms, f64 bits), from the arrival histogram; 0 = unknown.
    jitter_target_ms: AtomicU64,
    /// Round trip of the best clock-sync sample (µs).
    clock_rtt_us: AtomicI64,
    /// Phone time (µs) when the PC last sent anything; 0 = never.
    last_rx_us: AtomicI64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            stop: AtomicBool::new(false),
            status: Mutex::default(),
            playout: Mutex::default(),
            clock_offset_us: AtomicI64::new(NO_OFFSET),
            jitter_target_ms: AtomicU64::new(0),
            clock_rtt_us: AtomicI64::new(0),
            last_rx_us: AtomicI64::new(0),
        }
    }
}

fn now_us() -> i64 {
    monotonic_ns() / 1000
}

pub struct Engine {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl Engine {
    pub fn start() -> io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", CONTROL_PORT))?;
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared::default());
        set_state(&shared, "Waiting for PC…");
        let s = shared.clone();
        let control = thread::Builder::new().name("ar-control".into()).spawn(move || control_loop(listener, &s))?;
        let mut threads = vec![control];
        // Wi-Fi / LAN: answer PCs looking for us. Not fatal if the port is taken.
        match UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)) {
            Ok(socket) => {
                let s = shared.clone();
                threads.push(thread::Builder::new().name("ar-discovery".into()).spawn(move || discovery_loop(socket, &s))?);
            }
            Err(e) => log::warn!("discovery unavailable: {e}"),
        }
        Ok(Self { shared, threads })
    }

    /// Starts playing from an open USB accessory (fd from `UsbManager.openAccessory`), which
    /// the engine takes ownership of.
    pub fn attach_accessory(&self, fd: RawFd) -> io::Result<()> {
        let shared = self.shared.clone();
        thread::Builder::new().name("ar-accessory".into()).spawn(move || {
            if let Err(e) = accessory_session(fd, &shared) {
                log::warn!("accessory session: {e}");
            }
            let mut st = shared.status.lock().unwrap();
            *st = EngineStatus { state: "Waiting for PC…".into(), ..Default::default() };
        })?;
        Ok(())
    }

    pub fn status(&self) -> EngineStatus {
        let mut st = self.shared.status.lock().unwrap().clone();
        st.playout = *self.shared.playout.lock().unwrap();
        st
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        for h in self.threads.drain(..) {
            let _ = h.join();
        }
    }
}

fn set_state(shared: &Shared, state: &str) {
    shared.status.lock().unwrap().state = state.into();
}

fn control_loop(listener: TcpListener, shared: &Arc<Shared>) {
    while !shared.stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((conn, addr)) => {
                log::info!("host connected: {addr}");
                let result = session(conn, addr, shared);
                let mut st = shared.status.lock().unwrap();
                // A playing session that ends in an error means the PC vanished (cable pulled,
                // network gone); a clean close or a failed handshake just goes back to waiting.
                let lost = st.state == "Playing" && result.is_err();
                if let Err(e) = result {
                    log::warn!("session ended: {e}");
                }
                let state = if lost { "Lost" } else { "Waiting for PC…" };
                *st = EngineStatus { state: state.into(), ..Default::default() };
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                log::error!("accept: {e}");
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn session(mut conn: TcpStream, addr: SocketAddr, shared: &Arc<Shared>) -> io::Result<()> {
    conn.set_nonblocking(false)?;
    conn.set_nodelay(true)?;
    conn.set_read_timeout(Some(Duration::from_secs(2)))?;
    let hello = Hello::read_from(&mut conn)?;
    let status = if hello.version != PROTO_VERSION {
        Status::UnsupportedVersion
    } else if hello.sample_rate != SAMPLE_RATE || hello.channels != CHANNELS || hello.packet_frames == 0 {
        Status::UnsupportedFormat
    } else {
        Status::Ok
    };
    Accept { version: PROTO_VERSION, status }.write_to(&mut conn)?;
    if status != Status::Ok {
        return Err(io::Error::other(format!("rejected host: {status:?} ({hello:?})")));
    }

    let socket = UdpSocket::bind(("0.0.0.0", AUDIO_PORT))?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let (mut stream, producer) = start_playback(shared, hello.packet_frames as usize)?;
    {
        let mut st = shared.status.lock().unwrap();
        st.state = "Playing".into();
        st.transport = link_name(addr);
        st.host = Some(addr);
    }

    // Session ends when the host closes the control connection, the engine stops, or audio fails.
    let session_stop = Arc::new(AtomicBool::new(false));
    let net = {
        let stop = session_stop.clone();
        let shared = shared.clone();
        thread::Builder::new()
            .name("ar-net".into())
            .spawn(move || net_loop(socket, producer, &stop, &shared))?
    };
    // Control loop: clock-sync pings, liveness, and status updates.
    conn.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut sync = ClockSync::default();
    let mut next_ping = Instant::now();
    // Pongs are accumulated across reads, since a read timeout may split one.
    let mut pong = [0u8; Pong::LEN];
    let mut have = 0;
    let mut next_log = Instant::now() + Duration::from_secs(5);
    let mut last_pong = Instant::now();
    let result = loop {
        if Instant::now() >= next_log {
            next_log += Duration::from_secs(5);
            let _ = log_summary(shared);
        }
        if shared.stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        // TCP alone notices a pulled cable only after minutes; missing pongs show it in seconds.
        if last_pong.elapsed() > IDLE_TIMEOUT {
            break Err(io::Error::new(io::ErrorKind::TimedOut, "PC stopped answering"));
        }
        if let Some(e) = stream.error() {
            break Err(io::Error::other(e));
        }
        if Instant::now() >= next_ping {
            next_ping += PING_INTERVAL;
            if let Err(e) = conn.write_all(&Ping { phone_us: now_us() as u64 }.encode()) {
                break Err(e);
            }
        }
        match conn.read(&mut pong[have..]) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                have += n;
                if have == Pong::LEN {
                    have = 0;
                    match Pong::decode(&pong) {
                        Ok(p) => {
                            last_pong = Instant::now();
                            sync.add(p.phone_us, p.host_us, now_us() as u64);
                            if let Some(o) = sync.offset_us() {
                                shared.clock_offset_us.store(o, Ordering::Relaxed);
                            }
                        }
                        Err(e) => break Err(e),
                    }
                }
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(e) => break Err(e),
        }
        update_device_status(shared, &mut stream);
        if let Some(rtt) = sync.rtt_us() {
            shared.clock_rtt_us.store(rtt, Ordering::Relaxed);
        }
    };
    session_stop.store(true, Ordering::Relaxed);
    end_session(shared);
    let _ = net.join();
    drop(stream);
    log::info!("host disconnected: {addr}");
    result
}

/// Opens the AAudio stream fed by a fresh ring buffer; returns the stream and the ring's
/// write side for the receiving thread.
fn start_playback(shared: &Arc<Shared>, packet_frames: usize) -> io::Result<(OutputStream, RingSink)> {
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(RING_SAMPLES);
    let mut playout = Playout::new(PlayoutConfig {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS as usize,
        packet_frames,
        ..Default::default()
    });
    let mut source = RingSource(consumer);
    let stats_out = shared.clone();
    let mut pulls = 0u32;
    let render = Box::new(move |out: &mut [f32]| {
        let target = f64::from_bits(stats_out.jitter_target_ms.load(Ordering::Relaxed));
        if target > 0.0 {
            playout.set_jitter_target_ms(target);
        }
        playout.pull(&mut source, out);
        pulls = pulls.wrapping_add(1);
        if pulls.is_multiple_of(32)
            && let Ok(mut s) = stats_out.playout.try_lock()
        {
            *s = playout.stats();
        }
    });
    let stream = OutputStream::open(
        aaudio::Config {
            sample_rate: SAMPLE_RATE as i32,
            channels: CHANNELS as i32,
            exclusive: true,
            buffer_bursts: DEVICE_BUFFER_BURSTS,
        },
        render,
    )
    .map_err(io::Error::other)?;
    let info = stream.info();
    log::info!("audio stream: {info:?}");
    stream.start().map_err(io::Error::other)?;
    shared.status.lock().unwrap().device = Some(info);
    Ok((stream, RingSink(producer)))
}

/// Periodic device bookkeeping: output latency, xruns, and growing the device buffer on xruns.
fn update_device_status(shared: &Shared, stream: &mut OutputStream) {
    let mut st = shared.status.lock().unwrap();
    st.output_latency_ms = stream.output_latency_ms();
    let xruns = stream.xruns();
    if xruns > st.xruns {
        // The device buffer was too small for this phone: grow it by one burst.
        let burst = stream.info().burst_frames;
        let size = stream.info().buffer_frames;
        if size < burst * MAX_DEVICE_BUFFER_BURSTS {
            let applied = stream.set_buffer_frames(size + burst);
            log::info!("xrun: device buffer {size} -> {applied} frames");
            st.device = Some(stream.info());
        }
    }
    st.xruns = xruns;
    let rtt = shared.clock_rtt_us.load(Ordering::Relaxed);
    if let Some(t) = st.timing.as_mut() {
        t.clock_rtt_ms = rtt as f64 / 1000.0;
    }
}

fn end_session(shared: &Shared) {
    shared.clock_offset_us.store(NO_OFFSET, Ordering::Relaxed);
    shared.jitter_target_ms.store(0, Ordering::Relaxed);
}

/// Per-packet work shared by all transports: timing, jitter histogram, depacketizing.
struct PacketIn {
    depack: Depacketizer,
    timing: TimingMeter,
    lateness: DelayHistogram,
    warm_from: Instant,
    count: u32,
}

impl PacketIn {
    fn new() -> Self {
        Self {
            depack: Depacketizer::new(CHANNELS as usize),
            timing: TimingMeter::default(),
            lateness: DelayHistogram::new(JITTER_MEMORY_PACKETS),
            warm_from: Instant::now() + JITTER_WARMUP,
            count: 0,
        }
    }

    /// `wake_delay_us`: how long the packet waited in the kernel before being read, if known.
    fn handle(&mut self, datagram: &[u8], wake_delay_us: Option<i64>, sink: &mut RingSink, shared: &Shared) {
        let offset = shared.clock_offset_us.load(Ordering::Relaxed);
        if offset != NO_OFFSET
            && let Some((header, _)) = Header::read(datagram)
        {
            let buffered = (RING_SAMPLES - sink.free()) / CHANNELS as usize;
            let lag = self.timing.add(&header, offset, buffered, wake_delay_us);
            let packet_ms = header.frames as f64 * 1000.0 / SAMPLE_RATE as f64;
            if Instant::now() >= self.warm_from {
                self.lateness.add_packet(header.seq, lag, packet_ms);
            }
            if self.lateness.count() > 50 {
                let target = self.lateness.quantile(JITTER_QUANTILE);
                shared.jitter_target_ms.store(target.to_bits(), Ordering::Relaxed);
            }
        }
        self.depack.push(datagram, sink);
        self.count = self.count.wrapping_add(1);
        if self.count.is_multiple_of(50) {
            let mut st = shared.status.lock().unwrap();
            st.net = self.depack.stats();
            st.timing = self.timing.result();
        }
    }
}

fn net_loop(socket: UdpSocket, mut sink: RingSink, stop: &AtomicBool, shared: &Shared) {
    raise_thread_priority();
    enable_kernel_timestamps(&socket);
    let mut packets = PacketIn::new();
    let mut buf = vec![0u8; 65_536];
    while !stop.load(Ordering::Relaxed) {
        match recv_timestamped(&socket, &mut buf) {
            Ok((len, wake_delay_us)) => packets.handle(&buf[..len], wake_delay_us, &mut sink, shared),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(e) => {
                log::error!("udp recv: {e}");
                break;
            }
        }
    }
}

/// Plays audio arriving over the USB accessory pipe (`fd` from `UsbManager.openAccessory`).
/// Runs until the accessory is detached or the engine stops.
fn accessory_session(fd: RawFd, shared: &Arc<Shared>) -> io::Result<()> {
    // SAFETY: the app hands over ownership of an open accessory file descriptor.
    let mut reader = unsafe { File::from_raw_fd(fd) };
    let mut writer = reader.try_clone()?;
    let (mut stream, mut sink) = start_playback(shared, ACCESSORY_PACKET_FRAMES)?;
    {
        let mut st = shared.status.lock().unwrap();
        st.state = "Playing".into();
        st.transport = "USB direct (accessory)";
    }

    let done = Arc::new(AtomicBool::new(false));
    let rx = {
        let done = done.clone();
        let shared = shared.clone();
        thread::Builder::new().name("ar-usb".into()).spawn(move || {
            raise_thread_priority();
            let mut packets = PacketIn::new();
            let mut deframer = Deframer::default();
            let mut sync = ClockSync::default();
            // Accessory sessions outlive PC restarts (the pipe stays open until unplugged), so
            // per-stream state is reset whenever the PC announces a new stream.
            // Accessory reads must ask for at least 16 KB (Android USB accessory requirement).
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        log::warn!("accessory read: {e}");
                        break;
                    }
                };
                deframer.push(&buf[..n]);
                shared.last_rx_us.store(now_us(), Ordering::Relaxed);
                loop {
                    match deframer.next_message() {
                        Ok(Some((KIND_AUDIO, datagram))) => packets.handle(datagram, None, &mut sink, &shared),
                        Ok(Some((KIND_HELLO, _))) => {
                            log::info!("new stream from PC");
                            packets = PacketIn::new();
                            sync = ClockSync::default();
                            end_session(&shared);
                        }
                        Ok(Some((KIND_PONG, payload))) => {
                            if let Ok(bytes) = payload.try_into()
                                && let Ok(p) = Pong::decode(bytes)
                            {
                                sync.add(p.phone_us, p.host_us, now_us() as u64);
                                if let (Some(o), Some(rtt)) = (sync.offset_us(), sync.rtt_us()) {
                                    shared.clock_offset_us.store(o, Ordering::Relaxed);
                                    shared.clock_rtt_us.store(rtt, Ordering::Relaxed);
                                }
                            }
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(e) => {
                            log::warn!("accessory stream corrupt: {e}");
                            done.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            }
            done.store(true, Ordering::Relaxed);
        })?
    };

    let mut next_ping = Instant::now();
    let mut next_log = Instant::now() + Duration::from_secs(5);
    let mut frame = Vec::new();
    let result = loop {
        if shared.stop.load(Ordering::Relaxed) || done.load(Ordering::Relaxed) {
            break Ok(());
        }
        if let Some(e) = stream.error() {
            break Err(io::Error::other(e));
        }
        let now = Instant::now();
        if now >= next_ping {
            next_ping += PING_INTERVAL;
            frame.clear();
            stream_frame::encode(KIND_PING, &Ping { phone_us: now_us() as u64 }.encode(), &mut frame);
            if let Err(e) = writer.write_all(&frame) {
                break Err(e);
            }
        }
        if now >= next_log {
            next_log += Duration::from_secs(5);
            // Also sent to the PC: while it holds the USB device, adb/logcat can't reach the phone.
            let text = log_summary(shared);
            frame.clear();
            stream_frame::encode(KIND_STATS, text.as_bytes(), &mut frame);
            if let Err(e) = writer.write_all(&frame) {
                break Err(e);
            }
        }
        update_device_status(shared, &mut stream);
        // The pipe stays open when the PC program stops, so silence is the only sign of it.
        let idle_us = now_us() - shared.last_rx_us.load(Ordering::Relaxed);
        let state = if idle_us > IDLE_TIMEOUT.as_micros() as i64 { "Waiting for PC…" } else { "Playing" };
        shared.status.lock().unwrap().state = state.into();
        thread::sleep(Duration::from_millis(100));
    };
    end_session(shared);
    drop(stream);
    // The reader exits when the accessory is detached; it is not joined so a stuck read
    // can't block the engine.
    drop(rx);
    log::info!("accessory session ended");
    result
}

/// One logcat line per call, for reading results with `adb logcat -s audiomon`.
fn log_summary(shared: &Shared) -> String {
    let text = summary(shared);
    log::info!("{text}");
    text
}

fn summary(shared: &Shared) -> String {
    let st = shared.status.lock().unwrap().clone();
    let p = *shared.playout.lock().unwrap();
    let out = st.output_latency_ms.unwrap_or(f64::NAN);
    match st.timing {
        Some(t) => format!(
            "latency {:.1} ms = capture+link {:.1} + buffer {:.1} + output {:.1} | arrival jitter {:.2} ms (link {:.2}, app wake max {:.2}), \
             margin {:.1} ms (jitter est. {:.1}), drift {:+.0} ppm, underruns {}, xruns {} (device buf {} fr), lost {}, rtt {:.2} ms",
            t.capture_and_link_ms + t.buffer_ms + out,
            t.capture_and_link_ms,
            t.buffer_ms,
            out,
            t.arrival_jitter_ms,
            t.link_jitter_ms,
            t.wake_max_ms,
            p.margin_ms,
            p.jitter_ms,
            p.drift_ppm,
            p.underruns,
            st.xruns,
            st.device.map_or(0, |d| d.buffer_frames),
            st.net.lost,
            t.clock_rtt_ms,
        ),
        None => format!("clock not synced yet; output {out:.1} ms, underruns {}", p.underruns),
    }
}

/// Accumulates per-packet latency measurements (smoothed) and arrival jitter (windowed).
#[derive(Default)]
struct TimingMeter {
    age_ms: Option<f64>,
    buffer_ms: f64,
    window_start: Option<Instant>,
    /// (min, max) arrival lag in the current window: as seen by the app, and by the kernel.
    app_lag: (f64, f64),
    link_lag: (f64, f64),
    wake_max: f64,
    result: Timing,
}

impl TimingMeter {
    /// `wake_delay_us`: time between the kernel receiving the datagram and us reading it.
    /// Returns the packet's arrival lag (ms) as seen by this app.
    fn add(&mut self, header: &Header, clock_offset_us: i64, buffered_frames: usize, wake_delay_us: Option<i64>) -> f64 {
        let ms_per_frame = 1000.0 / SAMPLE_RATE as f64;
        // Age of the packet's first frame, in the phone's clock.
        let age = (now_us() - (header.host_ts_us as i64 - clock_offset_us)) as f64 / 1000.0;
        // Arrival time relative to when the packet was complete on the PC: its jitter is
        // what the playout buffer has to absorb.
        let app_lag = age - header.frames as f64 * ms_per_frame;
        let wake = wake_delay_us.unwrap_or(0) as f64 / 1000.0;
        let link_lag = app_lag - wake;
        let buffered = buffered_frames as f64 * ms_per_frame;

        const ALPHA: f64 = 0.05;
        match &mut self.age_ms {
            Some(a) => {
                *a += ALPHA * (age - *a);
                self.buffer_ms += ALPHA * (buffered - self.buffer_ms);
            }
            None => {
                self.age_ms = Some(age);
                self.buffer_ms = buffered;
            }
        }

        let now = Instant::now();
        let widen = |(lo, hi): (f64, f64), x: f64| (lo.min(x), hi.max(x));
        match self.window_start {
            Some(start) if now - start < JITTER_WINDOW => {
                self.app_lag = widen(self.app_lag, app_lag);
                self.link_lag = widen(self.link_lag, link_lag);
                self.wake_max = self.wake_max.max(wake);
            }
            started => {
                if started.is_some() {
                    self.result.arrival_jitter_ms = self.app_lag.1 - self.app_lag.0;
                    self.result.link_jitter_ms = self.link_lag.1 - self.link_lag.0;
                    self.result.wake_max_ms = self.wake_max;
                }
                self.window_start = Some(now);
                self.app_lag = (app_lag, app_lag);
                self.link_lag = (link_lag, link_lag);
                self.wake_max = wake;
            }
        }
        app_lag
    }

    fn result(&self) -> Option<Timing> {
        Some(Timing { capture_and_link_ms: self.age_ms?, buffer_ms: self.buffer_ms, ..self.result })
    }
}

/// Asks the kernel to timestamp every received datagram (SO_TIMESTAMPNS, realtime clock).
fn enable_kernel_timestamps(socket: &UdpSocket) {
    let on: libc::c_int = 1;
    let r = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_TIMESTAMPNS,
            (&on as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if r != 0 {
        log::warn!("SO_TIMESTAMPNS unavailable: {}", io::Error::last_os_error());
    }
}

/// `recv` that also returns how long the datagram waited in the kernel before we read it (µs).
fn recv_timestamped(socket: &UdpSocket, buf: &mut [u8]) -> io::Result<(usize, Option<i64>)> {
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    // u64 elements keep the control buffer aligned for cmsghdr.
    let mut control = [0u64; 8];
    // SAFETY: an all-zero msghdr is valid; the pointers set below outlive the call.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(&control) as _;
    let n = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut received_ns = None;
    // SAFETY: walking the control messages the kernel wrote into `control`.
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_TIMESTAMPNS {
                let ts: libc::timespec = std::ptr::read_unaligned(libc::CMSG_DATA(c).cast());
                received_ns = Some(timespec_ns(&ts));
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    let wake_us = received_ns.map(|r| (realtime_ns() - r) / 1000);
    Ok((n as usize, wake_us))
}

fn realtime_ns() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) };
    timespec_ns(&ts)
}

// The conversion is a no-op on arm64 but needed on armv7, where the fields are 32-bit.
#[allow(clippy::useless_conversion)]
fn timespec_ns(ts: &libc::timespec) -> i64 {
    i64::from(ts.tv_sec) * 1_000_000_000 + i64::from(ts.tv_nsec)
}

struct RingSource(rtrb::Consumer<f32>);

impl SampleSource for RingSource {
    fn available(&self) -> usize {
        self.0.slots()
    }

    fn read(&mut self, out: &mut [f32]) -> usize {
        let n = out.len().min(self.0.slots());
        let Ok(chunk) = self.0.read_chunk(n) else { return 0 };
        let (a, b) = chunk.as_slices();
        out[..a.len()].copy_from_slice(a);
        out[a.len()..n].copy_from_slice(b);
        chunk.commit_all();
        n
    }

    fn skip(&mut self, n: usize) {
        if let Ok(chunk) = self.0.read_chunk(n.min(self.0.slots())) {
            chunk.commit_all();
        }
    }
}

struct RingSink(rtrb::Producer<f32>);

impl SampleSink for RingSink {
    fn free(&self) -> usize {
        self.0.slots()
    }

    fn write(&mut self, data: &[f32]) -> usize {
        let n = data.len().min(self.0.slots());
        match self.0.write_chunk_uninit(n) {
            Ok(chunk) => chunk.fill_from_iter(data[..n].iter().copied()),
            Err(_) => 0,
        }
    }
}

/// Gives the calling thread the scheduling priority Android uses for audio
/// (`THREAD_PRIORITY_URGENT_AUDIO` = -19), so packets are picked up as soon as they arrive.
fn raise_thread_priority() {
    const URGENT_AUDIO: libc::c_int = -19;
    let tid = unsafe { libc::gettid() };
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, URGENT_AUDIO) } != 0 {
        log::warn!("could not raise network thread priority: {}", io::Error::last_os_error());
    }
}

/// Answers discovery broadcasts so PCs on the same network (Wi-Fi) can find this phone.
fn discovery_loop(socket: UdpSocket, shared: &Shared) {
    let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
    let mut buf = [0u8; 64];
    while !shared.stop.load(Ordering::Relaxed) {
        if let Ok((n, from)) = socket.recv_from(&mut buf)
            && &buf[..n] == DISCOVERY_REQUEST
        {
            let _ = socket.send_to(DISCOVERY_REPLY, from);
        }
    }
}

/// Names the link the PC reached us over: the interface whose subnet contains the PC's
/// address. (Our own local address is no guide: with Wi-Fi and USB tethering both up, the PC
/// may reach our Wi-Fi address through the tethering link.)
fn link_name(peer: SocketAddr) -> &'static str {
    let SocketAddr::V4(peer) = peer else { return "network" };
    let peer = u32::from(*peer.ip());
    let mut found = "Wi-Fi";
    // SAFETY: getifaddrs/freeifaddrs pair; entries are only read in between.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return "network";
        }
        let mut cur = list;
        while let Some(ifa) = cur.as_ref() {
            cur = ifa.ifa_next;
            let (addr, mask) = (ifa.ifa_addr, ifa.ifa_netmask);
            if addr.is_null() || mask.is_null() || i32::from((*addr).sa_family) != libc::AF_INET {
                continue;
            }
            let ip = u32::from_be((*(addr as *const libc::sockaddr_in)).sin_addr.s_addr);
            let mask = u32::from_be((*(mask as *const libc::sockaddr_in)).sin_addr.s_addr);
            if mask == 0 || ip & mask != peer & mask {
                continue;
            }
            let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy();
            if ["rndis", "usb", "ncm"].iter().any(|p| name.starts_with(p)) {
                found = "USB tethering";
            }
        }
        libc::freeifaddrs(list);
    }
    found
}
