import XCTest

/// The app as a person uses it, in the simulator: set RR_URL (TEST_RUNNER_RR_URL) to a server running
/// `rein-remote serve --test-pattern 1280x720`. A tap on the stream goes through UIKit's touches and
/// the gestures to the server, which draws its cross there: the app reads it back from the video.
final class UITests: XCTestCase {
    func testATapReachesTheServer() throws {
        guard let url = ProcessInfo.processInfo.environment["RR_URL"], !url.isEmpty else { throw XCTSkip("set RR_URL") }
        let app = XCUIApplication()
        app.launchArguments = ["-RRConnect", url, "-RRMeasure", "YES", "-RRAudio", "NO"]
        app.launch()
        let stream = app.otherElements["stream"]
        XCTAssertTrue(stream.waitForExistence(timeout: 20))
        let status = app.staticTexts["status"]
        XCTAssertTrue(status.waitForExistence(timeout: 20))
        // Streaming: the status shows frames a second.
        let streaming = NSPredicate(format: "label CONTAINS ' fps'")
        expectation(for: streaming, evaluatedWith: status)
        waitForExpectations(timeout: 20)
        stream.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        let seen = app.staticTexts["input"]
        XCTAssertTrue(seen.waitForExistence(timeout: 10), "the pattern drew its cross where the tap was")
        print("ui test: \(seen.label); \(status.label)")
    }

    func testTheServerListFindsServersOnTheNetwork() throws {
        guard let name = ProcessInfo.processInfo.environment["RR_EXPECT_SERVER"], !name.isEmpty else { throw XCTSkip("set RR_EXPECT_SERVER") }
        let app = XCUIApplication()
        app.launchArguments = ["-RRMeasure", "YES", "-RRAudio", "NO"]
        app.launch()
        let server = app.buttons["server-\(name)"]
        XCTAssertTrue(server.waitForExistence(timeout: 20), "found \(name) by Bonjour")
        // Paired already (testPairsThenSignsIn, with RR_PAIR_URL): a tap resolves it and signs in.
        guard server.label.contains("paired") && !server.label.contains("pair with") else { return }
        server.tap()
        let status = app.staticTexts["status"]
        XCTAssertTrue(status.waitForExistence(timeout: 20))
        expectation(for: NSPredicate(format: "label CONTAINS ' fps'"), evaluatedWith: status)
        waitForExpectations(timeout: 20)
        print("ui test: \(name) by Bonjour, signed in: \(status.label)")
    }
}
