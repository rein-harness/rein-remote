import CoreVideo
import GameController
import UIKit
import XCTest
@testable import Rein_Remote

final class AnnexBTests: XCTestCase {
    func testSplitsOnThreeAndFourByteStartCodes() {
        let d = Data([0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5, 0])
        let u = AnnexB.units(d)
        XCTAssertEqual(u, [Data([0x67, 1, 2]), Data([0x68, 3]), Data([0x65, 4, 5, 0])])
        XCTAssertEqual(u.map { AnnexB.type($0, hevc: false) }, [7, 8, 5])
        XCTAssertTrue(AnnexB.isParameterSet(7, hevc: false))
        XCTAssertFalse(AnnexB.isParameterSet(5, hevc: false))
    }

    func testHevcTypes() {
        // VPS 32, SPS 33, PPS 34, IDR_W_RADL 19: the type is bits 1-6 of the first byte.
        XCTAssertEqual(AnnexB.type(Data([0x40, 0x01]), hevc: true), 32)
        XCTAssertEqual(AnnexB.type(Data([0x42, 0x01]), hevc: true), 33)
        XCTAssertEqual(AnnexB.type(Data([0x44, 0x01]), hevc: true), 34)
        XCTAssertEqual(AnnexB.type(Data([0x26, 0x01]), hevc: true), 19)
        XCTAssertTrue(AnnexB.isParameterSet(33, hevc: true))
    }

    func testLengthPrefixed() {
        XCTAssertEqual(AnnexB.lengthPrefixed([Data([9, 9]), Data([7])]), Data([0, 0, 0, 2, 9, 9, 0, 0, 0, 1, 7]))
    }
}

final class PairLinkTests: XCTestCase {
    func testTheQrCodesLink() {
        let p = PairLink("http://192.168.1.20:7979/#pair=123-456&id=abcd-efgh-2345-6789")
        XCTAssertEqual(p, PairLink(url: "http://192.168.1.20:7979/", code: "123-456", id: "abcd-efgh-2345-6789"))
    }

    func testOtherLinksAsTheyAre() {
        XCTAssertEqual(PairLink("https://box.ts.net:7979/?token=abc")?.url, "https://box.ts.net:7979/?token=abc")
        XCTAssertNil(PairLink("https://relay.example/?room=r#k=KEY")?.code)
        XCTAssertNil(PairLink("not a link"))
        XCTAssertNil(PairLink("ftp://x/"))
    }
}

/// What gestures sent.
final class Recorder: InputSink {
    var events: [String] = []
    func pointerMove(_ x: Int, _ y: Int) { events.append("move \(x),\(y)") }
    func button(_ b: MouseButton, down: Bool) { events.append("\(b) \(down ? "down" : "up")") }
    func wheel(dx: Int, dy: Int) { events.append("wheel \(dx),\(dy)") }
    func key(hid: UInt16, down: Bool, name: String) { events.append("key \(name) \(down ? "down" : "up")") }
    func text(_ t: String) { events.append("text \(t)") }
}

final class TouchGestureTests: XCTestCase {
    var keep: [NSObject] = []

    private func make() -> (TouchGestures, Recorder, ObjectIdentifier, ObjectIdentifier) {
        let r = Recorder()
        let (o1, o2) = (NSObject(), NSObject())
        keep = [o1, o2]
        return (TouchGestures(sink: r), r, ObjectIdentifier(o1), ObjectIdentifier(o2))
    }

    func testATapClicks() {
        let (g, r, f, _) = make()
        g.down(id: f, at: CGPoint(x: 10, y: 10), stream: CGPoint(x: 100, y: 50))
        g.up(id: f, stream: CGPoint(x: 101, y: 51))
        XCTAssertEqual(r.events, ["move 101,51", "left down", "left up"])
        XCTAssertEqual(g.mode, .idle)
    }

