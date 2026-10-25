//! Adaptive playout: keeps the receive buffer as small as the observed jitter allows and
//! corrects sender/receiver clock drift by resampling slightly (cubic Hermite interpolation).
//!
//! The buffer level is measured as the *minimum* fill seen during each control window (the
//! low-water mark, just before the next packet arrives). The controller steers that minimum
//! towards a safety margin, and the margin follows the observed jitter.
//!
//! [`Playout::pull`] is real-time safe: no allocation, locking or logging.

use crate::SampleSource;

#[derive(Debug, Clone, Copy)]
pub struct PlayoutConfig {
    pub sample_rate: u32,
    pub channels: usize,
    /// Frames per network packet (the sender's capture period).
    pub packet_frames: usize,
    /// Largest number of frames requested by one `pull`; longer requests are split.
    pub max_pull_frames: usize,
    /// Lower bound of the safety margin kept in the buffer.
    pub min_margin_ms: f64,
    /// Upper bound of the safety margin.
    pub max_margin_ms: f64,
    /// Jitter assumed before anything is measured.
    pub initial_jitter_ms: f64,
}

impl Default for PlayoutConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            packet_frames: 480,
            max_pull_frames: 1024,
            min_margin_ms: 2.0,
            max_margin_ms: 60.0,
            initial_jitter_ms: 2.0,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PlayoutStats {
    pub underruns: u64,
    /// Times the buffer was too full and audio was discarded to catch up.
    pub skips: u64,
    /// Input frames consumed per output frame (1.0 = clocks match).
    pub ratio: f64,
    /// Learned clock difference (sender vs. receiver) in parts per million.
    pub drift_ppm: f64,
    pub margin_ms: f64,
    pub jitter_ms: f64,
    pub playing: bool,
}

/// Seconds between controller updates.
const WINDOW_S: f64 = 0.25;
/// Controller time constant.
const TAU_S: f64 = 8.0;
/// Resampling ratio limit (±0.5%).
const MAX_RATIO_DEV: f64 = 0.005;
/// Jitter estimate half-life.
const JITTER_HALF_LIFE_S: f64 = 30.0;
/// Jitter added to the estimate after an underrun.
const UNDERRUN_BUMP_MS: f64 = 2.0;
/// Half-life of the extra margin added after underruns: long, so one spike that caused an
/// audible glitch keeps the buffer safer for minutes instead of seconds.
const PENALTY_HALF_LIFE_S: f64 = 300.0;
/// Underruns in the first seconds of playback (connection settling) add no penalty.
const SETTLE_S: f64 = 5.0;
/// Fade-in after (re)starting playback.
const FADE_IN_MS: f64 = 2.0;
/// History frames kept for the 4-point interpolator.
const HIST: usize = 4;

pub struct Playout {
    cfg: PlayoutConfig,
    playing: bool,
    /// Interleaved input frames: HIST history frames followed by the ones read this pull.
    scratch: Vec<f32>,
    /// Position of the next output frame between scratch frames 1 and 2.
    frac: f64,
    ratio: f64,
    integral: f64,
    kp: f64,
    ki: f64,
    window_frames: usize,
    window_pos: usize,
    window_min: usize,
    /// Smoothed window minimum, the reference for measuring jitter dips.
    avg_min: Option<f64>,
    jitter: f64,
    jitter_decay: f64,
    /// Jitter target (frames) supplied by an arrival-delay estimator; replaces `jitter` when set.
    external_jitter: Option<f64>,
    /// Extra margin (frames) added after underruns while `external_jitter` is in use; decays.
    penalty: f64,
    penalty_decay: f64,
    /// Output frames played since this playout was created.
    played_frames: u64,
    margin: f64,
    gain: f32,
    gain_step: f32,
    stats: PlayoutStats,
}

