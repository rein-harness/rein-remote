package dev.reinharness.remote

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Rect
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.view.PixelCopy
import android.view.SurfaceHolder

/** What the stream measured, for the overlay, the self-test and the tests. */
class StreamMetrics {
    @Volatile var frames = 0
    @Volatile var fps = 0
    @Volatile var keyframeAsks = 0
    /**
     * Glass to glass (ms) from the test pattern's barcode: its capture time to the frame that carries
     * it being handed to the display (read back from the screen's own buffer).
     */
    val glass = Window(2000)
    /** The same to when the screen's buffer was seen holding it (an upper bound: includes the copy). */
    val glassSeen = Window(2000)
    /** Arrival to decoded (ms). */
    val decode = Window(2000)
    /** Capture to arrival (ms), the core's estimate from its pings. */
    val network = Window(2000)
    @Volatile var barcodeMisses = 0
    @Volatile var audioPackets = 0
    @Volatile var audioDecoded = 0
    @Volatile var audioHeadMs = 0L
    @Volatile var audioPlayed = 0
    @Volatile var audioDropped = 0
    @Volatile var decoder = ""
    @Volatile var codec = ""
    @Volatile var width = 0
    @Volatile var height = 0
    @Volatile var transport = ""
    @Volatile var rttMs = 0.0
    @Volatile var framesLost = 0L
    @Volatile var fecRecovered = 0L
    @Volatile var errors = 0
}

/** Input on its way to the session, the pointer's last place noted (measuring: the pattern draws a cross there). */
class InputTap(private val s: RemoteSession) : InputSink {
    @Volatile var lastPointer: Pair<Int, Int>? = null
        private set

    override fun pointerMove(x: Int, y: Int) {
        lastPointer = x to y
        s.pointerMove(x, y)
    }

    override fun button(b: MouseButton, down: Boolean) = s.button(b, down)
    override fun wheel(dx: Int, dy: Int) = s.wheel(dx, dy)
    override fun key(hid: Int, down: Boolean, name: String) = s.key(hid, down, name)
    override fun text(t: String) = s.text(t)
}

/**
 * One stream: the session, decoding, sound, gamepads, and what's measured. [connect] blocks (call
 * it off the main thread); the view's surface is handed to the decoder as it comes and goes.
 */
class StreamController(private val context: Context, val view: StreamView) : SurfaceHolder.Callback {
    val m = StreamMetrics()
    @Volatile var session: RemoteSession? = null
        private set
    @Volatile var input: InputTap? = null
        private set
    val decoder = VideoDecoder()
    private var audio: AudioPlayer? = null
    val gamepads = Gamepads { g -> if (serverInput and 8 != 0) session?.gamepad(g) }
    private val rate = FrameRate()
    private val main = Handler(Looper.getMainLooper())
    @Volatile var serverInput = 0
        private set
    @Volatile var serverName = ""
        private set
    @Volatile var state = "connecting"
        private set
    @Volatile private var clockOffsetUs: Long? = null
    /** Read the test pattern back from the screen: glass to glass, the pointer's cross. */
    @Volatile var measure = false
    /** Measuring: what was last seen at the pointer (the cross's redness), and when. */
    @Volatile var crossRedness = 0.0
        private set
    @Volatile var crossAt: Pair<Int, Int>? = null
        private set
    var onState: (() -> Unit)? = null

    // Measuring: frames handed to the display, by capture time (server ms, mod 2³²) → when (Unix µs).
    private val handed = LinkedHashMap<Long, Long>()
    private val measureThread = HandlerThread("rein-remote measure").apply { start() }
    private val measureHandler = Handler(measureThread.looper)
    @Volatile private var copying = false
    private var stripBitmap: Bitmap? = null
    private var crossBitmap: Bitmap? = null

    init {
        view.video.holder.addCallback(this)
        decoder.onFrame = { f -> decoded(f) }
        decoder.onNeedKeyframe = {
            m.keyframeAsks++
            session?.keyframe()
        }
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        decoder.surface = holder.surface
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        decoder.surface = null
    }

    /** Connects and starts streaming (blocking: off the main thread). */
    fun connect(target: String, pairCode: String?, options: ConnectOptions) {
        val o = options
        val s = RemoteSession(target, Build.MODEL ?: "Android", pairCode, o)
        session = s
        val tap = InputTap(s)
        input = tap
        if (o.audio) audio = AudioPlayer()
        main.post {
            view.sink = tap
            state = "connected"
            onState?.invoke()
            main.postDelayed(::tick, 500)
        }
        s.start(::event)
    }

