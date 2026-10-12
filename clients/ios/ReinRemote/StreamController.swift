import AVFoundation
import Combine
import UIKit
import VideoToolbox

/// What the stream measured, for the overlay, the self-test and the tests.
struct StreamMetrics {
    var frames = 0
    var fps = 0
    var keyframeAsks = 0
    /// Glass to glass from the test pattern's barcode (ms): capture to decoded and handed to the display.
    var glass = Window(size: 2000)
    /// Arrival to decoded (ms).
    var decode = Window(size: 2000)
    /// Capture to arrival (ms), the core's estimate from its pings.
    var network = Window(size: 2000)
    /// Each beep against its flash (ms, positive: the sound after the picture).
    var avSync = Window(size: 200)
    var audioPackets = 0
    var audioPlayed = 0
    var audioDropped = 0
    var audioLoud = 0
    var decoder = ""
    var codec = ""
    var width = 0
    var height = 0
    var transport = ""
    var rttMs = 0.0
    var fecRecovered: UInt64 = 0
    var framesLost: UInt64 = 0
}

/// Input on its way to the session, the pointer's last place noted (measuring: the test pattern draws
/// a cross there once the server has it).
final class InputTap: InputSink {
    let session: RemoteSession
    private let lock = NSLock()
    private var last: (Int, Int)?
    init(_ s: RemoteSession) { session = s }
    var lastPointer: (Int, Int)? {
        lock.lock()
        defer { lock.unlock() }
        return last
    }

    func pointerMove(_ x: Int, _ y: Int) {
        lock.lock()
        last = (x, y)
        lock.unlock()
        session.pointerMove(x, y)
    }

    func button(_ b: MouseButton, down: Bool) { session.button(b, down: down) }
    func wheel(dx: Int, dy: Int) { session.wheel(dx: dx, dy: dy) }
    func key(hid: UInt16, down: Bool, name: String) { session.key(hid: hid, down: down, name: name) }
    func text(_ t: String) { session.text(t) }
}

/// One stream: the session, decoding, sound, gamepads, and what's measured.
final class StreamController: ObservableObject {
    /// Measuring: where the pointer was last sent, once the test pattern's cross shows there.
    @Published private(set) var inputSeen = ""
    private(set) var input: InputTap?
    @Published private(set) var state = "connecting"
    @Published private(set) var overlay = ""
    @Published private(set) var serverName = ""
    let view = StreamUIView(frame: .zero)
    private(set) var session: RemoteSession?
    private let decoder = VideoDecoder()
    private var audio: AudioPlayer?
    private var gamepads: Gamepads?
    private let lock = NSLock()
    private var m = StreamMetrics()
    private var rate = FrameRate()
    private var clockOffsetUs: Int64?
    private var lastImage: CVPixelBuffer?
    private var flashOn = false
    private var flashes: [UInt64] = []
    private var beeps: [UInt64] = []
    private var timer: Timer?
    private var serverInput: UInt8 = 0
    /// Read the test pattern's barcode from every frame.
    var measure = false

    var metrics: StreamMetrics {
        lock.lock()
        defer { lock.unlock() }
        return m
    }

    /// The newest decoded frame.
    var latestFrame: CVPixelBuffer? {
        lock.lock()
        defer { lock.unlock() }
        return lastImage
    }

    init() {
        decoder.onFrame = { [weak self] f in self?.decoded(f) }
        decoder.onNeedKeyframe = { [weak self] in
            guard let self else { return }
            self.lock.lock()
            self.m.keyframeAsks += 1
            self.lock.unlock()
            self.session?.keyframe()
        }
    }

