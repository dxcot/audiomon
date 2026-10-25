package dev.audiorast

import android.animation.Animator
import android.animation.AnimatorListenerAdapter
import android.animation.TimeAnimator
import android.animation.ValueAnimator
import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.Path
import android.graphics.RectF
import android.graphics.SweepGradient
import android.graphics.Typeface
import android.util.AttributeSet
import android.view.View
import android.view.animation.AccelerateInterpolator
import android.view.animation.DecelerateInterpolator
import kotlin.math.PI
import kotlin.math.min

/**
 * The record: black vinyl with grooves around a navy center label printed in cream.
 * The state word arcs over the spindle and the delay is printed beneath it like a speed marking.
 * When sound starts the groove sheen makes one turntable turn (spin up, coast to rest); a
 * problem shows as a gap in the grooves. The label text never rotates.
 */
class DiscView(context: Context, attrs: AttributeSet? = null) : View(context, attrs) {

    enum class Mode { PLAYING, WAITING, STOPPED, PROBLEM }

    /** What is printed on the label. */
    data class Print(
        /** Word on the upper arc, e.g. PLAYING. */
        val word: String,
        /** Big figure under the spindle (the delay); empty to print [caption] in its place. */
        val figure: String,
        /** Small unit after the figure. */
        val unit: String,
        /** Tracked caps under the figure, or a short phrase when there is no figure. */
        val caption: String,
    )

    var mode = Mode.STOPPED
        private set
    private var print = Print("", "", "", "")
    /** The previous print while a state change crossfades; null otherwise. */
    private var fadingOut: Print? = null
    private var fade = 1f

    private val dp = resources.displayMetrics.density
    private val maxSize = (340 * dp).toInt()

    private val vinyl = context.getColor(R.color.vinyl)
    private val grooveDark = context.getColor(R.color.groove_dark)
    private val grooveLight = context.getColor(R.color.groove_light)
    private val labelColor = context.getColor(R.color.label)
    private val cream = context.getColor(R.color.cream)
    private val labelMuted = context.getColor(R.color.label_muted)
    private val gold = context.getColor(R.color.gold)

    private val bold: Typeface = resources.getFont(R.font.barlow_condensed_bold)
    private val semibold: Typeface = resources.getFont(R.font.barlow_condensed_semibold)

