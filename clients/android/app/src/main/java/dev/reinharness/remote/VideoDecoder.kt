package dev.reinharness.remote

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.Process
import android.util.Log
import android.view.Surface
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/** One decoded frame, handed to the display. */
class DecodedFrame(val frame: Int, val timestampUs: Long, val receivedUs: Long, val decodedUs: Long)

/** A decoder this device has for a codec. */
data class DecoderChoice(val name: String, val hardware: Boolean, val lowLatency: Boolean)

object Decoders {
    /** Software decoders by name, where MediaCodecInfo can't say (before Android 10). */
    private fun softwareByName(n: String) =
        n.startsWith("OMX.google.") || n.startsWith("c2.android.") || n.contains(".sw.") || n.startsWith("OMX.ffmpeg")

    /** The decoders for [mime], best first: hardware, then low latency. */
    fun list(mime: String): List<DecoderChoice> {
        val infos = MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.filter { info ->
            !info.isEncoder && info.supportedTypes.any { it.equals(mime, ignoreCase = true) }
        }
        return infos.map { info ->
            val hw = if (Build.VERSION.SDK_INT >= 29) info.isHardwareAccelerated else !softwareByName(info.name)
            val caps = info.getCapabilitiesForType(mime)
            val ll = Build.VERSION.SDK_INT >= 30 &&
                caps.isFeatureSupported(MediaCodecInfo.CodecCapabilities.FEATURE_LowLatency)
            DecoderChoice(info.name, hw, ll)
        }.sortedWith(compareByDescending<DecoderChoice> { it.hardware }.thenByDescending { it.lowLatency })
    }

    fun best(mime: String): DecoderChoice? = list(mime).firstOrNull()

    /** Whether this device decodes [mime] in hardware. */
    fun hardware(mime: String) = best(mime)?.hardware == true
}

/**
 * Decodes the stream with MediaCodec (hardware where the device has it) straight onto the screen's
 * Surface, each frame rendered the moment it's out (no B-frames: the server never sends them, so
 * frames come out in the order they go in). Low latency asked for wherever the decoder offers it
 * (KEY_LOW_LATENCY, and the vendors' own keys). On an error it asks for a keyframe and skips frames
 * until one comes, as the other clients do; a decoder that falls behind skips to a keyframe too.
 *
 * [decode] runs on the session's event thread; MediaCodec's callbacks on a thread of their own.
 */
class VideoDecoder {
    private val lock = Object()
    private var codec: MediaCodec? = null
    private var codecType: Codec = Codec.H264
    private var width = 1280
    private var height = 720
    private var sets: List<ByteArray> = emptyList()
    private var waitKey = true
    private var reset = false
    /** When a keyframe was last asked for while waiting (Unix µs). */
    private var askedUs = 0L
    /** Which decoder a callback came from (one made again leaves the old one's callbacks ignored). */
    @Volatile private var generation = 0
    private val inputs = LinkedBlockingQueue<Int>()
    private val thread = HandlerThread("rein-remote decoder", Process.THREAD_PRIORITY_URGENT_DISPLAY).apply { start() }
    private val handler = Handler(thread.looper)

    /**
     * What each frame in the decoder is (by its presentation time: the capture time, µs), and how many
     * are in it: under [meta], never [lock] (callbacks never wait for decoding or for a release).
     */
    private val meta = Object()
    private val pending = HashMap<Long, Pair<Int, Long>>()
    @Volatile private var lost = 0
    @Volatile var errors = 0
        private set
    private var inFlight = 0
    /** The decoder failed in a callback: made again at the next keyframe. */
    @Volatile private var failed = false

    /** Where frames go. Set from the UI thread as the surface comes and goes. */
    var surface: Surface? = null
        set(v) {
            synchronized(lock) {
                field = v
                release()
                waitKey = true
            }
            if (v != null) onNeedKeyframe?.invoke()
        }

    var onFrame: ((DecodedFrame) -> Unit)? = null
    var onNeedKeyframe: (() -> Unit)? = null
    /** Decoders to use (tests: a software one); null: the best. */
    var prefer: String? = null

    /** What's decoding, for the stats. */
    @Volatile var description = ""
        private set
    @Volatile var choice: DecoderChoice? = null
        private set

