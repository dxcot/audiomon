//! End-to-end simulation of the receiver pipeline against a sender whose clock runs at a
//! different rate, with network jitter and loss. Time is simulated, so minutes run in seconds.

use std::collections::VecDeque;

use audiomon_core::SampleSource;
use audiomon_core::depacketizer::Depacketizer;
use audiomon_core::jitter::DelayHistogram;
use audiomon_core::packet::{Header, encode};
use audiomon_core::playout::{Playout, PlayoutConfig};

const RATE: f64 = 48_000.0;
const PACKET: usize = 480;
const TONE_HZ: f64 = 1000.0;
const AMP: f64 = 0.5;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Default)]
struct Scenario {
    skew_ppm: f64,
    /// Uniform random extra delay per packet, 0..jitter_ms.
    jitter_ms: f64,
    /// Probability that a packet is additionally delayed by up to `spike_ms` (USB-like spikes).
    spike_prob: f64,
    spike_ms: f64,
    /// Size the buffer from a [`DelayHistogram`] of arrival lateness instead of the built-in estimate.
    histogram: bool,
    loss: f64,
    minutes: f64,
    /// Device callback size in frames.
    burst: usize,
}

#[derive(Debug)]
struct Report {
    underruns_after_warmup: u64,
    skips: u64,
    lost: u64,
    /// Mean buffered audio over the first and last minute after warm-up.
    latency_first_ms: f64,
    latency_last_ms: f64,
    /// Largest sample-to-sample step after warm-up; a clean tone stays below ~0.066.
    max_step: f32,
    ratio: f64,
    drift_ppm: f64,
    margin_ms: f64,
}

const WARMUP_S: f64 = 10.0;

fn run(s: Scenario) -> Report {
    let host_rate = RATE * (1.0 + s.skew_ppm * 1e-6);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut depack = Depacketizer::new(2);
    let mut playout = Playout::new(PlayoutConfig::default());
    let mut ring: VecDeque<f32> = VecDeque::new();
    let mut out = vec![0.0f32; s.burst * 2];
    let mut datagram = Vec::new();
    let mut samples = Vec::with_capacity(PACKET * 2);

    let mut seq = 0u32;
    let mut last_arrival = 0.0f64;
    let schedule = |seq: u32, rng: &mut Rng, last: &mut f64| {
        let sent = (seq as f64 + 1.0) * PACKET as f64 / host_rate;
        // Base USB latency plus jitter; the link never reorders.
        let mut delay = 0.001 + rng.next() * s.jitter_ms / 1000.0;
        if rng.next() < s.spike_prob {
            delay += rng.next() * s.spike_ms / 1000.0;
        }
        let arrival = (sent + delay).max(*last);
        *last = arrival;
        arrival
    };
    let mut next_arrival = schedule(seq, &mut rng, &mut last_arrival);
    let mut hist = DelayHistogram::new(3000);

    let callbacks = (s.minutes * 60.0 * RATE / s.burst as f64) as u64;
    let warmup = (WARMUP_S * RATE / s.burst as f64) as u64;
    let per_minute = (60.0 * RATE / s.burst as f64) as u64;
    let mut underruns_at_warmup = 0;
    let (mut first_sum, mut last_sum) = (0.0, 0.0);
    let mut max_step = 0.0f32;
    let mut prev_sample: Option<f32> = None;

    for cb in 0..callbacks {
        let now = (cb + 1) as f64 * s.burst as f64 / RATE;
        while next_arrival <= now {
            let sent = (seq as f64 + 1.0) * PACKET as f64 / host_rate;
            if rng.next() >= s.loss {
                hist.add_packet(seq, (next_arrival - sent) * 1000.0, PACKET as f64 * 1000.0 / RATE);
                samples.clear();
                let base = seq as u64 * PACKET as u64;
                for f in 0..PACKET as u64 {
                    let x = (AMP * (std::f64::consts::TAU * TONE_HZ * (base + f) as f64 / RATE).sin()) as f32;
                    samples.extend([x, x]);
                }
                encode(&Header { seq, frames: PACKET as u32, host_ts_us: 0 }, &samples, &mut datagram);
                depack.push(&datagram, &mut ring);
            }
            seq += 1;
            next_arrival = schedule(seq, &mut rng, &mut last_arrival);
        }

        if s.histogram && hist.count() > 50 {
            playout.set_jitter_target_ms(hist.quantile(0.9995));
        }
        let buffered_ms = ring.available() as f64 / 2.0 / RATE * 1000.0;
        playout.pull(&mut ring, &mut out);

        if cb == warmup {
            underruns_at_warmup = playout.stats().underruns;
        }
        if cb >= warmup {
            if cb < warmup + per_minute {
                first_sum += buffered_ms;
            }
            if cb >= callbacks - per_minute {
                last_sum += buffered_ms;
            }
            for frame in out.as_chunks::<2>().0 {
                if let Some(p) = prev_sample {
                    max_step = max_step.max((frame[0] - p).abs());
                }
                prev_sample = Some(frame[0]);
            }
        }
    }

    let st = playout.stats();
    Report {
        underruns_after_warmup: st.underruns - underruns_at_warmup,
        skips: st.skips,
        lost: depack.stats().lost,
        latency_first_ms: first_sum / per_minute as f64,
        latency_last_ms: last_sum / per_minute as f64,
        max_step,
        ratio: st.ratio,
        drift_ppm: st.drift_ppm,
        margin_ms: st.margin_ms,
    }
}

