import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

/// H.264 and HEVC in Annex B (start codes, parameter sets in band before each keyframe), as the server
/// sends them, made into what VideoToolbox takes: a format description from the parameter sets, and
/// each frame's NAL units with 4-byte lengths in place of start codes.
enum AnnexB {
    /// The NAL units in `data`, without their start codes.
    static func units(_ data: Data) -> [Data] {
        var out: [Data] = []
        data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            let b = raw.bindMemory(to: UInt8.self)
            let n = b.count
            var i = 0
            var start = -1
            while i + 2 < n {
                if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
                    if start >= 0 {
                        // A 4-byte start code's leading zero belongs to it, not to the unit before.
                        var end = i
                        while end > start && b[end - 1] == 0 { end -= 1 }
                        out.append(Data(b[start..<end]))
                    }
                    i += 3
                    start = i
                } else {
                    i += 1
                }
            }
            if start >= 0 && start < n { out.append(Data(b[start..<n])) }
        }
        return out
    }

    /// The unit's type: H.264's 5 bits, HEVC's 6.
    static func type(_ nal: Data, hevc: Bool) -> UInt8 {
        guard let first = nal.first else { return 0 }
        return hevc ? (first >> 1) & 0x3f : first & 0x1f
    }

    static func isParameterSet(_ t: UInt8, hevc: Bool) -> Bool {
        hevc ? (t == 32 || t == 33 || t == 34) : (t == 7 || t == 8)
    }

    /// The units joined with 4-byte big-endian lengths (AVCC / HVCC).
    static func lengthPrefixed(_ units: [Data]) -> Data {
        var out = Data(capacity: units.reduce(0) { $0 + $1.count + 4 })
        for u in units {
            var len = UInt32(u.count).bigEndian
            withUnsafeBytes(of: &len) { out.append(contentsOf: $0) }
            out.append(u)
        }
        return out
    }

    /// A format description from parameter sets (SPS and PPS; VPS, SPS and PPS for HEVC), in order.
    static func format(_ sets: [Data], hevc: Bool) -> CMVideoFormatDescription? {
        guard !sets.isEmpty else { return nil }
        var desc: CMFormatDescription?
        let bytes = sets.map { [UInt8]($0) }
        var status: OSStatus = -1
        withPointers(bytes) { ptrs in
            let sizes = bytes.map { $0.count }
            if hevc {
                status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: ptrs.count, parameterSetPointers: ptrs,
                    parameterSetSizes: sizes, nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &desc)
            } else {
                status = CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: ptrs.count, parameterSetPointers: ptrs,
                    parameterSetSizes: sizes, nalUnitHeaderLength: 4, formatDescriptionOut: &desc)
            }
        }
        return status == noErr ? desc : nil
    }

    private static func withPointers(_ arrays: [[UInt8]], _ body: ([UnsafePointer<UInt8>]) -> Void) {
        var ptrs: [UnsafePointer<UInt8>] = []
        func go(_ i: Int) {
            if i == arrays.count { return body(ptrs) }
            arrays[i].withUnsafeBufferPointer { p in
                ptrs.append(p.baseAddress!)
                go(i + 1)
            }
        }
        go(0)
    }
}

/// One decoded frame.
struct DecodedFrame {
    let image: CVPixelBuffer
    let frame: UInt32
    let timestampUs: UInt64
    let receivedUs: UInt64
    let decodedUs: UInt64
}

/// Decodes the stream with VideoToolbox (hardware on a device), frames out in decode order (no
/// B-frames: the server never sends them). On an error it asks for a keyframe and skips frames until
/// one comes, as the web client does.
final class VideoDecoder {
    private var session: VTDecompressionSession?
    private var format: CMVideoFormatDescription?
    private var sets: [Data] = []
    private var hevc = false
    private var waitKey = true
    /// Frames skipped while waiting for a keyframe, since the last feedback.
    private(set) var lost: UInt16 = 0
    private var resetPending = false
    /// Frames handed to VideoToolbox and not out yet. A decoder that falls behind (the first frames,
    /// while it starts; a slow device) would show everything late: past `maxInFlight`, frames are
    /// skipped until a keyframe, which is asked for.
    private var inFlight = 0
    /// Which decoder a callback came from (one made again leaves the old one's frames uncounted).
    private var generation = 0
    static let maxInFlight = 3
    static let lateUs: UInt64 = 100_000
    private(set) var skippedBehind = 0
    private let lock = NSRecursiveLock()

    /// A frame decoded (on VideoToolbox's thread).
    var onFrame: ((DecodedFrame) -> Void)?
    /// The decoder lost its place: ask the server for a keyframe.
    var onNeedKeyframe: (() -> Void)?
    /// What's decoding (shown in the stats).
    private(set) var description = ""
    private(set) var errors = 0

    /// The stream (re)started: the next frame must be a keyframe.
    func configure(codec: Codec?) {
        lock.lock()
        defer { lock.unlock() }
        hevc = codec == .h265
        waitKey = true
        description = hevc ? "HEVC" : "H.264"
    }

    func takeLost() -> UInt16 {
        lock.lock()
        defer { lock.unlock() }
        let l = lost
        lost = 0
        return l
    }

