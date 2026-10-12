package dev.reinharness.remote

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.Rect
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.text.InputType
import android.view.HapticFeedbackConstants
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceView
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.widget.FrameLayout
import kotlin.math.max
import kotlin.math.roundToInt

/**
 * The remote screen: the decoded video (a SurfaceView whose buffer is the stream's size, placed to fit
 * and scaled by the compositor), the remote pointer drawn here from the server's Cursor messages, and
 * every kind of input: touch gestures, a mouse or trackpad (moves, buttons, scrolling), the on-screen
 * and hardware keyboards. Gamepads are the activity's (they're not focused on a view).
 */
class StreamView(context: Context) : FrameLayout(context) {
    val video = SurfaceView(context)
    private val cursorView = CursorView(context)
    val keyboard = KeyboardView(context, this)
    var sink: InputSink? = null
        set(v) {
            field = v
            touches.sink = v
        }
    private val main = Handler(Looper.getMainLooper())
    val touches = TouchGestures(null, { ms, r ->
        val run = Runnable { r() }
        main.postDelayed(run, ms)
        ({ main.removeCallbacks(run) })
    }, slop = 10 * resources.displayMetrics.density)
    var streamW = 1280
        private set
    var streamH = 720
        private set
    private var lastMove = 0L
    private var buttons = 0

