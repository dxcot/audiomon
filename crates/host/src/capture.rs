//! System audio capture via WASAPI loopback on the default render device.
//!
//! Loopback delivers one packet per audio-engine period (10 ms by default). To cut that, a
//! silent low-latency render stream is opened first with `IAudioClient3`, which switches the
//! engine to its minimum period (often 3 ms) while it is open.

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use wasapi::{AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, Handle, SampleType, StreamMode, TimingMode, WaveFormat};
use windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW;
use windows::core::w;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;

/// One packet as delivered by the Windows audio engine.
pub struct Packet<'a> {
    /// Interleaved f32 samples (`frames * CHANNELS` values). Empty when `silent`.
    pub samples: &'a [f32],
    pub frames: u32,
    /// Silent packet: flagged by Windows, or synthesized while nothing plays.
    pub silent: bool,
    /// Windows reported a gap before this packet (we were too slow).
    pub discontinuity: bool,
    /// When the packet was read.
    pub at: Instant,
}

pub struct Capture {
    client: AudioClient,
    capture: AudioCaptureClient,
    event: Handle,
    block_align: usize,
    period_frames: usize,
    /// Keeps the engine at a short period; must stay alive while capturing.
    _pacer: Option<AudioClient>,
}

impl Capture {
    /// Opens loopback capture. Must be used (`run`) on the same thread.
    pub fn open() -> Result<Self, Box<dyn Error>> {
        wasapi::initialize_mta().ok()?;

        let device = DeviceEnumerator::new()?.get_default_device(&Direction::Render)?;
        log::info!("capturing from: {}", device.get_friendlyname()?);

        let pacer = match start_pacer(&device) {
            Ok(p) => Some(p),
            Err(e) => {
                log::warn!("low-latency engine period unavailable, using default: {e}");
                None
            }
        };

        let mut client = device.get_iaudioclient()?;
        let format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE as usize, CHANNELS as usize, None);
        let (_, min_period) = client.get_device_period()?;
        // Capture on a render device = loopback.
        let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min_period };
        client.initialize_client(&format, &Direction::Capture, &mode)?;
        let event = client.set_get_eventhandle()?;
        let capture = client.get_audiocaptureclient()?;

        // The engine period as the loopback stream sees it.
        let period_frames = match client.get_current_shared_mode_engine_period() {
            Ok((fmt, frames)) => (frames as u64 * SAMPLE_RATE as u64 / fmt.get_samplespersec() as u64) as usize,
            Err(_) => SAMPLE_RATE as usize / 100,
        };
        log::info!(
            "engine period: {period_frames} frames ({:.2} ms)",
            period_frames as f64 * 1000.0 / SAMPLE_RATE as f64
        );

        Ok(Self {
            client,
            capture,
            event,
            block_align: format.get_blockalign() as usize,
            period_frames,
            _pacer: pacer,
        })
    }

    /// Frames per packet the engine delivers.
    pub fn period_frames(&self) -> usize {
        self.period_frames
    }

    /// Captures until `stop` is set, calling `on_packet` for every packet.
    /// `on_packet` runs on this (real-time priority) thread: it must not block.
    pub fn run(&self, stop: &AtomicBool, mut on_packet: impl FnMut(&Packet)) -> Result<(), Box<dyn Error>> {
        // Ask the scheduler for "Pro Audio" priority (MMCSS). Not fatal if it fails.
        let mut task_index = 0u32;
        if let Err(e) = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) } {
            log::warn!("MMCSS registration failed: {e}");
        }

        let period = Duration::from_secs_f64(self.period_frames as f64 / SAMPLE_RATE as f64);
        // Loopback delivers nothing while Windows plays nothing. After this long without a
        // packet, synthesize silence so the receiver's timeline keeps running.
        let idle_after = period + Duration::from_millis(2);

        let mut bytes = vec![0u8; self.client.get_buffer_size()? as usize * self.block_align];
        let mut samples: Vec<f32> = Vec::with_capacity(bytes.len() / 4);
        // Audio time covered by packets emitted so far, used to pace synthesized silence.
        let mut covered_until = Instant::now();

        self.client.start_stream()?;
        while !stop.load(Ordering::Relaxed) {
            let wait = (covered_until + idle_after).saturating_duration_since(Instant::now());
            if self.event.wait_for_event(wait.as_millis().max(1) as u32).is_err() {
                let now = Instant::now();
                if now.duration_since(covered_until) > Duration::from_millis(200) {
                    // Long idle (or first start): don't send a burst of catch-up silence.
                    covered_until = now - period;
                }
                while now.duration_since(covered_until) >= idle_after {
                    covered_until += period;
                    on_packet(&Packet {
                        samples: &[],
                        frames: self.period_frames as u32,
                        silent: true,
                        discontinuity: false,
                        at: now,
                    });
                }
                continue;
            }
            while let Some(n) = self.capture.get_next_packet_size()?
                && n > 0
            {
                let needed = n as usize * self.block_align;
                if bytes.len() < needed {
                    bytes.resize(needed, 0);
                }
                let (frames, info) = self.capture.read_from_device(&mut bytes[..needed])?;
                samples.clear();
                if !info.flags.silent {
                    samples.extend(
                        bytes[..frames as usize * self.block_align]
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|&b| f32::from_le_bytes(b)),
                    );
                }
                let at = Instant::now();
                covered_until = at;
                on_packet(&Packet {
                    samples: &samples,
                    frames,
                    silent: info.flags.silent,
                    discontinuity: info.flags.data_discontinuity,
                    at,
                });
            }
        }
        self.client.stop_stream()?;
        Ok(())
    }
}

