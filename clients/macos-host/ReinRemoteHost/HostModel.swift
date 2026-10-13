// The app's view of the host: found through control.json, followed through /control/events, started
// as a child when nothing answers, and reconnected to when it restarts.

import AppKit
import Foundation
import Observation

enum Page: String, CaseIterable, Identifiable, Hashable {
    case sharing, viewers, devices, settings, permissions
    case signin, policy, sessions, audit, metrics
    var id: String { rawValue }

    var title: String {
        switch self {
        case .sharing: return "Sharing"
        case .viewers: return "Viewers"
        case .devices: return "Paired devices"
        case .settings: return "Settings"
        case .permissions: return "Permissions"
        case .signin: return "Sign-in"
        case .policy: return "Policy"
        case .sessions: return "Live sessions"
        case .audit: return "Audit log"
        case .metrics: return "Metrics"
        }
    }

    var symbol: String {
        switch self {
        case .sharing: return "dot.radiowaves.left.and.right"
        case .viewers: return "person.2"
        case .devices: return "iphone"
        case .settings: return "slider.horizontal.3"
        case .permissions: return "lock.shield"
        case .signin: return "person.badge.key"
        case .policy: return "list.bullet.rectangle"
        case .sessions: return "rectangle.stack.person.crop"
        case .audit: return "doc.text.magnifyingglass"
        case .metrics: return "chart.bar"
        }
    }

    static let main: [Page] = [.sharing, .viewers, .devices, .settings, .permissions]
    static let admin: [Page] = [.signin, .policy, .sessions, .audit, .metrics]
}

@MainActor
@Observable
final class HostModel {
    enum Connection: Equatable {
        /// Looking for the host (control.json, then a request).
        case connecting
        /// This app started it and is waiting for its API.
        case starting
        /// Nothing answers and this app isn't starting one: why, when known.
        case notRunning(String?)
        case connected
    }

    var connection: Connection = .connecting
    var state: HostState?
    var viewers: [Viewer] = []
    var devices: [Device] = []
    var settings: Settings?
    /// The last action's error, as the server wrote it (shown in a banner until dismissed).
    var lastError: String?
    var busy = false
    var page: Page = .sharing

    /// Starts `rein-remote host` when none answers.
    var autoStart: Bool {
        get { !UserDefaults.standard.bool(forKey: "noAutoStart") }
        set { UserDefaults.standard.set(!newValue, forKey: "noAutoStart") }
    }

    private(set) var client: ControlClient?
    private(set) var child: HostChild?
    private var launches: [Date] = []
    private var supervisor: Task<Void, Never>?

    var isConnected: Bool { connection == .connected && client != nil }
    var weStartedIt: Bool { child?.isRunning == true }

    var statusText: String {
        switch connection {
        case .connecting: return "Connecting…"
        case .starting: return "Starting…"
        case .notRunning: return "Not running"
        case .connected:
            guard let s = state else { return "Connecting…" }
            if s.sharing { return s.viewers == 1 ? "Sharing, 1 viewer" : "Sharing, \(s.viewers) viewers" }
            return s.error == nil ? "Not sharing" : "Not sharing (failed to start)"
        }
    }

    // MARK: Following the host

    func begin() {
        guard supervisor == nil else { return }
        supervisor = Task { [weak self] in await self?.supervise() }
    }

    func end() {
        supervisor?.cancel()
        child?.terminate()
    }

    private func supervise() async {
        var startedWaiting: Date?
        while !Task.isCancelled {
            if let file = HostFiles.readControl(), let c = try? ControlClient(file: file),
               let s = try? await c.state(timeout: 2) {
                startedWaiting = nil
                client = c
                state = s
                connection = .connected
                do {
                    try await c.events { [weak self] ev in await self?.apply(ev) }
                } catch {
                    NSLog("rein-remote events: %@", String(describing: error))
                }
                // The stream ended: the host exited (or restarted). Find it again.
                client = nil
                connection = .connecting
                try? await Task.sleep(for: .milliseconds(300))
                continue
            }
            client = nil
            if let child, child.isRunning {
                connection = .starting
                if startedWaiting == nil { startedWaiting = Date() }
                if let t = startedWaiting, Date().timeIntervalSince(t) > 30 {
                    // Running but no API: say what it printed.
                    connection = .notRunning(child.lastMessage ?? "rein-remote host didn't start its control API")
                }
            } else if autoStart, canLaunch() {
                launch()
                startedWaiting = Date()
            } else {
                connection = .notRunning(child?.lastMessage ?? (HostFiles.serverBinary == nil ? "rein-remote isn't in this app" : nil))
            }
            try? await Task.sleep(for: .milliseconds(connection == .starting ? 250 : 1000))
        }
    }

