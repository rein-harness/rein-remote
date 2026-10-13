// A real `rein-remote host --test-pattern 1280x720` on a home folder of its own, for the UI tests:
// started and stopped here, read through its control API, with viewers (`rein-remote connect`) to
// connect. The binary: TEST_RUNNER_RR_BIN (xcodebuild passes TEST_RUNNER_* to this runner as RR_BIN),
// else the workspace's target/release/rein-remote.

import Foundation
import XCTest

final class HostHarness {
    let bin: URL
    let home: URL
    let process = Process()
    private(set) var url = ""
    private(set) var token = ""
    private var viewers: [Process] = []
    private var clientHomes: [URL] = []

    static var binary: URL {
        if let b = ProcessInfo.processInfo.environment["RR_BIN"], !b.isEmpty { return URL(fileURLWithPath: b) }
        // clients/macos-host/ReinRemoteHostUITests/HostHarness.swift → the workspace's target/release.
        return URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("target/release/rein-remote")
    }

    init(extra: [String] = []) throws {
        bin = Self.binary
        guard FileManager.default.isExecutableFile(atPath: bin.path) else {
            throw XCTSkip("no rein-remote at \(bin.path): cargo build --release -p rr-server (or TEST_RUNNER_RR_BIN)")
        }
        home = FileManager.default.temporaryDirectory.appendingPathComponent("rr-host-ui-\(UUID().uuidString.prefix(8))")
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        try start(extra: extra)
    }

    private func start(extra: [String]) throws {
        process.executableURL = bin
        // Viewers on this computer only (a port of its own), no mDNS, the picture's size held.
        process.arguments = ["host", "--test-pattern", "1280x720", "--listen", "127.0.0.1:0", "--mdns", "off"] + extra
        var env = ProcessInfo.processInfo.environment
        env["REIN_REMOTE_HOME"] = home.path
        env["RR_ADAPT"] = "off"
        process.environment = env
        FileManager.default.createFile(atPath: home.appendingPathComponent("host.log").path, contents: nil)
        let logFile = try FileHandle(forWritingTo: home.appendingPathComponent("host.log"))
        process.standardError = logFile
        process.standardOutput = logFile
        try process.run()
        let deadline = Date().addingTimeInterval(20)
        while Date() < deadline {
            if let d = try? Data(contentsOf: home.appendingPathComponent("control.json")),
               let o = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
               (o["pid"] as? Int).map(Int32.init) == process.processIdentifier {
                url = o["url"] as? String ?? ""
                token = o["token"] as? String ?? ""
                if (try? api("GET", "/control/state")) != nil { return }
            }
            Thread.sleep(forTimeInterval: 0.1)
        }
        throw NSError(domain: "HostHarness", code: 1, userInfo: [NSLocalizedDescriptionKey: "the host didn't start: \(log())"])
    }

    func log() -> String {
        (try? String(contentsOf: home.appendingPathComponent("host.log"), encoding: .utf8)) ?? ""
    }

    /// One request to the control API; the answer's JSON (an error for a non-2xx).
    @discardableResult
    func api(_ method: String, _ path: String, _ body: [String: Any]? = nil) throws -> [String: Any] {
        var r = URLRequest(url: URL(string: url + path)!)
        r.httpMethod = method
        r.timeoutInterval = 20
        r.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        if let body {
            r.httpBody = try JSONSerialization.data(withJSONObject: body)
            r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let done = DispatchSemaphore(value: 0)
        var out: (Data?, URLResponse?, Error?)
        URLSession.shared.dataTask(with: r) { out = ($0, $1, $2); done.signal() }.resume()
        done.wait()
        if let e = out.2 { throw e }
        let status = (out.1 as? HTTPURLResponse)?.statusCode ?? 0
        let json = (try? JSONSerialization.jsonObject(with: out.0 ?? Data())) as? [String: Any] ?? [:]
        guard (200..<300).contains(status) else {
            throw NSError(domain: "HostHarness", code: status, userInfo: [NSLocalizedDescriptionKey: "\(method) \(path): \(status) \(json)"])
        }
        return json
    }

    func state() throws -> [String: Any] { try api("GET", "/control/state") }

    /// Sharing on; its first link.
    @discardableResult
    func share() throws -> String {
        var s = try state()
        if s["sharing"] as? Bool != true { s = try api("POST", "/control/sharing/start") }
        let links = s["links"] as? [[String: Any]] ?? []
        return links.first?["url"] as? String ?? ""
    }

    var hostToml: String { (try? String(contentsOf: home.appendingPathComponent("host.toml"), encoding: .utf8)) ?? "" }

    /// A viewer (`rein-remote connect`) that stays for `seconds`, with keys in a folder of its own.
    @discardableResult
    func connectViewer(_ target: String, name: String, pair: String? = nil, seconds: Int = 120) throws -> Process {
        let p = Process()
        p.executableURL = bin
        p.arguments = ["connect", target, "--seconds", String(seconds), "--name", name, "--webtransport", "off"] + (pair.map { ["--pair", $0] } ?? [])
        let h = home.appendingPathComponent("client-\(viewers.count)")
        try FileManager.default.createDirectory(at: h, withIntermediateDirectories: true)
        var env = ProcessInfo.processInfo.environment
        env["REIN_REMOTE_HOME"] = h.path
        p.environment = env
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try p.run()
        viewers.append(p)
        return p
    }

    /// A device paired with a code (it stays connected for a few seconds, then leaves).
    func pairDevice(name: String) throws {
        let link = try share()
        let page = (link.components(separatedBy: "/?").first ?? link) + "/"
        let code = try api("POST", "/control/pairing/code")["code"] as? String ?? ""
        let p = try connectViewer(page, name: name, pair: code, seconds: 3)
        p.waitUntilExit()
    }

    /// Waits for `f` (polling), up to `timeout`.
    func eventually(_ timeout: TimeInterval = 20, _ f: () throws -> Bool) -> Bool {
        let end = Date().addingTimeInterval(timeout)
        while Date() < end {
            if (try? f()) == true { return true }
            Thread.sleep(forTimeInterval: 0.2)
        }
        return false
    }

    /// Everything stopped: the viewers, this host, and any host the app started on this home.
    func stop() {
        for v in viewers where v.isRunning { v.terminate() }
        if process.isRunning { process.terminate(); process.waitUntilExit() }
        if let d = try? Data(contentsOf: home.appendingPathComponent("control.json")),
           let o = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
           let pid = o["pid"] as? Int, pid != Int(process.processIdentifier), pid > 1 {
            // Only if it's still a rein-remote (a pid can be reused).
            var buf = [CChar](repeating: 0, count: 4096)
            if proc_pidpath(Int32(pid), &buf, UInt32(buf.count)) > 0, String(cString: buf).hasSuffix("/rein-remote") {
                kill(pid_t(pid), SIGTERM)
            }
        }
        try? FileManager.default.removeItem(at: home)
    }
}
