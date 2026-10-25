//! Phase 3 spike: play a tone through AAudio and report what the device granted.
//!
//! Build and run from the PC:
//! ```text
//! cargo ndk -t arm64-v8a --platform 28 build --release -p audiorast-receiver --example tone
//! adb push target/aarch64-linux-android/release/examples/tone /data/local/tmp/
//! adb shell /data/local/tmp/tone [seconds] [shared] [bursts]
//! ```

#[cfg(target_os = "android")]
fn main() {
    use audiorast::aaudio::{Config, OutputStream};
    use std::time::Duration;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let secs: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(5);
    let exclusive = !args.iter().any(|a| a == "shared");
    let buffer_bursts: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2);

    let cfg = Config { sample_rate: 48_000, channels: 2, exclusive, buffer_bursts };
    let mut phase = 0.0f32;
    let step = std::f32::consts::TAU * 440.0 / cfg.sample_rate as f32;
    let render = Box::new(move |out: &mut [f32]| {
        for frame in out.as_chunks_mut::<2>().0 {
            let x = 0.2 * phase.sin();
            *frame = [x, x];
            phase = (phase + step) % std::f32::consts::TAU;
        }
    });

    let stream = match OutputStream::open(cfg, render) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open failed: {e}");
            std::process::exit(1);
        }
    };
    let i = stream.info();
    let ms = |f: i32| f as f64 * 1000.0 / i.sample_rate as f64;
    println!("requested: {} | granted: {}, low-latency: {}", if exclusive { "exclusive" } else { "shared" },
        if i.exclusive { "EXCLUSIVE (MMAP)" } else { "shared" }, i.low_latency);
    println!("rate {} Hz, {} ch, burst {} frames ({:.2} ms), buffer {} ({:.2} ms), capacity {}",
        i.sample_rate, i.channels, i.burst_frames, ms(i.burst_frames), i.buffer_frames, ms(i.buffer_frames), i.capacity_frames);

    stream.start().expect("start");
    for s in 1..=secs {
        std::thread::sleep(Duration::from_secs(1));
        let lat = stream.output_latency_ms().map_or("n/a".into(), |l| format!("{l:.2} ms"));
        println!("t={s}s output latency {lat}, xruns {}", stream.xruns());
        if let Some(e) = stream.error() {
            println!("stream error: {e}");
            break;
        }
    }
    stream.stop().ok();
}

#[cfg(not(target_os = "android"))]
fn main() {
    eprintln!("this example only runs on Android");
}
