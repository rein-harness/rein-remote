package dev.reinharness.remote

import android.view.KeyEvent
import android.view.MotionEvent
import kotlin.math.hypot
import kotlin.math.roundToInt

/** Runs something later, cancellably (the main thread's Handler in the app, by hand in tests). */
fun interface Scheduler {
    fun after(ms: Long, r: () -> Unit): () -> Unit
}

/**
 * Touch, for a server that drives a mouse (as every desktop does), as the web and iOS clients do it:
 * a tap clicks, a swipe scrolls (the content follows the finger), hold then move drags, a two-finger
 * tap right-clicks. `at` is in the view's pixels (for distances), `stream` the same place in the
 * stream's pixels; `scale` is stream pixels per view pixel (for scrolling).
 */
class TouchGestures(var sink: InputSink?, private val scheduler: Scheduler, private val slop: Float = 24f) {
    enum class Mode { IDLE, MAYBE, SCROLL, DRAG, TWO, DONE }

    private class Finger(var atX: Float, var atY: Float, val startX: Float, val startY: Float, var sx: Int, var sy: Int)

    private val fingers = LinkedHashMap<Int, Finger>()
    var mode = Mode.IDLE
        private set
    private var cancelHold: (() -> Unit)? = null
    /** A hold turned into a drag (a buzz). */
    var onDragStart: (() -> Unit)? = null

    fun down(id: Int, x: Float, y: Float, sx: Int, sy: Int) {
        fingers[id] = Finger(x, y, x, y, sx, sy)
        if (fingers.size == 1) {
            mode = Mode.MAYBE
            cancelHold?.invoke()
            cancelHold = scheduler.after(HOLD_MS) { held(sx, sy) }
        } else if (fingers.size == 2 && mode == Mode.MAYBE) {
            mode = Mode.TWO
            cancelHold?.invoke()
        }
    }

    /** The hold timer fired. */
    fun held(sx: Int, sy: Int) {
        if (mode != Mode.MAYBE) return
        mode = Mode.DRAG
        sink?.pointerMove(sx, sy)
        sink?.button(MouseButton.LEFT, true)
        onDragStart?.invoke()
    }

    fun move(id: Int, x: Float, y: Float, sx: Int, sy: Int, scale: Float) {
        val f = fingers[id] ?: return
        val (px, py) = f.atX to f.atY
        f.atX = x
        f.atY = y
        f.sx = sx
        f.sy = sy
        if (mode == Mode.DRAG) {
            sink?.pointerMove(sx, sy)
            return
        }
        if (mode == Mode.MAYBE && hypot(x - f.startX, y - f.startY) > slop) {
            mode = Mode.SCROLL
            cancelHold?.invoke()
            // Scroll what's under the finger.
            sink?.pointerMove(sx, sy)
        }
        if (mode == Mode.SCROLL) {
            // Content follows the finger: moving up scrolls down.
            val dx = ((px - x) * scale).roundToInt()
            val dy = ((py - y) * scale).roundToInt()
            if (dx != 0 || dy != 0) sink?.wheel(dx, dy)
        }
    }

    fun up(id: Int, sx: Int, sy: Int, cancelled: Boolean = false) {
        val f = fingers.remove(id) ?: return
        cancelHold?.invoke()
        when {
            mode == Mode.DRAG -> {
                sink?.pointerMove(sx, sy)
                sink?.button(MouseButton.LEFT, false)
            }
            mode == Mode.MAYBE && !cancelled -> {
                sink?.pointerMove(sx, sy)
                sink?.button(MouseButton.LEFT, true)
                sink?.button(MouseButton.LEFT, false)
            }
            mode == Mode.TWO && fingers.size == 1 && !cancelled -> {
                val o = fingers.values.first()
                sink?.pointerMove((o.sx + f.sx) / 2, (o.sy + f.sy) / 2)
                sink?.button(MouseButton.RIGHT, true)
                sink?.button(MouseButton.RIGHT, false)
                mode = Mode.DONE
                return
            }
        }
        if (fingers.isEmpty()) mode = Mode.IDLE
    }

