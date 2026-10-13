// The API client against the examples in docs/control-api.md (copied into Fixtures/; where the doc
// writes "…" for more keys, they're spelled out), plus two answers captured from a real host
// (state-not-sharing.json, metrics-fresh.json).

import XCTest
@testable import ReinRemoteHost

final class DecodingTests: XCTestCase {
    private func fixture(_ name: String) throws -> Data {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: name, withExtension: nil, subdirectory: "Fixtures")
            ?? Bundle(for: Self.self).url(forResource: name, withExtension: nil), "no fixture \(name)")
        return try Data(contentsOf: url)
    }

    private func decode<T: Decodable>(_ name: String, as _: T.Type = T.self) throws -> T {
        try ControlClient.decode(status: 200, data: try fixture(name))
    }

    func testControlFile() throws {
        let c = try JSONDecoder().decode(ControlFile.self, from: try fixture("control.json"))
        XCTAssertEqual(c.url, "http://127.0.0.1:52817")
        XCTAssertEqual(c.pid, 41237)
        XCTAssertEqual(c.api, 1)
        let client = try ControlClient(file: c)
        let r = client.request("GET", "/control/state")
        XCTAssertEqual(r.url?.absoluteString, "http://127.0.0.1:52817/control/state")
        XCTAssertEqual(r.value(forHTTPHeaderField: "Authorization"), "Bearer 4f9c0d…(64 hex)")
        XCTAssertFalse(r.url!.absoluteString.contains("token"), "the token never goes in the URL")
    }

    func testState() throws {
        let s: HostState = try decode("state.json")
        XCTAssertTrue(s.sharing)
        XCTAssertNil(s.error)
        XCTAssertEqual(s.source, "display:1")
        XCTAssertEqual(s.links.map(\.kind), ["tailnet", "lan", "local", "relay"])
        XCTAssertEqual(s.links[1].label, "this network")
        XCTAssertEqual(s.links[3].qr, s.links[3].url)
        XCTAssertEqual(s.pairing.code, "123-456")
        XCTAssertEqual(s.pairing.expiresAt, ControlJSON.date("2026-10-13T09:40:00Z"))
        XCTAssertEqual(s.viewers, 1)
        XCTAssertFalse(s.restartNeeded)
    }

    func testStateNotSharing() throws {
        let s: HostState = try decode("state-not-sharing.json")
        XCTAssertFalse(s.sharing)
        XCTAssertTrue(s.links.isEmpty)
        XCTAssertFalse(s.pairing.enabled)
        XCTAssertNil(s.pairing.code)
        XCTAssertNil(s.pairing.expiresAt)
        XCTAssertEqual(s.source, "test-pattern:1280x720")
    }

    func testSources() throws {
        let s: Sources = try decode("sources.json")
        XCTAssertEqual(s.displays.map(\.id), ["display:1", "display:2"])
        XCTAssertEqual(s.displays[0].width, 3024)
        XCTAssertEqual(s.windows.first?.app, "Safari")
        XCTAssertEqual(s.apps.first?.windows, 2)
        XCTAssertEqual(s.other.first?.id, "test-pattern")
        XCTAssertNil(s.error)
    }

    func testPairing() throws {
        let p: Pairing = try decode("pairing.json")
        XCTAssertEqual(p.expiresInS, 512)
        XCTAssertEqual(p.fingerprint, "abcd-efgh-ijkl-mnop")
        XCTAssertEqual(p.qr, p.url)
    }

    func testDevicesAndViewers() throws {
        let d: Devices = try decode("devices.json")
        XCTAssertEqual(d.devices.first?.name, "Gary's iPhone")
        XCTAssertNotNil(d.devices.first?.since)
        let v: Viewers = try decode("viewers.json")
        let row = try XCTUnwrap(v.viewers.first)
        XCTAssertEqual(row.id, "5f1c2a9b0e7d4c31")
        XCTAssertEqual(row.via, "paired")
        XCTAssertEqual(row.rttMs, 4.2)
        XCTAssertEqual(row.fps, 60)
        XCTAssertEqual(row.viewOnly, false)
        XCTAssertEqual(row.since, ControlJSON.date("2026-10-13T09:31:02.481Z"))
    }

    func testSettings() throws {
        let s: Settings = try decode("settings.json")
        XCTAssertEqual(s.settings.prefer, "framerate")
        XCTAssertEqual(s.settings.fps, .auto)
        XCTAssertEqual(s.settings.bitrate, .auto)
        XCTAssertEqual(s.settings.relay, ["wss://relay.example"])
        XCTAssertTrue(s.settings.relaySecretSet)
        XCTAssertTrue(s.settings.tailscaleCert)
        XCTAssertNil(s.settings.record)
        XCTAssertEqual(s.overridden, ["test-pattern"])
        XCTAssertNil(s.applied)

        let put: Settings = try decode("settings-put.json")
        XCTAssertEqual(put.settings.fps, .value(60))
        XCTAssertEqual(put.settings.bitrate, .value(20000))
        XCTAssertEqual(put.settings.record, "/Users/me/recordings")
        XCTAssertEqual(put.applied, "on_restart")
        XCTAssertTrue(put.restartNeeded)
        XCTAssertNotNil(put.appliedText)
    }

    func testSettingsChangesAreEncodedAsTheDocSays() throws {
        let body: [String: JSONValue] = ["prefer": .string("quality"), "fps": .number(60), "relay-secret": .string("s3cret"), "record": .null]
        let json = String(decoding: try ControlJSON.encoder.encode(body), as: UTF8.self)
        XCTAssertEqual(json, #"{"fps":60,"prefer":"quality","record":null,"relay-secret":"s3cret"}"#)
        XCTAssertEqual(String(decoding: try ControlJSON.encoder.encode(PolicyBody(text: nil, format: nil)), as: UTF8.self), #"{"text":null}"#)
    }

    func testStartAtLoginAndPermissions() throws {
        let l: StartAtLogin = try decode("start-at-login.json")
        XCTAssertTrue(l.enabled)
        XCTAssertEqual(l.kind, "launchd")
        let p: Permissions = try decode("permissions.json")
        XCTAssertEqual(p.screenRecording, "granted")
        XCTAssertEqual(p.accessibility, "denied")
        let o: PermissionOpen = try decode("permissions-open.json")
        XCTAssertFalse(o.opened)
        XCTAssertTrue(o.url!.hasSuffix("Privacy_ScreenCapture"))
    }

    func testSignIn() throws {
        let s: SignIn = try decode("signin.json")
        XCTAssertEqual(s.kind, "oidc")
        XCTAssertEqual(s.publicURL, "https://remote.example.com")
        XCTAssertEqual(s.oidcClientID, "0oa1b2c3")
        XCTAssertTrue(s.oidcClientSecretSet)
        XCTAssertNil(s.samlIdpMetadata)
        XCTAssertEqual(s.samlGroupsAttr, "groups")
        XCTAssertFalse(s.samlAvailable)
    }

    func testPolicy() throws {
        let p: Policy = try decode("policy.json")
        XCTAssertTrue(p.enabled)
        XCTAssertEqual(p.rules, 2)
        XCTAssertTrue(p.text!.contains("[[rule]]"))
        let off: Policy = try decode("policy-off.json")
        XCTAssertFalse(off.enabled)
        XCTAssertNil(off.text)
    }

    func testErrorsAreShownAsTheServerWroteThem() throws {
        XCTAssertThrowsError(try ControlClient.decode(status: 404, data: try fixture("error.json"), as: Viewers.self)) { e in
            XCTAssertEqual(e as? ControlError, .server(status: 404, message: "no viewer has that id (it may have just left)"))
            XCTAssertEqual(e.localizedDescription, "no viewer has that id (it may have just left)")
        }
        XCTAssertThrowsError(try ControlClient.decode(status: 400, data: try fixture("policy-error.json"), as: Policy.self)) { e in
            XCTAssertTrue(e.localizedDescription.hasPrefix("policy: unknown field `view_ony`"))
            XCTAssertEqual((e as? ControlError)?.status, 400)
        }
        // Not JSON (a proxy's page, say): the status still says something.
        XCTAssertThrowsError(try ControlClient.decode(status: 502, data: Data("<html>".utf8), as: Policy.self)) { e in
            XCTAssertEqual(e.localizedDescription, "HTTP 502")
        }
    }

    func testAudit() throws {
        let a: AuditPage = try decode("audit.json")
        XCTAssertEqual(a.entries.count, 2)
        XCTAssertEqual(a.entries[0]["event"], .string("session_end"))
        XCTAssertEqual(a.entries[0]["reason"], .string("kicked"))
        XCTAssertEqual(a.entries[0]["seconds"]?.display, "250.1")
        XCTAssertEqual(a.next, "1841")
        let off: AuditPage = try decode("audit-off.json")
        XCTAssertNil(off.path)
        XCTAssertNil(off.next)
    }

    func testAuditQuery() {
        var q = AuditQuery()
        q.events = ["session_start", "session_end"]
        q.user = " alice "
        q.since = "2026-10-12T00:00:00Z"
        q.limit = 50
        q.before = "1841"
        var c = URLComponents(string: "http://127.0.0.1:1/control/admin/audit")!
        c.queryItems = q.queryItems
        XCTAssertEqual(c.url?.query, "event=session_start,session_end&user=alice&since=2026-10-12T00:00:00Z&limit=50&before=1841")
        XCTAssertEqual(AuditQuery.eventTypes.count, 18)
    }

    func testSessionsAndMetrics() throws {
        let s: Sessions = try decode("sessions.json")
        XCTAssertEqual(s.sessions.first?.rule, "admins")
        XCTAssertEqual(s.sessions.first?.groups, ["rr-admins"])
        XCTAssertNil(s.sessions.first?.endsAt)
        let m: Metrics = try decode("metrics.json")
        XCTAssertEqual(m.uptimeS, 3600)
        XCTAssertEqual(m.videoBytesSentTotal, 911_203_344)
        XCTAssertEqual(m.relays.first?.rttMs, 12.5)
        let fresh: Metrics = try decode("metrics-fresh.json")
        XCTAssertEqual(fresh.uptimeS, 0)
        XCTAssertTrue(fresh.relays.isEmpty)
    }

    func testEventStream() throws {
        let text = String(decoding: try fixture("events.txt"), as: UTF8.self)
        var p = EventParser()
        var events: [HostEvent] = []
        for line in text.components(separatedBy: "\n") {
            if let e = p.feed(line), let ev = try HostEvent.decode(e) { events.append(ev) }
        }
        XCTAssertEqual(events.count, 4, "state, viewers, devices, settings; the keepalive ignored")
        guard case .state(let s) = events[0], case .viewers(let v) = events[1],
              case .devices(let d) = events[2], case .settings(let st) = events[3] else {
            return XCTFail("in the doc's order: \(events)")
        }
        XCTAssertEqual(s.pairing.code, "123-456")
        XCTAssertEqual(v.count, 1)
        XCTAssertEqual(d.count, 1)
        XCTAssertEqual(st.path, "/Users/me/.rein-remote/host.toml")
    }

    func testEventParserEdges() {
        var p = EventParser()
        XCTAssertNil(p.feed("event: viewers\r"))
        XCTAssertNil(p.feed("data: {\"viewers\":"))
        XCTAssertNil(p.feed("data: []}"))
        XCTAssertEqual(p.feed("\r"), ServerEvent(name: "viewers", data: "{\"viewers\":\n[]}"))
        XCTAssertNil(p.feed(": keepalive"))
        XCTAssertNil(p.feed(""), "a blank line with nothing before it is no event")
        XCTAssertNil(p.feed("data:x"))
        XCTAssertEqual(p.feed(""), ServerEvent(name: "message", data: "x"))
    }

    func testQRCode() throws {
        let img = try XCTUnwrap(QRCode.image(for: "http://192.168.1.20:7979/#pair=123456&id=abcd-efgh-ijkl-mnop", size: 200))
        XCTAssertGreaterThanOrEqual(img.size.width, 150)
        XCTAssertEqual(img.size.width, img.size.height)
    }

    func testCountdown() {
        XCTAssertEqual(countdown(599), "9:59")
        XCTAssertEqual(countdown(5), "0:05")
        XCTAssertEqual(countdown(-3), "0:00")
    }
}