impl Playout {
    pub fn new(cfg: PlayoutConfig) -> Self {
        let rate = cfg.sample_rate as f64;
        let ms = rate / 1000.0;
        let kp = 1.0 / (TAU_S * rate);
        let window_frames = (WINDOW_S * rate) as usize;
        let max_in = (cfg.max_pull_frames as f64 * (1.0 + MAX_RATIO_DEV)).ceil() as usize + 2;
        let mut p = Self {
            cfg,
            playing: false,
            scratch: vec![0.0; (HIST + max_in) * cfg.channels],
            frac: 0.0,
            ratio: 1.0,
            integral: 0.0,
            kp,
            // Damping ratio ~0.7 for s² + kp·s + ki: settles quickly with little overshoot.
            ki: kp * kp / 2.0,
            window_frames,
            window_pos: 0,
            window_min: usize::MAX,
            avg_min: None,
            jitter: cfg.initial_jitter_ms * ms,
            jitter_decay: 0.5f64.powf(WINDOW_S / JITTER_HALF_LIFE_S),
            external_jitter: None,
            penalty: 0.0,
            penalty_decay: 0.5f64.powf(WINDOW_S / PENALTY_HALF_LIFE_S),
            played_frames: 0,
            margin: 0.0,
            gain: 0.0,
            gain_step: (1.0 / (FADE_IN_MS * ms)) as f32,
            stats: PlayoutStats { ratio: 1.0, ..Default::default() },
        };
        p.update_margin();
        p
    }

    /// Uses `ms` (e.g. a high quantile of measured arrival lateness, see
    /// [`crate::jitter::DelayHistogram`]) as the jitter to cover, instead of the built-in estimate
    /// derived from buffer levels. Cheap: may be called on every pull.
    pub fn set_jitter_target_ms(&mut self, ms: f64) {
        self.external_jitter = Some(ms * self.cfg.sample_rate as f64 / 1000.0);
    }

    pub fn stats(&self) -> PlayoutStats {
        PlayoutStats { playing: self.playing, ..self.stats }
    }

    /// Fills `out` (interleaved) with audio from `src`, or silence while buffering.
    pub fn pull(&mut self, src: &mut impl SampleSource, out: &mut [f32]) {
        for chunk in out.chunks_mut(self.cfg.max_pull_frames * self.cfg.channels) {
            self.pull_chunk(src, chunk);
        }
    }

    fn pull_chunk(&mut self, src: &mut impl SampleSource, out: &mut [f32]) {
        let ch = self.cfg.channels;
        let n = out.len() / ch;
        let mut avail = src.available() / ch;
        let prime = self.margin as usize + self.cfg.packet_frames;

        if !self.playing {
            if avail < prime {
                out.fill(0.0);
                return;
            }
            self.playing = true;
            self.scratch[..HIST * ch].fill(0.0);
            self.frac = 0.0;
            self.gain = 0.0;
            self.window_pos = 0;
            self.window_min = usize::MAX;
        }

        // Far too much buffered (e.g. after a stall on the sender): jump back to the prime level.
        let limit = self.cfg.max_margin_ms * self.cfg.sample_rate as f64 / 1000.0
            + 2.0 * self.cfg.packet_frames as f64;
        if avail as f64 > limit.max((prime + self.cfg.packet_frames) as f64) {
            src.skip((avail - prime) * ch);
            avail = prime;
            self.stats.skips += 1;
            self.window_min = usize::MAX;
            self.avg_min = None;
        }

        let end = self.frac + n as f64 * self.ratio;
        let need = end as usize;
        if avail < need {
            out.fill(0.0);
            self.playing = false;
            self.stats.underruns += 1;
            // Dropouts while the stream is still settling say nothing about the link; a
            // long-lived penalty for them would inflate the delay for minutes.
            let settling = self.played_frames < (SETTLE_S * self.cfg.sample_rate as f64) as u64;
            if !settling {
                let bump = UNDERRUN_BUMP_MS * self.cfg.sample_rate as f64 / 1000.0;
                if self.external_jitter.is_some() {
                    self.penalty += bump;
                } else {
                    self.jitter += bump;
                }
            }
            self.update_margin();
            return;
        }
        // What is left after this read is the real safety margin against the next underrun.
        self.window_min = self.window_min.min(avail - need);

        self.played_frames += n as u64;
        let read = src.read(&mut self.scratch[HIST * ch..(HIST + need) * ch]);
        debug_assert_eq!(read, need * ch);
        let s = &self.scratch;
        for (i, frame) in out.chunks_exact_mut(ch).enumerate() {
            let p = self.frac + i as f64 * self.ratio;
            let b = p as usize;
            let t = (p - b as f64) as f32;
            for (c, o) in frame.iter_mut().enumerate() {
                let x = |k: usize| s[(b + k) * ch + c];
                *o = hermite(x(0), x(1), x(2), x(3), t) * self.gain;
            }
            self.gain = (self.gain + self.gain_step).min(1.0);
        }
        self.scratch.copy_within(need * ch..(need + HIST) * ch, 0);
        self.frac = end - need as f64;

        self.window_pos += n;
        if self.window_pos >= self.window_frames {
            self.control();
        }
    }

