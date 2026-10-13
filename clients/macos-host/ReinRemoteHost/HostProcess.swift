// Where the host is: its home folder and control.json (as the host itself finds them), the
// rein-remote binary inside this app, and the child process this app starts when no host answers.

import Foundation

enum HostFiles {
    /// `$REIN_REMOTE_HOME`, else `~/.rein-remote` (the host's own rule on macOS).
    static var home: URL {
        if let h = ProcessInfo.processInfo.environment["REIN_REMOTE_HOME"], !h.isEmpty {
            return URL(fileURLWithPath: (h as NSString).expandingTildeInPath, isDirectory: true)
        }
        return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".rein-remote", isDirectory: true)
    }

    static var controlFile: URL { home.appendingPathComponent("control.json") }

    static func readControl() -> ControlFile? {
        guard let data = try? Data(contentsOf: controlFile) else { return nil }
        return try? JSONDecoder().decode(ControlFile.self, from: data)
    }

    /// The server binary: `$REIN_REMOTE_BIN`, else the one in this app (Contents/MacOS, or Helpers).
    static var serverBinary: URL? {
        let fm = FileManager.default
        if let b = ProcessInfo.processInfo.environment["REIN_REMOTE_BIN"], fm.isExecutableFile(atPath: b) {
            return URL(fileURLWithPath: b)
        }
        if let u = Bundle.main.url(forAuxiliaryExecutable: "rein-remote"), fm.isExecutableFile(atPath: u.path) {
            return u
        }
        let helper = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/rein-remote")
        return fm.isExecutableFile(atPath: helper.path) ? helper : nil
    }
}

/// `rein-remote host` as this app's child (so macOS asks about permissions for this app, its
/// responsible process), with the last lines it printed kept to show when it stops.
final class HostChild: @unchecked Sendable {
    let process = Process()
    private let lock = NSLock()
    private var lines: [String] = []
    private var partial = Data()

    init(binary: URL, arguments: [String] = []) {
        process.executableURL = binary
        process.arguments = ["host"] + arguments
        process.environment = ProcessInfo.processInfo.environment
        let pipe = Pipe()
        process.standardError = pipe
        process.standardOutput = pipe
        process.standardInput = FileHandle.nullDevice
        pipe.fileHandleForReading.readabilityHandler = { [weak self] h in
            let d = h.availableData
            if d.isEmpty { h.readabilityHandler = nil; return }
            self?.take(d)
        }
    }

    private func take(_ d: Data) {
        lock.lock()
        defer { lock.unlock() }
        partial.append(d)
        while let nl = partial.firstIndex(of: UInt8(ascii: "\n")) {
            let line = String(decoding: partial[partial.startIndex..<nl], as: UTF8.self)
            partial.removeSubrange(partial.startIndex...nl)
            // Its per-second statistics are JSON lines: not something to show.
            if !line.hasPrefix("rein-remote: {") { lines.append(line) }
        }
        if lines.count > 40 { lines.removeFirst(lines.count - 40) }
    }

    var output: [String] { lock.lock(); defer { lock.unlock() }; return lines }

    /// What it said last, without the `rein-remote: ` prefix: why it stopped, usually.
    var lastMessage: String? {
        output.last { !$0.trimmingCharacters(in: .whitespaces).isEmpty }.map {
            $0.hasPrefix("rein-remote: ") ? String($0.dropFirst("rein-remote: ".count)) : $0
        }
    }

    func start() throws { try process.run() }
    var isRunning: Bool { process.isRunning }
    var pid: Int32 { process.processIdentifier }
    func terminate() { if process.isRunning { process.terminate() } }
}
