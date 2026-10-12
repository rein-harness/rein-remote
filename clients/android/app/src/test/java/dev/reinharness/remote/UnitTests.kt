package dev.reinharness.remote

import android.view.KeyEvent
import android.view.MotionEvent
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** What input sent. */
class Recorder : InputSink {
    val events = mutableListOf<String>()
    override fun pointerMove(x: Int, y: Int) {
        events += "move $x,$y"
    }

    override fun button(b: MouseButton, down: Boolean) {
        events += "${b.name.lowercase()} ${if (down) "down" else "up"}"
    }

    override fun wheel(dx: Int, dy: Int) {
        events += "wheel $dx,$dy"
    }

    override fun key(hid: Int, down: Boolean, name: String) {
        events += "key $name ${if (down) "down" else "up"}"
    }

    override fun text(t: String) {
        events += "text $t"
    }
}

/** A scheduler run by hand: the hold timer fires when the test says. */
class ManualScheduler : Scheduler {
    val pending = mutableListOf<() -> Unit>()
    override fun after(ms: Long, r: () -> Unit): () -> Unit {
        pending += r
        return { pending.remove(r) }
    }

    fun fire() = pending.toList().forEach { it() }.also { pending.clear() }
}

class TouchGestureTests {
    private val r = Recorder()
    private val clock = ManualScheduler()
    private val g = TouchGestures(r, clock, slop = 10f)

    @Test
    fun aTapClicks() {
        g.down(0, 10f, 10f, 100, 50)
        g.up(0, 101, 51)
        assertEquals(listOf("move 101,51", "left down", "left up"), r.events)
        assertEquals(TouchGestures.Mode.IDLE, g.mode)
        assertTrue("the hold timer is cancelled", clock.pending.isEmpty())
    }

    @Test
    fun aSwipeScrollsWithTheContentFollowingTheFinger() {
        g.down(0, 100f, 100f, 200, 200)
        g.move(0, 100f, 80f, 200, 160, 2f)
        g.move(0, 100f, 70f, 200, 140, 2f)
        g.up(0, 200, 140)
        // Over the slop: the pointer goes where the finger is, then moving up 20 and 10 pixels
        // scrolls down 40 and 20 stream pixels; no click.
        assertEquals(listOf("move 200,160", "wheel 0,40", "wheel 0,20"), r.events)
    }

    @Test
    fun holdThenMoveDrags() {
        g.down(0, 10f, 10f, 20, 20)
        clock.fire()
        g.move(0, 60f, 10f, 120, 20, 2f)
        g.up(0, 120, 20)
        assertEquals(listOf("move 20,20", "left down", "move 120,20", "move 120,20", "left up"), r.events)
    }

    @Test
    fun twoFingerTapRightClicksBetweenThem() {
        g.down(0, 10f, 10f, 100, 100)
        g.down(1, 30f, 10f, 140, 100)
        g.up(1, 140, 100)
        g.up(0, 100, 100)
        assertEquals(listOf("move 120,100", "right down", "right up"), r.events)
        assertEquals(TouchGestures.Mode.IDLE, g.mode)
    }

    @Test
    fun theHoldTimerDoesNothingAfterAMove() {
        g.down(0, 0f, 0f, 0, 0)
        g.move(0, 0f, 30f, 0, 30, 1f)
        g.held(0, 0)
        assertFalse(r.events.contains("left down"))
    }

    @Test
    fun aCancelledTouchClicksNothing() {
        g.down(0, 0f, 0f, 5, 5)
        g.cancelAll(5, 5)
        assertTrue(r.events.isEmpty())
        assertEquals(TouchGestures.Mode.IDLE, g.mode)
    }
}