const CLEAN_STEP: f32 = 0.07;

#[test]
fn ideal_link() {
    let r = run(Scenario { skew_ppm: 0.0, jitter_ms: 0.0, loss: 0.0, minutes: 3.0, burst: 192, ..Default::default() });
    println!("{r:#?}");
    assert_eq!(r.underruns_after_warmup, 0);
    assert_eq!(r.skips, 0);
    assert!(r.latency_last_ms < 12.0);
    assert!(r.max_step < CLEAN_STEP);
}

fn assert_drift_handled(skew_ppm: f64) {
    let r = run(Scenario { skew_ppm, jitter_ms: 2.0, loss: 0.0, minutes: 10.0, burst: 192, ..Default::default() });
    println!("{r:#?}");
    assert_eq!(r.underruns_after_warmup, 0);
    assert_eq!(r.skips, 0);
    assert!(r.latency_last_ms < 20.0, "latency too high");
    assert!((r.latency_last_ms - r.latency_first_ms).abs() < 3.0, "latency drifts");
    assert!((r.drift_ppm - skew_ppm).abs() < 30.0, "drift estimate did not lock to the clock skew");
    assert!(((r.ratio - 1.0) * 1e6 - skew_ppm).abs() < 500.0, "resampling ratio wobbles too much");
    assert!(r.max_step < CLEAN_STEP, "clicks in output");
}

#[test]
fn sender_clock_fast() {
    assert_drift_handled(200.0);
}

#[test]
fn sender_clock_slow() {
    assert_drift_handled(-200.0);
}

#[test]
fn small_device_bursts() {
    let r = run(Scenario { skew_ppm: 80.0, jitter_ms: 1.0, loss: 0.0, minutes: 5.0, burst: 48, ..Default::default() });
    println!("{r:#?}");
    assert_eq!(r.underruns_after_warmup, 0);
    assert!(r.latency_last_ms < 15.0);
    assert!(r.max_step < CLEAN_STEP);
}

#[test]
fn lossy_jittery_link() {
    let r = run(Scenario { skew_ppm: -150.0, jitter_ms: 5.0, loss: 0.01, minutes: 10.0, burst: 192, histogram: true, ..Default::default() });
    println!("{r:#?}");
    assert!(r.lost > 500, "loss model not exercised");
    assert!(r.underruns_after_warmup <= 2, "concealment should keep playback running");
    assert_eq!(r.skips, 0);
    assert!(r.latency_last_ms < 30.0);
    assert!((r.latency_last_ms - r.latency_first_ms).abs() < 5.0);
    assert!(r.margin_ms < 20.0);
}

/// The plan's acceptance test: one hour without a single dropout on a realistic USB link.
#[test]
fn one_hour_no_dropouts() {
    let r = run(Scenario { skew_ppm: 120.0, jitter_ms: 1.5, loss: 0.0, minutes: 60.0, burst: 192, ..Default::default() });
    println!("{r:#?}");
    assert_eq!(r.underruns_after_warmup, 0);
    assert_eq!(r.skips, 0);
    assert!((r.latency_last_ms - r.latency_first_ms).abs() < 3.0, "latency drifts");
    assert!(r.latency_last_ms < 15.0);
    assert!(r.max_step < CLEAN_STEP);
}

/// A link like the one measured over USB tethering: ~1 ms of jitter plus rare multi-ms spikes.
fn spiky(histogram: bool) -> Report {
    run(Scenario {
        skew_ppm: 150.0,
        jitter_ms: 1.0,
        spike_prob: 0.01,
        spike_ms: 6.0,
        minutes: 10.0,
        burst: 96,
        histogram,
        ..Default::default()
    })
}

#[test]
fn histogram_sizing_beats_builtin_on_spiky_link() {
    let builtin = spiky(false);
    let hist = spiky(true);
    println!("builtin: {builtin:#?}\nhistogram: {hist:#?}");
    assert!(hist.underruns_after_warmup <= 1, "histogram sizing glitches too often");
    assert_eq!(hist.skips, 0);
    assert!(hist.max_step < CLEAN_STEP || hist.underruns_after_warmup > 0);
    assert!(
        hist.latency_last_ms + 2.0 < builtin.latency_last_ms,
        "expected at least 2 ms less buffering: {:.1} vs {:.1}",
        hist.latency_last_ms,
        builtin.latency_last_ms
    );
}

#[test]
fn histogram_sizing_one_hour() {
    let r = run(Scenario {
        skew_ppm: -120.0,
        jitter_ms: 1.0,
        spike_prob: 0.005,
        spike_ms: 5.0,
        minutes: 60.0,
        burst: 96,
        histogram: true,
        ..Default::default()
    });
    println!("{r:#?}");
    assert!(r.underruns_after_warmup <= 2);
    assert!((r.latency_last_ms - r.latency_first_ms).abs() < 3.0, "latency drifts");
}
