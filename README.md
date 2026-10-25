# audiomon

**Use your Android phone as your PC's speaker.**

No speakers on your computer? audiomon plays everything your Windows PC plays (videos, music, games) through your phone, over the USB cable or your Wi-Fi, with as little delay as the hardware allows.

- **Low delay.** About 25–30 ms from PC to phone speaker over USB tethering on the reference setup, low enough for videos to stay in sync. Wi-Fi works too, with a bit more delay depending on the network.
- **No setup.** Open the app on the phone and audiomon on the PC: they find each other by themselves. If the USB cable and USB tethering are on, it switches to the faster link automatically.
- **Light.** The PC app is a small tray icon that uses under 1% CPU. The phone app spends no power on animations, so it can play for hours with the screen locked.
- **Free and open source.** English and Spanish.

## How to use it

1. **On the PC** (Windows 10 or 11): install audiomon. It lives in the system tray as a record icon near the clock: grey while it looks for the phone, coloured while it plays. Right-click it for the status and the "Start with Windows" option.
2. **On the phone** (Android 9 or newer): install and open the audiomon app.
3. **Connect**, either way:
   - **Wi-Fi:** keep the phone and the PC on the same network. Nothing else to do.
   - **USB cable (lowest delay):** plug the phone into the PC and turn on **USB tethering** in the phone's settings (*Hotspot and tethering*).

Play anything on the PC; it comes out of the phone. The phone screen shows whether sound is playing, over which link, and the current delay. **Details** shows how that delay breaks down.

> Android does not let apps switch USB tethering on by themselves, so that one toggle is up to you.

**Download:** the Windows installer and the Android app are on the [Releases page](https://github.com/dxcot/audiomon/releases/latest). See [PRIVACY.md](PRIVACY.md) for what the apps do (and don't do) with data.

## How it works

```
Windows PC                                   Android phone
──────────                                   ─────────────
audio the PC plays                           AAudio low-latency output
   │  WASAPI loopback capture                    ▲
   ▼                                             │  playout: adaptive buffer +
48 kHz stereo packets ──── UDP ───────────►  network     drift-correcting resampler
   ▲                  (USB tethering        thread ─► ring buffer ─┘
   │                    or Wi-Fi)              │
clock sync ◄──────── TCP control ──────────────┘
```

- **Capture:** the PC records what Windows is playing (WASAPI loopback) and sends each 10 ms packet immediately, uncompressed.
- **Transport:** plain UDP over whatever link connects the two. USB tethering is preferred; Wi-Fi is found with a discovery broadcast. A TCP control channel carries the handshake and clock synchronisation.
- **Playback:** the phone keeps only as much audio buffered as the link's measured jitter requires (a high percentile of recent packet lateness), and gently resamples to follow the small clock difference between PC and phone. Audio goes out through AAudio in exclusive (MMAP) mode when the phone grants it.

### Where the delay comes from

| Stage | Typical |
|---|---|
| Windows audio engine (capture period) | ~10 ms |
| USB link | ~1 ms |
| Phone buffer (depends on link jitter) | ~6–12 ms |
| Phone audio output | ~8–10 ms |

Measured with the app's own instrumentation on one reference setup; other PCs and phones will differ. Wi-Fi adds jitter, so the phone buffer grows to match.

## Building from source

The engine on both sides is Rust; the Android app is a thin Kotlin layer around it.

**Requirements**

- [Rust](https://rustup.rs) (stable) with the MSVC toolchain, plus Visual Studio Build Tools (C++ workload and Windows SDK)
- For the phone app: Android SDK and NDK, a JDK 21 (Android Studio's bundled one works), and [cargo-ndk](https://github.com/bbqsrc/cargo-ndk), with the targets `aarch64-linux-android` and `armv7-linux-androideabi`
- For the installer: [Inno Setup 6](https://jrsoftware.org/isinfo.php)

**PC app**

```powershell
cargo build --release -p audiomon-host          # target\release\audiomon.exe
powershell -File installer\build.ps1            # target\installer\audiomon-setup-<version>.exe
```

`audiomon.exe` without arguments starts the tray app. From a terminal it also offers `stream`, `usb`, `devices`, `outputs` and `record` (run it with an unknown argument for the list).

**Phone app**

```powershell
cd android
.\gradlew.bat assembleRelease                    # builds the Rust engine through cargo-ndk first
# app\build\outputs\apk\release\app-release.apk
```

Create `android/local.properties` with `sdk.dir=<path to your Android SDK>` first.

Release builds are signed with the key described in `android/keystore.properties` (not in the repository):

```properties
storeFile=C:/path/to/your-release.jks
storePassword=...
keyAlias=...
keyPassword=...
```

Without that file the release build falls back to the debug key, which is fine for trying it out. Official audiomon APKs are signed with a certificate whose SHA-256 fingerprint is
`7ae2151d7b4c6416c0cebf76efe01407f2d1f9673bfc25b008e0d207db0ac743`
(check with `apksigner verify --print-certs audiomon.apk`).

**Tests**

```powershell
cargo test --workspace
```

The core crate includes simulation tests that run minutes to an hour of simulated audio against clock drift, network jitter and packet loss.

## Project layout

| Path | What it is |
|---|---|
| `crates/core` | Shared, OS-independent logic: packet format, handshake and clock sync, depacketizer with loss concealment, adaptive playout and resampler |
| `crates/host` | Windows tray app: capture, discovery, UDP sender, direct-USB mode, icon |
| `crates/receiver` | Android engine: AAudio bindings, receive threads, JNI bridge |
| `android` | Kotlin app (one screen and a foreground service) |
| `installer` | Inno Setup script and build script for the Windows installer |

## Advanced: direct USB

Besides USB tethering, audiomon can talk to the phone over a raw USB pipe (Android Open Accessory), skipping the network stack. It saves about 1 ms but needs USB debugging enabled and the WinUSB driver installed for the "Android Accessory Interface" (for example with [Zadig](https://zadig.akeo.ie)). Choose it from the tray menu.

## License

Licensed under either the [MIT license](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option.
The label typeface, Barlow Condensed, is licensed under the SIL Open Font License (`android/third_party/BarlowCondensed-OFL.txt`).
