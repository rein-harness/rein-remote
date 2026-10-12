package dev.reinharness.remote

/**
 * The Rust core (crates/rr-jni over crates/rr-ffi, librr_jni.so): connecting, pairing, the session's
 * events and input. The protocol, the transports and pairing live there, shared with every client.
 *
 * Threads: [connect] blocks (call it off the main thread); one thread waits with [nextEvent];
 * everything else may be called from any thread. A session is a handle until [free].
 */
object Native {
    init {
        System.loadLibrary("rr_jni")
    }

    // connect's flags
    const val FLAG_WEBTRANSPORT = 1
    const val FLAG_DATAGRAMS = 2
    const val FLAG_DIRECT = 4
    const val FLAG_AUDIO = 8
    const val FLAG_GAMEPAD = 16
    const val FLAG_HARDWARE = 32

    /** How many numbers [nextEvent] fills, and their places. */
    const val FIELDS = 23
    const val F_KIND = 0
    const val F_CODEC = 1
    const val F_WIDTH = 2
    const val F_HEIGHT = 3
    const val F_FPS = 4
    const val F_BITRATE = 5
    const val F_FRAME = 6
    const val F_KEYFRAME = 7
    const val F_TIMESTAMP = 8
    const val F_RECEIVED = 9
    const val F_HAS_LATENCY = 10
    const val F_LATENCY = 11
    const val F_X = 12
    const val F_Y = 13
    const val F_VISIBLE = 14
    const val F_HAS_SHAPE = 15
    const val F_SHAPE_W = 16
    const val F_SHAPE_H = 17
    const val F_HOT_X = 18
    const val F_HOT_Y = 19
    const val F_SERVER_INPUT = 20
    const val F_SERVER_AUDIO = 21
    const val F_IS_TEXT = 22

    // Event kinds (rr_ffi.h's RR_EVENT_*)
    const val EVENT_HELLO = 1
    const val EVENT_CONFIG = 2
    const val EVENT_VIDEO = 3
    const val EVENT_CURSOR = 4
    const val EVENT_AUDIO = 5
    const val EVENT_CLIPBOARD = 6
    const val EVENT_TRANSPORT = 7
    const val EVENT_NOTE = 8
    const val EVENT_CLOSED = 9

    /** How many numbers [stats] fills. */
    const val STATS = 11

    /** Where pairings are kept (a folder only this app reads); once, before anything else. */
    external fun setHome(path: String): Boolean
    external fun version(): String
    /** Unix time in µs: the clock arrivals are stamped with. */
    external fun nowUs(): Long

    /** A session's handle; throws [java.io.IOException] with why it failed. */
    @Throws(java.io.IOException::class)
    external fun connect(target: String, name: String, pairCode: String?, flags: Int, codecs: Int, refreshMhz: Int): Long

    /** The next event's numbers into [fields] (`fields[F_KIND]` 0 when none came in [waitMs]); its bytes or text. */
    external fun nextEvent(h: Long, waitMs: Int, fields: LongArray): ByteArray?

    external fun pointerMove(h: Long, x: Int, y: Int)
    external fun pointerDelta(h: Long, dx: Int, dy: Int)
    external fun button(h: Long, button: Int, down: Boolean)
    external fun wheel(h: Long, dx: Int, dy: Int)
    external fun key(h: Long, hid: Int, down: Boolean, name: String)
    external fun text(h: Long, text: String)
    external fun touch(h: Long, ids: IntArray, xs: IntArray, ys: IntArray)
    external fun gamepad(h: Long, pad: Int, buttons: Int, lx: Int, ly: Int, rx: Int, ry: Int, lt: Int, rt: Int)
    external fun clipboard(h: Long, text: String)
    external fun resize(h: Long, width: Int, height: Int)
    external fun keyframe(h: Long)
    external fun feedback(h: Long, frame: Int, receivedUs: Long, decodedUs: Long, lost: Int)
    /** The numbers into [out] (frames, keyframes, bytes, frames_lost, fec_recovered, fec_lost, loss‰, rtt, rtt_min, has_offset, offset_us); the transport's name. */
    external fun stats(h: Long, out: DoubleArray): String
    external fun close(h: Long)
    external fun free(h: Long)

    /** Pairs with the server at [url] with the code it shows: its fingerprint. */
    @Throws(java.io.IOException::class)
    external fun pair(url: String, code: String, name: String): String
    external fun isPaired(id: String): Boolean
    /** Servers found by the core's own mDNS, as JSON (the app uses Android's NsdManager instead). */
    @Throws(java.io.IOException::class)
    external fun discover(waitMs: Int): String
}
