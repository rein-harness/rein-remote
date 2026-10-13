// The control API's objects (docs/control-api.md), as the host sends them. Keys that mirror
// host.toml are the flags' names (`relay-secret-set`); the rest are snake_case.

import Foundation

/// `control.json`: where the API is and its token.
struct ControlFile: Codable, Equatable {
    var url: String
    var token: String
    var pid: Int32?
    var version: String?
    var api: Int?
}

/// Any JSON value: for audit lines (whatever keys they have) and partial PUT bodies.
enum JSONValue: Codable, Equatable, Hashable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .null
        } else if let b = try? c.decode(Bool.self) {
            self = .bool(b)
        } else if let n = try? c.decode(Double.self) {
            self = .number(n)
        } else if let s = try? c.decode(String.self) {
            self = .string(s)
        } else if let a = try? c.decode([JSONValue].self) {
            self = .array(a)
        } else {
            self = .object(try c.decode([String: JSONValue].self))
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let b): try c.encode(b)
        case .number(let n):
            if n == n.rounded(), abs(n) < 1e15 { try c.encode(Int64(n)) } else { try c.encode(n) }
        case .string(let s): try c.encode(s)
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        }
    }

    var string: String? { if case .string(let s) = self { return s } else { return nil } }

    /// As a person reads it in a table.
    var display: String {
        switch self {
        case .null: return "–"
        case .bool(let b): return b ? "yes" : "no"
        case .number(let n): return n == n.rounded() && abs(n) < 1e15 ? String(Int64(n)) : String(format: "%.1f", n)
        case .string(let s): return s
        case .array(let a): return a.map(\.display).joined(separator: ", ")
        case .object(let o): return o.keys.sorted().map { "\($0)=\(o[$0]!.display)" }.joined(separator: " ")
        }
    }
}

/// `"auto"` or a number (fps, bitrate).
enum AutoOrNumber: Codable, Equatable, Hashable {
    case auto
    case value(Int)

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if let n = try? c.decode(Int.self) {
            self = .value(n)
        } else if let s = try? c.decode(String.self) {
            if s == "auto" { self = .auto } else if let n = Int(s) { self = .value(n) } else {
                throw DecodingError.dataCorruptedError(in: c, debugDescription: "auto or a number, not \(s)")
            }
        } else {
            throw DecodingError.dataCorruptedError(in: c, debugDescription: "auto or a number")
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .auto: try c.encode("auto")
        case .value(let n): try c.encode(n)
        }
    }

    var jsonValue: JSONValue {
        switch self {
        case .auto: return .string("auto")
        case .value(let n): return .number(Double(n))
        }
    }
}

// MARK: State

struct Link: Codable, Equatable, Hashable, Identifiable {
    var kind: String
    var label: String
    var url: String
    var qr: String
    var id: String { url }
}

struct Pairing: Codable, Equatable {
    var enabled: Bool
    var code: String?
    var expiresAt: Date?
    var expiresInS: Int?
    var url: String?
    var qr: String?
    var fingerprint: String?

    enum CodingKeys: String, CodingKey {
        case enabled, code, url, qr, fingerprint
        case expiresAt = "expires_at"
        case expiresInS = "expires_in_s"
    }
}

struct HostState: Codable, Equatable {
    var api: Int
    var version: String
    var pid: Int32
    var sharing: Bool
    var error: String?
    var source: String
    var listen: String
    var links: [Link]
    var pairing: Pairing
    var viewers: Int
    var restartNeeded: Bool

    enum CodingKeys: String, CodingKey {
        case api, version, pid, sharing, error, source, listen, links, pairing, viewers
        case restartNeeded = "restart_needed"
    }
}

// MARK: Sources

struct Sources: Codable, Equatable {
    struct Display: Codable, Equatable, Hashable { var id: String; var name: String; var width: Int?; var height: Int? }
    struct Window: Codable, Equatable, Hashable { var id: String; var app: String; var title: String; var width: Int?; var height: Int? }
    struct App: Codable, Equatable, Hashable { var id: String; var name: String; var windows: Int? }
    struct Other: Codable, Equatable, Hashable { var id: String; var name: String }

    var displays: [Display]
    var windows: [Window]
    var apps: [App]
    var other: [Other]
    var error: String?
}