class KeyTests {
    @Test
    fun namesAsTheWebHasThem() {
        assertEquals("Enter", Keys.name(KeyEvent.KEYCODE_ENTER, '\n'.code, '\n'.code, false))
        assertEquals("ArrowLeft", Keys.name(KeyEvent.KEYCODE_DPAD_LEFT, 0, 0, false))
        assertEquals("Backspace", Keys.name(KeyEvent.KEYCODE_DEL, 0, 0, false))
        assertEquals(" ", Keys.name(KeyEvent.KEYCODE_SPACE, ' '.code, ' '.code, false))
        assertEquals("A", Keys.name(KeyEvent.KEYCODE_A, 'A'.code, 'A'.code, false))
        // Ctrl+C is "c", as browsers give it.
        assertEquals("c", Keys.name(KeyEvent.KEYCODE_C, 0, 'c'.code, true))
        assertEquals("Shift", Keys.name(KeyEvent.KEYCODE_SHIFT_LEFT, 0, 0, false))
        assertEquals("Meta", Keys.name(KeyEvent.KEYCODE_META_RIGHT, 0, 0, false))
        assertEquals("F5", Keys.name(KeyEvent.KEYCODE_F5, 0, 0, false))
        assertEquals("é", Keys.name(KeyEvent.KEYCODE_E, 'é'.code, 'e'.code, false))
        // A volume key: neither named nor a character, left to the system.
        assertNull(Keys.name(KeyEvent.KEYCODE_VOLUME_UP, 0, 0, false))
    }

    @Test
    fun hidUsages() {
        assertEquals(0x04, Keys.hid(KeyEvent.KEYCODE_A))
        assertEquals(0x1D, Keys.hid(KeyEvent.KEYCODE_Z))
        assertEquals(0x1E, Keys.hid(KeyEvent.KEYCODE_1))
        assertEquals(0x27, Keys.hid(KeyEvent.KEYCODE_0))
        assertEquals(0x28, Keys.hid(KeyEvent.KEYCODE_ENTER))
        assertEquals(0x2A, Keys.hid(KeyEvent.KEYCODE_DEL))
        assertEquals(0x3A, Keys.hid(KeyEvent.KEYCODE_F1))
        assertEquals(0x45, Keys.hid(KeyEvent.KEYCODE_F12))
        assertEquals(0x52, Keys.hid(KeyEvent.KEYCODE_DPAD_UP))
        assertEquals(0xE0, Keys.hid(KeyEvent.KEYCODE_CTRL_LEFT))
        assertEquals(0x38, Keys.hid(KeyEvent.KEYCODE_SLASH))
        assertEquals(0, Keys.hid(KeyEvent.KEYCODE_VOLUME_UP))
    }
}

class ImeTests {
    private val r = Recorder()
    private val ime = ImeText { r }

    @Test
    fun committedTextAndNewlines() {
        ime.commit("hi\nthere")
        ime.deleteBefore(1)
        assertEquals(listOf("text hi", "key Enter down", "key Enter up", "text there", "key Backspace down", "key Backspace up"), r.events)
    }

    @Test
    fun aComposedWordSendsOnlyWhatChanged() {
        // Gboard, word by word: "h", "he", "hel", then autocorrected to "help", then committed.
        ime.setComposing("h")
        ime.setComposing("he")
        ime.setComposing("hex")
        ime.setComposing("help")
        ime.commit("help ")
        assertEquals(
            listOf("text h", "text e", "text x", "key Backspace down", "key Backspace up", "text lp", "text  "),
            r.events,
        )
        assertEquals("", ime.composing)
    }
}

class GamepadTests {
    private fun of(pressed: Set<Int> = emptySet(), axes: Map<Int, Float> = emptyMap()) =
        GamepadState.of(2, pressed) { axes[it] ?: 0f }

    @Test
    fun axesAndTriggers() {
        assertEquals(32767, GamepadState.axis(1f))
        assertEquals(-32767, GamepadState.axis(-1f))
        assertEquals(32767, GamepadState.axis(2f))
        assertEquals(0, GamepadState.axis(0f))
        assertEquals(255, GamepadState.trigger(1f))
        assertEquals(128, GamepadState.trigger(0.5f))
        assertEquals(0, GamepadState.trigger(-1f))
    }

    @Test
    fun buttonsInTheStandardOrder() {
        // The same bits the iOS app and the desktop viewer send (rr-proto's gamepad module).
        val s = of(setOf(KeyEvent.KEYCODE_BUTTON_A, KeyEvent.KEYCODE_BUTTON_START, KeyEvent.KEYCODE_BUTTON_MODE))
        assertEquals(2, s.pad)
        assertEquals((1 shl 0) or (1 shl 9) or (1 shl 16), s.buttons)
        assertEquals(1 shl 3, of(setOf(KeyEvent.KEYCODE_BUTTON_Y)).buttons)
        assertEquals(1 shl 8, of(setOf(KeyEvent.KEYCODE_BUTTON_SELECT)).buttons)
        assertEquals(1 shl 11, of(setOf(KeyEvent.KEYCODE_BUTTON_THUMBR)).buttons)
    }

