// The app driven through its UI against a real `rein-remote host --test-pattern 1280x720` (each test a
// host and a home folder of its own): sharing on and off, the source changed, a viewer seen then
// kicked (and the kick in the audit log), a new pairing code, a setting in host.toml, a policy's error
// shown, the app finding the host again after it restarts, and a screenshot of each page.
//
// Screenshots go to $RR_SHOTS (TEST_RUNNER_RR_SHOTS for xcodebuild) when set, and are attached.

import AppKit
import XCTest

final class UITests: XCTestCase {
    var host: HostHarness!
    var app: XCUIApplication!

    override func setUpWithError() throws {
        continueAfterFailure = false
        host = try HostHarness()
    }

    override func tearDown() {
        app?.terminate()
        host?.stop()
    }

    /// The app on this test's host, its window open on `page`.
    func launch(page: String = "sharing", appearance: String = "light") {
        app = XCUIApplication()
        app.launchEnvironment = [
            "REIN_REMOTE_HOME": host.home.path,
            "REIN_REMOTE_BIN": host.bin.path,
            "RR_UI_TEST": "1",
            "RR_OPEN_WINDOW": "1",
            "RR_PAGE": page,
            "RR_APPEARANCE": appearance,
        ]
        app.launch()
        XCTAssertTrue(app.windows["main"].waitForExistence(timeout: 20) || app.windows.firstMatch.waitForExistence(timeout: 5), "the window")
        XCTAssert(el("sidebarStatus").waitForExistence(timeout: 20), "the sidebar")
    }

    func el(_ id: String) -> XCUIElement { app.descendants(matching: .any)[id].firstMatch }

    func go(_ page: String) {
        let row = el("nav.\(page)")
        XCTAssert(row.waitForExistence(timeout: 10), "sidebar row \(page)")
        row.click()
    }

