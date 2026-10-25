package dev.audiorast

/** Bridge to the Rust engine in libaudiorast.so (crates/receiver/src/jni.rs). */
object Native {
    init {
        System.loadLibrary("audiorast")
    }

    /** Starts listening for the PC. Safe to call twice. */
    @JvmStatic external fun start()

    /** Plays audio from an open USB accessory; takes ownership of [fd]. */
    @JvmStatic external fun startAccessory(fd: Int)

    @JvmStatic external fun stop()

    /** Status as `key=value` lines; parse with [Snapshot.parse]. */
    @JvmStatic external fun snapshot(): String
}