    fun close() {
        main.removeCallbacksAndMessages(null)
        session?.close()
        audio?.close()
        decoder.close()
        measureThread.quitSafely()
    }

    private fun event(ev: RemoteEvent) {
        when (ev) {
            is RemoteEvent.Hello -> {
                serverInput = ev.input
                serverName = ev.name
                main.post { onState?.invoke() }
            }
            is RemoteEvent.Config -> {
                decoder.configure(ev.codec, ev.width, ev.height)
                m.codec = ev.codec?.label ?: "?"
                m.width = ev.width
                m.height = ev.height
                main.post {
                    view.setStreamSize(ev.width, ev.height)
                    state = "streaming"
                    onState?.invoke()
                }
            }
            is RemoteEvent.Video -> {
                ev.latencyUs?.let { m.network.add(it / 1000.0) }
                decoder.decode(ev)
            }
            is RemoteEvent.Cursor -> main.post { view.cursor(ev) }
            is RemoteEvent.Audio -> {
                m.audioPackets++
                audio?.packet(ev.data)
            }
            is RemoteEvent.Clipboard -> main.post {
                context.getSystemService(ClipboardManager::class.java)
                    ?.setPrimaryClip(ClipData.newPlainText("Rein Remote", ev.text))
            }
            is RemoteEvent.Transport -> Log.i(TAG, "now over ${ev.name}")
            is RemoteEvent.Note -> Log.i(TAG, ev.text)
            is RemoteEvent.Closed -> {
                Log.i(TAG, "closed (${ev.why ?: "by the server"})")
                state = ev.why?.let { "closed: $it" } ?: "closed"
                main.post { onState?.invoke() }
            }
        }
    }

    /** A frame out of the decoder and on its way to the screen (the decoder's thread). */
    private fun decoded(f: DecodedFrame) {
        session?.feedback(f.frame, f.receivedUs, f.decodedUs, decoder.takeLost())
        m.frames++
        m.fps = rate.tick(SystemClock.elapsedRealtimeNanos())
        m.decode.add((f.decodedUs - f.receivedUs) / 1000.0)
        if (measure) {
            synchronized(handed) {
                handed[(f.timestampUs / 1000) and 0xffffffffL] = f.decodedUs
                while (handed.size > 120) handed.remove(handed.keys.first())
            }
            if (!copying && m.frames % 2 == 0) {
                copying = true
                // After the frame has had a moment to reach the surface.
                measureHandler.postDelayed(::copy, 4)
            }
        }
    }

    /** Copies the screen's buffer back: the barcode's strip, and the place the pointer was sent. */
    private fun copy() {
        val (w, h) = m.width to m.height
        if (w < Pattern.BLOCKS.last() * 34 || h < Pattern.BLOCK || Build.VERSION.SDK_INT < 24 || !view.video.holder.surface.isValid) {
            copying = false
            return
        }
        val strip = stripBitmap?.takeIf { it.width == w } ?: Bitmap.createBitmap(w, Pattern.BLOCK, Bitmap.Config.ARGB_8888)
        stripBitmap = strip
        try {
            PixelCopy.request(view.video, Rect(0, h - Pattern.BLOCK, w, h), strip, { r ->
                val seenUs = Native.nowUs()
                if (r == PixelCopy.SUCCESS) readStrip(strip, w, h, seenUs)
                copyCross(w, h)
            }, measureHandler)
        } catch (e: Exception) {
            Log.w(TAG, "measure: $e")
            copying = false
        }
    }

    private fun readStrip(b: Bitmap, w: Int, h: Int, seenUs: Long) {
        val off = clockOffsetUs ?: return
        val px = IntArray(w * Pattern.BLOCK)
        b.getPixels(px, 0, w, 0, 0, w, Pattern.BLOCK)
        val serverSeenMs = (seenUs - off) / 1000
        val (ms, _) = Pattern.readBarcode(Pixels(px, w, Pattern.BLOCK, 0, h - Pattern.BLOCK), w, h, serverSeenMs) ?: run {
            m.barcodeMisses++
            return
        }
        // On the server's clock: when it was handed to the display, and when it was seen there.
        val handedUs = synchronized(handed) { handed[ms] ?: handed[(ms + 1) and 0xffffffffL] ?: handed[(ms - 1) and 0xffffffffL] }
        if (handedUs != null) Pattern.since(ms, (handedUs - off) / 1000)?.let { m.glass.add(it.toDouble()) }
        else m.barcodeMisses++
        Pattern.since(ms, serverSeenMs)?.let { m.glassSeen.add(it.toDouble()) }
    }

