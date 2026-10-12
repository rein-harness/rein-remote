package dev.reinharness.remote

/**
 * H.264 and HEVC in Annex B (start codes, parameter sets in band before each keyframe), as the server
 * sends them. MediaCodec takes Annex B as it is; the parameter sets are also what the decoder is
 * configured with (csd-0, csd-1), and a change in them means a new stream (a new size).
 */
object AnnexB {
    /** Where each NAL unit is in [data] (without its start code): start, end (exclusive). */
    fun units(data: ByteArray): List<IntRange> {
        val out = ArrayList<IntRange>()
        val n = data.size
        var i = 0
        var start = -1
        while (i + 2 < n) {
            if (data[i].toInt() == 0 && data[i + 1].toInt() == 0 && data[i + 2].toInt() == 1) {
                if (start >= 0) {
                    // A 4-byte start code's leading zero belongs to it, not to the unit before.
                    var end = i
                    while (end > start && data[end - 1].toInt() == 0) end--
                    out.add(start until end)
                }
                i += 3
                start = i
            } else {
                i++
            }
        }
        if (start in 0 until n) out.add(start until n)
        return out
    }

    /** The unit's type: H.264's 5 bits, HEVC's 6. */
    fun type(data: ByteArray, unit: IntRange, hevc: Boolean): Int {
        if (unit.isEmpty()) return 0
        val b = data[unit.first].toInt() and 0xff
        return if (hevc) (b shr 1) and 0x3f else b and 0x1f
    }

    fun isParameterSet(t: Int, hevc: Boolean) = if (hevc) t in 32..34 else t == 7 || t == 8

    /** The parameter sets in [data], each with a 4-byte start code, in order (VPS, SPS, PPS). */
    fun parameterSets(data: ByteArray, hevc: Boolean): List<ByteArray> =
        units(data).map { it to type(data, it, hevc) }
            .filter { isParameterSet(it.second, hevc) }
            .sortedBy { it.second }
            .map { (r, _) -> byteArrayOf(0, 0, 0, 1) + data.copyOfRange(r.first, r.last + 1) }
}

/**
 * Reading the test pattern (`rein-remote serve --test-pattern`) back from what's on the screen, as the
 * web, iOS and desktop clients do: its capture time drawn as a barcode along the bottom (glass to
 * glass), the flash in its top left sixth, and the red cross it draws where the pointer last moved
 * (input that reached the server, seen coming back). Pixels are ARGB, as a Bitmap gives them.
 */
class Pixels(val argb: IntArray, val width: Int, val height: Int, val x0: Int = 0, val y0: Int = 0) {
    /** The pixel at (x, y) in the stream's coordinates, null outside this piece. */
    fun at(x: Int, y: Int): Int? {
        val (lx, ly) = x - x0 to y - y0
        if (lx < 0 || ly < 0 || lx >= width || ly >= height) return null
        return argb[ly * width + lx]
    }
}

object Pattern {
    const val BLOCK = 24

    private fun luma(p: Int): Int {
        val r = (p shr 16) and 0xff
        val g = (p shr 8) and 0xff
        val b = p and 0xff
        return (r * 299 + g * 587 + b * 114) / 1000
    }

    /** Redness: red above the mean of green and blue. */
    private fun red(p: Int): Int = ((p shr 16) and 0xff) - (((p shr 8) and 0xff) + (p and 0xff)) / 2

    /**
     * The barcode's capture time (ms, Unix time mod 2³²), if [px] has one: blocks of [block]×[block]
     * along the bottom of a stream [streamH] high, white, black, then 32 bits high first. The pattern
     * draws 24-pixel blocks; a picture the server scaled down has smaller ones.
     */
    fun barcodeMs(px: Pixels, streamW: Int, streamH: Int, block: Int = BLOCK): Long? {
        if (streamW < 34 * block || streamH < block) return null
        val y = streamH - block / 2
        fun bit(k: Int): Boolean? = px.at(k * block + block / 2, y)?.let { luma(it) > 128 }
        if (bit(0) != true || bit(1) != false) return null
        var ms = 0L
        for (k in 2 until 34) ms = (ms shl 1) or (if (bit(k) ?: return null) 1L else 0L)
        return ms
    }

    /** Block sizes to try: the pattern's own, then as the server scales a picture down (¾, ⅔, ½, ⅓, ¼). */
    val BLOCKS = intArrayOf(24, 18, 16, 12, 8, 6)

