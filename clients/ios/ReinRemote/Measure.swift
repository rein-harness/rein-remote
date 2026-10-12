import CoreVideo
import Foundation

/// Reading the test pattern (`rein-remote serve --test-pattern`) back from decoded frames, as the web
/// client's `measure` does: its capture time drawn as a barcode along the bottom (glass to glass, when
/// this device shares the server's clock: the simulator on the same Mac, or both on NTP), the flash in
/// its top left sixth (sound against picture), and the cross it draws where the pointer last moved
/// (input that reached the server, seen coming back).
enum Pattern {
    /// The barcode's capture time (ms, Unix time mod 2³²), if the frame has one: blocks of 24×24 along
    /// the bottom, white, black, then 32 bits high first.
    static func barcodeMs(_ px: CVPixelBuffer) -> UInt32? {
        withLuma(px) { luma, stride, w, h in
            guard w >= 34 * 24, h >= 24 else { return nil }
            let row = luma + (h - 12) * stride
            func bit(_ k: Int) -> Bool { row[k * 24 + 12] > 128 }
            guard bit(0), !bit(1) else { return nil }
            var ms: UInt32 = 0
            for k in 2..<34 { ms = ms << 1 | (bit(k) ? 1 : 0) }
            return ms
        }
    }

    /// Glass to glass (ms) for a frame shown at `nowMs` (Unix ms), when its barcode reads.
    static func glassMs(_ px: CVPixelBuffer, nowMs: UInt64) -> Double? {
        guard let ms = barcodeMs(px) else { return nil }
        let now = UInt32(truncatingIfNeeded: nowMs)
        let d = now &- ms
        return d < 10_000 ? Double(d) : nil
    }

    /// Whether the flash (white, the top left sixth) is on.
    static func flashing(_ px: CVPixelBuffer) -> Bool {
        withLuma(px) { luma, stride, w, h in
            guard w >= 60 else { return false }
            return luma[(h / 12) * stride + w / 12] > 200
        } ?? false
    }

    /// Redness above which the cross is taken to be there.
    static let crossThreshold = 12.0

    /// How red (Cr above its surroundings) the pattern's cross is around (x, y): its arms are 1 pixel
    /// wide and 20 long, red over a gradient whose own red is low. Samples along the right and lower
    /// arms against the same points 6 pixels off the arm.
    static func crossRedness(_ px: CVPixelBuffer, x: Int, y: Int) -> Double {
        CVPixelBufferLockBaseAddress(px, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(px, .readOnly) }
        guard CVPixelBufferGetPlaneCount(px) == 2, let base = CVPixelBufferGetBaseAddressOfPlane(px, 1) else { return 0 }
        let rowBytes = CVPixelBufferGetBytesPerRowOfPlane(px, 1)
        let (cw, ch) = (CVPixelBufferGetWidthOfPlane(px, 1), CVPixelBufferGetHeightOfPlane(px, 1))
        let p = base.assumingMemoryBound(to: UInt8.self)
        // Cr at a luma position (chroma is half size, Cb then Cr).
        func cr(_ lx: Int, _ ly: Int) -> Double? {
            let (cx, cy) = (lx / 2, ly / 2)
            guard cx >= 0, cy >= 0, cx < cw, cy < ch else { return nil }
            return Double(p[cy * rowBytes + cx * 2 + 1])
        }
        var diffs: [Double] = []
        for d in stride(from: 6, through: 16, by: 2) {
            if let on = cr(x + d, y), let off = cr(x + d, y + 6) { diffs.append(on - off) }
            if let on = cr(x, y + d), let off = cr(x + 6, y + d) { diffs.append(on - off) }
        }
        guard !diffs.isEmpty else { return 0 }
        return diffs.reduce(0, +) / Double(diffs.count)
    }

    private static func withLuma<T>(_ px: CVPixelBuffer, _ body: (UnsafePointer<UInt8>, Int, Int, Int) -> T?) -> T? {
        CVPixelBufferLockBaseAddress(px, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(px, .readOnly) }
        let planar = CVPixelBufferGetPlaneCount(px) > 0
        guard let base = planar ? CVPixelBufferGetBaseAddressOfPlane(px, 0) : CVPixelBufferGetBaseAddress(px) else { return nil }
        let stride = planar ? CVPixelBufferGetBytesPerRowOfPlane(px, 0) : CVPixelBufferGetBytesPerRow(px)
        let w = planar ? CVPixelBufferGetWidthOfPlane(px, 0) : CVPixelBufferGetWidth(px)
        let h = planar ? CVPixelBufferGetHeightOfPlane(px, 0) : CVPixelBufferGetHeight(px)
        return body(base.assumingMemoryBound(to: UInt8.self), stride, w, h)
    }
}

/// Percentiles of the last values.
struct Window {
    private(set) var values: [Double] = []
    let size: Int
    init(size: Int = 1000) { self.size = size }
    mutating func add(_ v: Double) {
        values.append(v)
        if values.count > size { values.removeFirst(values.count - size) }
    }
    func pct(_ p: Double) -> Double? {
        guard !values.isEmpty else { return nil }
        let s = values.sorted()
        return s[min(s.count - 1, Int(Double(s.count) * p))]
    }
    var count: Int { values.count }
}

/// Frames shown in the last second.
struct FrameRate {
    private var times: [CFTimeInterval] = []
    mutating func tick(_ t: CFTimeInterval) -> Int {
        times.append(t)
        while let f = times.first, f < t - 1 { times.removeFirst() }
        return times.count
    }
}
