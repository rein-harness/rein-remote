import AVFoundation
import Foundation

/// The server's sound: Opus packets (48 kHz, stereo, 10 ms) decoded by Apple's own Opus decoder
/// (AVAudioConverter) and played through AVAudioEngine, each at its capture time plus the delay the
/// picture has, as the web client places them (`sharedOffset`, `placePacket`): sound and picture at
/// the same delay, packets back to back while they keep time, moved (or dropped, when too late) when
/// they're off by more than 20 ms.
final class AudioPlayer {
    private let engine = AVAudioEngine()
    private let node = AVAudioPlayerNode()
    private let pcm: AVAudioFormat
    private let opus: AVAudioFormat
    private let converter: AVAudioConverter
    private let queue = DispatchQueue(label: "rein-remote audio", qos: .userInteractive)
    /// The last ~3 s of how late after capture the picture was ready (µs, local − server clock).
    private var videoUs = Window(size: 180)
    /// How late after capture sound is ready to play.
    private var readyUs = Window(size: 300)
    /// Where the last packet scheduled ends (Unix µs), 0: none yet.
    private var playheadUs: UInt64 = 0
    private(set) var packets = 0
    private(set) var played = 0
    private(set) var dropped = 0
    private(set) var loud = 0
    private(set) var failed: String?
    /// Measuring: the times (Unix µs) beeps start to be heard, for sound against picture.
    var onBeep: ((UInt64) -> Void)?
    private var wasLoud = false

    init?() {
        var asbd = AudioStreamBasicDescription(mSampleRate: 48_000, mFormatID: kAudioFormatOpus, mFormatFlags: 0, mBytesPerPacket: 0,
                                               mFramesPerPacket: 480, mBytesPerFrame: 0, mChannelsPerFrame: 2, mBitsPerChannel: 0, mReserved: 0)
        guard let opus = AVAudioFormat(streamDescription: &asbd),
              let pcm = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48_000, channels: 2, interleaved: false),
              let converter = AVAudioConverter(from: opus, to: pcm) else { return nil }
        self.opus = opus
        self.pcm = pcm
        self.converter = converter
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, mode: .default, options: [.mixWithOthers])
        try? session.setPreferredIOBufferDuration(0.005)
        try? session.setActive(true)
        engine.attach(node)
        engine.connect(node, to: engine.mainMixerNode, format: pcm)
        do {
            try engine.start()
            node.play()
        } catch {
            failed = error.localizedDescription
            NSLog("rein-remote: no sound (%@)", error.localizedDescription)
        }
    }

    /// A frame was ready to show: how late after capture (local µs minus capture µs, both as Unix
    /// time with the server's clock offset folded in by the caller).
    func pictureDelay(_ us: Double) {
        queue.async { self.videoUs.add(us) }
    }

    func packet(timestampUs: UInt64, data: Data, offsetUs: Int64) {
        queue.async { self.play(timestampUs: timestampUs, data: data, offsetUs: offsetUs) }
    }

    /// The decoded packet, or nil.
    func decode(_ data: Data) -> AVAudioPCMBuffer? {
        let input = AVAudioCompressedBuffer(format: opus, packetCapacity: 1, maximumPacketSize: max(data.count, 1))
        data.withUnsafeBytes { input.data.copyMemory(from: $0.baseAddress!, byteCount: data.count) }
        input.byteLength = UInt32(data.count)
        input.packetCount = 1
        input.packetDescriptions?[0] = AudioStreamPacketDescription(mStartOffset: 0, mVariableFramesInPacket: 0, mDataByteSize: UInt32(data.count))
        guard let out = AVAudioPCMBuffer(pcmFormat: pcm, frameCapacity: 960) else { return nil }
        var given = false
        var err: NSError?
        let status = converter.convert(to: out, error: &err) { _, outStatus in
            if given {
                outStatus.pointee = .noDataNow
                return nil
            }
            given = true
            outStatus.pointee = .haveData
            return input
        }
        if status == .error || out.frameLength == 0 {
            if failed == nil { failed = "Opus: \(err?.localizedDescription ?? "nothing decoded")" }
            return nil
        }
        return out
    }

    private func play(timestampUs: UInt64, data: Data, offsetUs: Int64) {
        packets += 1
        guard data.count > 0, let buf = decode(data) else { return }
        let now = Core.nowUs()
        // When it should be heard: capture time on this clock, plus the delay both share.
        let capturedHere = Int64(timestampUs) + offsetUs
        readyUs.add(Double(Int64(now) - capturedHere))
        let shared = max(videoUs.pct(0.5) ?? 0, (readyUs.pct(0.5) ?? 0) + 15_000)
        let latencyUs = UInt64(max(0, (AVAudioSession.sharedInstance().outputLatency + AVAudioSession.sharedInstance().ioBufferDuration) * 1e6))
        let want = UInt64(max(0, capturedHere + Int64(shared)))
        let duration = UInt64(Double(buf.frameLength) / 48.0 * 1000)
        let quiet = peak(buf) < 0.001
        let err = playheadUs == 0 ? Int64.max : Int64(playheadUs) - Int64(want)
        var at: UInt64? = nil
        if err == Int64.max || abs(err) > 20_000 || (quiet && abs(err) > 1_000) {
            // Off: start again where it should be, or drop it when that's past.
            if want < now + latencyUs {
                dropped += 1
                if err != Int64.max && err <= 20_000 { playheadUs = 0 }
                return
            }
            at = want
            playheadUs = want + duration
        } else {
            playheadUs = max(playheadUs, now + latencyUs) + duration
        }
        let loudNow = peak(buf) > 0.1
        if loudNow && !wasLoud { onBeep?(at ?? (playheadUs - duration)) }
        wasLoud = loudNow
        if loudNow { loud += 1 }
        played += 1
        if let at {
            // Unix µs → the host clock the engine schedules on.
            let ahead = Double(Int64(at) - Int64(now) - Int64(latencyUs)) / 1e6
            let host = mach_absolute_time() + AVAudioTime.hostTime(forSeconds: max(0, ahead))
            node.scheduleBuffer(buf, at: AVAudioTime(hostTime: host), options: [], completionHandler: nil)
        } else {
            node.scheduleBuffer(buf, completionHandler: nil)
        }
    }

    private func peak(_ b: AVAudioPCMBuffer) -> Float {
        guard let ch = b.floatChannelData else { return 0 }
        var m: Float = 0
        for c in 0..<Int(b.format.channelCount) {
            for i in 0..<Int(b.frameLength) { m = max(m, abs(ch[c][i])) }
        }
        return m
    }

    func close() {
        node.stop()
        engine.stop()
    }
}
