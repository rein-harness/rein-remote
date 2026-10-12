import XCTest
@testable import Rein_Remote

/// The whole way against a real server, in the simulator: set RR_URL (xcodebuild test …
/// TEST_RUNNER_RR_URL='http://127.0.0.1:7979/?token=…', a server running `rein-remote serve
/// --test-pattern 1280x720`). Skipped without it.
final class StreamTests: XCTestCase {
    private func env(_ k: String) -> String? {
        ProcessInfo.processInfo.environment[k].flatMap { $0.isEmpty ? nil : $0 }
    }

    /// Connects, decodes with VideoToolbox, sends the pointer and sees the pattern's cross come back,
    /// measures (fps, glass to glass, sound): the self-test, its numbers in the log.
    func testStreamsAndControls() async throws {
        guard let url = env("RR_URL") else { throw XCTSkip("set RR_URL to run this") }
        let c = StreamController()
        c.measure = true
        var o = ConnectOptions()
        o.webtransport = env("RR_WEBTRANSPORT") != "off"
        o.audio = env("RR_AUDIO") != "off"
        // HEVC even where it isn't in hardware (the simulator decodes it in software).
        if env("RR_CODEC") == "HEVC" { o.codecs = [.h265, .h264] }
        try await c.connect(target: url, pairCode: nil, options: o)
        let line = await SelfTest.run(c, seconds: Double(env("RR_SECONDS") ?? "8") ?? 8)
        c.close()
        print(line)
        XCTAssertTrue(line.hasPrefix("selftest: PASS"), line)
        let m = c.metrics
        if let want = env("RR_CODEC") { XCTAssertTrue(m.decoder.hasPrefix(want), m.decoder) }
    }

    /// Pairing with the code a server shows (RR_PAIR_URL: its address with no token; RR_PAIR_CODE),
    /// then signing in with keys, with nothing to type.
    func testPairsThenSignsIn() async throws {
        guard let url = env("RR_PAIR_URL"), let code = env("RR_PAIR_CODE") else { throw XCTSkip("set RR_PAIR_URL and RR_PAIR_CODE") }
        for (i, pair) in [code, nil].enumerated() {
            let c = StreamController()
            try await c.connect(target: url, pairCode: pair, options: ConnectOptions())
            let end = Date().addingTimeInterval(20)
            while c.metrics.frames < 10 && Date() < end { try await Task.sleep(nanoseconds: 100_000_000) }
            c.close()
            XCTAssertGreaterThanOrEqual(c.metrics.frames, 10, i == 0 ? "frames after pairing" : "frames after signing in")
        }
    }
}

/// A real certificate checks out on iOS (the core brings Mozilla's roots: an app can't read the
/// system's): an HTTPS server that isn't rein-remote answers pairing with an HTTP error, not a
/// certificate one. Needs the internet: RR_NET=1.
final class TlsTests: XCTestCase {
    func testRealCertificatesCheckOut() throws {
        guard ProcessInfo.processInfo.environment["RR_NET"] == "1" else { throw XCTSkip("set RR_NET=1") }
        XCTAssertThrowsError(try Core.pair(url: "https://example.com/", code: "000-000", name: "t")) { e in
            let m = e.localizedDescription
            XCTAssertTrue(m.contains("HTTP") || m.contains("pairing"), m)
            XCTAssertFalse(m.lowercased().contains("certificate"), m)
            print("tls test: \(m)")
        }
    }
}
