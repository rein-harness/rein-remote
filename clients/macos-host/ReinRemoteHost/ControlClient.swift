// The control API over HTTP: the bearer token from control.json on every request, JSON both ways,
// and the server's `{"error": "…"}` turned into an error a person can read.

import Foundation

enum ControlError: LocalizedError, Equatable {
    /// The API answered with an error: its status and its message, shown as it is.
    case server(status: Int, message: String)
    /// No answer: the host isn't running (or just stopped).
    case unreachable(String)
    /// An answer this app doesn't understand.
    case decoding(String)

    var errorDescription: String? {
        switch self {
        case .server(_, let message): return message
        case .unreachable(let why): return "The host isn't answering (\(why))."
        case .decoding(let why): return "The host's answer wasn't understood: \(why)"
        }
    }

    var status: Int? { if case .server(let s, _) = self { return s } else { return nil } }
}

struct ControlClient: Sendable {
    let base: URL
    let token: String
    let session: URLSession

    init(file: ControlFile, session: URLSession = ControlClient.makeSession()) throws {
        guard let url = URL(string: file.url) else { throw ControlError.unreachable("control.json's url: \(file.url)") }
        base = url
        token = file.token
        self.session = session
    }

    static func makeSession() -> URLSession {
        let c = URLSessionConfiguration.ephemeral
        c.timeoutIntervalForRequest = 15
        c.requestCachePolicy = .reloadIgnoringLocalCacheData
        c.httpShouldUsePipelining = false
        c.connectionProxyDictionary = [:] // never through a proxy: this is 127.0.0.1
        return URLSession(configuration: c)
    }

    func request(_ method: String, _ path: String, query: [URLQueryItem] = [], body: Data? = nil, timeout: TimeInterval? = nil) -> URLRequest {
        var comps = URLComponents(url: base.appendingPathComponent(path), resolvingAgainstBaseURL: false)!
        if !query.isEmpty { comps.queryItems = query }
        var r = URLRequest(url: comps.url!)
        r.httpMethod = method
        r.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            r.httpBody = body
            r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        if let timeout { r.timeoutInterval = timeout }
        return r
    }

    /// One request; the answer decoded as `T`, or the server's error.
    func send<T: Decodable>(_ method: String, _ path: String, query: [URLQueryItem] = [], body: Data? = nil, timeout: TimeInterval? = nil, as _: T.Type = T.self) async throws -> T {
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request(method, path, query: query, body: body, timeout: timeout))
        } catch {
            throw ControlError.unreachable(error.localizedDescription)
        }
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        return try Self.decode(status: status, data: data)
    }

    /// An answer: `T` for a 2xx, the server's message otherwise.
    static func decode<T: Decodable>(status: Int, data: Data, as _: T.Type = T.self) throws -> T {
        guard (200..<300).contains(status) else {
            if let e = try? ControlJSON.decoder.decode(ErrorBody.self, from: data) {
                throw ControlError.server(status: status, message: e.error)
            }
            throw ControlError.server(status: status, message: "HTTP \(status)")
        }
        do {
            return try ControlJSON.decoder.decode(T.self, from: data)
        } catch {
            throw ControlError.decoding(String(describing: error))
        }
    }

    func body<B: Encodable>(_ b: B) -> Data { (try? ControlJSON.encoder.encode(b)) ?? Data("{}".utf8) }

    // MARK: Endpoints

    func state(timeout: TimeInterval? = nil) async throws -> HostState { try await send("GET", "/control/state", timeout: timeout) }
    func startSharing() async throws -> HostState { try await send("POST", "/control/sharing/start") }
    func stopSharing(reason: String? = nil) async throws -> HostState {
        try await send("POST", "/control/sharing/stop", body: reason.map { body(ReasonBody(reason: $0)) })
    }
    func restartSharing() async throws -> HostState { try await send("POST", "/control/sharing/restart") }
    func sources() async throws -> Sources { try await send("GET", "/control/sources", timeout: 20) }
    func setSource(_ id: String) async throws -> HostState { try await send("PUT", "/control/source", body: body(SourceBody(source: id))) }
    func pairing() async throws -> Pairing { try await send("GET", "/control/pairing") }
    func newPairingCode() async throws -> Pairing { try await send("POST", "/control/pairing/code") }
    func devices() async throws -> Devices { try await send("GET", "/control/devices") }
    func unpair(_ id: String) async throws -> Devices {
        try await send("DELETE", "/control/devices/\(id.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? id)")
    }
    func viewers() async throws -> Viewers { try await send("GET", "/control/viewers") }
    func kick(_ id: String, reason: String? = nil) async throws -> Viewers {
        try await send("POST", "/control/viewers/\(id)/kick", body: reason.map { body(ReasonBody(reason: $0)) })
    }
    func settings() async throws -> Settings { try await send("GET", "/control/settings") }
    /// A partial change: only the keys given (`.null` puts one back to its default).
    func putSettings(_ changes: [String: JSONValue]) async throws -> Settings {
        try await send("PUT", "/control/settings", body: body(changes))
    }
    func startAtLogin() async throws -> StartAtLogin { try await send("GET", "/control/start-at-login") }
    func setStartAtLogin(_ on: Bool) async throws -> StartAtLogin {
        try await send("PUT", "/control/start-at-login", body: body(["enabled": on]))
    }
    func permissions() async throws -> Permissions { try await send("GET", "/control/permissions") }
    /// Where System Settings shows a permission; `open: false` leaves the opening to this app.
    func permissionURL(_ which: String) async throws -> PermissionOpen {
        try await send("POST", "/control/permissions/open", body: body(["which": JSONValue.string(which), "open": .bool(false)]))
    }
    func signIn() async throws -> SignIn { try await send("GET", "/control/admin/signin") }
    func putSignIn(_ changes: [String: JSONValue]) async throws -> SignIn {
        try await send("PUT", "/control/admin/signin", body: body(changes))
    }
    func policy() async throws -> Policy { try await send("GET", "/control/admin/policy") }
    func putPolicy(text: String?, format: String?) async throws -> Policy {
        try await send("PUT", "/control/admin/policy", body: body(PolicyBody(text: text, format: format)))
    }
    func sessions() async throws -> Sessions { try await send("GET", "/control/admin/sessions") }
    func audit(_ q: AuditQuery) async throws -> AuditPage { try await send("GET", "/control/admin/audit", query: q.queryItems) }
    func metrics() async throws -> Metrics { try await send("GET", "/control/admin/metrics") }
}