    init {
        setBackgroundColor(0xff000000.toInt())
        addView(video)
        addView(cursorView, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
        addView(keyboard, LayoutParams(1, 1))
        isFocusable = true
        isFocusableInTouchMode = true
        contentDescription = "Remote screen"
        touches.onDragStart = { performHapticFeedback(HapticFeedbackConstants.LONG_PRESS) }
        video.holder.setFixedSize(streamW, streamH)
    }

    /** The stream's size in pixels (Config). */
    fun setStreamSize(w: Int, h: Int) {
        if (w <= 0 || h <= 0 || (w == streamW && h == streamH)) return
        streamW = w
        streamH = h
        video.holder.setFixedSize(w, h)
        requestLayout()
    }

    /** Where the stream is drawn in this view (aspect fit). */
    fun videoRect(): Rect {
        val (vw, vh) = width to height
        if (vw == 0 || vh == 0) return Rect()
        val k = minOf(vw.toFloat() / streamW, vh.toFloat() / streamH)
        val (w, h) = (streamW * k).roundToInt() to (streamH * k).roundToInt()
        val x = (vw - w) / 2
        val y = (vh - h) / 2
        return Rect(x, y, x + w, y + h)
    }

    override fun onLayout(changed: Boolean, l: Int, t: Int, r: Int, b: Int) {
        val v = videoRect()
        video.layout(v.left, v.top, v.right, v.bottom)
        cursorView.layout(0, 0, r - l, b - t)
        keyboard.layout(0, b - t - 1, 1, b - t)
        cursorView.invalidate()
    }

    /** A point in the view, in the stream's pixels. */
    fun streamX(x: Float): Int {
        val r = videoRect()
        if (r.width() == 0) return 0
        return ((x - r.left) * streamW / r.width()).roundToInt().coerceIn(0, streamW - 1)
    }

    fun streamY(y: Float): Int {
        val r = videoRect()
        if (r.height() == 0) return 0
        return ((y - r.top) * streamH / r.height()).roundToInt().coerceIn(0, streamH - 1)
    }

    // ---- the pointer, drawn here ----

    fun cursor(c: RemoteEvent.Cursor) {
        c.shape?.let { cursorView.setShape(it) }
        cursorView.at = c.x to c.y
        cursorView.visible = c.visible
        cursorView.invalidate()
    }

    inner class CursorView(context: Context) : View(context) {
        private var bitmap: Bitmap? = null
        private var hot = 0 to 0
        var at = 0 to 0
        var visible = false
        /** Where the local mouse is, drawn there at once (for a quarter second after it moves). */
        var local: Triple<Float, Float, Long>? = null
        private val paint = Paint(Paint.FILTER_BITMAP_FLAG)
        private val m = Matrix()

        fun setShape(s: CursorShape) {
            if (s.width <= 0 || s.height <= 0 || s.rgba.size < s.width * s.height * 4) {
                bitmap = null
                return
            }
            val px = IntArray(s.width * s.height)
            for (i in px.indices) {
                val o = i * 4
                val r = s.rgba[o].toInt() and 0xff
                val g = s.rgba[o + 1].toInt() and 0xff
                val b = s.rgba[o + 2].toInt() and 0xff
                val a = s.rgba[o + 3].toInt() and 0xff
                px[i] = (a shl 24) or (r shl 16) or (g shl 8) or b
            }
            bitmap = Bitmap.createBitmap(px, s.width, s.height, Bitmap.Config.ARGB_8888)
            hot = s.hotX to s.hotY
        }

        override fun onDraw(canvas: Canvas) {
            val b = bitmap ?: return
            if (!visible) return
            val r = videoRect()
            if (r.width() == 0) return
            // Drawn at its size on the stream (not smaller than 0.6 of its pixels).
            val k = max(r.width().toFloat() / streamW, 0.6f)
            val l = local
            val (x, y) = if (l != null && SystemClock.uptimeMillis() - l.third < 250) {
                l.first to l.second
            } else {
                r.left + at.first * r.width().toFloat() / streamW to r.top + at.second * r.height().toFloat() / streamH
            }
            m.reset()
            m.postScale(k, k)
            m.postTranslate(x - hot.first * k, y - hot.second * k)
            canvas.drawBitmap(b, m, paint)
        }
    }

    // ---- touch, and a mouse's or trackpad's drags ----

    private fun isMouse(e: MotionEvent) =
        e.isFromSource(InputDevice.SOURCE_MOUSE) && e.getToolType(0) == MotionEvent.TOOL_TYPE_MOUSE

    override fun onTouchEvent(e: MotionEvent): Boolean {
        if (!hasFocus()) requestFocus()
        if (isMouse(e)) {
            // Moves while a button is held; the buttons themselves come as ACTION_BUTTON_*.
            if (e.actionMasked == MotionEvent.ACTION_MOVE) pointerTo(e.x, e.y)
            return true
        }
        val scale = if (videoRect().width() > 0) streamW.toFloat() / videoRect().width() else 1f
        val i = e.actionIndex
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN ->
                touches.down(e.getPointerId(i), e.getX(i), e.getY(i), streamX(e.getX(i)), streamY(e.getY(i)))
            MotionEvent.ACTION_MOVE -> for (p in 0 until e.pointerCount) {
                touches.move(e.getPointerId(p), e.getX(p), e.getY(p), streamX(e.getX(p)), streamY(e.getY(p)), scale)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP ->
                touches.up(e.getPointerId(i), streamX(e.getX(i)), streamY(e.getY(i)))
            MotionEvent.ACTION_CANCEL -> touches.cancelAll(streamX(e.x), streamY(e.y))
        }
        return true
    }

    override fun onGenericMotionEvent(e: MotionEvent): Boolean {
        if (!e.isFromSource(InputDevice.SOURCE_CLASS_POINTER)) return super.onGenericMotionEvent(e)
        when (e.actionMasked) {
            MotionEvent.ACTION_HOVER_MOVE, MotionEvent.ACTION_HOVER_ENTER -> pointerTo(e.x, e.y)
            MotionEvent.ACTION_HOVER_EXIT -> {
                cursorView.local = null
                cursorView.invalidate()
            }
            MotionEvent.ACTION_SCROLL -> {
                // A notch is 100 pixels, as browsers have it; positive is down and right.
                val dy = (-e.getAxisValue(MotionEvent.AXIS_VSCROLL) * 100).roundToInt()
                val dx = (e.getAxisValue(MotionEvent.AXIS_HSCROLL) * 100).roundToInt()
                pointerTo(e.x, e.y, force = true)
                if (dx != 0 || dy != 0) sink?.wheel(dx, dy)
            }
            MotionEvent.ACTION_BUTTON_PRESS, MotionEvent.ACTION_BUTTON_RELEASE -> {
                val b = mouseButton(e.actionButton) ?: return true
                pointerTo(e.x, e.y, force = true)
                sink?.button(b, e.actionMasked == MotionEvent.ACTION_BUTTON_PRESS)
            }
            else -> return super.onGenericMotionEvent(e)
        }
        return true
    }

    private fun mouseButton(b: Int): MouseButton? = when (b) {
        MotionEvent.BUTTON_PRIMARY -> MouseButton.LEFT
        MotionEvent.BUTTON_SECONDARY -> MouseButton.RIGHT
        MotionEvent.BUTTON_TERTIARY -> MouseButton.MIDDLE
        MotionEvent.BUTTON_BACK -> MouseButton.BACK
        MotionEvent.BUTTON_FORWARD -> MouseButton.FORWARD
        else -> null
    }

    /** The local pointer moved: drawn there at once, sent at most every 8 ms. */
    private fun pointerTo(x: Float, y: Float, force: Boolean = false) {
        cursorView.local = Triple(x, y, SystemClock.uptimeMillis())
        cursorView.invalidate()
        val now = SystemClock.uptimeMillis()
        if (!force && now - lastMove < 8) return
        lastMove = now
        sink?.pointerMove(streamX(x), streamY(y))
    }

    // ---- keyboards ----

    /** A hardware keyboard's key: to the server by name and HID usage; false when it isn't one. */
    fun key(e: KeyEvent): Boolean {
        if (e.keyCode == KeyEvent.KEYCODE_BACK || KeyEvent.isGamepadButton(e.keyCode)) return false
        if (e.action != KeyEvent.ACTION_DOWN && e.action != KeyEvent.ACTION_UP) return false
        val (name, hid) = Keys.ofEvent(e) ?: return false
        sink?.key(hid, e.action == KeyEvent.ACTION_DOWN, name)
        return true
    }

    fun toggleKeyboard() {
        val imm = context.getSystemService(InputMethodManager::class.java)
        if (keyboard.hasFocus() && keyboard.shown) {
            imm.hideSoftInputFromWindow(windowToken, 0)
            keyboard.shown = false
            requestFocus()
        } else {
            keyboard.requestFocus()
            imm.showSoftInput(keyboard, InputMethodManager.SHOW_IMPLICIT)
            keyboard.shown = true
        }
    }
}

/**
 * Takes the on-screen keyboard's typing ([ImeText]: text as text, any language or emoji, a newline
 * as Enter, delete as Backspace), with autocorrect and suggestions off.
 */
class KeyboardView(context: Context, private val owner: StreamView) : View(context) {
    val ime = ImeText { owner.sink }
    var shown = false

