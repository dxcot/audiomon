// Release builds are a tray app without a console window; subcommands attach to the
// terminal they were started from.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod discover;
mod glyph;
mod sender;
mod settings;
mod status;
mod tray;
mod usb;

use std::error::Error;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

const USAGE: &str = "usage:
  audiomon                    tray app: stream PC audio to the phone in the background
  audiomon stream             stream over USB tethering in this terminal
  audiomon --phone <ip>       stream to a phone at a known address
  audiomon usb                stream over direct USB (Android accessory mode)
  audiomon devices            list phones found over USB tethering or Wi-Fi
  audiomon outputs            list PC output devices and their latency capability
  audiomon record [secs] [file.wav]   capture system audio to a file";

fn main() {
    if std::env::var_os("RUST_LOG").is_none() {
        // SAFETY: single-threaded at this point.
        unsafe { std::env::set_var("RUST_LOG", "info") };
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        init_file_log();
    } else {
        attach_console();
        env_logger::init();
    }

    let never_quit = AtomicBool::new(false);
    let result = match args.first().map(String::as_str) {
        None => tray::run(),
        Some("stream") => {
            sender::run(None, &never_quit);
            Ok(())
        }
        Some("--phone") => match args.get(1).and_then(|s| s.parse::<Ipv4Addr>().ok()) {
            Some(ip) => {
                sender::run(Some(ip), &never_quit);
                Ok(())
            }
            None => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        },
        Some("outputs") => capture::print_output_devices(),
        Some("usb") => {
            usb::run(&never_quit);
            Ok(())
        }
        Some("devices") => {
            let mut found = discover::candidates();
            found.extend(discover::lan_phones(Duration::from_millis(500)));
            if found.is_empty() {
                println!("No phone found. Open the audiomon app on the phone (same Wi-Fi, or USB tethering on).");
            }
            for c in found {
                println!("{}  via {} ({})", c.ip, c.link.label(), c.adapter);
            }
            Ok(())
        }
        Some("record") => {
            let secs = args.get(1).map_or(Ok(10), |s| s.parse()).unwrap_or_else(|_| {
                eprintln!("{USAGE}");
                std::process::exit(2);
            });
            let path = args.get(2).map_or("capture.wav", String::as_str);
            record(secs, path)
        }
        _ => {
            eprintln!("audiomon {}\n{USAGE}", env!("CARGO_PKG_VERSION"));
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        log::error!("{e}");
        std::process::exit(1);
    }
}

/// Phase 1 spike: capture system audio to a WAV file and print timing/CPU stats.
fn record(secs: u64, path: &str) -> Result<(), Box<dyn Error>> {
    let spec = hound::WavSpec {
        channels: capture::CHANNELS,
        sample_rate: capture::SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::create(path, spec)?;
    let stop = AtomicBool::new(false);
    let mut stats = Stats::default();
    let cpu_start = process_cpu_time();
    let wall_start = Instant::now();

    log::info!("recording {secs} s to {path}, play something...");
    thread::scope(|s| -> Result<(), Box<dyn Error>> {
        s.spawn(|| {
            thread::sleep(Duration::from_secs(secs));
            stop.store(true, Ordering::Relaxed);
        });
        let capture = capture::Capture::open()?;
        capture.run(&stop, |p| {
            stats.add(p);
            // The WAV writer is buffered; fine for a spike, not for the real-time path.
            if p.silent {
                for _ in 0..p.frames * capture::CHANNELS as u32 {
                    let _ = wav.write_sample(0i16);
                }
            }
            for &x in p.samples {
                let _ = wav.write_sample((x.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            }
        })
    })?;
    wav.finalize()?;

    let wall = wall_start.elapsed();
    let cpu = process_cpu_time() - cpu_start;
    stats.print(wall);
    println!(
        "cpu: {:.3} s over {:.1} s = {:.2}% of one core",
        cpu.as_secs_f64(),
        wall.as_secs_f64(),
        100.0 * cpu.as_secs_f64() / wall.as_secs_f64()
    );
    Ok(())
}

#[derive(Default)]
struct Stats {
    packets: u64,
    frames: u64,
    silent: u64,
    discontinuities: u64,
    min_frames: u32,
    max_frames: u32,
    last: Option<Instant>,
    min_gap: Duration,
    max_gap: Duration,
    peak: f32,
}

impl Stats {
    fn add(&mut self, p: &capture::Packet) {
        if self.packets == 0 {
            self.min_frames = u32::MAX;
            self.min_gap = Duration::MAX;
        }
        self.packets += 1;
        self.frames += p.frames as u64;
        self.silent += p.silent as u64;
        self.discontinuities += p.discontinuity as u64;
        self.min_frames = self.min_frames.min(p.frames);
        self.max_frames = self.max_frames.max(p.frames);
        if let Some(last) = self.last {
            let gap = p.at - last;
            self.min_gap = self.min_gap.min(gap);
            self.max_gap = self.max_gap.max(gap);
        }
        self.last = Some(p.at);
        self.peak = p.samples.iter().fold(self.peak, |m, x| m.max(x.abs()));
    }

    fn print(&self, wall: Duration) {
        if self.packets == 0 {
            println!("no packets captured: was anything playing?");
            return;
        }
        let ms = |f: u64| f as f64 * 1000.0 / capture::SAMPLE_RATE as f64;
        println!(
            "packets: {} ({} silent, {} discontinuities)",
            self.packets, self.silent, self.discontinuities
        );
        println!(
            "frames/packet: min {} max {} avg {:.1} (= {:.2} ms)",
            self.min_frames,
            self.max_frames,
            self.frames as f64 / self.packets as f64,
            ms(self.frames) / self.packets as f64
        );
        println!(
            "gap between packets: min {:.2} ms, max {:.2} ms",
            self.min_gap.as_secs_f64() * 1000.0,
            self.max_gap.as_secs_f64() * 1000.0
        );
        println!(
            "audio captured: {:.2} s of {:.2} s wall, peak level {:.3}",
            ms(self.frames) / 1000.0,
            wall.as_secs_f64(),
            self.peak
        );
    }
}

fn process_cpu_time() -> Duration {
    let (mut c, mut e, mut k, mut u) = Default::default();
    unsafe {
        if GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u).is_err() {
            return Duration::ZERO;
        }
    }
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    Duration::from_nanos((ticks(k) + ticks(u)) * 100)
}

/// Lets subcommands print to the terminal they were started from, since release builds have
/// no console of their own. Output that is already redirected (pipes, files) is left alone.
fn attach_console() {
    let redirected = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.is_ok_and(|h| !h.is_invalid() && !h.0.is_null());
    if !redirected {
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
}

/// The tray app has no terminal: log to `%LOCALAPPDATA%\audiomon\audiomon.log` (fresh each run).
fn init_file_log() {
    let mut builder = env_logger::Builder::from_default_env();
    if let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| std::path::Path::new(&d).join("audiomon"))
        && std::fs::create_dir_all(&dir).is_ok()
        && let Ok(file) = std::fs::File::create(dir.join("audiomon.log"))
    {
        builder.target(env_logger::Target::Pipe(Box::new(file)));
    }
    builder.init();
}