struct SourceBody: Codable { var source: String }

// MARK: Devices and viewers

struct Device: Codable, Equatable, Identifiable {
    var id: String
    var name: String
    var since: Date?
}

struct Devices: Codable, Equatable { var devices: [Device] }

struct Viewer: Codable, Equatable, Identifiable {
    var id: String
    var device: String?
    var user: String?
    var via: String?
    var transport: String?
    var addr: String?
    var fps: Double?
    var rttMs: Double?
    var kbps: Double?
    var width: Int?
    var height: Int?
    var since: Date?
    var viewOnly: Bool?

    enum CodingKeys: String, CodingKey {
        case id, device, user, via, transport, addr, fps, kbps, width, height, since
        case rttMs = "rtt_ms"
        case viewOnly = "view_only"
    }
}

struct Viewers: Codable, Equatable { var viewers: [Viewer] }

/// A live session with the policy's view of it (`GET /control/admin/sessions`).
struct Session: Codable, Equatable, Identifiable {
    var id: String
    var device: String?
    var user: String?
    var via: String?
    var transport: String?
    var addr: String?
    var since: Date?
    var viewOnly: Bool?
    var groups: [String]?
    var rule: String?
    var clipboard: Bool?
    var audio: Bool?
    var recorded: Bool?
    var endsAt: Date?

    enum CodingKeys: String, CodingKey {
        case id, device, user, via, transport, addr, since, groups, rule, clipboard, audio, recorded
        case viewOnly = "view_only"
        case endsAt = "ends_at"
    }
}

struct Sessions: Codable, Equatable { var sessions: [Session] }

struct ReasonBody: Codable { var reason: String? }

// MARK: Settings

struct SettingsValues: Codable, Equatable {
    var prefer: String
    var fps: AutoOrNumber
    var bitrate: AutoOrNumber
    var codec: String
    var audio: Bool
    var listen: String
    var mdns: Bool
    var pair: Bool
    var relay: [String]
    var relaySecretSet: Bool
    var webtransport: Bool
    var tailscaleCert: Bool
    var record: String?
    var audit: String?

    enum CodingKeys: String, CodingKey {
        case prefer, fps, bitrate, codec, audio, listen, mdns, pair, relay, webtransport, record, audit
        case relaySecretSet = "relay-secret-set"
        case tailscaleCert = "tailscale-cert"
    }
}

struct Settings: Codable, Equatable {
    var settings: SettingsValues
    var sharing: Bool
    var source: String
    var path: String
    var overridden: [String]
    var restartNeeded: Bool
    /// Only in a PUT's answer: `saved`, `restarted` or `on_restart`.
    var applied: String?

    enum CodingKeys: String, CodingKey {
        case settings, sharing, source, path, overridden, applied
        case restartNeeded = "restart_needed"
    }
}

// MARK: Start at login, permissions

struct StartAtLogin: Codable, Equatable {
    var enabled: Bool
    var kind: String?
    var name: String?
    var path: String?
}

struct Permissions: Codable, Equatable {
    var platform: String
    var screenRecording: String
    var accessibility: String

    enum CodingKeys: String, CodingKey {
        case platform, accessibility
        case screenRecording = "screen_recording"
    }
}

struct PermissionOpen: Codable, Equatable {
    var opened: Bool
    var url: String?
}

// MARK: Admin

struct SignIn: Codable, Equatable {
    var kind: String?
    var publicURL: String?
    var oidcIssuer: String?
    var oidcClientID: String?
    var oidcClientSecretSet: Bool
    var oidcScopes: String?
    var oidcUserClaim: String?
    var oidcGroupsClaim: String?
    var samlIdpMetadata: String?
    var samlEntityID: String?
    var samlUserAttr: String?
    var samlGroupsAttr: String?
    var samlAvailable: Bool
    var applied: String?
    var restartNeeded: Bool?

    enum CodingKeys: String, CodingKey {
        case kind, applied
        case publicURL = "public-url"
        case oidcIssuer = "oidc-issuer"
        case oidcClientID = "oidc-client-id"
        case oidcClientSecretSet = "oidc-client-secret-set"
        case oidcScopes = "oidc-scopes"
        case oidcUserClaim = "oidc-user-claim"
        case oidcGroupsClaim = "oidc-groups-claim"
        case samlIdpMetadata = "saml-idp-metadata"
        case samlEntityID = "saml-entity-id"
        case samlUserAttr = "saml-user-attr"
        case samlGroupsAttr = "saml-groups-attr"
        case samlAvailable = "saml_available"
        case restartNeeded = "restart_needed"
    }
}

