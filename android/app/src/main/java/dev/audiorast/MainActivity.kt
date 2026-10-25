package dev.audiorast

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.view.Gravity
import android.view.View
import android.view.WindowInsets
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView
import java.util.Locale
import kotlin.math.roundToLong

class MainActivity : Activity() {
    companion object {
        /** One accessory session per process; the fd belongs to the engine once opened. */
        @Volatile
        private var accessoryOpen = false

        /** Status refresh while the screen is visible; nothing refreshes while it is locked. */
        private const val REFRESH_MS = 500L

        /** How long the "use USB for less delay" tip stays after the link becomes Wi-Fi. */
        private const val HINT_MS = 20_000L
    }

    private val handler = Handler(Looper.getMainLooper())
    private lateinit var disc: DiscView
    private lateinit var linkRow: View
    private lateinit var link: TextView
    private lateinit var pc: TextView
    private lateinit var message: TextView
    private lateinit var primary: Button
    private lateinit var secondary: Button
    private lateinit var hint: TextView
    private lateinit var details: LinearLayout
    private lateinit var help: LinearLayout

    /** The outlined button opens details while playing and the connection help otherwise. */
    private var detailsOpen = false
    private var helpOpen = false
    private var lastLink: String? = null
    private var hintUntil = 0L

    private val refresh = object : Runnable {
        override fun run() {
            render()
            handler.postDelayed(this, REFRESH_MS)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        val root = findViewById<View>(R.id.root)
        // Edge to edge: keep content clear of the status and navigation bars.
        root.setOnApplyWindowInsetsListener { v, insets ->
            val bars = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
            v.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            insets
        }
        disc = findViewById(R.id.disc)
        linkRow = findViewById(R.id.link_row)
        link = findViewById(R.id.link)
        pc = findViewById(R.id.pc)
        message = findViewById(R.id.message)
        primary = findViewById(R.id.primary)
        secondary = findViewById(R.id.details_toggle)
        hint = findViewById(R.id.hint)
        details = findViewById(R.id.details)
        help = findViewById(R.id.help)

        // Open sections survive a recreated screen (rotation, font size).
        detailsOpen = savedInstanceState?.getBoolean("details") ?: false
        helpOpen = savedInstanceState?.getBoolean("help") ?: false
        primary.setOnClickListener { toggleService() }
        secondary.setOnClickListener {
            if (isPlaying()) detailsOpen = !detailsOpen else helpOpen = !helpOpen
            render()
        }
        buildHelp()

        if (Build.VERSION.SDK_INT >= 33) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0)
        }
        // Opening the app is enough: start listening for the PC right away. Only on a fresh
        // launch: a recreated screen (rotation, font size) must not undo a tap on Stop.
        if (savedInstanceState == null && !AudioService.running) {
            startForegroundService(Intent(this, AudioService::class.java))
        }
        openAccessory(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        openAccessory(intent)
    }