    /// No more than three starts a minute: a host that exits at once is shown, not restarted forever.
    private func canLaunch() -> Bool {
        launches = launches.filter { Date().timeIntervalSince($0) < 60 }
        return launches.count < 3 && HostFiles.serverBinary != nil
    }

    private func launch() {
        guard let bin = HostFiles.serverBinary else { return }
        launches.append(Date())
        let c = HostChild(binary: bin)
        do {
            try c.start()
            child = c
            connection = .starting
        } catch {
            connection = .notRunning("couldn't start \(bin.path): \(error.localizedDescription)")
        }
    }

    /// "Start" when it's not running: start it now, whatever the last tries did.
    func startHost() {
        launches.removeAll()
        if child?.isRunning != true { launch() }
    }

    /// Restarts the host this app started (macOS applies a new Screen Recording grant on a restart).
    func restartHost() {
        guard let child, child.isRunning else { return }
        child.terminate()
        launches.removeAll()
    }

    private func apply(_ ev: HostEvent) {
        switch ev {
        case .state(let s): state = s
        case .viewers(let v): viewers = v
        case .devices(let d): devices = d
        case .settings(let s): settings = s
        }
    }

    // MARK: Actions

    /// One call to the API; its error (the server's text) kept for the banner, or for the caller.
    @discardableResult
    func call<T>(showError: Bool = true, _ op: (ControlClient) async throws -> T) async -> Result<T, ControlError> {
        guard let client else {
            let e = ControlError.unreachable("not connected")
            if showError { lastError = e.localizedDescription }
            return .failure(e)
        }
        busy = true
        defer { busy = false }
        do {
            let v = try await op(client)
            if showError { lastError = nil }
            return .success(v)
        } catch let e as ControlError {
            if showError { lastError = e.localizedDescription }
            return .failure(e)
        } catch {
            let e = ControlError.unreachable(error.localizedDescription)
            if showError { lastError = e.localizedDescription }
            return .failure(e)
        }
    }

    func toggleSharing() async {
        guard let s = state else { return }
        let r = await call { s.sharing ? try await $0.stopSharing() : try await $0.startSharing() }
        if case .success(let n) = r { state = n }
    }

    func restartSharing() async {
        if case .success(let n) = await call({ try await $0.restartSharing() }) { state = n }
    }

    func setSource(_ id: String) async {
        if case .success(let n) = await call({ try await $0.setSource(id) }) { state = n }
    }

    func newCode() async {
        if case .success(let p) = await call({ try await $0.newPairingCode() }) {
            state?.pairing.code = p.code
            state?.pairing.expiresAt = p.expiresAt
            state?.pairing.url = p.url
            state?.pairing.qr = p.qr
        }
    }

    func kick(_ id: String) async {
        if case .success(let v) = await call({ try await $0.kick(id) }) { viewers = v.viewers }
    }

    func unpair(_ id: String) async {
        if case .success(let d) = await call({ try await $0.unpair(id) }) { devices = d.devices }
    }

    /// A partial settings change; the answer (with `applied`) for the caller to show.
    func putSettings(_ changes: [String: JSONValue]) async -> Result<Settings, ControlError> {
        let r = await call(showError: false) { try await $0.putSettings(changes) }
        if case .success(let s) = r { settings = s }
        return r
    }
}

extension Settings {
    var appliedText: String? { ReinRemoteHost.appliedText(applied) }
}

func appliedText(_ applied: String?) -> String? {
    switch applied {
    case "saved": return "Saved. Used the next time sharing starts."
    case "restarted": return "Saved. Sharing restarted to apply it."
    case "on_restart": return "Saved. Viewers are connected: it applies when sharing restarts."
    default: return nil
    }
}
