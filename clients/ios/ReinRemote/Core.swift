import Foundation
import ReinRemoteCore

/// The Rust core (crates/rr-ffi) as Swift: connecting, pairing, the session's events and input. The
/// protocol, the transports and pairing live there, shared with every other client.
enum Core {
    /// Where pairings are kept: the app's Application Support folder (backed up, never shared).
    static func setUp() {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("rein-remote", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        _ = rr_set_home(dir.path)
    }

    static var version: String { String(cString: rr_version()) }

    /// Unix time in µs: the clock the core stamps arrivals with.
    static func nowUs() -> UInt64 { rr_now_us() }

    /// Pairs with the server at `url` (http://host:port/) with the code it shows: its fingerprint.
    static func pair(url: String, code: String, name: String) throws -> String {
        var err: UnsafeMutablePointer<CChar>?
        guard let id = rr_pair(url, code, name, &err) else { throw CoreError(take(err) ?? "pairing failed") }
        return take(id) ?? ""
    }

    static func isPaired(_ id: String) -> Bool { rr_is_paired(id) }

    /// A string the core returned, freed.
    static func take(_ p: UnsafeMutablePointer<CChar>?) -> String? {
        guard let p else { return nil }
        defer { rr_string_free(p) }
        return String(cString: p)
    }
}

struct CoreError: LocalizedError {
    let message: String
    init(_ m: String) { message = m }
    var errorDescription: String? { message }
}

enum Codec: UInt8 {
    case h264 = 1, h265 = 2, av1 = 3, vp9 = 4, vp8 = 5
}

/// What comes from the server.
enum RemoteEvent {
    case hello(name: String, input: UInt8, audio: Bool)
    case config(codec: Codec?, width: Int, height: Int, fps: Int, kbps: Int)
    case video(frame: UInt32, keyframe: Bool, timestampUs: UInt64, receivedUs: UInt64, latencyUs: Int64?, data: Data)
    case cursor(x: Int, y: Int, visible: Bool, shape: CursorShape?)
    case audio(timestampUs: UInt64, data: Data)
    case clipboard(String)
    case transport(String)
    case note(String)
    case closed(String?)
}

struct CursorShape {
    let width: Int, height: Int, hotX: Int, hotY: Int
    let rgba: Data
}

struct RemoteStats {
    var frames: UInt64 = 0
    var keyframes: UInt64 = 0
    var bytes: UInt64 = 0
    var framesLost: UInt64 = 0
    var fecRecovered: UInt64 = 0
    var rttMs: Double = 0
    /// The server's clock against this one: add to a server time for this clock's (µs).
    var clockOffsetUs: Int64?
    var transport = ""
}

struct ConnectOptions {
    var webtransport = true
    var datagrams = true
    var direct = true
    var audio = true
    var gamepad = true
    var codecs: [Codec] = [.h264]
    var hardware = true
    var refreshMhz: UInt32 = 60_000
}

/// One session with a server. Events come on a thread of its own (`onEvent` is called there); input
/// may be sent from any thread.
final class RemoteSession {
    private let handle: OpaquePointer
    private var thread: Thread?
    private let lock = NSLock()
    private var freed = false

    /// Connects (blocking: call it off the main thread).
    init(target: String, name: String, pairCode: String?, options o: ConnectOptions) throws {
        var opts = rr_options_default()
        opts.webtransport = o.webtransport
        opts.datagrams = o.datagrams
        opts.direct = o.direct
        opts.audio = o.audio
        opts.gamepad = o.gamepad
        opts.codecs = o.codecs.reduce(0) { $0 | (1 << UInt32($1.rawValue)) }
        opts.hardware = o.hardware
        opts.refresh_mhz = o.refreshMhz
        var err: UnsafeMutablePointer<CChar>?
        guard let h = rr_connect(target, name, pairCode, &opts, &err) else {
            throw CoreError(Core.take(err) ?? "couldn't connect")
        }
        handle = h
    }

    deinit {
        close()
        // The event thread holds `self` until it has seen the end, so no call can still be running.
        rr_free(handle)
    }