/// Prints every active output device with its shared-mode engine periods, to see which ones
/// allow low-latency capture.
pub fn print_output_devices() -> Result<(), Box<dyn Error>> {
    wasapi::initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let default_id = enumerator.get_default_device(&Direction::Render)?.get_id()?;
    let devices = enumerator.get_device_collection(&Direction::Render)?;
    for i in 0..devices.get_nbr_devices()? {
        let device = devices.get_device_at_index(i)?;
        let marker = if device.get_id()? == default_id { "*" } else { " " };
        let periods = device.get_iaudioclient().and_then(|c| {
            let format = c.get_mixformat()?;
            let p = c.get_shared_mode_engine_period(&format)?;
            Ok((p, format.get_samplespersec()))
        });
        let ms = |frames: u32, rate: u32| frames as f64 * 1000.0 / rate as f64;
        match periods {
            Ok((p, rate)) => println!(
                "{marker} {:<50} default {:>5.2} ms, min {:>5.2} ms",
                device.get_friendlyname()?,
                ms(p.default, rate),
                ms(p.min, rate)
            ),
            Err(e) => println!("{marker} {:<50} ({e})", device.get_friendlyname()?),
        }
    }
    println!("(* = default output; audiorast captures from the default)");
    Ok(())
}

/// Opens a silent render stream at the engine's minimum period. While it runs, the shared
/// engine (and with it loopback capture) cycles at that period.
fn start_pacer(device: &wasapi::Device) -> Result<AudioClient, Box<dyn Error>> {
    let mut client = device.get_iaudioclient()?;
    let format = client.get_mixformat()?;
    let periods = client.get_shared_mode_engine_period(&format)?;
    log::info!(
        "engine periods (frames @ {} Hz): default {}, min {}, max {}",
        format.get_samplespersec(),
        periods.default,
        periods.min,
        periods.max
    );
    if periods.min >= periods.default {
        return Err("device has no shorter period than the default".into());
    }
    client.initialize_shared_audio_stream(&format, TimingMode::Polling, periods.min)?;
    // Never written to: the engine renders silence for it. Only its period matters.
    client.start_stream()?;
    Ok(client)
}