    @Test
    fun theHatIsTheDpadAndAnalogTriggersPress() {
        val s = of(axes = mapOf(MotionEvent.AXIS_HAT_X to 1f, MotionEvent.AXIS_HAT_Y to -1f, MotionEvent.AXIS_RTRIGGER to 1f, MotionEvent.AXIS_BRAKE to 0.2f))
        assertEquals((1 shl 15) or (1 shl 12) or (1 shl 7), s.buttons)
        assertEquals(255, s.rt)
        assertEquals(51, s.lt)
        assertEquals((1 shl 13) or (1 shl 14), of(axes = mapOf(MotionEvent.AXIS_HAT_X to -1f, MotionEvent.AXIS_HAT_Y to 1f)).buttons)
    }

    @Test
    fun sticksKeepAndroidsDirections() {
        // Android has down as positive, as the protocol does: nothing flipped.
        val s = of(axes = mapOf(MotionEvent.AXIS_X to 0.5f, MotionEvent.AXIS_Y to -1f, MotionEvent.AXIS_Z to -1f, MotionEvent.AXIS_RZ to 1f))
        assertEquals(16384, s.lx)
        assertEquals(-32767, s.ly)
        assertEquals(-32767, s.rx)
        assertEquals(32767, s.ry)
    }
}

class AnnexBTests {
    @Test
    fun splitsOnThreeAndFourByteStartCodes() {
        val d = byteArrayOf(0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5, 0)
        val u = AnnexB.units(d)
        assertEquals(listOf(4..6, 10..11, 16..19), u)
        assertEquals(listOf(7, 8, 5), u.map { AnnexB.type(d, it, false) })
        val sets = AnnexB.parameterSets(d, false)
        assertEquals(2, sets.size)
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x67, 1, 2), sets[0])
        assertArrayEquals(byteArrayOf(0, 0, 0, 1, 0x68, 3), sets[1])
    }

    @Test
    fun hevcTypes() {
        // VPS 32, SPS 33, PPS 34, IDR_W_RADL 19: the type is bits 1-6 of the first byte.
        val d = byteArrayOf(0, 0, 1, 0x42, 1, 0, 0, 1, 0x40, 1, 0, 0, 1, 0x44, 1, 0, 0, 1, 0x26, 1)
        val types = AnnexB.units(d).map { AnnexB.type(d, it, true) }
        assertEquals(listOf(33, 32, 34, 19), types)
        // In the order a decoder wants them: VPS, SPS, PPS.
        assertEquals(listOf(0x40, 0x42, 0x44), AnnexB.parameterSets(d, true).map { it[4].toInt() })
    }
}

class PatternTests {
    /** A 1280×720 frame of grey with the test pattern's barcode for [ms] (ARGB). */
    private fun frame(ms: Long, w: Int = 1280, h: Int = 720): Pixels {
        val px = IntArray(w * h) { 0xff3c3c3c.toInt() }
        for (k in 0 until 34) {
            val on = when (k) {
                0 -> true
                1 -> false
                else -> (ms shr (33 - k)) and 1L == 1L
            }
            for (y in h - 24 until h) for (x in k * 24 until minOf(w, k * 24 + 24)) px[y * w + x] = if (on) -1 else 0xff000000.toInt()
        }
        return Pixels(px, w, h)
    }

    @Test
    fun readsTheBarcode() {
        val f = frame(0xDEADBEEFL)
        assertEquals(0xDEADBEEFL, Pattern.barcodeMs(f, 1280, 720))
        assertEquals(12L, Pattern.since(0xDEADBEEFL, 0xDEADBEEFL + 12))
        // Across the wrap of 2³² ms.
        assertEquals(5L, Pattern.since(0xFFFFFFFEL, 0x1_0000_0003L))
        // Clocks a long way apart: no number.
        assertNull(Pattern.since(0xDEADBEEFL, 0xDEADBEEFL + 20_000))
        // Too narrow for the barcode.
        assertNull(Pattern.barcodeMs(frame(5, 640, 360), 640, 360))
    }