    /// Connects (off the main thread) and starts streaming.
    func connect(target: String, pairCode: String?, options: ConnectOptions = ConnectOptions()) async throws {
        var o = options
        o.refreshMhz = UInt32(await UIScreen.main.maximumFramesPerSecond * 1000)
        // HEVC where this device decodes it in hardware; H.264 always.
        if VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC) && !o.codecs.contains(.h265) { o.codecs.append(.h265) }
        o.hardware = VTIsHardwareDecodeSupported(kCMVideoCodecType_H264)
        let name = await UIDevice.current.name
        let opts = o
        let s = try await Task.detached(priority: .userInitiated) {
            try RemoteSession(target: target, name: name, pairCode: pairCode, options: opts)
        }.value
        session = s
        let tap = InputTap(s)
        input = tap
        await MainActor.run {
            view.sink = tap
            state = "connected"
            if opts.audio { audio = AudioPlayer() }
            audio?.onBeep = { [weak self] at in self?.beep(at) }
            if opts.gamepad {
                gamepads = Gamepads()
                gamepads?.onState = { [weak self] g in
                    guard let self, self.serverInput & 8 != 0 else { return }
                    self.session?.gamepad(g)
                }
            }
            timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in self?.tick() }
        }
        s.start { [weak self] ev in self?.event(ev) }
    }

    func close() {
        timer?.invalidate()
        session?.close()
        audio?.close()
        decoder.close()
        gamepads = nil
    }

    private func event(_ ev: RemoteEvent) {
        switch ev {
        case let .hello(name, input, _):
            serverInput = input
            DispatchQueue.main.async { self.serverName = name }
        case let .config(codec, width, height, _, _):
            decoder.configure(codec: codec)
            lock.lock()
            m.codec = codec == .h265 ? "HEVC" : codec == .h264 ? "H.264" : "?"
            (m.width, m.height) = (width, height)
            lock.unlock()
            DispatchQueue.main.async {
                self.view.streamSize = CGSize(width: width, height: height)
                self.state = "streaming"
            }
        case let .video(frame, keyframe, ts, received, latency, data):
            if let l = latency {
                lock.lock()
                m.network.add(Double(l) / 1000)
                lock.unlock()
            }
            decoder.decode(frame: frame, keyframe: keyframe, timestampUs: ts, receivedUs: received, data: data)
        case let .cursor(x, y, visible, shape):
            DispatchQueue.main.async { self.view.cursor(x: x, y: y, visible: visible, shape: shape) }
        case let .audio(ts, data):
            lock.lock()
            let off = clockOffsetUs
            m.audioPackets += 1
            lock.unlock()
            if let off { audio?.packet(timestampUs: ts, data: data, offsetUs: off) }
        case let .clipboard(text):
            DispatchQueue.main.async { UIPasteboard.general.string = text }
        case let .transport(t):
            NSLog("rein-remote: now over %@", t)
        case let .note(n):
            NSLog("rein-remote: %@", n)
        case let .closed(why):
            NSLog("rein-remote: closed (%@)", why ?? "by the server")
            DispatchQueue.main.async {
                self.state = why.map { "closed: \($0)" } ?? "closed"
                self.timer?.invalidate()
            }
        }
    }

    /// A frame out of the decoder (VideoToolbox's thread): feedback, on screen, measured.
    private func decoded(_ f: DecodedFrame) {
        session?.feedback(frame: f.frame, receivedUs: f.receivedUs, decodedUs: f.decodedUs, lost: decoder.takeLost())
        present(f.image)
        let glass = measure ? Pattern.glassMs(f.image, nowMs: Core.nowUs() / 1000) : nil
        let flash = measure && audio != nil ? Pattern.flashing(f.image) : false
        lock.lock()
        m.frames += 1
        m.fps = rate.tick(CACurrentMediaTime())
        m.decode.add(Double(f.decodedUs &- f.receivedUs) / 1000)
        if let glass { m.glass.add(glass) }
        lastImage = f.image
        let off = clockOffsetUs
        if flash && !flashOn {
            flashes.append(f.decodedUs)
            pairFlashes()
        }
        flashOn = flash
        lock.unlock()
        if let off { audio?.pictureDelay(Double(Int64(f.decodedUs) - (Int64(f.timestampUs) + off))) }
        // Measuring: does the pattern draw its cross where the pointer was sent? (Every 6th frame.)
        if measure, f.frame % 6 == 0, let (x, y) = input?.lastPointer {
            let red = Pattern.crossRedness(f.image, x: x, y: y)
            if red > Pattern.crossThreshold {
                let seen = "input seen at \(x),\(y)"
                DispatchQueue.main.async { if self.inputSeen != seen { self.inputSeen = seen } }
            }
        }
    }

    /// A beep heard at `heardUs`: against the flash nearest it (the pattern flashes once a second).
    private func beep(_ heardUs: UInt64) {
        lock.lock()
        defer { lock.unlock() }
        beeps.append(heardUs)
        if beeps.count > 8 { beeps.removeFirst() }
        pairFlashes()
    }

    /// Called with the lock held: each flash with a beep within 300 ms of it, once.
    private func pairFlashes() {
        for f in flashes {
            guard let b = beeps.min(by: { abs(Int64($0) - Int64(f)) < abs(Int64($1) - Int64(f)) }) else { return }
            let d = Double(Int64(b) - Int64(f)) / 1000
            if abs(d) < 300 {
                m.avSync.add(d)
                flashes.removeAll { $0 == f }
                beeps.removeAll { $0 == b }
            }
        }
        let old = Core.nowUs() &- 2_000_000
        flashes.removeAll { $0 < old }
    }

    private func present(_ image: CVPixelBuffer) {
        var desc: CMVideoFormatDescription?
        guard CMVideoFormatDescriptionCreateForImageBuffer(allocator: nil, imageBuffer: image, formatDescriptionOut: &desc) == noErr, let desc else { return }
        var timing = CMSampleTimingInfo(duration: .invalid, presentationTimeStamp: CMClockGetTime(CMClockGetHostTimeClock()), decodeTimeStamp: .invalid)
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReadyWithImageBuffer(allocator: nil, imageBuffer: image, formatDescription: desc, sampleTiming: &timing,
                                                       sampleBufferOut: &sample) == noErr, let sample else { return }
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true) as? [NSMutableDictionary] {
            attachments.first?[kCMSampleAttachmentKey_DisplayImmediately] = true
        }
        let r = view.display.sampleBufferRenderer
        if r.status == .failed { r.flush() }
        r.enqueue(sample)
    }

    private func tick() {
        guard let s = session else { return }
        let st = s.stats
        lock.lock()
        clockOffsetUs = st.clockOffsetUs
        m.transport = st.transport
        m.rttMs = st.rttMs
        m.fecRecovered = st.fecRecovered
        m.framesLost = st.framesLost
        m.decoder = decoder.description
        if let a = audio {
            (m.audioPlayed, m.audioDropped, m.audioLoud) = (a.played, a.dropped, a.loud)
        }
        let mm = m
        lock.unlock()
        var lines = ["\(mm.decoder.isEmpty ? mm.codec : mm.decoder) · \(mm.fps) fps · \(mm.transport) · rtt \(String(format: "%.0f", mm.rttMs)) ms"]
        if let g = mm.glass.pct(0.5), let g95 = mm.glass.pct(0.95) {
            lines.append("glass to glass \(String(format: "%.0f", g)) ms (p95 \(String(format: "%.0f", g95)))")
        }
        if audio != nil { lines.append("sound: \(mm.audioPlayed) played, \(mm.audioDropped) dropped") }
        overlay = lines.joined(separator: "\n")
    }
}
