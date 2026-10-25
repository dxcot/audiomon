//! Minimal AAudio bindings (NDK `libaaudio.so`, API 26+) and a safe output-stream wrapper.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

#[allow(non_camel_case_types)]
type aaudio_result_t = i32;

#[repr(C)]
struct AAudioStreamBuilder {
    _p: [u8; 0],
}
#[repr(C)]
struct AAudioStream {
    _p: [u8; 0],
}

type DataCallback = extern "C" fn(*mut AAudioStream, *mut c_void, *mut c_void, i32) -> i32;
type ErrorCallback = extern "C" fn(*mut AAudioStream, *mut c_void, aaudio_result_t);

const AAUDIO_OK: i32 = 0;
const AAUDIO_FORMAT_PCM_FLOAT: i32 = 2;
const AAUDIO_SHARING_MODE_EXCLUSIVE: i32 = 0;
const AAUDIO_SHARING_MODE_SHARED: i32 = 1;
const AAUDIO_PERFORMANCE_MODE_LOW_LATENCY: i32 = 12;
const AAUDIO_USAGE_MEDIA: i32 = 1;
const AAUDIO_CONTENT_TYPE_MUSIC: i32 = 2;
const AAUDIO_CALLBACK_RESULT_CONTINUE: i32 = 0;
const CLOCK_MONOTONIC: i32 = 1;

#[link(name = "aaudio")]
unsafe extern "C" {
    fn AAudio_createStreamBuilder(builder: *mut *mut AAudioStreamBuilder) -> aaudio_result_t;
    fn AAudio_convertResultToText(result: aaudio_result_t) -> *const std::ffi::c_char;
    fn AAudioStreamBuilder_setPerformanceMode(b: *mut AAudioStreamBuilder, mode: i32);
    fn AAudioStreamBuilder_setSharingMode(b: *mut AAudioStreamBuilder, mode: i32);
    fn AAudioStreamBuilder_setFormat(b: *mut AAudioStreamBuilder, format: i32);
    fn AAudioStreamBuilder_setChannelCount(b: *mut AAudioStreamBuilder, channels: i32);
    fn AAudioStreamBuilder_setSampleRate(b: *mut AAudioStreamBuilder, rate: i32);
    fn AAudioStreamBuilder_setUsage(b: *mut AAudioStreamBuilder, usage: i32);
    fn AAudioStreamBuilder_setContentType(b: *mut AAudioStreamBuilder, content: i32);
    fn AAudioStreamBuilder_setDataCallback(b: *mut AAudioStreamBuilder, cb: DataCallback, user: *mut c_void);
    fn AAudioStreamBuilder_setErrorCallback(b: *mut AAudioStreamBuilder, cb: ErrorCallback, user: *mut c_void);
    fn AAudioStreamBuilder_openStream(b: *mut AAudioStreamBuilder, stream: *mut *mut AAudioStream) -> aaudio_result_t;
    fn AAudioStreamBuilder_delete(b: *mut AAudioStreamBuilder) -> aaudio_result_t;
    fn AAudioStream_requestStart(s: *mut AAudioStream) -> aaudio_result_t;
    fn AAudioStream_requestStop(s: *mut AAudioStream) -> aaudio_result_t;
    fn AAudioStream_close(s: *mut AAudioStream) -> aaudio_result_t;
    fn AAudioStream_getFramesPerBurst(s: *mut AAudioStream) -> i32;
    fn AAudioStream_getBufferSizeInFrames(s: *mut AAudioStream) -> i32;
    fn AAudioStream_setBufferSizeInFrames(s: *mut AAudioStream, frames: i32) -> aaudio_result_t;
    fn AAudioStream_getBufferCapacityInFrames(s: *mut AAudioStream) -> i32;
    fn AAudioStream_getSharingMode(s: *mut AAudioStream) -> i32;
    fn AAudioStream_getPerformanceMode(s: *mut AAudioStream) -> i32;
    fn AAudioStream_getSampleRate(s: *mut AAudioStream) -> i32;
    fn AAudioStream_getChannelCount(s: *mut AAudioStream) -> i32;
    fn AAudioStream_getXRunCount(s: *mut AAudioStream) -> i32;
    fn AAudioStream_getFramesWritten(s: *mut AAudioStream) -> i64;
    fn AAudioStream_getTimestamp(s: *mut AAudioStream, clock: i32, frame_pos: *mut i64, nanos: *mut i64) -> aaudio_result_t;
}

#[derive(Debug)]
pub struct Error(pub i32);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = unsafe { std::ffi::CStr::from_ptr(AAudio_convertResultToText(self.0)) };
        write!(f, "AAudio error {} ({})", self.0, text.to_string_lossy())
    }
}

impl std::error::Error for Error {}