    private fun copyCross(w: Int, h: Int) {
        val p = input?.lastPointer
        if (p == null || m.frames % 6 != 0) {
            copying = false
            return
        }
        val (x, y) = p
        val r = Rect((x - 4).coerceAtLeast(0), (y - 4).coerceAtLeast(0), (x + 28).coerceAtMost(w), (y + 28).coerceAtMost(h))
        if (r.width() <= 0 || r.height() <= 0) {
            copying = false
            return
        }
        val b = Bitmap.createBitmap(r.width(), r.height(), Bitmap.Config.ARGB_8888)
        crossBitmap = b
        try {
            PixelCopy.request(view.video, r, b, { res ->
                if (res == PixelCopy.SUCCESS) {
                    val px = IntArray(r.width() * r.height())
                    b.getPixels(px, 0, r.width(), 0, 0, r.width(), r.height())
                    crossRedness = Pattern.crossRedness(Pixels(px, r.width(), r.height(), r.left, r.top), x, y)
                    crossAt = p
                }
                copying = false
            }, measureHandler)
        } catch (e: Exception) {
            copying = false
        }
    }

    /** Asks for the redness at (x, y) now (measuring), waiting for it. */
    fun crossNow(x: Int, y: Int, timeoutMs: Long = 1000): Double? {
        val (w, h) = m.width to m.height
        val r = Rect((x - 4).coerceAtLeast(0), (y - 4).coerceAtLeast(0), (x + 28).coerceAtMost(w), (y + 28).coerceAtMost(h))
        if (r.width() <= 0 || r.height() <= 0 || Build.VERSION.SDK_INT < 24) return null
        val b = Bitmap.createBitmap(r.width(), r.height(), Bitmap.Config.ARGB_8888)
        val done = java.util.concurrent.CountDownLatch(1)
        var out: Double? = null
        try {
            PixelCopy.request(view.video, r, b, { res ->
                if (res == PixelCopy.SUCCESS) {
                    val px = IntArray(r.width() * r.height())
                    b.getPixels(px, 0, r.width(), 0, 0, r.width(), r.height())
                    out = Pattern.crossRedness(Pixels(px, r.width(), r.height(), r.left, r.top), x, y)
                }
                done.countDown()
            }, measureHandler)
        } catch (e: Exception) {
            return null
        }
        done.await(timeoutMs, java.util.concurrent.TimeUnit.MILLISECONDS)
        return out
    }

    private fun tick() {
        val s = session ?: return
        val st = s.stats
        clockOffsetUs = st.clockOffsetUs
        m.transport = st.transport
        m.rttMs = st.rttMs
        m.framesLost = st.framesLost
        m.fecRecovered = st.fecRecovered
        m.decoder = decoder.description
        m.errors = decoder.errors
        audio?.let {
            m.audioDecoded = it.decoded
            m.audioHeadMs = it.headMs
            m.audioPlayed = it.played
            m.audioDropped = it.dropped
        }
        onState?.invoke()
        main.postDelayed(::tick, 500)
    }

    /** The overlay's lines. */
    fun overlay(): String {
        val lines = mutableListOf(
            "${m.decoder.ifEmpty { m.codec }}\n${m.fps} fps · ${m.transport} · rtt ${"%.0f".format(m.rttMs)} ms" +
                " · decode ${m.decode.pct(0.5)?.let { "%.1f".format(it) } ?: "-"} ms",
        )
        val g = m.glass.pct(0.5)
        val g95 = m.glass.pct(0.95)
        if (g != null && g95 != null) lines.add("glass to glass ${"%.0f".format(g)} ms (p95 ${"%.0f".format(g95)})")
        m.network.pct(0.5)?.let { lines.add("capture to arrival ${"%.0f".format(it)} ms") }
        if (audio != null) lines.add("sound: ${m.audioDecoded} decoded, ${m.audioPlayed} played, ${m.audioDropped} dropped${audio?.failed?.let { " ($it)" } ?: ""}")
        if (gamepads.connected > 0) {
            val p = gamepads.lastState ?: GamepadState()
            val sent = if (serverInput and 8 != 0) "sent" else "not sent: the server takes no gamepads"
            lines.add("gamepads: ${gamepads.connected} · buttons 0x${Integer.toHexString(p.buttons)} · left ${p.lx},${p.ly} · $sent")
        }
        return lines.joinToString("\n")
    }
}