    /** Every finger gone at once (ACTION_CANCEL). */
    fun cancelAll(sx: Int, sy: Int) {
        for (id in fingers.keys.toList()) up(id, sx, sy, cancelled = true)
        mode = Mode.IDLE
    }

    companion object {
        const val HOLD_MS = 400L
    }
}

/**
 * Keys as the protocol names them (the web's `KeyboardEvent.key`: "a", "A", "Enter", "ArrowLeft",
 * "Shift", " ") with their USB HID usage (page 7), from Android's key codes.
 */
object Keys {
    private val named = mapOf(
        KeyEvent.KEYCODE_ENTER to ("Enter" to 0x28), KeyEvent.KEYCODE_NUMPAD_ENTER to ("Enter" to 0x58),
        KeyEvent.KEYCODE_ESCAPE to ("Escape" to 0x29), KeyEvent.KEYCODE_DEL to ("Backspace" to 0x2A),
        KeyEvent.KEYCODE_FORWARD_DEL to ("Delete" to 0x4C), KeyEvent.KEYCODE_TAB to ("Tab" to 0x2B),
        KeyEvent.KEYCODE_SPACE to (" " to 0x2C), KeyEvent.KEYCODE_CAPS_LOCK to ("CapsLock" to 0x39),
        KeyEvent.KEYCODE_DPAD_RIGHT to ("ArrowRight" to 0x4F), KeyEvent.KEYCODE_DPAD_LEFT to ("ArrowLeft" to 0x50),
        KeyEvent.KEYCODE_DPAD_DOWN to ("ArrowDown" to 0x51), KeyEvent.KEYCODE_DPAD_UP to ("ArrowUp" to 0x52),
        KeyEvent.KEYCODE_INSERT to ("Insert" to 0x49), KeyEvent.KEYCODE_MOVE_HOME to ("Home" to 0x4A),
        KeyEvent.KEYCODE_PAGE_UP to ("PageUp" to 0x4B), KeyEvent.KEYCODE_MOVE_END to ("End" to 0x4D),
        KeyEvent.KEYCODE_PAGE_DOWN to ("PageDown" to 0x4E),
        KeyEvent.KEYCODE_SHIFT_LEFT to ("Shift" to 0xE1), KeyEvent.KEYCODE_SHIFT_RIGHT to ("Shift" to 0xE5),
        KeyEvent.KEYCODE_CTRL_LEFT to ("Control" to 0xE0), KeyEvent.KEYCODE_CTRL_RIGHT to ("Control" to 0xE4),
        KeyEvent.KEYCODE_ALT_LEFT to ("Alt" to 0xE2), KeyEvent.KEYCODE_ALT_RIGHT to ("Alt" to 0xE6),
        KeyEvent.KEYCODE_META_LEFT to ("Meta" to 0xE3), KeyEvent.KEYCODE_META_RIGHT to ("Meta" to 0xE7),
    )

    /** The punctuation keys' HID usages (US layout positions). */
    private val punct = mapOf(
        KeyEvent.KEYCODE_MINUS to 0x2D, KeyEvent.KEYCODE_EQUALS to 0x2E, KeyEvent.KEYCODE_LEFT_BRACKET to 0x2F,
        KeyEvent.KEYCODE_RIGHT_BRACKET to 0x30, KeyEvent.KEYCODE_BACKSLASH to 0x31, KeyEvent.KEYCODE_SEMICOLON to 0x33,
        KeyEvent.KEYCODE_APOSTROPHE to 0x34, KeyEvent.KEYCODE_GRAVE to 0x35, KeyEvent.KEYCODE_COMMA to 0x36,
        KeyEvent.KEYCODE_PERIOD to 0x37, KeyEvent.KEYCODE_SLASH to 0x38,
    )

