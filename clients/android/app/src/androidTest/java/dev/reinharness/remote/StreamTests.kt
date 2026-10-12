package dev.reinharness.remote

import android.content.Intent
import android.os.SystemClock
import android.util.Log
import android.view.InputDevice
import android.view.MotionEvent
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The whole way against a real server: `rein-remote serve --test-pattern 1280x720 --listen
 * 127.0.0.1:7979 --token t --mdns off` on the machine running the emulator (which the emulator
 * reaches at 10.0.2.2), and
 *
 *   ./gradlew connectedDebugAndroidTest -Pandroid.testInstrumentationRunnerArguments.rrUrl='http://10.0.2.2:7979/?token=t'
 *
 * Skipped without `rrUrl`. Also: rrCodec=HEVC (against `serve --codec h265`), rrWebTransport=off,
 * rrAudio=off, rrSeconds=N, rrDecoder=NAME (c2.android.avc.decoder: the software one); rrPairUrl and rrPairCode for pairing.
 */
@RunWith(AndroidJUnit4::class)
class StreamTests {
    private val args = InstrumentationRegistry.getArguments()
    private fun arg(k: String): String? = args.getString(k)?.takeIf { it.isNotEmpty() }
    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    private fun intent(url: String) = Intent(context, StreamActivity::class.java)
        .putExtra(StreamActivity.EXTRA_URL, url)
        .putExtra("webtransport", arg("rrWebTransport") != "off")
        .putExtra("audio", arg("rrAudio") != "off")
        .putExtra("hevc", arg("rrCodec") == "HEVC")
        .putExtra("decoder", arg("rrDecoder"))

    private fun waitFor(ms: Long, until: () -> Boolean): Boolean {
        val end = SystemClock.uptimeMillis() + ms
        while (SystemClock.uptimeMillis() < end) {
            if (until()) return true
            Thread.sleep(100)
        }
        return until()
    }

    /** The screen on and unlocked: a fresh emulator starts with the keyguard over everything. */
    @org.junit.Before
    fun wake() {
        val ui = InstrumentationRegistry.getInstrumentation().uiAutomation
        for (c in listOf("input keyevent KEYCODE_WAKEUP", "wm dismiss-keyguard", "settings put system screen_off_timeout 1800000")) {
            ui.executeShellCommand(c).close()
        }
        Thread.sleep(500)
    }

    private fun report(line: String) {
        Log.i(TAG, "android test: $line")
        println("android test: $line")
    }

    /**
     * Connects through the core, decodes with MediaCodec onto the screen, sends the pointer to three
     * places and finds the pattern's red cross at each on the screen (the input reached the server and
     * came back in the video), then measures: the self-test, its numbers in the log.
     */
    @Test
    fun streamsAndControls() {
        val url = arg("rrUrl")
        assumeTrue("set rrUrl to run this", url != null)
        val seconds = arg("rrSeconds")?.toDouble() ?: 8.0
        ActivityScenario.launch<StreamActivity>(intent(url!!).putExtra("selftest", true).putExtra("seconds", seconds)).use { s ->
            var a: StreamActivity? = null
            s.onActivity { a = it }
            val done = waitFor(((seconds + 60) * 1000).toLong()) { a!!.selfTestLine != null || a!!.failed != null }
            val line = a!!.selfTestLine ?: "no self-test line (${a!!.failed ?: "timed out"})"
            report(line)
            assertTrue(line, done && line.startsWith("selftest: PASS"))
            arg("rrCodec")?.let { want -> assertTrue(a!!.controller.m.decoder, a!!.controller.m.decoder.startsWith(want)) }
        }
    }

    /** A tap on the stream goes through Android's touch events and the gestures as a click; the cross shows where it landed. */
    @Test
    fun aTapThroughTheViewClicks() {
        val url = arg("rrUrl")
        assumeTrue("set rrUrl to run this", url != null)
        ActivityScenario.launch<StreamActivity>(intent(url!!).putExtra("measure", true)).use { s ->
            var a: StreamActivity? = null
            s.onActivity { a = it }
            val c = a!!.controller
            assertTrue("frames", waitFor(30_000) { c.m.frames >= 30 })
            val view = c.view
            val loc = IntArray(2)
            // Where to tap: in the stream's pixels at its size now (the server may have scaled it).
            val (sx, sy) = (c.m.width * 0.7).toInt() to (c.m.height * 0.7).toInt()
            var x = 0f
            var y = 0f
            s.onActivity {
                view.getLocationOnScreen(loc)
                val r = view.videoRect()
                x = loc[0] + r.left + (sx + 0.5f) * r.width() / view.streamW
                y = loc[1] + r.top + (sy + 0.5f) * r.height() / view.streamH
            }
            val before = c.crossNow(sx, sy) ?: 0.0
            val inst = InstrumentationRegistry.getInstrumentation()
            val t = SystemClock.uptimeMillis()
            // Through the input system, as a finger's touch comes (not aimed at a window).
            for ((dt, action) in listOf(0L to MotionEvent.ACTION_DOWN, 50L to MotionEvent.ACTION_UP)) {
                val ev = MotionEvent.obtain(t, t + dt, action, x, y, 0).apply { source = InputDevice.SOURCE_TOUCHSCREEN }
                assertTrue("touch injected", inst.uiAutomation.injectInputEvent(ev, true))
                ev.recycle()
            }
            val tapped = SystemClock.uptimeMillis()
            var after = 0.0
            val p = c.input?.lastPointer
            val seen = waitFor(3000) {
                after = c.crossNow(p?.first ?: sx, p?.second ?: sy) ?: 0.0
                after > Pattern.CROSS_THRESHOLD
            }
            report("tap at $sx,$sy sent as ${p?.first},${p?.second}: redness ${"%.0f".format(before)}→${"%.0f".format(after)} in ${SystemClock.uptimeMillis() - tapped} ms")
            assertTrue("the tap's pointer is where the finger was: $p", p != null && Math.abs(p.first - sx) <= 2 && Math.abs(p.second - sy) <= 2)
            assertTrue("the cross where the tap landed ($before → $after)", seen && before <= Pattern.CROSS_THRESHOLD)
        }
    }

    /** Pairing with the code a server shows (rrPairUrl: its address with no token), then signing in with keys and nothing typed. */
    @Test
    fun pairsThenSignsIn() {
        val url = arg("rrPairUrl")
        val code = arg("rrPairCode")
        assumeTrue("set rrPairUrl and rrPairCode", url != null && code != null)
        for ((i, pair) in listOf(code, null).withIndex()) {
            val x = intent(url!!).apply { if (pair != null) putExtra("pair", pair) }
            ActivityScenario.launch<StreamActivity>(x).use { s ->
                var a: StreamActivity? = null
                s.onActivity { a = it }
                val ok = waitFor(30_000) { a!!.controller.m.frames >= 10 || a!!.failed != null }
                val what = if (i == 0) "after pairing" else "after signing in with its key"
                report("frames $what: ${a!!.controller.m.frames} ${a!!.failed ?: ""}")
                assertTrue("frames $what (${a!!.failed})", ok && a!!.controller.m.frames >= 10)
            }
        }
    }

    /** The core is there, and pairings go in app-private storage that isn't backed up. */
    @Test
    fun theCoreLoads() {
        Core.setUp(context)
        assertTrue(Native.version().isNotEmpty())
        assertTrue(Native.nowUs() > 1_600_000_000_000_000L)
        assertEquals(false, Native.isPaired("nobody"))
        assertTrue(java.io.File(context.noBackupFilesDir, "rein-remote").isDirectory)
    }
}