    @Test
    fun readsTheBarcodeFromTheBottomStripAlone() {
        val f = frame(123456789L)
        val strip = Pixels(f.argb.copyOfRange(1280 * 696, 1280 * 720), 1280, 24, 0, 696)
        assertEquals(123456789L, Pattern.barcodeMs(strip, 1280, 720))
    }

    @Test
    fun findsTheCross() {
        // The pattern's background (red 64, green and blue changing) with its red cross at (300, 200).
        val (w, h) = 1280 to 720
        val px = IntArray(w * h) { i ->
            val (x, y) = i % w to i / w
            (0xff shl 24) or (64 shl 16) or ((y * 255 / h) shl 8) or (x * 255 / w)
        }
        val before = Pixels(px.copyOf(), w, h)
        for (d in 0 until 20) {
            for ((x, y) in listOf(300 + d to 200, 300 - d to 200, 300 to 200 + d, 300 to 200 - d)) px[y * w + x] = 0xffff0000.toInt()
        }
        val after = Pixels(px, w, h)
        assertTrue(Pattern.crossRedness(before, 300, 200) < Pattern.CROSS_THRESHOLD)
        assertTrue(Pattern.crossRedness(after, 300, 200) > Pattern.CROSS_THRESHOLD)
    }

    @Test
    fun windowsAndRates() {
        val win = Window(3)
        listOf(5.0, 1.0, 3.0, 4.0).forEach(win::add)
        assertEquals(3, win.count)
        // The last three: 1, 3, 4.
        assertEquals(1.0, win.pct(0.0)!!, 0.0)
        assertEquals(3.0, win.pct(0.5)!!, 0.0)
        assertEquals(4.0, win.pct(0.99)!!, 0.0)
        val fr = FrameRate()
        var n = 0
        for (i in 0 until 90) n = fr.tick(i * 16_666_667L)
        assertEquals(60, n)
    }
}

class LinkTests {
    @Test
    fun theQrCodesLink() {
        assertEquals(
            PairLink("http://192.168.1.20:7979/", "123-456", "abcd-efgh-2345-6789"),
            PairLink.parse("http://192.168.1.20:7979/#pair=123-456&id=abcd-efgh-2345-6789"),
        )
    }

    @Test
    fun otherLinksAsTheyAre() {
        assertEquals("https://box.ts.net:7979/?token=abc", PairLink.parse(" https://box.ts.net:7979/?token=abc ")?.url)
        assertNull(PairLink.parse("https://relay.example/?room=r#k=KEY")?.code)
        assertNull(PairLink.parse("not a link"))
        assertNull(PairLink.parse("ftp://x/"))
    }

    @Test
    fun aServersTxtRecord() {
        val t = ServerTxt.of(mapOf("id" to "abcd-efgh".toByteArray(), "wt" to "7979".toByteArray(), "x" to null))
        assertEquals(ServerTxt("abcd-efgh", 7979), t)
        assertEquals(ServerTxt("", null), ServerTxt.of(mapOf("wt" to "0".toByteArray())))
    }

    @Test
    fun connectFlagsAndCodecs() {
        val o = ConnectOptions(audio = false, codecs = listOf(Codec.H265, Codec.H264))
        assertEquals((1 shl 1) or (1 shl 2), o.codecBits)
        assertEquals(Native_FLAGS_ALL and 8.inv(), o.flags)
    }

    companion object {
        /** The flags rr-jni defines (crates/rr-jni FLAG_*): every one set. */
        const val Native_FLAGS_ALL = 1 or 2 or 4 or 8 or 16 or 32
    }
}

class OpusTests {
    @Test
    fun theIdentificationHeader() {
        val h = AudioPlayer.opusHead()
        assertEquals(19, h.size)
        assertEquals("OpusHead", String(h, 0, 8, Charsets.US_ASCII))
        assertEquals(1, h[8].toInt())
        assertEquals(2, h[9].toInt())
        // 48000 little-endian at 12.
        assertArrayEquals(byteArrayOf(0x80.toByte(), 0xBB.toByte(), 0, 0), h.copyOfRange(12, 16))
    }
}
