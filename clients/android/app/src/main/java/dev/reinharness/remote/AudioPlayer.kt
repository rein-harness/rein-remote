package dev.reinharness.remote

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Process
import android.util.Log
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * The server's sound: Opus packets (48 kHz, stereo, 10 ms) decoded by Android's own Opus decoder
 * (MediaCodec, audio/opus) and played through an AudioTrack in its low-latency mode. Sound is kept
 * close behind the picture: past [MAX_QUEUED_MS] waiting to be played, packets are dropped rather
 * than letting the sound fall behind.
 */
class AudioPlayer {
    private val queue = LinkedBlockingQueue<ByteArray>(64)
    @Volatile private var running = true
    @Volatile var packets = 0
        private set
    @Volatile var played = 0
        private set
    @Volatile var dropped = 0
        private set
    @Volatile var failed: String? = null
        private set
    @Volatile var decoder = ""
        private set
    private val thread = Thread({ run() }, "rein-remote audio").apply { start() }

    fun packet(data: ByteArray) {
        packets++
        if (data.isEmpty() || !queue.offer(data)) dropped++
    }

    private fun run() {
        Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
        val codec: MediaCodec
        val track: AudioTrack
        try {
            codec = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_OPUS)
            codec.configure(format(), null, null, 0)
            codec.start()
            decoder = codec.name
            val min = AudioTrack.getMinBufferSize(RATE, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_16BIT)
            track = AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_GAME)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE).build(),
                )
                .setAudioFormat(
                    AudioFormat.Builder().setSampleRate(RATE).setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO).build(),
                )
                .setBufferSizeInBytes(maxOf(min, RATE / 1000 * 4 * 40))
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build()
            track.play()
        } catch (e: Exception) {
            failed = "no sound: $e"
            Log.w(TAG, failed!!)
            return
        }
        val info = MediaCodec.BufferInfo()
        var written = 0L // frames
        try {
            while (running) {
                val p = queue.poll(100, TimeUnit.MILLISECONDS) ?: continue
                val inIdx = codec.dequeueInputBuffer(10_000)
                if (inIdx < 0) {
                    dropped++
                    continue
                }
                codec.getInputBuffer(inIdx)!!.apply { clear(); put(p) }
                codec.queueInputBuffer(inIdx, 0, p.size, 0, 0)
                while (true) {
                    val outIdx = codec.dequeueOutputBuffer(info, 5_000)
                    if (outIdx < 0) break
                    val out = codec.getOutputBuffer(outIdx)!!
                    // Frames not yet played: drop this one when they're more than the queue may hold.
                    val queuedMs = (written - track.playbackHeadPosition.toLong()) * 1000 / RATE
                    if (info.size > 0 && queuedMs <= MAX_QUEUED_MS) {
                        out.position(info.offset)
                        out.limit(info.offset + info.size)
                        val n = track.write(out, info.size, AudioTrack.WRITE_NON_BLOCKING)
                        if (n > 0) {
                            written += n / 4
                            played++
                        } else {
                            dropped++
                        }
                    } else if (info.size > 0) {
                        dropped++
                    }
                    codec.releaseOutputBuffer(outIdx, false)
                }
            }
        } catch (e: Exception) {
            if (running) {
                failed = "sound stopped: $e"
                Log.w(TAG, failed!!)
            }
        } finally {
            try {
                codec.stop()
            } catch (_: Exception) {
            }
            codec.release()
            track.release()
        }
    }

    fun close() {
        running = false
        thread.interrupt()
    }

    companion object {
        const val RATE = 48_000
        const val MAX_QUEUED_MS = 60L

        /**
         * MediaCodec's Opus decoder wants three buffers before the first packet: the identification
         * header (OpusHead: version 1, 2 channels, no pre-skip, 48 kHz, no gain, mapping family 0), the
         * codec delay and the seek pre-roll, each as nanoseconds.
         */
        fun format(): MediaFormat {
            val f = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_OPUS, RATE, 2)
            f.setByteBuffer("csd-0", ByteBuffer.wrap(opusHead()))
            f.setByteBuffer("csd-1", longLe(0))
            f.setByteBuffer("csd-2", longLe(80_000_000))
            return f
        }

        fun opusHead(): ByteArray {
            val b = ByteBuffer.allocate(19).order(ByteOrder.LITTLE_ENDIAN)
            b.put("OpusHead".toByteArray(Charsets.US_ASCII))
            b.put(1) // version
            b.put(2) // channels
            b.putShort(0) // pre-skip
            b.putInt(RATE)
            b.putShort(0) // output gain
            b.put(0) // channel mapping family
            return b.array()
        }

        private fun longLe(v: Long): ByteBuffer =
            ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN).putLong(v).apply { flip() }
    }
}
