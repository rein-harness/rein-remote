import Foundation

/// `-RRSelfTest YES`: with `rein-remote serve --test-pattern`, checks the whole way and measures it. It
/// waits for frames, sends the pointer to three places and looks for the pattern's cross at each in
/// the decoded video (input that reached the server, seen coming back), then measures for a while:
/// frames a second, glass to glass from the barcode, decoding, sound. One line out, on the screen and
/// in the log ("rein-remote selftest: …").
enum SelfTest {
    static func run(_ c: StreamController, seconds: Double) async -> String {
        func wait(_ s: Double, until: () -> Bool) async -> Bool {
            let end = Date().addingTimeInterval(s)
            while Date() < end {
                if until() { return true }
                try? await Task.sleep(nanoseconds: 50_000_000)
            }
            return until()
        }
        guard await wait(20, until: { c.metrics.frames >= 30 }), let tap = c.input else {
            return report("FAIL no video (\(c.metrics.frames) frames)", [:])
        }
        let m0 = c.metrics
        let (w, h) = (m0.width, m0.height)
        var cross: [String] = []
        var crossOk = true
        for (fx, fy) in [(0.25, 0.3), (0.75, 0.6), (0.5, 0.45)] {
            let (x, y) = (Int(Double(w) * fx), Int(Double(h) * fy))
            let before = c.latestFrame.map { Pattern.crossRedness($0, x: x, y: y) } ?? 0
            let sent = Date()
            tap.pointerMove(x, y)
            var after = 0.0
            let seen = await wait(3) {
                after = c.latestFrame.map { Pattern.crossRedness($0, x: x, y: y) } ?? 0
                return after > Pattern.crossThreshold
            }
            let ms = Date().timeIntervalSince(sent) * 1000
            cross.append(String(format: "%d,%d: %.0f→%.0f%@", x, y, before, after, seen ? String(format: " in %.0f ms", ms) : " NOT SEEN"))
            crossOk = crossOk && seen && before <= Pattern.crossThreshold
        }
        let start = c.metrics
        let t0 = Date()
        try? await Task.sleep(nanoseconds: UInt64(seconds * 1e9))
        let m = c.metrics
        let fps = Double(m.frames - start.frames) / Date().timeIntervalSince(t0)
        let f = { (v: Double?) in v.map { String(format: "%.1f", $0) } ?? "-" }
        let fields: [String: String] = [
            "codec": m.decoder,
            "transport": m.transport,
            "frames": "\(m.frames)",
            "fps": String(format: "%.1f", fps),
            "glass_ms_p50": f(m.glass.pct(0.5)),
            "glass_ms_p95": f(m.glass.pct(0.95)),
            "glass_samples": "\(m.glass.count)",
            "decode_ms_p50": f(m.decode.pct(0.5)),
            "decode_ms_p95": f(m.decode.pct(0.95)),
            "network_ms_p50": f(m.network.pct(0.5)),
            "rtt_ms": String(format: "%.1f", m.rttMs),
            "keyframe_asks": "\(m.keyframeAsks)",
            "frames_lost": "\(m.framesLost)",
            "audio_packets": "\(m.audioPackets)",
            "audio_played": "\(m.audioPlayed)",
            "audio_dropped": "\(m.audioDropped)",
            "audio_loud": "\(m.audioLoud)",
            "av_sync_ms_p50": f(m.avSync.pct(0.5)),
            "cross": cross.joined(separator: "; "),
        ]
        let ok = crossOk && fps > 5 && m.glass.count > 0
        return report(ok ? "PASS" : "FAIL", fields)
    }

    private static func report(_ verdict: String, _ fields: [String: String]) -> String {
        let body = fields.keys.sorted().map { "\($0)=\(fields[$0]!)" }.joined(separator: " ")
        let line = "selftest: \(verdict) \(body)"
        NSLog("rein-remote %@", line)
        print("rein-remote \(line)")
        return line
    }
}