    /**
     * The barcode at whichever scale reads as a time within 10 s of [serverNowMs] (the server's clock
     * now, ms): the capture time, and the block size it was read at.
     */
    fun readBarcode(px: Pixels, streamW: Int, streamH: Int, serverNowMs: Long): Pair<Long, Int>? {
        for (b in BLOCKS) {
            val ms = barcodeMs(px, streamW, streamH, b) ?: continue
            if (since(ms, serverNowMs) != null) return ms to b
        }
        return null
    }

    /** Milliseconds from capture time [barcodeMs] (server clock, mod 2³²) to [serverNowMs]; null when far apart. */
    fun since(barcodeMs: Long, serverNowMs: Long): Long? {
        val d = (serverNowMs - barcodeMs) and 0xffffffffL
        return if (d < 10_000) d else null
    }

    /** Whether the flash (white, the top left sixth) is on. */
    fun flashing(px: Pixels, streamW: Int, streamH: Int): Boolean =
        px.at(streamW / 12, streamH / 12)?.let { luma(it) > 200 } ?: false

    /**
     * Redness above which the cross is taken to be there: the background reads 0 to 5, the cross 40
     * to 60 at full size and 20 to 35 in a picture the server scaled to half (its 1-pixel arms blurred).
     */
    const val CROSS_THRESHOLD = 15.0

    /**
     * How red the pattern's cross is around (x, y): its arms are 1 pixel wide and 20 long (fewer in a
     * scaled picture), red over a gradient whose own red is low. Samples along the right and lower
     * arms, near enough to the middle for a picture scaled to a third, against the same points 6
     * pixels off the arm.
     */
    fun crossRedness(px: Pixels, x: Int, y: Int): Double {
        val diffs = ArrayList<Int>()
        for (d in 2..9) {
            val on1 = px.at(x + d, y)
            val off1 = px.at(x + d, y + 6)
            if (on1 != null && off1 != null) diffs.add(red(on1) - red(off1))
            val on2 = px.at(x, y + d)
            val off2 = px.at(x + 6, y + d)
            if (on2 != null && off2 != null) diffs.add(red(on2) - red(off2))
        }
        return if (diffs.isEmpty()) 0.0 else diffs.average()
    }
}

/** Percentiles of the last values. */
class Window(private val size: Int = 1000) {
    private val values = ArrayDeque<Double>()

    @Synchronized
    fun add(v: Double) {
        values.addLast(v)
        while (values.size > size) values.removeFirst()
    }

    @Synchronized
    fun pct(p: Double): Double? {
        if (values.isEmpty()) return null
        val s = values.sorted()
        return s[minOf(s.size - 1, (s.size * p).toInt())]
    }

    val count: Int @Synchronized get() = values.size
}

/** Frames shown in the last second. */
class FrameRate {
    private val times = ArrayDeque<Long>()

    @Synchronized
    fun tick(nowNs: Long): Int {
        times.addLast(nowNs)
        while (times.first() < nowNs - 1_000_000_000L) times.removeFirst()
        return times.size
    }
}

/**
 * What the QR code a server prints holds, and what a pasted link may: `http://host:port/#pair=CODE&id=FINGERPRINT`
 * (pair with the code, then sign in with keys), or a link with its token, or a relay's link.
 */
data class PairLink(val url: String, val code: String?, val id: String?) {
    companion object {
        fun parse(text: String): PairLink? {
            val t = text.trim()
            val u = try {
                java.net.URI(t)
            } catch (e: java.net.URISyntaxException) {
                return null
            }
            val scheme = u.scheme?.lowercase() ?: return null
            if (scheme !in setOf("http", "https", "ws", "wss") || u.host.isNullOrEmpty()) return null
            val fields = (u.rawFragment ?: "").split('&').mapNotNull { part ->
                val kv = part.split('=', limit = 2)
                if (kv.size == 2) kv[0] to java.net.URLDecoder.decode(kv[1], "UTF-8") else null
            }.toMap()
            val code = fields["pair"] ?: return PairLink(t, null, null)
            return PairLink(t.substringBefore('#'), code, fields["id"])
        }
    }
}

/** A server's TXT record (`rein-remote serve` advertises `id`, its key's fingerprint, and `wt`, its WebTransport port). */
data class ServerTxt(val id: String, val webtransport: Int?) {
    companion object {
        fun of(attrs: Map<String, ByteArray?>): ServerTxt {
            fun s(k: String) = attrs[k]?.let { String(it, Charsets.UTF_8) }
            return ServerTxt(s("id") ?: "", s("wt")?.toIntOrNull()?.takeIf { it != 0 })
        }
    }
}