    /// Runs once per window: updates the jitter estimate, margin and resampling ratio.
    fn control(&mut self) {
        let min = self.window_min as f64;
        let w = self.window_pos as f64;
        self.window_pos = 0;
        self.window_min = usize::MAX;

        let avg = self.avg_min.get_or_insert(min);
        let dip = (*avg - min).max(0.0);
        *avg += 0.1 * (min - *avg);
        self.jitter = (self.jitter * self.jitter_decay).max(dip);
        self.penalty *= self.penalty_decay;
        self.update_margin();

        let error = min - self.margin;
        self.integral = (self.integral + self.ki * error * w).clamp(-MAX_RATIO_DEV, MAX_RATIO_DEV);
        self.ratio = (1.0 + self.kp * error + self.integral).clamp(1.0 - MAX_RATIO_DEV, 1.0 + MAX_RATIO_DEV);
        self.stats.ratio = self.ratio;
        self.stats.drift_ppm = self.integral * 1e6;
    }

    fn update_margin(&mut self) {
        let ms = self.cfg.sample_rate as f64 / 1000.0;
        let cover = match self.external_jitter {
            Some(j) => j + self.penalty,
            None => 2.0 * self.jitter,
        };
        self.margin = (self.cfg.min_margin_ms * ms + cover)
            .clamp(self.cfg.min_margin_ms * ms, self.cfg.max_margin_ms * ms);
        self.stats.margin_ms = self.margin / ms;
        self.stats.jitter_ms = self.external_jitter.unwrap_or(self.jitter) / ms;
    }
}

/// 4-point cubic Hermite interpolation between `x0` and `x1` at `t ∈ [0, 1)`.
#[inline]
fn hermite(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + x0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn hermite_hits_samples() {
        assert_eq!(hermite(1.0, 2.0, 3.0, 4.0, 0.0), 2.0);
        assert!((hermite(1.0, 2.0, 3.0, 4.0, 0.5) - 2.5).abs() < 1e-6);
    }

    #[test]
    fn buffers_until_primed_then_plays() {
        let mut p = Playout::new(PlayoutConfig::default());
        let mut ring: VecDeque<f32> = VecDeque::new();
        let mut out = vec![1.0; 2 * 192];
        ring.extend(std::iter::repeat_n(0.5, 2 * 100));
        p.pull(&mut ring, &mut out);
        assert!(out.iter().all(|&x| x == 0.0));
        assert_eq!(ring.len(), 200, "nothing consumed while buffering");

        ring.extend(std::iter::repeat_n(0.5, 2 * 1000));
        p.pull(&mut ring, &mut out);
        // Fades in, then reaches the input level.
        assert_eq!(out[0], 0.0);
        assert!((out[out.len() - 1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn underrun_returns_to_buffering() {
        let mut p = Playout::new(PlayoutConfig::default());
        let mut ring: VecDeque<f32> = VecDeque::new();
        ring.extend(std::iter::repeat_n(0.5, 2 * 900));
        let mut out = vec![0.0; 2 * 192];
        for _ in 0..5 {
            p.pull(&mut ring, &mut out);
        }
        assert_eq!(p.stats().underruns, 1);
        assert!(!p.playing);
    }
}
