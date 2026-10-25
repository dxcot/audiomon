//! JNI entry points for `dev.audiorast.Native` (Kotlin `object` with `@JvmStatic external` funs).

use std::ffi::CString;
use std::fmt::Write;
use std::sync::Mutex;

use jni_sys::{JNIEnv, jclass, jint, jstring};

use crate::engine::Engine;

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_audiorast_Native_start(_env: *mut JNIEnv, _class: jclass) {
    crate::logger::init();
    let mut engine = ENGINE.lock().unwrap();
    if engine.is_none() {
        match Engine::start() {
            Ok(e) => *engine = Some(e),
            Err(e) => log::error!("engine start failed: {e}"),
        }
    }
}

/// Plays from a USB accessory; takes ownership of `fd` (a detached ParcelFileDescriptor).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_audiorast_Native_startAccessory(env: *mut JNIEnv, class: jclass, fd: jint) {
    Java_dev_audiorast_Native_start(env, class);
    match ENGINE.lock().unwrap().as_ref() {
        Some(engine) => {
            if let Err(e) = engine.attach_accessory(fd) {
                log::error!("accessory start failed: {e}");
            }
        }
        None => log::error!("no engine for accessory"),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_audiorast_Native_stop(_env: *mut JNIEnv, _class: jclass) {
    ENGINE.lock().unwrap().take();
}

/// Current status as `key=value` lines, for the app to lay out (see `Snapshot.kt`).
/// Keys whose value is unknown are left out.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_audiorast_Native_snapshot(env: *mut JNIEnv, _class: jclass) -> jstring {
    let text = match ENGINE.lock().unwrap().as_ref() {
        Some(e) => snapshot(e),
        None => "state=stopped\n".to_string(),
    };
    let text = CString::new(text).unwrap_or_default();
    // SAFETY: `env` is the valid JNIEnv for this thread, provided by the JVM.
    unsafe { ((**env).v1_1.NewStringUTF)(env, text.as_ptr()) }
}

fn snapshot(engine: &Engine) -> String {
    let st = engine.status();
    let mut s = String::new();
    let state = match st.state.as_str() {
        "Playing" => "playing",
        "Lost" => "problem",
        _ => "waiting",
    };
    let _ = writeln!(s, "state={state}");
    if st.transport.is_empty() {
        return s;
    }
    let link = match st.transport {
        "USB tethering" => "usb_tethering",
        "Wi-Fi" => "wifi",
        "USB direct (accessory)" => "usb_direct",
        _ => "network",
    };
    let _ = writeln!(s, "link={link}");
    if let Some(host) = st.host {
        let _ = writeln!(s, "pc={}", host.ip());
    }
    let out = st.output_latency_ms;
    if let Some(o) = out {
        let _ = writeln!(s, "output_ms={o:.1}");
    }
    if let Some(t) = st.timing {
        if let Some(o) = out {
            let _ = writeln!(s, "total_ms={:.1}", t.capture_and_link_ms + t.buffer_ms + o);
        }
        let _ = writeln!(s, "capture_ms={:.1}", t.capture_and_link_ms);
        let _ = writeln!(s, "buffer_ms={:.1}", t.buffer_ms);
        let _ = writeln!(s, "jitter_ms={:.2}", t.arrival_jitter_ms);
    }
    if let Some(d) = st.device {
        let _ = writeln!(s, "exclusive={}", d.exclusive);
    }
    let p = &st.playout;
    let _ = writeln!(s, "margin_ms={:.1}", p.margin_ms);
    let _ = writeln!(s, "drift_ppm={:.0}", p.drift_ppm);
    let _ = writeln!(s, "underruns={}", p.underruns);
    let _ = writeln!(s, "xruns={}", st.xruns);
    let _ = writeln!(s, "lost={}", st.net.lost);
    s
}