    init {
        isFocusable = true
        isFocusableInTouchMode = true
    }

    override fun onCheckIsTextEditor() = true

    override fun onCreateInputConnection(out: EditorInfo): InputConnection {
        out.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD or
            InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        out.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_ACTION_NONE
        return object : BaseInputConnection(this, false) {
            override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean {
                ime.commit(text.toString())
                return true
            }

            override fun setComposingText(text: CharSequence, newCursorPosition: Int): Boolean {
                ime.setComposing(text.toString())
                return true
            }

            override fun finishComposingText(): Boolean {
                ime.finishComposing()
                return true
            }

            override fun deleteSurroundingText(before: Int, after: Int): Boolean {
                if (before > 0) ime.deleteBefore(before)
                return true
            }

            override fun sendKeyEvent(e: KeyEvent): Boolean {
                // Keyboards send Enter and delete as keys.
                if (!owner.key(e)) return super.sendKeyEvent(e)
                return true
            }

            override fun performEditorAction(action: Int): Boolean {
                ime.tap("Enter", 0x28)
                return true
            }
        }
    }

    override fun dispatchKeyEvent(e: KeyEvent): Boolean = owner.key(e) || super.dispatchKeyEvent(e)
}

/**
 * Gamepads (Xbox, PlayStation, Switch, any HID pad Android knows): each one's whole state to the
 * session whenever any of it changes, numbered in the order they're first used.
 */
class Gamepads(private val send: (GamepadState) -> Unit) {
    private class Pad(val n: Int) {
        val pressed = HashSet<Int>()
        val axes = HashMap<Int, Float>()
        var last: GamepadState? = null
    }

    private val pads = LinkedHashMap<Int, Pad>()
    var lastState: GamepadState? = null
        private set
    val connected: Int get() = pads.size

    private fun pad(device: Int): Pad = pads.getOrPut(device) {
        Pad((0..255).first { n -> pads.values.none { it.n == n } })
    }

    private fun changed(p: Pad) {
        val s = GamepadState.of(p.n, p.pressed) { p.axes[it] ?: 0f }
        if (s != p.last) {
            p.last = s
            lastState = s
            send(s)
        }
    }

    /** A button; false when it isn't a gamepad's. */
    fun key(e: KeyEvent): Boolean {
        val fromPad = e.isFromSource(InputDevice.SOURCE_GAMEPAD) || e.isFromSource(InputDevice.SOURCE_JOYSTICK)
        if (!fromPad || !GamepadState.isButton(e.keyCode)) return false
        val p = pad(e.deviceId)
        if (e.action == KeyEvent.ACTION_DOWN) p.pressed.add(e.keyCode) else if (e.action == KeyEvent.ACTION_UP) p.pressed.remove(e.keyCode)
        changed(p)
        return true
    }

    /** Sticks, triggers and the hat; false when it isn't a gamepad's. */
    fun motion(e: MotionEvent): Boolean {
        if (!e.isFromSource(InputDevice.SOURCE_JOYSTICK) || e.actionMasked != MotionEvent.ACTION_MOVE) return false
        val p = pad(e.deviceId)
        val dev = e.device
        for (a in AXES) {
            var v = e.getAxisValue(a)
            // Inside the stick's dead zone: at rest.
            val flat = dev?.getMotionRange(a, e.source)?.flat ?: 0f
            if (kotlin.math.abs(v) <= flat) v = 0f
            p.axes[a] = v
        }
        changed(p)
        return true
    }

    /** A pad went away: once more at rest, so nothing stays held on the server. */
    fun removed(device: Int) {
        val p = pads.remove(device) ?: return
        send(GamepadState(pad = p.n))
    }

    companion object {
        val AXES = intArrayOf(
            MotionEvent.AXIS_X, MotionEvent.AXIS_Y, MotionEvent.AXIS_Z, MotionEvent.AXIS_RZ,
            MotionEvent.AXIS_HAT_X, MotionEvent.AXIS_HAT_Y, MotionEvent.AXIS_LTRIGGER, MotionEvent.AXIS_RTRIGGER,
            MotionEvent.AXIS_BRAKE, MotionEvent.AXIS_GAS,
        )
    }
}