    private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.FILL }
    private val stroke = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE }
    private val sheenPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE }
    private val text = Paint(Paint.ANTI_ALIAS_FLAG).apply { textAlign = Paint.Align.LEFT }
    private val arcPath = Path()
    private val oval = RectF()
    private val sheenMatrix = Matrix()
    private var sheen: SweepGradient? = null
    private var sheenFor = 0f

    // Turntable motion: `speed` ramps between 0 (resting) and 1 (33⅓ rpm).
    private var angle = 0f
    private var speed = 0f
    private val clock = TimeAnimator().apply {
        setTimeListener { _, _, deltaMs ->
            angle = (angle + 360f * speed * deltaMs / 1800f) % 360f
            invalidate()
            // Stop redrawing as soon as the record is at rest.
            if (speed == 0f && ramp?.isStarted != true) end()
        }
    }
    private var ramp: ValueAnimator? = null
    /** The start-of-play turn already happened for the current playing stretch. */
    private var turned = false
    private var crossfade: ValueAnimator? = null

    /** Shows [mode] with [next] printed on the label, animating the change when allowed. */
    fun show(mode: Mode, next: Print) {
        val modeChanged = mode != this.mode
        if (!modeChanged && next == print) return
        if (modeChanged && canAnimate() && print.word.isNotEmpty()) {
            fadingOut = print
            crossfade?.cancel()
            crossfade = ValueAnimator.ofFloat(0f, 1f).apply {
                duration = 260
                interpolator = DecelerateInterpolator()
                addUpdateListener {
                    fade = it.animatedValue as Float
                    if (fade >= 1f) fadingOut = null
                    invalidate()
                }
                start()
            }
        }
        print = next
        if (modeChanged) {
            this.mode = mode
            updateMotion()
        }
        invalidate()
    }

    private fun canAnimate() = ValueAnimator.areAnimatorsEnabled() && isAttachedToWindow

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val available = MeasureSpec.getSize(widthMeasureSpec)
        val size = if (MeasureSpec.getMode(widthMeasureSpec) == MeasureSpec.UNSPECIFIED) maxSize else min(available, maxSize)
        setMeasuredDimension(size, size)
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        updateMotion()
    }

    override fun onDetachedFromWindow() {
        ramp?.cancel()
        crossfade?.cancel()
        clock.end()
        super.onDetachedFromWindow()
    }

    override fun onVisibilityAggregated(isVisible: Boolean) {
        super.onVisibilityAggregated(isVisible)
        updateMotion()
    }

    /**
     * One turn per start, not a constant spin: when sound starts the sheen spins up like a
     * turntable and coasts to rest within about three seconds, then the view stops redrawing.
     * The app is mostly used with the screen locked, so nothing here may cost power at rest.
     * "Remove animations" keeps it still.
     */
    private fun updateMotion() {
        if (!ValueAnimator.areAnimatorsEnabled() || !isAttachedToWindow || !isShown) {
            ramp?.cancel()
            clock.end()
            speed = 0f
            return
        }
        if (mode != Mode.PLAYING) {
            turned = false
            if (speed > 0f) coast()
            return
        }
        if (turned) return
        turned = true
        if (!clock.isStarted) clock.start()
        ramp?.cancel()
        ramp = ValueAnimator.ofFloat(speed, 1f).apply {
            duration = 450
            interpolator = AccelerateInterpolator()
            addUpdateListener { speed = it.animatedValue as Float }
            addListener(object : AnimatorListenerAdapter() {
                override fun onAnimationEnd(animation: Animator) {
                    if (mode == Mode.PLAYING) coast(startDelay = 900)
                }
            })
            start()
        }
    }

    /** A record takes longer to coast down than to reach speed. */
    private fun coast(startDelay: Long = 0) {
        if (!clock.isStarted) clock.start()
        ramp?.cancel()
        ramp = ValueAnimator.ofFloat(speed, 0f).apply {
            this.startDelay = startDelay
            duration = 1400
            interpolator = DecelerateInterpolator()
            addUpdateListener { speed = it.animatedValue as Float }
            start()
        }
    }

    override fun onDraw(canvas: Canvas) {
        val cx = width / 2f
        val cy = height / 2f
        val r = min(width, height) / 2f
        val labelR = r * 216f / 340f

        // Vinyl and its grooves; a problem breaks them with a gap.
        fill.color = grooveDark
        canvas.drawCircle(cx, cy, r, fill)
        stroke.strokeWidth = 1.2f * dp
        stroke.color = grooveLight
        val gap = if (mode == Mode.PROBLEM) GAP_DEGREES else 0f
        var gr = labelR + 6 * dp
        while (gr < r - 8 * dp) {
            if (gap == 0f) {
                canvas.drawCircle(cx, cy, gr, stroke)
            } else {
                oval.set(cx - gr, cy - gr, cx + gr, cy + gr)
                canvas.drawArc(oval, GAP_START + gap, 360f - gap, false, stroke)
            }
            gr += 3 * dp
        }

        // Light catching the grooves: turns while playing, rests otherwise.
        val inner = labelR + 4 * dp
        val outer = r - 7 * dp
        val alpha = when (mode) {
            Mode.PLAYING -> 0x3A
            Mode.WAITING, Mode.PROBLEM -> 0x1E
            Mode.STOPPED -> 0x10
        }
        sheenPaint.shader = sheenShader(cx, cy, alpha)
        sheenMatrix.setRotate(angle + 35f, cx, cy)
        sheenPaint.shader?.setLocalMatrix(sheenMatrix)
        sheenPaint.strokeWidth = outer - inner
        if (gap == 0f) {
            canvas.drawCircle(cx, cy, (inner + outer) / 2f, sheenPaint)
        } else {
            val mid = (inner + outer) / 2f
            oval.set(cx - mid, cy - mid, cx + mid, cy + mid)
            canvas.drawArc(oval, GAP_START + gap, 360f - gap, false, sheenPaint)
        }

        stroke.color = vinyl
        stroke.strokeWidth = 6 * dp
        canvas.drawCircle(cx, cy, r - 3 * dp, stroke)

        // The label, with the spindle hole on the rotation axis.
        fill.color = labelColor
        canvas.drawCircle(cx, cy, labelR, fill)
        stroke.color = gold
        stroke.strokeWidth = 1.5f * dp
        canvas.drawCircle(cx, cy, labelR * 0.926f, stroke)
        fill.color = vinyl
        canvas.drawCircle(cx, cy, labelR * 0.06f, fill)

        val out = fadingOut
        if (out != null) drawPrint(canvas, out, cx, cy, labelR, 1f - fade)
        drawPrint(canvas, print, cx, cy, labelR, if (out != null) fade else 1f)
    }

    private fun drawPrint(canvas: Canvas, p: Print, cx: Float, cy: Float, labelR: Float, opacity: Float) {
        // Words follow the system font size, as far as the label can hold them.
        val scale = resources.configuration.fontScale.coerceIn(1f, 1.35f)
        val alpha = (opacity * 255).toInt()

        // State word on the upper arc.
        text.typeface = bold
        text.color = cream
        text.alpha = alpha
        text.textSize = labelR * 0.15f * scale
        text.letterSpacing = 0.30f / scale
        val arcR = labelR * 0.68f
        oval.set(cx - arcR, cy - arcR, cx + arcR, cy + arcR)
        arcPath.reset()
        arcPath.addArc(oval, 180f, 180f)
        val arcLen = (PI * arcR).toFloat()
        canvas.drawTextOnPath(p.word, arcPath, (arcLen - text.measureText(p.word)) / 2f, text.textSize * 0.35f, text)

        if (p.figure.isNotEmpty()) {
            // The delay and its unit, centered together below the spindle.
            text.letterSpacing = -0.02f
            text.typeface = bold
            text.textSize = labelR * 0.44f
            val figureWidth = text.measureText(p.figure)
            text.typeface = semibold
            text.textSize = labelR * 0.18f
            val unitGap = if (p.unit.isEmpty()) 0f else 3 * dp
            val unitWidth = if (p.unit.isEmpty()) 0f else text.measureText(p.unit)
            var x = cx - (figureWidth + unitGap + unitWidth) / 2f
            val baseline = cy + labelR * 0.47f
            text.typeface = bold
            text.textSize = labelR * 0.44f
            canvas.drawText(p.figure, x, baseline, text)
            if (p.unit.isNotEmpty()) {
                x += figureWidth + unitGap
                text.typeface = semibold
                text.textSize = labelR * 0.18f
                canvas.drawText(p.unit, x, baseline, text)
            }
            text.textSize = labelR * 0.11f * scale
            text.letterSpacing = 0.20f / scale
            text.color = labelMuted
            text.alpha = alpha
            canvas.drawText(p.caption, cx - text.measureText(p.caption) / 2f, cy + labelR * 0.68f, text)
        } else {
            // No figure: the caption is printed as a short phrase where the figure would sit.
            text.typeface = semibold
            text.textSize = labelR * 0.14f * scale
            text.letterSpacing = 0.16f / scale
            text.color = labelMuted
            text.alpha = alpha
            canvas.drawText(p.caption, cx - text.measureText(p.caption) / 2f, cy + labelR * 0.42f, text)
        }
    }

    /** Two soft highlights opposite each other, as light falls on a record. */
    private fun sheenShader(cx: Float, cy: Float, alpha: Int): SweepGradient {
        val key = cx * 31 + cy + alpha
        sheen?.takeIf { sheenFor == key }?.let { return it }
        val light = Color.argb(alpha, Color.red(cream), Color.green(cream), Color.blue(cream))
        val none = Color.TRANSPARENT
        return SweepGradient(
            cx, cy,
            intArrayOf(none, light, none, none, light, none, none),
            floatArrayOf(0f, 0.06f, 0.16f, 0.5f, 0.56f, 0.66f, 1f),
        ).also {
            sheen = it
            sheenFor = key
        }
    }

    private companion object {
        /** Where the groove gap starts (degrees, clockwise from 3 o'clock) and how wide it is. */
        const val GAP_START = 20f
        const val GAP_DEGREES = 38f
    }
}