    /** The key's HID usage, 0 when it has none here. */
    fun hid(keyCode: Int): Int = when (keyCode) {
        in KeyEvent.KEYCODE_A..KeyEvent.KEYCODE_Z -> 0x04 + (keyCode - KeyEvent.KEYCODE_A)
        KeyEvent.KEYCODE_0 -> 0x27
        in KeyEvent.KEYCODE_1..KeyEvent.KEYCODE_9 -> 0x1E + (keyCode - KeyEvent.KEYCODE_1)
        in KeyEvent.KEYCODE_F1..KeyEvent.KEYCODE_F12 -> 0x3A + (keyCode - KeyEvent.KEYCODE_F1)
        else -> named[keyCode]?.second ?: punct[keyCode] ?: 0
    }

    /**
     * The key's name: a named key, else the character it types ([unicode], with the modifiers held)
     * — unshifted ([plain], without Control, Alt or Meta) while one of those is held, as browsers give
     * "c" for Ctrl+C. Null for a key that's neither (a volume key: left to the system).
     */
    fun name(keyCode: Int, unicode: Int, plain: Int, chord: Boolean): String? {
        named[keyCode]?.let { return it.first }
        if (keyCode in KeyEvent.KEYCODE_F1..KeyEvent.KEYCODE_F12) return "F${keyCode - KeyEvent.KEYCODE_F1 + 1}"
        val c = if (chord) plain else unicode
        val ch = if (c > 0 && (c and KeyCharacterMapCombining) == 0) c else plain
        return if (ch > 0 && (ch and KeyCharacterMapCombining) == 0) String(Character.toChars(ch)) else null
    }

    /** KeyCharacterMap.COMBINING_ACCENT: a dead key's flag (not a character yet). */
    private const val KeyCharacterMapCombining = -0x80000000

    fun ofEvent(e: KeyEvent): Pair<String, Int>? {
        val meta = e.metaState
        val chord = meta and (KeyEvent.META_CTRL_MASK or KeyEvent.META_ALT_MASK or KeyEvent.META_META_MASK) != 0
        val plain = e.getUnicodeChar(meta and (KeyEvent.META_CTRL_MASK or KeyEvent.META_ALT_MASK or KeyEvent.META_META_MASK).inv())
        val n = name(e.keyCode, e.unicodeChar, plain, chord) ?: return null
        return n to hid(e.keyCode)
    }
}