    /// Waits until the element's label or value contains `text`.
    func waitText(_ e: XCUIElement, contains text: String, timeout: TimeInterval = 20, _ message: String = "") {
        let p = NSPredicate { obj, _ in
            guard let e = obj as? XCUIElement, e.exists else { return false }
            return e.label.contains(text) || ((e.value as? String)?.contains(text) ?? false)
        }
        let ok = XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: p, object: e)], timeout: timeout) == .completed
        XCTAssert(ok, "\(message) waiting for \"\(text)\" in \(e.exists ? "\(e.label) / \(String(describing: e.value))" : "(missing)")")
    }

    /// A button of the alert on the window (not the Touch Bar's copy of it).
    func alertButton(_ title: String) -> XCUIElement {
        for q in [app.sheets, app.dialogs, app.alerts] where q.buttons[title].firstMatch.waitForExistence(timeout: 2) {
            return q.buttons[title].firstMatch
        }
        return app.windows.buttons[title].firstMatch
    }

    func sharing() -> Bool { (try? host.state())?["sharing"] as? Bool == true }

    // MARK: Tests

    func testStartAndStopSharing() throws {
        launch()
        let status = el("sharingStatus")
        waitText(status, contains: "Not sharing")
        el("sharingToggle").click()
        waitText(status, contains: "Sharing", "after Start")
        XCTAssert(host.eventually { self.sharing() }, "the host shares")
        XCTAssert(el("link.0").waitForExistence(timeout: 10), "a link while sharing")
        XCTAssert(el("linkQR").exists, "its QR code")
        el("sharingToggle").click()
        waitText(status, contains: "Not sharing", "after Stop")
        XCTAssert(host.eventually { !self.sharing() }, "the host stopped")
    }

    func testChangeTheSource() throws {
        try host.share()
        launch()
        waitText(el("currentSource"), contains: "test-pattern:1280x720")
        let picker = el("sourcePicker")
        XCTAssert(picker.waitForExistence(timeout: 10))
        // The list is read when the page opens; wait for it, then pick the plain test pattern.
        let deadline = Date().addingTimeInterval(20)
        repeat {
            picker.click()
            let item = app.menuItems["Test pattern"]
            if item.waitForExistence(timeout: 3) { item.click(); break }
            app.typeKey(.escape, modifierFlags: [])
        } while Date() < deadline
        waitText(el("currentSource"), contains: "Current source: test-pattern", "after picking")
        XCTAssert(host.eventually { (try self.host.state())["source"] as? String == "test-pattern" }, "the host's source")
        XCTAssert(host.hostToml.contains("test-pattern = true"), host.hostToml)
    }

    func testAViewerAppearsThenIsKickedAndAudited() throws {
        let link = try host.share()
        launch(page: "viewers")
        XCTAssert(el("noViewers").waitForExistence(timeout: 10), "no one yet")
        try host.connectViewer(link, name: "UI test viewer")
        let row = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'viewer.'")).firstMatch
        XCTAssert(row.waitForExistence(timeout: 30), "the viewer shows")
        let kick = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'kick.'")).firstMatch
        XCTAssert(kick.exists)
        kick.click()
        // Confirm first.
        let confirm = alertButton("Kick")
        XCTAssert(confirm.waitForExistence(timeout: 10), "the confirmation")
        confirm.click()
        XCTAssert(el("noViewers").waitForExistence(timeout: 20), "the viewer is gone")
        XCTAssert(host.eventually { ((try self.host.api("GET", "/control/viewers"))["viewers"] as? [Any])?.isEmpty == true })

        // The kick in the audit log, filtered to it.
        go("audit")
        XCTAssert(el("auditTable").waitForExistence(timeout: 10))
        let filter = el("auditEvents")
        filter.click()
        let item = app.menuItems["session_kicked"].firstMatch
        XCTAssert(item.waitForExistence(timeout: 5), "the event filter")
        item.click()
        let cell = app.staticTexts.matching(NSPredicate(format: "value == 'session_kicked' OR label == 'session_kicked'")).firstMatch
        XCTAssert(cell.waitForExistence(timeout: 10), "session_kicked in the audit table")
        XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "value == 'sharing_start' OR label == 'sharing_start'")).firstMatch.exists,
                       "only the filtered event")
    }

    func testNewPairingCode() throws {
        try host.share()
        launch()
        let code = el("pairingCode")
        XCTAssert(code.waitForExistence(timeout: 10))
        let before = (try host.state())["pairing"].flatMap { ($0 as? [String: Any])?["code"] as? String } ?? ""
        waitText(code, contains: before)
        XCTAssert(el("pairingCountdown").exists, "the countdown")
        el("newCode").click()
        XCTAssert(host.eventually {
            let now = (try self.host.state())["pairing"].flatMap { ($0 as? [String: Any])?["code"] as? String } ?? before
            return now != before
        }, "a new code on the host")
        let after = (try host.state())["pairing"].flatMap { ($0 as? [String: Any])?["code"] as? String } ?? ""
        waitText(code, contains: after, "the new code shown")
        XCTAssertNotEqual(before, after)
    }

    func testASettingIsWrittenToHostToml() throws {
        launch(page: "settings")
        let prefer = el("prefer")
        XCTAssert(prefer.waitForExistence(timeout: 10))
        let quality = prefer.radioButtons["Quality first"].exists ? prefer.radioButtons["Quality first"] : app.radioButtons["Quality first"]
        XCTAssert(quality.waitForExistence(timeout: 5), "the Quality first segment")
        quality.click()
        XCTAssert(host.eventually { self.host.hostToml.contains(#"prefer = "quality""#) }, "host.toml: \(host.hostToml)")
        XCTAssert(el("settingsApplied").waitForExistence(timeout: 10), "what happened is said")
    }

    func testAPolicyWithAnErrorShowsTheError() throws {
        launch(page: "policy")
        let editor = el("policyText")
        XCTAssert(editor.waitForExistence(timeout: 10))
        let text = editor.textViews.firstMatch.exists ? editor.textViews.firstMatch : editor
        text.click()
        text.typeText("[[rule]]\nview_ony = true\n")
        el("policySave").click()
        let error = el("policyError")
        XCTAssert(error.waitForExistence(timeout: 10), "the server's error")
        waitText(error, contains: "view_ony")
        XCTAssertFalse(host.hostToml.contains("policy"), "nothing changed")
    }

    func testFindsTheHostAgainAfterItRestarts() throws {
        // What the app's own host will use: this computer only, no mDNS, a test pattern.
        try host.api("PUT", "/control/settings", ["listen": "127.0.0.1:0", "mdns": false])
        try host.api("PUT", "/control/source", ["source": "test-pattern"])
        launch()
        waitText(el("sharingStatus"), contains: "Not sharing")
        let old = host.process.processIdentifier
        host.process.terminate()
        host.process.waitUntilExit()
        // The app starts the rein-remote inside it, and follows that one.
        XCTAssert(host.eventually(30) {
            guard let d = try? Data(contentsOf: self.host.home.appendingPathComponent("control.json")),
                  let o = try JSONSerialization.jsonObject(with: d) as? [String: Any] else { return false }
            return (o["pid"] as? Int).map(Int32.init) != old
        }, "a new host")
        waitText(el("sharingStatus"), contains: "Not sharing", timeout: 30, "connected again")
        el("sharingToggle").click()
        waitText(el("sharingStatus"), contains: "Sharing", "sharing through the new host")
    }

    /// The menu bar item clicked: its panel (a dialog).
    @discardableResult
    func openMenuBarPanel() -> XCUIElement {
        let item = app.statusItems.firstMatch.exists ? app.statusItems.firstMatch : app.menuBars.statusItems.firstMatch
        XCTAssert(item.waitForExistence(timeout: 10), "the menu bar item")
        item.click()
        let panel = app.dialogs.containing(.any, identifier: "menuStatus").firstMatch
        XCTAssert(panel.waitForExistence(timeout: 10), "the menu bar panel")
        return panel
    }

    func testTheMenuBarPanel() throws {
        let link = try host.share()
        try host.connectViewer(link, name: "Menu viewer")
        XCTAssert(host.eventually { ((try self.host.api("GET", "/control/viewers"))["viewers"] as? [Any])?.count == 1 })
        launch()
        openMenuBarPanel()
        waitText(el("menuStatus"), contains: "Sharing, 1 viewer")
        waitText(el("menuViewers"), contains: "1")
        XCTAssert(el("menuSource").exists, "the source")
        XCTAssert(el("menuPairingCode").exists, "the pairing code")
        el("menuSharingToggle").click()
        XCTAssert(host.eventually { !self.sharing() }, "stopped from the menu bar")
        waitText(el("menuStatus"), contains: "Not sharing")
        el("menuSharingToggle").click()
        XCTAssert(host.eventually { self.sharing() }, "started from the menu bar")
        XCTAssert(el("menuOpen").exists && el("menuQuit").exists)
        app.typeKey(.escape, modifierFlags: [])
    }

    // MARK: Screenshots

    func testScreenshotsOfEachPage() throws {
        try host.pairDevice(name: "Test iPhone")
        let link = try host.share()
        try host.connectViewer(link, name: "Test iPad")
        XCTAssert(host.eventually { ((try self.host.api("GET", "/control/viewers"))["viewers"] as? [Any])?.count == 1 }, "the viewer")
        try host.api("PUT", "/control/admin/policy", ["text": "[[rule]]\nname = \"paired devices\"\nvia = [\"paired\", \"token\"]\n", "format": "toml"])
        launch()
        waitText(el("sharingStatus"), contains: "Sharing")
        shot("sharing")
        let pages = ["viewers", "devices", "settings", "permissions", "signin", "policy", "sessions", "audit", "metrics"]
        for p in pages {
            go(p)
            Thread.sleep(forTimeInterval: 1.5)
            shot(p)
        }
        // The menu bar's panel.
        let panel = openMenuBarPanel()
        if panel.exists { shot("menu-bar", element: panel) }
        app.typeKey(.escape, modifierFlags: [])
        // Dark.
        app.terminate()
        launch(page: "sharing", appearance: "dark")
        waitText(el("sharingStatus"), contains: "Sharing")
        shot("sharing-dark")
        go("viewers")
        Thread.sleep(forTimeInterval: 1)
        shot("viewers-dark")
    }

    /// The window (never the whole screen), at most 1000 points wide, as PNG.
    func shot(_ name: String, element: XCUIElement? = nil) {
        let target = element ?? (app.windows["main"].exists ? app.windows["main"] : app.windows.firstMatch)
        let img = target.screenshot().image
        let a = XCTAttachment(image: img)
        a.name = name
        a.lifetime = .keepAlways
        add(a)
        guard let dir = ProcessInfo.processInfo.environment["RR_SHOTS"], !dir.isEmpty else { return }
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        let scale = min(1, 1000 / max(1, img.size.width))
        let w = Int(img.size.width * scale), h = Int(img.size.height * scale)
        guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: w, pixelsHigh: h, bitsPerSample: 8, samplesPerPixel: 4,
                                         hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { return }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        NSGraphicsContext.current?.imageInterpolation = .high
        img.draw(in: NSRect(x: 0, y: 0, width: w, height: h))
        NSGraphicsContext.restoreGraphicsState()
        try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(name).png"))
    }
}