/**
 * The self-test (`--ez selftest true`, and the instrumented test): with `rein-remote serve
 * --test-pattern`, checks the whole way and measures it. It waits for frames, sends the pointer to
 * three places and looks for the pattern's cross at each on the screen (input that reached the
 * server, seen coming back), then measures for a while: frames a second, glass to glass from the
 * barcode, decoding, sound. One line out, on the screen and in the log ("rein-remote selftest: …").
 */
object SelfTest {
    private fun waitFor(ms: Long, until: () -> Boolean): Boolean {
        val end = SystemClock.uptimeMillis() + ms
        while (SystemClock.uptimeMillis() < end) {
            if (until()) return true
            Thread.sleep(50)
        }
        return until()
    }

    fun run(c: StreamController, seconds: Double): String {
        c.measure = true
        if (!waitFor(20_000) { c.m.frames >= 30 } || c.input == null) {
            return report("FAIL no video (${c.m.frames} frames, ${c.state})", emptyMap())
        }
        val tap = c.input!!
        val cross = mutableListOf<String>()
        var crossOk = true
        for ((fx, fy) in listOf(0.25 to 0.3, 0.75 to 0.6, 0.5 to 0.45)) {
            // At the stream's size now (the server may have scaled the picture since).
            val (x, y) = (c.m.width * fx).toInt() to (c.m.height * fy).toInt()
            val before = c.crossNow(x, y) ?: 0.0
            val sent = SystemClock.uptimeMillis()
            tap.pointerMove(x, y)
            var after = 0.0
            val seen = waitFor(3000) {
                after = c.crossNow(x, y) ?: 0.0
                after > Pattern.CROSS_THRESHOLD
            }
            val ms = SystemClock.uptimeMillis() - sent
            cross.add("$x,$y: ${"%.0f".format(before)}→${"%.0f".format(after)}${if (seen) " in $ms ms" else " NOT SEEN"}")
            crossOk = crossOk && seen && before <= Pattern.CROSS_THRESHOLD
        }
        val f0 = c.m.frames
        val t0 = SystemClock.uptimeMillis()
        Thread.sleep((seconds * 1000).toLong())
        val m = c.m
        val fps = (m.frames - f0) * 1000.0 / (SystemClock.uptimeMillis() - t0)
        fun f(v: Double?) = v?.let { "%.1f".format(it) } ?: "-"
        val fields = mapOf(
            "codec" to m.decoder.replace(' ', '_'),
            "transport" to m.transport,
            "frames" to "${m.frames}",
            "fps" to "%.1f".format(fps),
            "glass_ms_p50" to f(m.glass.pct(0.5)),
            "glass_ms_p95" to f(m.glass.pct(0.95)),
            "glass_samples" to "${m.glass.count}",
            "glass_seen_ms_p50" to f(m.glassSeen.pct(0.5)),
            "glass_seen_ms_p95" to f(m.glassSeen.pct(0.95)),
            "barcode_misses" to "${m.barcodeMisses}",
            "decode_ms_p50" to f(m.decode.pct(0.5)),
            "decode_ms_p95" to f(m.decode.pct(0.95)),
            "network_ms_p50" to f(m.network.pct(0.5)),
            "rtt_ms" to "%.1f".format(m.rttMs),
            "keyframe_asks" to "${m.keyframeAsks}",
            "frames_lost" to "${m.framesLost}",
            "decoder_errors" to "${m.errors}",
            "audio_packets" to "${m.audioPackets}",
            "audio_decoded" to "${m.audioDecoded}",
            "audio_played_ms" to "${m.audioHeadMs}",
            "audio_played" to "${m.audioPlayed}",
            "audio_dropped" to "${m.audioDropped}",
            "cross" to cross.joinToString("; "),
        )
        val ok = crossOk && fps > 5 && m.glass.count > 0
        return report(if (ok) "PASS" else "FAIL", fields)
    }

    private fun report(verdict: String, fields: Map<String, String>): String {
        val line = "selftest: $verdict " + fields.keys.sorted().joinToString(" ") { "$it=${fields[it]}" }
        Log.i(TAG, line)
        return line
    }
}