    /// Starts taking events; `onEvent` runs on the session's own thread, until `.closed`.
    func start(_ onEvent: @escaping (RemoteEvent) -> Void) {
        let t = Thread { [self] in
            while true {
                guard let p = rr_next_event(handle, 250) else { continue }
                let ev = Self.convert(p.pointee)
                rr_event_free(p)
                onEvent(ev)
                if case .closed = ev { break }
            }
        }
        t.name = "rein-remote events"
        t.qualityOfService = .userInteractive
        thread = t
        t.start()
    }

    private static func convert(_ e: RrEvent) -> RemoteEvent {
        let text = e.text.map { String(cString: $0) }
        let data = e.data.map { Data(bytes: $0, count: e.len) } ?? Data()
        switch Int(e.kind) {
        case Int(RR_EVENT_HELLO): return .hello(name: text ?? "", input: e.server_input, audio: e.server_audio)
        case Int(RR_EVENT_CONFIG):
            return .config(codec: Codec(rawValue: e.codec), width: Int(e.width), height: Int(e.height), fps: Int(e.fps), kbps: Int(e.bitrate_kbps))
        case Int(RR_EVENT_VIDEO):
            return .video(frame: e.frame, keyframe: e.keyframe, timestampUs: e.timestamp_us, receivedUs: e.received_us,
                          latencyUs: e.has_latency ? e.latency_us : nil, data: data)
        case Int(RR_EVENT_CURSOR):
            let shape = e.has_shape ? CursorShape(width: Int(e.shape_width), height: Int(e.shape_height), hotX: Int(e.hot_x), hotY: Int(e.hot_y), rgba: data) : nil
            return .cursor(x: Int(e.x), y: Int(e.y), visible: e.visible, shape: shape)
        case Int(RR_EVENT_AUDIO): return .audio(timestampUs: e.timestamp_us, data: data)
        case Int(RR_EVENT_CLIPBOARD): return .clipboard(text ?? "")
        case Int(RR_EVENT_TRANSPORT): return .transport(text ?? "")
        case Int(RR_EVENT_NOTE): return .note(text ?? "")
        default: return .closed(text)
        }
    }

    // ---- input, in the stream's pixels ----

    func pointerMove(_ x: Int, _ y: Int) { rr_pointer_move(handle, clamp16(x), clamp16(y)) }
    func button(_ b: MouseButton, down: Bool) { rr_button(handle, b.rawValue, down) }
    func wheel(dx: Int, dy: Int) { rr_wheel(handle, clampI16(dx), clampI16(dy)) }
    func key(hid: UInt16, down: Bool, name: String) { rr_key(handle, hid, down, name) }
    func text(_ t: String) { rr_text(handle, t) }
    func gamepad(_ g: GamepadState) {
        rr_gamepad(handle, g.pad, g.buttons, g.lx, g.ly, g.rx, g.ry, g.lt, g.rt)
    }
    func clipboard(_ t: String) { rr_clipboard(handle, t) }
    func keyframe() { rr_keyframe(handle) }
    func feedback(frame: UInt32, receivedUs: UInt64, decodedUs: UInt64, lost: UInt16) {
        rr_feedback(handle, frame, receivedUs, decodedUs, lost)
    }

    var stats: RemoteStats {
        var s = RrStats()
        rr_stats(handle, &s)
        return RemoteStats(frames: s.frames, keyframes: s.keyframes, bytes: s.bytes, framesLost: s.frames_lost,
                           fecRecovered: s.fec_recovered, rttMs: s.rtt_ms, clockOffsetUs: s.has_clock_offset ? s.clock_offset_us : nil,
                           transport: s.transport.map { String(cString: $0) } ?? "")
    }

    func close() { rr_close(handle) }
}

enum MouseButton: UInt8 {
    case left = 0, right = 1, middle = 2, back = 3, forward = 4
}

private func clamp16(_ v: Int) -> UInt16 { UInt16(max(0, min(Int(UInt16.max), v))) }
private func clampI16(_ v: Int) -> Int16 { Int16(max(Int(Int16.min), min(Int(Int16.max), v))) }