// MARK: Events

/// One Server-Sent Event: its name and its data (a full object, never a diff).
struct ServerEvent: Equatable {
    var name: String
    var data: String
}

/// Reads `text/event-stream` a line at a time: `event:` and `data:` lines until a blank one, comments
/// (`: keepalive`) ignored.
struct EventParser {
    private var name = ""
    private var data: [String] = []

    /// One line (without its newline); an event when a blank line ends one.
    mutating func feed(_ rawLine: String) -> ServerEvent? {
        let line = rawLine.hasSuffix("\r") ? String(rawLine.dropLast()) : rawLine
        if line.isEmpty {
            defer { name = ""; data = [] }
            guard !data.isEmpty else { return nil }
            return ServerEvent(name: name.isEmpty ? "message" : name, data: data.joined(separator: "\n"))
        }
        if line.hasPrefix(":") { return nil }
        let (field, value): (Substring, Substring)
        if let colon = line.firstIndex(of: ":") {
            field = line[..<colon]
            var v = line[line.index(after: colon)...]
            if v.hasPrefix(" ") { v = v.dropFirst() }
            value = v
        } else {
            field = Substring(line)
            value = ""
        }
        switch field {
        case "event": name = String(value)
        case "data": data.append(String(value))
        default: break
        }
        return nil
    }
}

/// What an event carries, decoded.
enum HostEvent {
    case state(HostState)
    case viewers([Viewer])
    case devices([Device])
    case settings(Settings)

    static func decode(_ e: ServerEvent) throws -> HostEvent? {
        let data = Data(e.data.utf8)
        let d = ControlJSON.decoder
        switch e.name {
        case "state": return .state(try d.decode(HostState.self, from: data))
        case "viewers": return .viewers(try d.decode(Viewers.self, from: data).viewers)
        case "devices": return .devices(try d.decode(Devices.self, from: data).devices)
        case "settings": return .settings(try d.decode(Settings.self, from: data))
        default: return nil
        }
    }
}

extension ControlClient {
    /// `GET /control/events`, until the stream ends (the host exited) or the task is cancelled.
    func events(_ onEvent: @escaping @Sendable (HostEvent) async -> Void) async throws {
        var r = request("GET", "/control/events")
        r.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        r.timeoutInterval = 60 // a keepalive comes every 15 s
        let (bytes, response) = try await session.bytes(for: r)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        if status != 200 {
            var data = Data()
            for try await b in bytes { data.append(b) }
            let _: JSONValue = try Self.decode(status: status, data: data)
            return
        }
        var parser = EventParser()
        var line = Data()
        // Byte by byte (AsyncBytes buffers): `lines` would drop the blank lines that end events.
        for try await b in bytes {
            if b == UInt8(ascii: "\n") {
                if let e = parser.feed(String(decoding: line, as: UTF8.self)), let ev = try? HostEvent.decode(e) {
                    await onEvent(ev)
                }
                line.removeAll(keepingCapacity: true)
            } else {
                line.append(b)
            }
        }
    }
}