    /** The stream (re)started: the next frame must be a keyframe. */
    fun configure(c: Codec?, w: Int, h: Int) {
        synchronized(lock) {
            val type = c ?: Codec.H264
            if (type != codecType || w != width || h != height) reset = true
            codecType = type
            width = w
            height = h
            waitKey = true
        }
    }

    fun takeLost(): Int {
        val l = lost
        lost = 0
        return l
    }

    /** With [lock] held: [v] is dropped; the decoder starts again at a keyframe, asked for once (again, when it was one). */
    private fun skip(v: RemoteEvent.Video, why: String) {
        lost++
        if (!waitKey || v.keyframe) {
            waitKey = true
            askedUs = Native.nowUs()
            Log.i(TAG, "video: $why: skipping to a keyframe")
            onNeedKeyframe?.invoke()
        }
    }

    fun decode(v: RemoteEvent.Video) {
        val t0 = android.os.SystemClock.elapsedRealtime()
        decodeLocked(v)
        val ms = android.os.SystemClock.elapsedRealtime() - t0
        if (ms > 50) Log.i(TAG, "video: frame ${v.frame}${if (v.keyframe) " (keyframe)" else ""} took $ms ms to hand to the decoder")
    }

    private fun decodeLocked(v: RemoteEvent.Video) = synchronized(lock) {
        val s = surface
        if (s == null) {
            lost++
            waitKey = true
            return
        }
        if (failed) {
            failed = false
            waitKey = true
            reset = true
        }
        if (waitKey && !v.keyframe) {
            lost++
            // Still none a second after asking (the ask or the keyframe lost): ask again.
            val now = Native.nowUs()
            if (now - askedUs > 1_000_000) {
                askedUs = now
                onNeedKeyframe?.invoke()
            }
            return
        }
        val busy = synchronized(meta) { inFlight }
        // Frames that waited too long to get here, or a decoder that holds too many: skip to a
        // keyframe (which is always taken: it's where the decoder starts over).
        val late = Native.nowUs() - v.receivedUs > LATE_US
        if (!v.keyframe && (late || busy >= MAX_IN_FLIGHT)) {
            skip(v, if (late) "a frame ${(Native.nowUs() - v.receivedUs) / 1000} ms late" else "$busy frames in the decoder")
            return
        }
        val hevc = codecType == Codec.H265
        if (v.keyframe) {
            val ps = AnnexB.parameterSets(v.data, hevc)
            val changed = ps.isNotEmpty() && (ps.size != sets.size || ps.zip(sets).any { (a, b) -> !a.contentEquals(b) })
            if (codec == null || changed || reset) {
                if (ps.isEmpty() && codec == null) return fail("no parameter sets before the first frame")
                if (ps.isNotEmpty()) sets = ps
                reset = false
                if (!create(s)) return fail("no decoder")
            }
        }
        val c = codec ?: return fail("no decoder")
        // A keyframe waits longer: a decoder just made can take a while to offer its first buffers.
        val idx = inputs.poll(if (v.keyframe) KEYFRAME_INPUT_WAIT_MS else INPUT_WAIT_MS, TimeUnit.MILLISECONDS)
        if (idx == null) {
            skip(v, "no input buffer free")
            return
        }
        try {
            val buf = c.getInputBuffer(idx) ?: return fail("no input buffer")
            if (buf.capacity() < v.data.size) {
                c.queueInputBuffer(idx, 0, 0, v.timestampUs, 0)
                return fail("a frame of ${v.data.size} bytes, buffers of ${buf.capacity()}")
            }
            buf.clear()
            buf.put(v.data)
            synchronized(meta) {
                pending[v.timestampUs] = v.frame to v.receivedUs
                if (pending.size > 64) pending.remove(pending.keys.min())
                inFlight++
            }
            waitKey = false
            c.queueInputBuffer(idx, 0, v.data.size, v.timestampUs, if (v.keyframe) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0)
        } catch (e: Exception) {
            fail("queueing: $e")
        }
    }

    /** With [lock] held: start over at the next keyframe, with a new decoder. */
    private fun fail(why: String) {
        errors++
        lost++
        Log.w(TAG, "video: $why; asking for a keyframe")
        waitKey = true
        reset = true
        onNeedKeyframe?.invoke()
    }