/** A gamepad's state as the protocol has it (rr-proto's `Input::Gamepad`: the W3C standard layout). */
data class GamepadState(
    val pad: Int = 0,
    val buttons: Int = 0,
    val lx: Int = 0,
    val ly: Int = 0,
    val rx: Int = 0,
    val ry: Int = 0,
    val lt: Int = 0,
    val rt: Int = 0,
) {
    companion object {
        /** The standard layout's order: bit i is button i (A, B, X, Y, LB, RB, LT, RT, Select, Start, L3, R3, Up, Down, Left, Right, Home). */
        val ORDER = intArrayOf(
            KeyEvent.KEYCODE_BUTTON_A, KeyEvent.KEYCODE_BUTTON_B, KeyEvent.KEYCODE_BUTTON_X, KeyEvent.KEYCODE_BUTTON_Y,
            KeyEvent.KEYCODE_BUTTON_L1, KeyEvent.KEYCODE_BUTTON_R1, KeyEvent.KEYCODE_BUTTON_L2, KeyEvent.KEYCODE_BUTTON_R2,
            KeyEvent.KEYCODE_BUTTON_SELECT, KeyEvent.KEYCODE_BUTTON_START, KeyEvent.KEYCODE_BUTTON_THUMBL,
            KeyEvent.KEYCODE_BUTTON_THUMBR, KeyEvent.KEYCODE_DPAD_UP, KeyEvent.KEYCODE_DPAD_DOWN,
            KeyEvent.KEYCODE_DPAD_LEFT, KeyEvent.KEYCODE_DPAD_RIGHT, KeyEvent.KEYCODE_BUTTON_MODE,
        )
        const val BIT_LT = 1 shl 6
        const val BIT_RT = 1 shl 7
        const val BIT_UP = 1 shl 12
        const val BIT_DOWN = 1 shl 13
        const val BIT_LEFT = 1 shl 14
        const val BIT_RIGHT = 1 shl 15

        /** Whether a key code is one of a gamepad's buttons. */
        fun isButton(keyCode: Int) = keyCode in ORDER || KeyEvent.isGamepadButton(keyCode)

        fun axis(v: Float): Int = (v.coerceIn(-1f, 1f) * 32767).roundToInt()
        fun trigger(v: Float): Int = (v.coerceIn(0f, 1f) * 255).roundToInt()

        /**
         * The state from the buttons held ([pressed], key codes) and the axes ([axis]: a MotionEvent
         * axis's value, 0 when the pad has none). Android's axes already have down as positive, as
         * the protocol does. The D-pad comes as keys on some pads and as the hat axes on others;
         * analog triggers count as pressed past half way (as the desktop viewer has it).
         */
        fun of(pad: Int, pressed: Set<Int>, axis: (Int) -> Float): GamepadState {
            var b = 0
            ORDER.forEachIndexed { i, k -> if (k in pressed) b = b or (1 shl i) }
            val hx = axis(MotionEvent.AXIS_HAT_X)
            val hy = axis(MotionEvent.AXIS_HAT_Y)
            if (hx < -0.5f) b = b or BIT_LEFT
            if (hx > 0.5f) b = b or BIT_RIGHT
            if (hy < -0.5f) b = b or BIT_UP
            if (hy > 0.5f) b = b or BIT_DOWN
            val lt = maxOf(axis(MotionEvent.AXIS_LTRIGGER), axis(MotionEvent.AXIS_BRAKE))
            val rt = maxOf(axis(MotionEvent.AXIS_RTRIGGER), axis(MotionEvent.AXIS_GAS))
            if (lt > 0.5f) b = b or BIT_LT
            if (rt > 0.5f) b = b or BIT_RT
            return GamepadState(
                pad = pad, buttons = b,
                lx = axis(axis(MotionEvent.AXIS_X)), ly = axis(axis(MotionEvent.AXIS_Y)),
                rx = axis(axis(MotionEvent.AXIS_Z)), ry = axis(axis(MotionEvent.AXIS_RZ)),
                lt = trigger(lt), rt = trigger(rt),
            )
        }
    }
}

/**
 * The on-screen keyboard's typing, as text and keys: committed text as text (a newline as Enter),
 * delete as Backspace. Keyboards that compose (Gboard, word by word) replace the word being composed
 * as it changes: only the difference is sent (Backspaces for what went, text for what came).
 */
class ImeText(private val sink: () -> InputSink?) {
    /** What's been sent of the word being composed. */
    var composing = ""
        private set

    fun setComposing(text: String) {
        replace(composing, text)
        composing = text
    }

    fun commit(text: String) {
        replace(composing, text)
        composing = ""
    }

    fun finishComposing() {
        composing = ""
    }

    fun deleteBefore(n: Int) {
        // Within the composed word first (a keyboard that deletes from it).
        val inWord = minOf(n, composing.length)
        if (inWord > 0) composing = composing.dropLast(inWord)
        repeat(n) { tap("Backspace", 0x2A) }
    }

    private fun replace(old: String, new: String) {
        var common = 0
        while (common < old.length && common < new.length && old[common] == new[common]) common++
        repeat(old.length - common) { tap("Backspace", 0x2A) }
        send(new.substring(common))
    }

    private fun send(text: String) {
        val s = sink() ?: return
        text.split('\n').forEachIndexed { i, line ->
            if (i > 0) tap("Enter", 0x28)
            if (line.isNotEmpty()) s.text(line)
        }
    }

    fun tap(name: String, hid: Int) {
        val s = sink() ?: return
        s.key(hid, true, name)
        s.key(hid, false, name)
    }
}