    func decode(frame: UInt32, keyframe: Bool, timestampUs: UInt64, receivedUs: UInt64, data: Data) {
        lock.lock()
        defer { lock.unlock() }
        if waitKey && !keyframe {
            lost &+= 1
            return
        }
        // Or frames that waited too long to get here (the decoder held this thread: its first frame
        // can take half a second). A keyframe is always taken: it's where the decoder starts over.
        let late = Core.nowUs() &- receivedUs > Self.lateUs
        if (inFlight >= Self.maxInFlight || late) && !keyframe {
            skippedBehind += 1
            lost &+= 1
            if !waitKey {
                waitKey = true
                NSLog("rein-remote: video: the decoder is behind (%d frames in it, this one %.0f ms late): skipping to a keyframe",
                      inFlight, Double(Core.nowUs() &- receivedUs) / 1000)
                onNeedKeyframe?()
            }
            return
        }
        let units = AnnexB.units(data)
        let params = units.filter { AnnexB.isParameterSet(AnnexB.type($0, hevc: hevc), hevc: hevc) }
        if !params.isEmpty {
            // In the order the format wants: VPS, SPS, PPS (HEVC) / SPS, PPS.
            let ordered = params.sorted { AnnexB.type($0, hevc: hevc) < AnnexB.type($1, hevc: hevc) }
            if ordered != sets || session == nil || resetPending {
                guard let f = AnnexB.format(ordered, hevc: hevc) else { return fail("bad parameter sets") }
                sets = ordered
                format = f
                if let s = session, !resetPending, VTDecompressionSessionCanAcceptFormatDescription(s, formatDescription: f) {
                    // The same decoder takes it.
                } else {
                    resetPending = false
                    makeSession()
                }
            }
        }
        guard let session, let format else { return fail("no parameter sets before the first frame") }
        waitKey = false
        let body = AnnexB.lengthPrefixed(units.filter { !AnnexB.isParameterSet(AnnexB.type($0, hevc: hevc), hevc: hevc) })
        guard let sample = Self.sample(body, format: format) else { return fail("no sample buffer") }
        let flags: VTDecodeFrameFlags = [._EnableAsynchronousDecompression]
        inFlight += 1
        let gen = generation
        let status = VTDecompressionSessionDecodeFrame(session, sampleBuffer: sample, flags: flags, infoFlagsOut: nil) {
            [weak self] status, _, image, _, _ in
            guard let self else { return }
            self.lock.lock()
            if gen == self.generation { self.inFlight -= 1 }
            self.lock.unlock()
            guard status == noErr, let image else {
                self.lock.lock()
                self.fail("decode error \(status)")
                self.lock.unlock()
                return
            }
            self.onFrame?(DecodedFrame(image: image, frame: frame, timestampUs: timestampUs, receivedUs: receivedUs, decodedUs: Core.nowUs()))
        }
        if status != noErr {
            inFlight -= 1
            fail("decode error \(status)")
        }
    }

    /// Called with the lock held (possibly from VideoToolbox's own callback: the decoder is made
    /// again at the next keyframe, not here).
    private func fail(_ why: String) {
        errors += 1
        NSLog("rein-remote: video: %@; asking for a keyframe", why)
        if !waitKey { lost &+= 1 }
        waitKey = true
        resetPending = true
        onNeedKeyframe?()
    }

    private func makeSession() {
        if let s = session {
            VTDecompressionSessionInvalidate(s)
            session = nil
        }
        inFlight = 0
        generation += 1
        guard let format else { return }
        let attrs: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
            kCVPixelBufferMetalCompatibilityKey: true,
        ]
        var s: VTDecompressionSession?
        let status = VTDecompressionSessionCreate(allocator: kCFAllocatorDefault, formatDescription: format, decoderSpecification: nil,
                                                  imageBufferAttributes: attrs as CFDictionary, outputCallback: nil, decompressionSessionOut: &s)
        guard status == noErr, let s else {
            NSLog("rein-remote: video: no decoder (%d)", status)
            return
        }
        VTSessionSetProperty(s, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        session = s
        let dims = CMVideoFormatDescriptionGetDimensions(format)
        var hw: CFBoolean?
        VTSessionCopyProperty(s, key: kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder, allocator: nil, valueOut: &hw)
        description = "\(hevc ? "HEVC" : "H.264") \(dims.width)×\(dims.height)\(hw == kCFBooleanTrue ? " (hardware)" : "")"
    }

    private static func sample(_ body: Data, format: CMVideoFormatDescription) -> CMSampleBuffer? {
        var block: CMBlockBuffer?
        let n = body.count
        guard CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: n, blockAllocator: kCFAllocatorDefault,
                                                 customBlockSource: nil, offsetToData: 0, dataLength: n, flags: kCMBlockBufferAssureMemoryNowFlag,
                                                 blockBufferOut: &block) == noErr, let block else { return nil }
        let copied = body.withUnsafeBytes { CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: n) }
        guard copied == noErr else { return nil }
        var sample: CMSampleBuffer?
        var size = n
        guard CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format, sampleCount: 1,
                                        sampleTimingEntryCount: 0, sampleTimingArray: nil, sampleSizeEntryCount: 1, sampleSizeArray: &size,
                                        sampleBufferOut: &sample) == noErr else { return nil }
        return sample
    }

    func close() {
        lock.lock()
        defer { lock.unlock() }
        if let s = session {
            VTDecompressionSessionWaitForAsynchronousFrames(s)
            VTDecompressionSessionInvalidate(s)
        }
        session = nil
    }
}