    /** With [lock] held. */
    private fun create(s: Surface): Boolean {
        release()
        val mime = codecType.mime
        val choices = Decoders.list(mime)
        val pick = prefer?.let { p -> choices.firstOrNull { it.name == p } } ?: choices.firstOrNull() ?: return false
        val gen = ++generation
        return try {
            val c = MediaCodec.createByCodecName(pick.name)
            val f = MediaFormat.createVideoFormat(mime, width, height)
            if (codecType == Codec.H265) {
                f.setByteBuffer("csd-0", java.nio.ByteBuffer.wrap(sets.fold(ByteArray(0)) { a, b -> a + b }))
            } else {
                sets.getOrNull(0)?.let { f.setByteBuffer("csd-0", java.nio.ByteBuffer.wrap(it)) }
                sets.getOrNull(1)?.let { f.setByteBuffer("csd-1", java.nio.ByteBuffer.wrap(it)) }
            }
            f.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, maxOf(width * height * 3 / 2, 1 shl 20))
            f.setInteger(MediaFormat.KEY_PRIORITY, 0) // real time
            if (Build.VERSION.SDK_INT >= 30) f.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            // The vendors' own low-latency modes (as Moonlight sets them), for decoders from before
            // KEY_LOW_LATENCY: Qualcomm and Samsung (Exynos).
            val n = pick.name.lowercase()
            if (n.contains("qti") || n.contains("qcom")) {
                f.setInteger("vendor.qti-ext-dec-low-latency.enable", 1)
                f.setInteger("vendor.qti-ext-dec-picture-order.enable", 1)
            }
            if (n.contains("exynos")) f.setInteger("vendor.rtc-ext-dec-low-latency.enable", 1)
            inputs.clear()
            synchronized(meta) {
                pending.clear()
                inFlight = 0
            }
            c.setCallback(Callback(gen), handler)
            c.configure(f, s, null, 0)
            c.start()
            codec = c
            choice = pick
            description = "${codecType.label} ${width}×$height ${pick.name} (${if (pick.hardware) "hardware" else "software"}" +
                "${if (pick.lowLatency) ", low latency" else ""})"
            Log.i(TAG, "video: $description")
            true
        } catch (e: Exception) {
            Log.e(TAG, "video: ${pick.name} wouldn't start", e)
            false
        }
    }

    /** With [lock] held. */
    private fun release() {
        val c = codec ?: return
        codec = null
        generation++
        inputs.clear()
        try {
            c.stop()
        } catch (_: Exception) {
        }
        c.release()
    }

    private inner class Callback(val gen: Int) : MediaCodec.Callback() {
        override fun onInputBufferAvailable(c: MediaCodec, index: Int) {
            if (gen == generation) inputs.offer(index)
        }

        override fun onOutputBufferAvailable(c: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
            val decodedUs = Native.nowUs()
            if (gen != generation) return
            val m: Pair<Int, Long>?
            synchronized(meta) {
                inFlight = maxOf(0, inFlight - 1)
                m = pending.remove(info.presentationTimeUs)
            }
            try {
                // On the screen now: no waiting for a presentation time.
                c.releaseOutputBuffer(index, true)
            } catch (e: IllegalStateException) {
                return
            }
            if (m != null) onFrame?.invoke(DecodedFrame(m.first, info.presentationTimeUs, m.second, decodedUs))
        }

        override fun onError(c: MediaCodec, e: MediaCodec.CodecException) {
            Log.w(TAG, "video: decoder error ${e.diagnosticInfo}")
            if (gen != generation) return
            errors++
            // The decoding thread starts over at the next keyframe.
            failed = true
            onNeedKeyframe?.invoke()
        }

        override fun onOutputFormatChanged(c: MediaCodec, format: MediaFormat) {
            Log.i(TAG, "video: output $format")
        }
    }

    fun close() {
        synchronized(lock) { release() }
        thread.quitSafely()
    }

    companion object {
        const val MAX_IN_FLIGHT = 4
        const val LATE_US = 100_000L
        const val INPUT_WAIT_MS = 30L
        const val KEYFRAME_INPUT_WAIT_MS = 500L
    }
}