    func testASwipeScrollsWithTheContentFollowingTheFinger() {
        let (g, r, f, _) = make()
        g.down(id: f, at: CGPoint(x: 100, y: 100), stream: CGPoint(x: 200, y: 200))
        g.move(id: f, at: CGPoint(x: 100, y: 80), stream: CGPoint(x: 200, y: 160), scale: 2)
        g.move(id: f, at: CGPoint(x: 100, y: 70), stream: CGPoint(x: 200, y: 140), scale: 2)
        g.up(id: f, stream: CGPoint(x: 200, y: 140))
        // Over the slop: the pointer goes where the finger is, then moving up 20 and 10 points
        // scrolls down 40 and 20 pixels; no click.
        XCTAssertEqual(r.events, ["move 200,160", "wheel 0,40", "wheel 0,20"])
    }

    func testHoldThenMoveDrags() {
        let (g, r, f, _) = make()
        g.down(id: f, at: CGPoint(x: 10, y: 10), stream: CGPoint(x: 20, y: 20))
        g.held(CGPoint(x: 20, y: 20))
        g.move(id: f, at: CGPoint(x: 60, y: 10), stream: CGPoint(x: 120, y: 20), scale: 2)
        g.up(id: f, stream: CGPoint(x: 120, y: 20))
        XCTAssertEqual(r.events, ["move 20,20", "left down", "move 120,20", "move 120,20", "left up"])
    }

    func testTwoFingerTapRightClicksBetweenThem() {
        let (g, r, f1, f2) = make()
        g.down(id: f1, at: CGPoint(x: 10, y: 10), stream: CGPoint(x: 100, y: 100))
        g.down(id: f2, at: CGPoint(x: 30, y: 10), stream: CGPoint(x: 140, y: 100))
        g.up(id: f2, stream: CGPoint(x: 140, y: 100))
        g.up(id: f1, stream: CGPoint(x: 100, y: 100))
        XCTAssertEqual(r.events, ["move 120,100", "right down", "right up"])
        XCTAssertEqual(g.mode, .idle)
    }

    func testTheHoldTimerDoesNothingAfterAMove() {
        let (g, r, f, _) = make()
        g.down(id: f, at: .zero, stream: .zero)
        g.move(id: f, at: CGPoint(x: 0, y: 30), stream: CGPoint(x: 0, y: 30), scale: 1)
        g.held(.zero)
        XCTAssertFalse(r.events.contains("left down"))
    }
}

final class KeyTests: XCTestCase {
    func testNamesAsTheWebHasThem() {
        XCTAssertEqual(Keys.name(keyCode: .keyboardReturnOrEnter, characters: "\r", unmodified: "\r", modifiers: []), "Enter")
        XCTAssertEqual(Keys.name(keyCode: .keyboardLeftArrow, characters: "", unmodified: "", modifiers: []), "ArrowLeft")
        XCTAssertEqual(Keys.name(keyCode: .keyboardDeleteOrBackspace, characters: "\u{8}", unmodified: "\u{8}", modifiers: []), "Backspace")
        XCTAssertEqual(Keys.name(keyCode: .keyboardSpacebar, characters: " ", unmodified: " ", modifiers: []), " ")
        XCTAssertEqual(Keys.name(keyCode: .keyboardA, characters: "A", unmodified: "a", modifiers: [.shift]), "A")
        // Ctrl+C is "c", as browsers give it.
        XCTAssertEqual(Keys.name(keyCode: .keyboardC, characters: "\u{3}", unmodified: "c", modifiers: [.control]), "c")
        XCTAssertEqual(Keys.name(keyCode: .keyboardLeftShift, characters: "", unmodified: "", modifiers: [.shift]), "Shift")
        XCTAssertEqual(Keys.name(keyCode: .keyboardF5, characters: "", unmodified: "", modifiers: []), "F5")
        // The HID usage is UIKit's key code.
        XCTAssertEqual(UIKeyboardHIDUsage.keyboardA.rawValue, 4)
    }