struct Policy: Codable, Equatable {
    var enabled: Bool
    var path: String?
    var format: String?
    var rules: Int
    var text: String?
    var applied: String?
}

struct PolicyBody: Codable, Equatable {
    var text: String?
    var format: String?

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(text, forKey: .text) // null turns the policy off: keep the key
        try c.encodeIfPresent(format, forKey: .format)
    }
}

struct AuditPage: Codable, Equatable {
    var path: String?
    var entries: [[String: JSONValue]]
    var next: String?
}

struct AuditQuery: Equatable {
    var events: [String] = []
    var user: String = ""
    var since: String = ""
    var limit: Int = 50
    var before: String?

    var queryItems: [URLQueryItem] {
        var q: [URLQueryItem] = []
        if !events.isEmpty { q.append(URLQueryItem(name: "event", value: events.joined(separator: ","))) }
        let u = user.trimmingCharacters(in: .whitespaces)
        if !u.isEmpty { q.append(URLQueryItem(name: "user", value: u)) }
        let s = since.trimmingCharacters(in: .whitespaces)
        if !s.isEmpty { q.append(URLQueryItem(name: "since", value: s)) }
        q.append(URLQueryItem(name: "limit", value: String(limit)))
        if let before { q.append(URLQueryItem(name: "before", value: before)) }
        return q
    }

    static let eventTypes = [
        "signin", "signin_denied", "signout", "session_start", "session_end", "session_refused", "session_kicked",
        "pair", "pair_failed", "device_signin", "device_signin_failed", "unpair", "server_start", "sharing_start",
        "sharing_stop", "settings_changed", "policy_changed", "signin_changed",
    ]
}

struct Metrics: Codable, Equatable {
    struct ViewerCounts: Codable, Equatable { var direct: Int; var relayed: Int }
    struct Relay: Codable, Equatable, Identifiable {
        var relay: String
        var up: Bool
        var rttMs: Double?
        var registered: Bool?
        var registrations: Int?
        var id: String { relay }
        enum CodingKeys: String, CodingKey {
            case relay, up, registered, registrations
            case rttMs = "rtt_ms"
        }
    }

    var version: String
    var uptimeS: Double
    var viewers: ViewerCounts
    var sessionsTotal: Int
    var framesSentTotal: Int
    var keyframesSentTotal: Int
    var videoBytesSentTotal: Int
    var framesSkippedTotal: Int
    var directOffersTotal: Int
    var directAnswersTotal: Int
    var relays: [Relay]

    enum CodingKeys: String, CodingKey {
        case version, viewers, relays
        case uptimeS = "uptime_s"
        case sessionsTotal = "sessions_total"
        case framesSentTotal = "frames_sent_total"
        case keyframesSentTotal = "keyframes_sent_total"
        case videoBytesSentTotal = "video_bytes_sent_total"
        case framesSkippedTotal = "frames_skipped_total"
        case directOffersTotal = "direct_offers_total"
        case directAnswersTotal = "direct_answers_total"
    }
}

/// Every error the API answers: `{"error": "…"}`.
struct ErrorBody: Codable { var error: String }

// MARK: Decoding

enum ControlJSON {
    /// RFC 3339 in UTC, with or without fractions of a second.
    static func date(_ s: String) -> Date? {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = f.date(from: s) { return d }
        f.formatOptions = [.withInternetDateTime]
        return f.date(from: s)
    }

    static var decoder: JSONDecoder {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .custom { decoder in
            let c = try decoder.singleValueContainer()
            let s = try c.decode(String.self)
            guard let date = date(s) else {
                throw DecodingError.dataCorruptedError(in: c, debugDescription: "not an RFC 3339 time: \(s)")
            }
            return date
        }
        return d
    }

    static var encoder: JSONEncoder {
        let e = JSONEncoder()
        e.outputFormatting = [.sortedKeys]
        return e
    }
}
