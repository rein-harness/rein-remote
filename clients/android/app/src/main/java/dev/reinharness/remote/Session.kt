package dev.reinharness.remote

import android.util.Log
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.locks.ReentrantReadWriteLock
import kotlin.concurrent.read
import kotlin.concurrent.write

const val TAG = "rein-remote"

enum class Codec(val id: Int, val mime: String, val label: String) {
    H264(1, "video/avc", "H.264"),
    H265(2, "video/hevc", "HEVC");

    companion object {
        fun of(id: Int): Codec? = entries.firstOrNull { it.id == id }
    }
}

/** What comes from the server. */
sealed class RemoteEvent {
    data class Hello(val name: String, val input: Int, val audio: Boolean) : RemoteEvent()
    data class Config(val codec: Codec?, val width: Int, val height: Int, val fps: Int, val kbps: Int) : RemoteEvent()
    class Video(
        val frame: Int, val keyframe: Boolean, val timestampUs: Long, val receivedUs: Long,
        val latencyUs: Long?, val data: ByteArray,
    ) : RemoteEvent()
    class Cursor(val x: Int, val y: Int, val visible: Boolean, val shape: CursorShape?) : RemoteEvent()
    class Audio(val timestampUs: Long, val data: ByteArray) : RemoteEvent()
    data class Clipboard(val text: String) : RemoteEvent()
    data class Transport(val name: String) : RemoteEvent()
    data class Note(val text: String) : RemoteEvent()
    data class Closed(val why: String?) : RemoteEvent()

    companion object {
        /** An event from [Native.nextEvent]'s numbers and bytes (null: none came). */
        fun of(f: LongArray, bytes: ByteArray?): RemoteEvent? {
            val text = if (f[Native.F_IS_TEXT] != 0L && bytes != null) String(bytes, Charsets.UTF_8) else null
            val data = bytes ?: ByteArray(0)
            return when (f[Native.F_KIND].toInt()) {
                0 -> null
                Native.EVENT_HELLO -> Hello(text ?: "", f[Native.F_SERVER_INPUT].toInt(), f[Native.F_SERVER_AUDIO] != 0L)
                Native.EVENT_CONFIG -> Config(
                    Codec.of(f[Native.F_CODEC].toInt()), f[Native.F_WIDTH].toInt(), f[Native.F_HEIGHT].toInt(),
                    f[Native.F_FPS].toInt(), f[Native.F_BITRATE].toInt(),
                )
                Native.EVENT_VIDEO -> Video(
                    f[Native.F_FRAME].toInt(), f[Native.F_KEYFRAME] != 0L, f[Native.F_TIMESTAMP], f[Native.F_RECEIVED],
                    if (f[Native.F_HAS_LATENCY] != 0L) f[Native.F_LATENCY] else null, data,
                )
                Native.EVENT_CURSOR -> Cursor(
                    f[Native.F_X].toInt(), f[Native.F_Y].toInt(), f[Native.F_VISIBLE] != 0L,
                    if (f[Native.F_HAS_SHAPE] != 0L) CursorShape(
                        f[Native.F_SHAPE_W].toInt(), f[Native.F_SHAPE_H].toInt(), f[Native.F_HOT_X].toInt(),
                        f[Native.F_HOT_Y].toInt(), data,
                    ) else null,
                )
                Native.EVENT_AUDIO -> Audio(f[Native.F_TIMESTAMP], data)
                Native.EVENT_CLIPBOARD -> Clipboard(text ?: "")
                Native.EVENT_TRANSPORT -> Transport(text ?: "")
                Native.EVENT_NOTE -> Note(text ?: "")
                else -> Closed(text)
            }
        }
    }
}

class CursorShape(val width: Int, val height: Int, val hotX: Int, val hotY: Int, val rgba: ByteArray)

data class RemoteStats(
    val frames: Long = 0,
    val keyframes: Long = 0,
    val framesLost: Long = 0,
    val fecRecovered: Long = 0,
    val rttMs: Double = 0.0,
    /** The server's clock against this one: add to a server time for this clock's (µs). */
    val clockOffsetUs: Long? = null,
    val transport: String = "",
)