    func testTheOnScreenKeyboardSendsTextAndKeys() {
        let v = StreamUIView(frame: .zero)
        let r = Recorder()
        v.sink = r
        v.keyboard.insertText("hi\nthere")
        v.keyboard.deleteBackward()
        XCTAssertEqual(r.events, ["text hi", "key Enter down", "key Enter up", "text there", "key Backspace down", "key Backspace up"])
    }
}

final class GamepadTests: XCTestCase {
    func testAxesAndTriggers() {
        XCTAssertEqual(GamepadState.axis(1), 32767)
        XCTAssertEqual(GamepadState.axis(-1), -32767)
        XCTAssertEqual(GamepadState.axis(2), 32767)
        XCTAssertEqual(GamepadState.axis(0), 0)
        XCTAssertEqual(GamepadState.trigger(1), 255)
        XCTAssertEqual(GamepadState.trigger(0.5), 128)
        XCTAssertEqual(GamepadState.trigger(-1), 0)
    }

    func testAVirtualControllersButtonsInTheStandardOrder() throws {
        // A controller made in code (GameController's snapshot gamepads): no hardware needed.
        let c = GCController.withExtendedGamepad()
        let g = try XCTUnwrap(c.extendedGamepad)
        g.buttonA.setValue(1)
        g.dpad.setValueForXAxis(1, yAxis: 0)
        g.leftThumbstick.setValueForXAxis(0, yAxis: 1)
        g.rightTrigger.setValue(1)
        let s = GamepadState(pad: 2, g)
        XCTAssertEqual(s.pad, 2)
        XCTAssertEqual(s.buttons, 1 << 0 | 1 << 7 | 1 << 15)
        // Up on the stick is negative, as on the web.
        XCTAssertEqual(s.ly, -32767)
        XCTAssertEqual(s.rt, 255)
    }
}

final class PatternTests: XCTestCase {
    /// An NV12 frame with the test pattern's barcode for `ms` and its flash.
    private func frame(ms: UInt32, flash: Bool, w: Int = 1280, h: Int = 720) -> CVPixelBuffer {
        var px: CVPixelBuffer?
        CVPixelBufferCreate(nil, w, h, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, nil, &px)
        let p = px!
        CVPixelBufferLockBaseAddress(p, [])
        let y = CVPixelBufferGetBaseAddressOfPlane(p, 0)!.assumingMemoryBound(to: UInt8.self)
        let stride = CVPixelBufferGetBytesPerRowOfPlane(p, 0)
        for row in 0..<h { memset(y + row * stride, 60, w) }
        for k in 0..<34 {
            let on = k == 0 ? true : k == 1 ? false : (ms >> (33 - k)) & 1 == 1
            for row in (h - 24)..<h { memset(y + row * stride + k * 24, on ? 235 : 16, 24) }
        }
        if flash { for row in 0..<(h / 6) { memset(y + row * stride, 235, w / 6) } }
        let c = CVPixelBufferGetBaseAddressOfPlane(p, 1)!.assumingMemoryBound(to: UInt8.self)
        for row in 0..<(h / 2) { memset(c + row * CVPixelBufferGetBytesPerRowOfPlane(p, 1), 128, w) }
        CVPixelBufferUnlockBaseAddress(p, [])
        return p
    }

    func testReadsTheBarcode() {
        let f = frame(ms: 0xDEAD_BEEF, flash: true)
        XCTAssertEqual(Pattern.barcodeMs(f), 0xDEAD_BEEF)
        XCTAssertTrue(Pattern.flashing(f))
        XCTAssertEqual(Pattern.glassMs(f, nowMs: 0xDEAD_BEEF + 12), 12)
        // Clocks a long way apart: no number.
        XCTAssertNil(Pattern.glassMs(f, nowMs: 0xDEAD_BEEF + 20_000))
        XCTAssertFalse(Pattern.flashing(frame(ms: 1, flash: false)))
        // Too narrow for the barcode.
        XCTAssertNil(Pattern.barcodeMs(frame(ms: 5, flash: false, w: 640, h: 360)))
    }
}