    /** Direct USB: the PC put the phone in accessory mode and Android launched us for it. */
    private fun openAccessory(intent: Intent) {
        if (intent.action != UsbManager.ACTION_USB_ACCESSORY_ATTACHED) return
        val accessory = if (Build.VERSION.SDK_INT >= 33) {
            intent.getParcelableExtra(UsbManager.EXTRA_ACCESSORY, UsbAccessory::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(UsbManager.EXTRA_ACCESSORY)
        } ?: return
        open(accessory)
    }

    /** The accessory may already be attached (e.g. the app was reinstalled while plugged in). */
    private fun openAttachedAccessory() {
        if (accessoryOpen) return
        val usb = getSystemService(UsbManager::class.java)
        val accessory = usb.accessoryList?.firstOrNull { it.manufacturer == "audiorast" } ?: return
        if (usb.hasPermission(accessory)) open(accessory)
    }

    private fun open(accessory: UsbAccessory) {
        val pfd = getSystemService(UsbManager::class.java).openAccessory(accessory) ?: return
        accessoryOpen = true
        val fd = pfd.detachFd()
        if (AudioService.running) {
            Native.startAccessory(fd)
        } else {
            AudioService.pendingAccessoryFd = fd
            startForegroundService(Intent(this, AudioService::class.java))
        }
    }

    override fun onResume() {
        super.onResume()
        openAttachedAccessory()
        handler.post(refresh)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putBoolean("details", detailsOpen)
        outState.putBoolean("help", helpOpen)
    }

    override fun onPause() {
        handler.removeCallbacks(refresh)
        super.onPause()
    }

    private fun toggleService() {
        val intent = Intent(this, AudioService::class.java)
        if (AudioService.running) stopService(intent) else startForegroundService(intent)
        handler.postDelayed({ render() }, 200)
    }

    private var snap = Snapshot.STOPPED

    private fun isPlaying() = snap.state == Snapshot.State.PLAYING && snap.link != null

    private fun render() {
        snap = if (AudioService.running) Snapshot.parse(Native.snapshot()) else Snapshot.STOPPED
        val linkName = when (snap.link) {
            "usb_tethering" -> getString(R.string.link_usb_tethering)
            "wifi" -> getString(R.string.link_wifi)
            "usb_direct" -> getString(R.string.link_usb_direct)
            null -> null
            else -> getString(R.string.link_network)
        }
        val total = snap.totalMs?.roundToLong()?.toString()

        when (snap.state) {
            Snapshot.State.PLAYING -> {
                disc.show(
                    DiscView.Mode.PLAYING,
                    DiscView.Print(
                        word = getString(R.string.label_playing),
                        figure = total ?: "",
                        unit = if (total != null) getString(R.string.unit_ms) else "",
                        caption = getString(if (total != null) R.string.label_delay else R.string.label_measuring),
                    ),
                )
                disc.contentDescription = if (total != null) {
                    getString(R.string.a11y_playing, total, linkName ?: "")
                } else {
                    getString(R.string.a11y_playing_measuring, linkName ?: "")
                }
                message.text = ""
            }
            Snapshot.State.WAITING -> {
                disc.show(DiscView.Mode.WAITING, phrase(R.string.label_waiting, R.string.label_no_pc))
                disc.contentDescription = getString(R.string.a11y_waiting)
                message.text = getString(R.string.msg_waiting)
            }
            Snapshot.State.PROBLEM -> {
                disc.show(DiscView.Mode.PROBLEM, phrase(R.string.label_problem, R.string.label_reconnecting))
                disc.contentDescription = getString(R.string.a11y_problem)
                message.text = getString(R.string.msg_problem)
            }
            Snapshot.State.STOPPED -> {
                disc.show(DiscView.Mode.STOPPED, phrase(R.string.label_stopped, R.string.label_tap_start))
                disc.contentDescription = getString(R.string.a11y_stopped)
                message.text = getString(R.string.msg_stopped)
            }
        }
        message.visibility = if (message.text.isEmpty()) View.GONE else View.VISIBLE

        val playing = isPlaying()
        linkRow.visibility = if (playing) View.VISIBLE else View.GONE
        if (playing) {
            link.text = linkName
            pc.text = snap.pc?.let { getString(R.string.pc_address, it) } ?: ""
        }

        // The Wi-Fi tip appears when the link becomes Wi-Fi, for a while, not on every glance.
        if (snap.link != lastLink) {
            lastLink = snap.link
            hintUntil = if (snap.link == "wifi") SystemClock.elapsedRealtime() + HINT_MS else 0L
        }
        hint.visibility = if (playing && SystemClock.elapsedRealtime() < hintUntil) View.VISIBLE else View.GONE

        primary.setText(if (AudioService.running) R.string.action_stop else R.string.action_start)
        // Outlined slot: Details while playing, Help connecting while waiting or lost.
        val canHelp = snap.state == Snapshot.State.WAITING || snap.state == Snapshot.State.PROBLEM
        if (!playing) detailsOpen = false
        if (!canHelp) helpOpen = false
        secondary.visibility = if (playing || canHelp) View.VISIBLE else View.GONE
        secondary.setText(
            when {
                playing && detailsOpen -> R.string.action_hide_details
                playing -> R.string.action_details
                helpOpen -> R.string.action_hide_details
                else -> R.string.action_help
            },
        )
        help.visibility = if (helpOpen) View.VISIBLE else View.GONE
        details.visibility = if (detailsOpen) View.VISIBLE else View.GONE
        if (detailsOpen) renderDetails(snap)
    }

    /** A label without a delay: the state word on the arc, a short phrase in the middle. */
    private fun phrase(word: Int, caption: Int) = DiscView.Print(getString(word), "", "", getString(caption))

    /** The numbers behind the delay: labels left, values right-aligned like running times. */
    private fun renderDetails(snap: Snapshot) {
        val rows = listOf(
            R.string.details_total to snap.totalMs?.let { ms(it) },
            R.string.details_capture to snap.captureMs?.let { ms(it) },
            R.string.details_buffer to snap.bufferMs?.let { ms(it) },
            R.string.details_output to snap.outputMs?.let { ms(it) },
            R.string.details_jitter to snap.jitterMs?.let { ms(it) },
            R.string.details_margin to snap.marginMs?.let { ms(it) },
            R.string.details_drift to snap.driftPpm?.let { getString(R.string.value_ppm, String.format(Locale.getDefault(), "%+.0f", it)) },
            R.string.details_dropouts to snap.underruns?.toString(),
            R.string.details_glitches to snap.xruns?.toString(),
            R.string.details_lost to snap.lost?.toString(),
            R.string.details_mode to snap.exclusive?.let { getString(if (it) R.string.mode_exclusive else R.string.mode_shared) },
        ).filter { it.second != null }

        // Rows are built once and only their values change afterwards.
        if (details.childCount != rows.size * 2) {
            details.removeAllViews()
            rows.forEach { (label, _) -> addRow(details, getString(label)) }
        }
        rows.forEachIndexed { i, (_, value) ->
            val row = details.getChildAt(i * 2 + 1) as LinearLayout
            (row.getChildAt(1) as TextView).text = value
        }
    }

    /** The connection steps, numbered because their order matters. */
    private fun buildHelp() {
        listOf(R.string.help_1, R.string.help_2, R.string.help_3).forEachIndexed { i, step ->
            addRow(help, (i + 1).toString(), value = getString(step), numbered = true)
        }
    }

    /** A ruled row: label on the left, value right-aligned (or wrapping text for help steps). */
    private fun addRow(parent: LinearLayout, label: String, value: String = "", numbered: Boolean = false) {
        val dp = resources.displayMetrics.density
        parent.addView(
            View(this).apply { setBackgroundColor(getColor(R.color.rule)) },
            LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, dp.toInt().coerceAtLeast(1)),
        )
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            minimumHeight = (44 * dp).toInt()
            gravity = if (numbered) Gravity.TOP else Gravity.CENTER_VERTICAL
            if (numbered) setPadding(0, (12 * dp).toInt(), 0, (12 * dp).toInt())
        }
        row.addView(
            TextView(this).apply {
                text = label
                textSize = 14f
                setTextColor(getColor(R.color.cream_muted))
                if (numbered) typeface = resources.getFont(R.font.barlow_condensed_bold)
            },
            if (numbered) {
                LinearLayout.LayoutParams((28 * dp).toInt(), LinearLayout.LayoutParams.WRAP_CONTENT)
            } else {
                LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
            },
        )
        row.addView(
            TextView(this).apply {
                text = value
                textSize = if (numbered) 15f else 14f
                setTextColor(getColor(R.color.cream))
                if (numbered) setLineSpacing(4 * dp, 1f) else fontFeatureSettings = "tnum"
            },
            if (numbered) {
                LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
            } else {
                LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT)
            },
        )
        parent.addView(row)
    }

    private fun ms(v: Double) = getString(R.string.value_ms, String.format(Locale.getDefault(), "%.1f", v))
}