data class ConnectOptions(
    val webtransport: Boolean = true,
    val datagrams: Boolean = true,
    val direct: Boolean = true,
    val audio: Boolean = true,
    val gamepad: Boolean = true,
    val codecs: List<Codec> = listOf(Codec.H264),
    val hardware: Boolean = true,
    val refreshMhz: Int = 60_000,
) {
    val flags: Int
        get() = (if (webtransport) Native.FLAG_WEBTRANSPORT else 0) or
            (if (datagrams) Native.FLAG_DATAGRAMS else 0) or
            (if (direct) Native.FLAG_DIRECT else 0) or
            (if (audio) Native.FLAG_AUDIO else 0) or
            (if (gamepad) Native.FLAG_GAMEPAD else 0) or
            (if (hardware) Native.FLAG_HARDWARE else 0)
    val codecBits: Int get() = codecs.fold(0) { b, c -> b or (1 shl c.id) }
}

enum class MouseButton(val id: Int) { LEFT(0), RIGHT(1), MIDDLE(2), BACK(3), FORWARD(4) }

/** Where input goes: the session, or a test's recorder. */
interface InputSink {
    fun pointerMove(x: Int, y: Int)
    fun button(b: MouseButton, down: Boolean)
    fun wheel(dx: Int, dy: Int)
    fun key(hid: Int, down: Boolean, name: String)
    fun text(t: String)
}

/**
 * One session with a server: connects (blocking: off the main thread), then events on a thread of its
 * own until [RemoteEvent.Closed]; input from any thread. The handle is freed once the event thread has
 * seen the end and [close] was called, so no call can still be running on it.
 */
class RemoteSession(target: String, name: String, pairCode: String?, o: ConnectOptions) : InputSink {
    private val handle: Long = Native.connect(target, name, pairCode, o.flags, o.codecBits, o.refreshMhz)
    private val closed = AtomicBoolean(false)
    /** Calls hold it to read; freeing the handle holds it to write. */
    private val rw = ReentrantReadWriteLock()
    private var freed = false
    private var ended = false
    private var thread: Thread? = null

    fun start(onEvent: (RemoteEvent) -> Unit) {
        val t = Thread({
            val f = LongArray(Native.FIELDS)
            while (true) {
                val bytes = Native.nextEvent(handle, 250, f)
                val ev = RemoteEvent.of(f, bytes) ?: continue
                try {
                    onEvent(ev)
                } catch (e: Throwable) {
                    Log.e(TAG, "event handler", e)
                }
                if (ev is RemoteEvent.Closed) break
            }
            synchronized(this) {
                ended = true
                if (closed.get()) free()
            }
        }, "rein-remote events")
        t.priority = Thread.MAX_PRIORITY
        thread = t
        t.start()
    }

    private inline fun live(f: (Long) -> Unit) {
        rw.read { if (!freed && !closed.get()) f(handle) }
    }

    private fun free() = rw.write {
        if (!freed) {
            freed = true
            Native.free(handle)
        }
    }

    override fun pointerMove(x: Int, y: Int) = live { Native.pointerMove(it, x, y) }
    override fun button(b: MouseButton, down: Boolean) = live { Native.button(it, b.id, down) }
    override fun wheel(dx: Int, dy: Int) = live { Native.wheel(it, dx, dy) }
    override fun key(hid: Int, down: Boolean, name: String) = live { Native.key(it, hid, down, name) }
    override fun text(t: String) = live { Native.text(it, t) }
    fun gamepad(g: GamepadState) = live { Native.gamepad(it, g.pad, g.buttons, g.lx, g.ly, g.rx, g.ry, g.lt, g.rt) }
    fun clipboard(t: String) = live { Native.clipboard(it, t) }
    fun keyframe() = live { Native.keyframe(it) }
    fun feedback(frame: Int, receivedUs: Long, decodedUs: Long, lost: Int) =
        live { Native.feedback(it, frame, receivedUs, decodedUs, lost) }

    val stats: RemoteStats
        get() {
            val v = DoubleArray(Native.STATS)
            var t = ""
            rw.read { if (!freed) t = Native.stats(handle, v) }
            return RemoteStats(
                frames = v[0].toLong(), keyframes = v[1].toLong(), framesLost = v[3].toLong(),
                fecRecovered = v[4].toLong(), rttMs = v[7], clockOffsetUs = if (v[9] != 0.0) v[10].toLong() else null,
                transport = t,
            )
        }

    /** Ends the session; the handle is freed when the event thread is done with it (now, if it is). */
    fun close() {
        synchronized(this) {
            if (closed.getAndSet(true)) return
            rw.read { if (!freed) Native.close(handle) }
            if (thread == null || ended) free()
        }
    }
}
