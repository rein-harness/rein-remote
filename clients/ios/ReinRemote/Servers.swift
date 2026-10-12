import Foundation
import Network

/// A server found on this network (Bonjour, `_rein-remote._tcp`): what `rein-remote serve` advertises
/// when it listens beyond its own machine. Network.framework does the browsing, which is what iOS
/// allows without the multicast entitlement (the core's own mDNS needs raw multicast sockets).
struct FoundServer: Identifiable, Hashable {
    let name: String
    /// Its key's fingerprint (what a pairing is kept under).
    let id: String
    let endpoint: NWEndpoint
    var webtransport: Int?
}

final class Discovery: ObservableObject {
    @Published private(set) var found: [FoundServer] = []
    @Published private(set) var error: String?
    private var browser: NWBrowser?

    func start() {
        guard browser == nil else { return }
        let b = NWBrowser(for: .bonjourWithTXTRecord(type: "_rein-remote._tcp", domain: nil), using: .tcp)
        b.browseResultsChangedHandler = { [weak self] results, _ in
            let list = results.compactMap { r -> FoundServer? in
                guard case let .service(name, _, _, _) = r.endpoint else { return nil }
                var id = "", wt: Int?
                if case let .bonjour(txt) = r.metadata {
                    id = txt["id"] ?? ""
                    wt = txt["wt"].flatMap(Int.init).flatMap { $0 == 0 ? nil : $0 }
                }
                return FoundServer(name: name, id: id, endpoint: r.endpoint, webtransport: wt)
            }
            DispatchQueue.main.async { self?.found = list.sorted { $0.name < $1.name } }
        }
        b.stateUpdateHandler = { [weak self] state in
            if case let .failed(e) = state { DispatchQueue.main.async { self?.error = e.localizedDescription } }
            if case let .waiting(e) = state { DispatchQueue.main.async { self?.error = e.localizedDescription } }
        }
        b.start(queue: .main)
        browser = b
    }

    func stop() {
        browser?.cancel()
        browser = nil
    }

    /// The server's link (http://address:port/), resolved by connecting to it once over TCP (an IPv4
    /// address first: the core takes those everywhere).
    static func resolve(_ s: FoundServer) async throws -> String {
        do {
            return try await resolve(s.endpoint, v4: true)
        } catch {
            return try await resolve(s.endpoint, v4: false)
        }
    }

    private static func resolve(_ e: NWEndpoint, v4: Bool) async throws -> String {
        let params = NWParameters.tcp
        if v4, let ip = params.defaultProtocolStack.internetProtocol as? NWProtocolIP.Options { ip.version = .v4 }
        let c = NWConnection(to: e, using: params)
        return try await withCheckedThrowingContinuation { cont in
            var done = false
            let finish: (Result<String, Error>) -> Void = { r in
                guard !done else { return }
                done = true
                c.cancel()
                cont.resume(with: r)
            }
            c.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    if case let .hostPort(host, port)? = c.currentPath?.remoteEndpoint {
                        finish(.success("http://\(Self.hostString(host)):\(port.rawValue)/"))
                    } else {
                        finish(.failure(CoreError("no address for it")))
                    }
                case let .failed(err): finish(.failure(err))
                case let .waiting(err): finish(.failure(err))
                default: break
                }
            }
            c.start(queue: .global())
            DispatchQueue.global().asyncAfter(deadline: .now() + 5) { finish(.failure(CoreError("it didn't answer"))) }
        }
    }

    static func hostString(_ h: NWEndpoint.Host) -> String {
        switch h {
        case let .ipv4(a): return "\(a)".components(separatedBy: "%")[0]
        case let .ipv6(a): return "[\("\(a)".components(separatedBy: "%")[0])]"
        case let .name(n, _): return n
        @unknown default: return "\(h)"
        }
    }
}

/// What the QR code a server prints holds, and what a pasted link may: `http://host:port/#pair=CODE&id=FINGERPRINT`
/// (pair with the code, then sign in with keys), or a link with its token, or a relay's link.
struct PairLink: Equatable {
    /// Where to connect: the link without its fragment when it pairs, else the whole link.
    let url: String
    let code: String?
    let id: String?

    init?(_ text: String) {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let comps = URLComponents(string: t), let scheme = comps.scheme, ["http", "https", "ws", "wss"].contains(scheme.lowercased()),
              comps.host != nil else { return nil }
        var fields: [String: String] = [:]
        for part in (comps.fragment ?? "").split(separator: "&") {
            let kv = part.split(separator: "=", maxSplits: 1).map(String.init)
            if kv.count == 2 { fields[kv[0]] = kv[1].removingPercentEncoding ?? kv[1] }
        }
        if let code = fields["pair"] {
            var base = comps
            base.fragment = nil
            url = base.string ?? t
            self.code = code
            id = fields["id"]
        } else {
            url = t
            code = nil
            id = nil
        }
    }
}

/// Servers kept on this device: ones paired with (by fingerprint, found again on the network by it)
/// and links added by hand.
struct SavedServer: Codable, Identifiable, Hashable {
    var name: String
    var url: String
    /// The fingerprint, for a paired one.
    var serverId: String?
    var id: String { serverId ?? url }
}

final class SavedServers: ObservableObject {
    @Published private(set) var list: [SavedServer] = []
    private let key = "servers"

    init() {
        if let d = UserDefaults.standard.data(forKey: key), let l = try? JSONDecoder().decode([SavedServer].self, from: d) { list = l }
    }

    func add(_ s: SavedServer) {
        list.removeAll { $0.id == s.id }
        list.insert(s, at: 0)
        save()
    }

    func remove(_ s: SavedServer) {
        list.removeAll { $0.id == s.id }
        save()
    }

    private func save() {
        if let d = try? JSONEncoder().encode(list) { UserDefaults.standard.set(d, forKey: key) }
    }
}
