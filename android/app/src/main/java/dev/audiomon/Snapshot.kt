package dev.audiomon

/** Engine status parsed from [Native.snapshot] (`key=value` lines; unknown values are absent). */
class Snapshot private constructor(private val values: Map<String, String>) {

    enum class State { STOPPED, WAITING, PLAYING, PROBLEM }

    val state: State = when (values["state"]) {
        "playing" -> State.PLAYING
        "waiting" -> State.WAITING
        "problem" -> State.PROBLEM
        else -> State.STOPPED
    }

    /** `usb_tethering`, `wifi`, `usb_direct` or `network`, once a PC is connected. */
    val link: String? get() = values["link"]
    val pc: String? get() = values["pc"]
    val totalMs: Double? get() = number("total_ms")
    val captureMs: Double? get() = number("capture_ms")
    val bufferMs: Double? get() = number("buffer_ms")
    val outputMs: Double? get() = number("output_ms")
    val jitterMs: Double? get() = number("jitter_ms")
    val marginMs: Double? get() = number("margin_ms")
    val driftPpm: Double? get() = number("drift_ppm")
    val underruns: Long? get() = values["underruns"]?.toLongOrNull()
    val xruns: Long? get() = values["xruns"]?.toLongOrNull()
    val lost: Long? get() = values["lost"]?.toLongOrNull()
    val exclusive: Boolean? get() = values["exclusive"]?.toBooleanStrictOrNull()

    private fun number(key: String) = values[key]?.toDoubleOrNull()

    companion object {
        val STOPPED = Snapshot(mapOf("state" to "stopped"))

        fun parse(text: String) = Snapshot(
            text.lineSequence()
                .mapNotNull { line -> line.indexOf('=').takeIf { it > 0 }?.let { line.substring(0, it) to line.substring(it + 1) } }
                .toMap(),
        )
    }
}
