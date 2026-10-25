# audiorast: use an Android phone as your PC's speaker over USB

## Goal
Two native apps written in Rust. A Windows **`.exe`** captures everything the PC plays and
sends it over the USB cable to an **Android app**, which plays it with the lowest possible delay.
It is for people who have no speakers but do have a phone. Nothing runs in a browser and
nothing goes over the internet: all traffic stays on the cable.

**Scope v1:** Windows 10/11 host and Android 9+ (API 28) receiver (AAudio).

**Targets**
| Metric | Target |
|---|---|
| End-to-end latency (PC sound to phone speaker) | **≤ 30 ms** typical, ≤ 40 ms worst |
| Host CPU / RAM / binary | < 1% / < 10 MB / < 3 MB |
| Android APK | < 3 MB |
| Dropouts | 0 in a 1-hour test |
| Setup | Plug in the cable, turn on USB tethering, open the app, and audio plays |

---

## 1. Where the delay comes from (latency budget)

| Stage | Typical | How we minimize it |
|---|---|---|
| Windows capture (WASAPI loopback) | 10 ms | Event-driven capture, send each packet the moment it arrives, MMCSS "Pro Audio" thread priority. (The 10 ms is the Windows audio engine period; shared-mode loopback can't go lower.) |
| USB link | 0.5–2 ms | Raw PCM over UDP: no codec, no TCP retransmits, no Nagle |
| Jitter buffer (phone) | 3–8 ms | Adaptive: shrinks to the smallest size that doesn't drop out |
| Android output (AAudio) | 5–20 ms | `LowLatency` + `Exclusive` (MMAP) mode, native 48 kHz (no resampling), 1–2 bursts of buffer |
| **Total** | **~20–40 ms** | Barely noticeable, even for video |

Most of the remaining delay comes from Windows and Android themselves. The USB link is
the smallest part of the delay, so the transport choice should favor being easy and reliable.

---

## 2. Key technical decisions

### 2.1 Capture (PC): WASAPI loopback
- Crate `wasapi` (direct bindings, finer control than `cpal`): loopback on the default render device, event-driven, 48 kHz float32 stereo.
- Convert to s16 (or send f32 directly; bandwidth is not a concern over USB).
- Follow default-device changes (the user switches output, or HDMI is unplugged).
- Detect silence and send a small "silence" packet instead of zeros, to save power on the phone.

### 2.2 Transport: USB tethering + UDP (primary), ADB (fallback)
| Method | Phone setup | Latency | Decision |
|---|---|---|---|
| **USB tethering** (RNDIS/NCM) | One toggle in Settings | ~1 ms | **Primary.** No driver, no developer mode |
| **ADB reverse/forward** | USB debugging on | ~2–3 ms (TCP) | **Fallback** via the `adb_client` crate (pure Rust, no bundled adb.exe) |
| Android Open Accessory (raw USB, `rusb`) | None | ~0.5 ms | **Rejected for v1.** Windows needs a WinUSB driver install (Zadig/libwdi) for a <1 ms gain |

**Zero-config discovery over tethering:** when tethering is on, Windows creates a network
adapter ("Remote NDIS" or "NCM"). The **phone is that adapter's gateway**, e.g. `192.0.2.1` (example address).
The host lists adapters (`GetAdaptersAddresses` via the `windows` crate), finds the tether
adapter, and connects straight to the gateway IP. You never type an IP address.
Traffic is bound only to that interface, so it never goes through the internet.

### 2.3 Audio format
- **Raw PCM, 48 kHz, stereo**: ~1.5 Mbps s16 or 3 Mbps f32. USB 2.0 handles this easily.
- No codec, so no encode/decode delay and no CPU cost.

### 2.4 Protocol
```
Control (TCP, port 47800):
  host → phone  HELLO{proto_ver, sample_rate, channels, sample_fmt, frame_samples}
  phone → host  ACCEPT{device_rate, burst_frames}   (or REJECT)
  both          PING/PONG every 1 s (RTT, liveness), VOLUME, BYE

Audio (UDP, port 47801), one datagram per capture packet (~10 ms = 480 frames = 1920 B in s16):
  [u32 seq][u32 frames][u64 host_ts_us][PCM payload]
```
- **Jitter buffer** (in `core`): target = observed jitter (p99) plus a safety margin, adapted slowly. It starts small and grows only when a dropout happens.
- **Clock drift correction**: the PC and phone sound cards run at slightly different rates (±100 ppm). A control loop watches how full the buffer is and adjusts playback speed by up to ±0.1% (`rubato` sinc, or cheap linear interpolation). Without this, the audio crackles or the delay keeps growing over time.
- **Packet loss**: fade the missing frame out and back in, never stall. Late packets are dropped.

### 2.5 Android app: Rust audio engine + minimal Kotlin shell
- **Rust `cdylib`** (`cargo-ndk`, targets `arm64-v8a` + `armeabi-v7a`): sockets, jitter buffer, drift control, and an AAudio stream via the `oboe` crate.
  - `PerformanceMode::LowLatency`, `SharingMode::Exclusive`, `Usage::Media`, 48 kHz float.
  - The real-time callback only reads from the jitter buffer. It never allocates, locks, or logs.
- **Kotlin shell** (~200 lines): one screen (status, latency readout, volume, connect button) plus a **foreground service** (so playback continues with the screen off) and a wake lock while streaming. It talks to Rust through 4–5 JNI functions (`start`, `stop`, `status`, `setVolume`).
- Why not a pure-Rust UI? Android requires Java/Kotlin for foreground services and notifications anyway, and a tiny Kotlin UI keeps the APK small.

---

## 3. Architecture

```
┌──────────────── Windows: audiorast.exe ────────────────┐
│ WASAPI loopback (MMCSS thread)                         │
│   → convert → UDP send ─────────────┐                  │
│ tether-adapter discovery            │  control (TCP)   │
│ tray icon: status / latency / quit  │                  │
└─────────────────────────────────────┼──────────────────┘
                                      │ USB cable (RNDIS/NCM)
┌──────────────── Android: audiorast.apk ┼───────────────┐
│ UDP recv thread → jitter buffer ◄─ drift controller    │
│                       ▼                                │
│        AAudio callback (Oboe, exclusive, low latency)  │
│ Kotlin: UI + foreground service ── JNI ── Rust (.so)   │
└────────────────────────────────────────────────────────┘
```

### Repository layout
```
audiorast/
├── Cargo.toml              # workspace
├── crates/
│   ├── core/               # protocol, jitter buffer, drift control (pure Rust, no OS deps, heavily tested)
│   ├── host/               # Windows exe: capture, discovery, sender, tray
│   └── receiver/           # Android cdylib: socket + oboe + JNI exports
├── android/                # Gradle project, Kotlin shell, loads libreceiver.so
└── tools/
    └── latency-probe/      # measures end-to-end latency (click → microphone)
```

### Dependencies (kept minimal)
| Crate | Use |
|---|---|
| `wasapi`, `windows` | Capture, MMCSS priority, adapter list (host) |
| (none, direct FFI) | AAudio playback (receiver) |
| `jni` | Kotlin ↔ Rust bridge |
| `rtrb` | Lock-free single-producer/single-consumer (SPSC) ring buffer |
| `rubato` | Drift resampling |
| `tray-icon` | Windows tray UI |
| `adb_client` | ADB fallback (optional feature) |
| `log` + `env_logger` / `android_logger` | Logging |

No async runtime: `std::net` plus 2–3 dedicated threads per side.
Release profile: `opt-level=3`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip=true`.

---

## 4. Roadmap

### Phase 0: Setup (½ day)
- Install `rustup` (MSVC toolchain), Visual Studio Build Tools, Android Studio + NDK, and `cargo install cargo-ndk`; add the Android targets.
- Workspace skeleton, `git init`, GitHub Actions (build + clippy + tests).

### Phase 1: Capture spike (1 day)
- `host`: WASAPI loopback → 10 s WAV file. Confirm the format, packet period, and CPU use.
- ✅ **Done** (`audiorast record [secs] [file]`). Measured on a USB audio output device:
  48 kHz f32 stereo, **480 frames (10.00 ms) per packet, every 9.8–10.2 ms**, 0 dropped packets,
  CPU below the timer resolution (<0.3%), release exe 282 KB.
  The device reports a 3 ms minimum period, but loopback still delivers at the engine's 10 ms period.
  Nothing arrives while Windows is silent, so the sender must handle gaps (send silence or a keep-alive).

### Phase 2: `core` (2–3 days)
- Packet encoding/decoding, jitter buffer, drift controller.
- Simulation tests: random jitter (0–5 ms), 1% packet loss, ±200 ppm skew, run for a simulated hour. No buffer underruns, and latency stays stable.
- ✅ **Done.** `depacketizer.rs` (ordering, loss concealment, silence packets) and `playout.rs`
  (adaptive margin from the measured jitter, PI drift control, cubic Hermite resampling).
  Payload is f32 LE; an empty payload means silence. Simulated results:
  0 underruns over 1 hour (+120 ppm, 1.5 ms jitter), receive buffer ~10–14 ms, no clicks,
  drift locked within ~10 ppm for ±200 ppm in the 10-minute tests.
  With 1% loss + 5 ms jitter: 2 underruns in 10 minutes (probably two losses in a row), buffer ~28 ms.

### Phase 3: Android playback spike (1–2 days)
- Rust `.so` + Oboe plays a sine tone. Check that exclusive/MMAP mode is granted and log the burst size and reported latency.
- ✅ **Done**, using direct AAudio FFI (`crates/receiver/src/aaudio.rs`) instead of the `oboe` crate: no C++ build, smaller binary.
  Example `tone` runs via `adb shell`. On the reference phone (Android 16):
  exclusive **MMAP granted**, burst 96 frames (**2 ms**), output latency **5–9 ms** with a 1–2 burst buffer, 0 xruns.
  Shared mode: 13–15 ms. Minimum API raised to 28 (`AAudioStreamBuilder_setUsage`).

### Phase 4: End-to-end MVP (3–4 days) ✅ first usable version
- Host discovers the tether adapter and sends audio; the phone plays it.
- Console host: `audiorast.exe` prints status and latency.
- **Milestone:** a YouTube video on the PC plays from the phone, in sync.
- ✅ **Working** on the reference phone over RNDIS tethering: auto-discovery, exclusive MMAP output (9–10 ms),
  0 lost datagrams. After 70 s: buffer margin 11.9 ms, drift estimate still settling (+219 ppm),
  2 underruns at startup, 4 AAudio xruns (possibly from the screenshots taken during the test).
  APK 705 KB, host exe < 1 MB. Open issues for Phase 5: startup transient, jitter estimate 5–11 ms
  (higher than expected for USB), xruns at a 2-burst device buffer.

### Phase 5: Latency tuning (2–3 days)
- `latency-probe`: play a click on the PC, record it with the phone mic, and measure the offset.
- Tune the jitter-buffer minimum and the AAudio buffer size (`setBufferSizeInFrames` = 1–2 bursts).
- Test on 3+ phones (Samsung, Pixel, Xiaomi), because AAudio behaves differently on each vendor.

### Phase 6: Polish (1 week)
- Host: tray icon, auto-reconnect on cable replug, "start with Windows", single portable `.exe`.
- Android: foreground service, notification, volume, latency display.
- ADB fallback for phones that can't tether.

### Later
- Wi-Fi mode (Opus codec).
- Several phones at once (left/right stereo).
- A Windows virtual audio driver, so audiorast shows up as a "speaker" in Windows' sound list.

---

## 5. Risks
| Risk | Mitigation |
|---|---|
| The PC has no output device at all, so loopback has nothing to capture | Windows almost always lists one (HDMI or "Speakers" with nothing plugged in); detect this and tell the user. Long-term fix is the virtual driver |
| The phone or carrier blocks USB tethering | ADB fallback |
| Exclusive/MMAP mode is not granted on some phones | Fall back to shared mode with a slightly larger buffer; show the actual latency in the app |
| Windows starts routing internet through the phone | The host warns about it; the user can turn off mobile data (the audio doesn't need it) |
| Android kills the app in the background | Foreground service + battery-optimization exemption prompt |
| Clock drift | Drift controller, verified by simulation in Phase 2 |