fn check(r: aaudio_result_t) -> Result<(), Error> {
    if r == AAUDIO_OK { Ok(()) } else { Err(Error(r)) }
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub sample_rate: i32,
    pub channels: i32,
    /// Request an exclusive (MMAP) stream; Android may still grant shared.
    pub exclusive: bool,
    /// Device buffer size in bursts; fewer = lower latency, more risk of glitches.
    pub buffer_bursts: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct Info {
    pub sample_rate: i32,
    pub channels: i32,
    pub exclusive: bool,
    pub low_latency: bool,
    pub burst_frames: i32,
    pub buffer_frames: i32,
    pub capacity_frames: i32,
}

/// Fills interleaved f32 output. Runs on the real-time audio thread.
pub type Render = dyn FnMut(&mut [f32]) + Send;

struct CallbackState {
    render: Box<Render>,
    channels: usize,
}

pub struct OutputStream {
    raw: *mut AAudioStream,
    _state: Box<CallbackState>,
    last_error: Arc<AtomicI32>,
    info: Info,
}

// The stream handle is only used through AAudio's thread-safe control calls.
unsafe impl Send for OutputStream {}

extern "C" fn data_callback(_s: *mut AAudioStream, user: *mut c_void, data: *mut c_void, frames: i32) -> i32 {
    // SAFETY: `user` is the CallbackState boxed in OutputStream, alive until the stream is closed,
    // and AAudio calls this from a single thread.
    let state = unsafe { &mut *(user as *mut CallbackState) };
    let out = unsafe { std::slice::from_raw_parts_mut(data as *mut f32, frames as usize * state.channels) };
    (state.render)(out);
    AAUDIO_CALLBACK_RESULT_CONTINUE
}

extern "C" fn error_callback(_s: *mut AAudioStream, user: *mut c_void, error: aaudio_result_t) {
    // SAFETY: `user` points at the AtomicI32 owned by OutputStream's Arc.
    unsafe { &*(user as *const AtomicI32) }.store(error, Ordering::Relaxed);
}

impl OutputStream {
    pub fn open(cfg: Config, render: Box<Render>) -> Result<Self, Error> {
        let mut state = Box::new(CallbackState { render, channels: cfg.channels as usize });
        let last_error = Arc::new(AtomicI32::new(AAUDIO_OK));
        let mut raw = std::ptr::null_mut();
        unsafe {
            let mut b = std::ptr::null_mut();
            check(AAudio_createStreamBuilder(&mut b))?;
            AAudioStreamBuilder_setPerformanceMode(b, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);
            AAudioStreamBuilder_setSharingMode(
                b,
                if cfg.exclusive { AAUDIO_SHARING_MODE_EXCLUSIVE } else { AAUDIO_SHARING_MODE_SHARED },
            );
            AAudioStreamBuilder_setFormat(b, AAUDIO_FORMAT_PCM_FLOAT);
            AAudioStreamBuilder_setChannelCount(b, cfg.channels);
            AAudioStreamBuilder_setSampleRate(b, cfg.sample_rate);
            AAudioStreamBuilder_setUsage(b, AAUDIO_USAGE_MEDIA);
            AAudioStreamBuilder_setContentType(b, AAUDIO_CONTENT_TYPE_MUSIC);
            AAudioStreamBuilder_setDataCallback(b, data_callback, &mut *state as *mut CallbackState as *mut c_void);
            AAudioStreamBuilder_setErrorCallback(b, error_callback, Arc::as_ptr(&last_error) as *mut c_void);
            let r = AAudioStreamBuilder_openStream(b, &mut raw);
            AAudioStreamBuilder_delete(b);
            check(r)?;
        }

        let burst = unsafe { AAudioStream_getFramesPerBurst(raw) };
        unsafe { AAudioStream_setBufferSizeInFrames(raw, burst * cfg.buffer_bursts.max(1)) };
        let info = unsafe {
            Info {
                sample_rate: AAudioStream_getSampleRate(raw),
                channels: AAudioStream_getChannelCount(raw),
                exclusive: AAudioStream_getSharingMode(raw) == AAUDIO_SHARING_MODE_EXCLUSIVE,
                low_latency: AAudioStream_getPerformanceMode(raw) == AAUDIO_PERFORMANCE_MODE_LOW_LATENCY,
                burst_frames: burst,
                buffer_frames: AAudioStream_getBufferSizeInFrames(raw),
                capacity_frames: AAudioStream_getBufferCapacityInFrames(raw),
            }
        };
        Ok(Self { raw, _state: state, last_error, info })
    }

    pub fn info(&self) -> Info {
        self.info
    }

    pub fn start(&self) -> Result<(), Error> {
        check(unsafe { AAudioStream_requestStart(self.raw) })
    }

    pub fn stop(&self) -> Result<(), Error> {
        check(unsafe { AAudioStream_requestStop(self.raw) })
    }

    /// Sets the device buffer size; returns the size actually applied.
    pub fn set_buffer_frames(&mut self, frames: i32) -> i32 {
        let applied = unsafe { AAudioStream_setBufferSizeInFrames(self.raw, frames) };
        if applied > 0 {
            self.info.buffer_frames = applied;
        }
        self.info.buffer_frames
    }

    pub fn xruns(&self) -> i32 {
        unsafe { AAudioStream_getXRunCount(self.raw) }
    }

    /// Error reported asynchronously (e.g. device disconnected); the stream must then be reopened.
    pub fn error(&self) -> Option<Error> {
        let e = self.last_error.load(Ordering::Relaxed);
        (e != AAUDIO_OK).then_some(Error(e))
    }

    /// Time from writing a frame now until it leaves the speaker, in ms.
    pub fn output_latency_ms(&self) -> Option<f64> {
        let (mut pos, mut nanos) = (0i64, 0i64);
        unsafe { check(AAudioStream_getTimestamp(self.raw, CLOCK_MONOTONIC, &mut pos, &mut nanos)).ok()? };
        let written = unsafe { AAudioStream_getFramesWritten(self.raw) };
        let presented_at_ns = nanos as f64 + (written - pos) as f64 * 1e9 / self.info.sample_rate as f64;
        Some((presented_at_ns - monotonic_ns() as f64) / 1e6)
    }
}

impl Drop for OutputStream {
    fn drop(&mut self) {
        unsafe {
            AAudioStream_requestStop(self.raw);
            AAudioStream_close(self.raw);
        }
    }
}

// The conversion is a no-op on arm64 but needed on armv7, where the fields are 32-bit.
#[allow(clippy::useless_conversion)]
pub(crate) fn monotonic_ns() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    i64::from(ts.tv_sec) * 1_000_000_000 + i64::from(ts.tv_nsec)
}
