import CoreGraphics
import Foundation
import GameController
import UIKit

/// Where input goes: the session, or a test's recorder.
protocol InputSink: AnyObject {
    func pointerMove(_ x: Int, _ y: Int)
    func button(_ b: MouseButton, down: Bool)
    func wheel(dx: Int, dy: Int)
    func key(hid: UInt16, down: Bool, name: String)
    func text(_ t: String)
}

extension RemoteSession: InputSink {}

/// Touch, for a server that drives a mouse (as every desktop does), as the web client does it: a tap
/// clicks, a swipe scrolls (content follows the finger), hold then move drags, a two-finger tap
/// right-clicks. Positions are in the stream's pixels; `scale` is stream pixels per point (scrolling).
final class TouchGestures {
    enum Mode { case idle, maybe, scroll, drag, two, done }
    static let holdSeconds = 0.4
    static let slop: CGFloat = 10

    private struct Finger { var at: CGPoint; let start: CGPoint; var stream: CGPoint }
    private var fingers: [ObjectIdentifier: Finger] = [:]
    private(set) var mode = Mode.idle
    private var hold: DispatchWorkItem?
    weak var sink: InputSink?
    /// Called when a hold turns into a drag (a tap of the haptics).
    var onDragStart: (() -> Void)?

    init(sink: InputSink?) { self.sink = sink }

    /// `id`: the touch; `at`: in points (for distances); `stream`: the same place in stream pixels.
    func down(id: ObjectIdentifier, at: CGPoint, stream: CGPoint) {
        fingers[id] = Finger(at: at, start: at, stream: stream)
        if fingers.count == 1 {
            mode = .maybe
            hold?.cancel()
            let item = DispatchWorkItem { [weak self] in self?.held(stream) }
            hold = item
            DispatchQueue.main.asyncAfter(deadline: .now() + Self.holdSeconds, execute: item)
        } else if fingers.count == 2 && mode == .maybe {
            mode = .two
            hold?.cancel()
        }
    }

    /// The hold timer fired (public for tests, which don't wait for it).
    func held(_ stream: CGPoint) {
        guard mode == .maybe else { return }
        mode = .drag
        sink?.pointerMove(Int(stream.x), Int(stream.y))
        sink?.button(.left, down: true)
        onDragStart?()
    }

    func move(id: ObjectIdentifier, at: CGPoint, stream: CGPoint, scale: CGFloat) {
        guard var f = fingers[id] else { return }
        let prev = f.at
        f.at = at
        f.stream = stream
        fingers[id] = f
        if mode == .drag { return sink?.pointerMove(Int(stream.x), Int(stream.y)) ?? () }
        if mode == .maybe && hypot(at.x - f.start.x, at.y - f.start.y) > Self.slop {
            mode = .scroll
            hold?.cancel()
            // Scroll what's under the finger.
            sink?.pointerMove(Int(stream.x), Int(stream.y))
        }
        if mode == .scroll {
            // Content follows the finger: moving up scrolls down.
            let dx = Int(((prev.x - at.x) * scale).rounded()), dy = Int(((prev.y - at.y) * scale).rounded())
            if dx != 0 || dy != 0 { sink?.wheel(dx: dx, dy: dy) }
        }
    }

    func up(id: ObjectIdentifier, stream: CGPoint, cancelled: Bool = false) {
        guard let f = fingers.removeValue(forKey: id) else { return }
        hold?.cancel()
        switch mode {
        case .drag:
            sink?.pointerMove(Int(stream.x), Int(stream.y))
            sink?.button(.left, down: false)
        case .maybe where !cancelled:
            sink?.pointerMove(Int(stream.x), Int(stream.y))
            sink?.button(.left, down: true)
            sink?.button(.left, down: false)
        case .two where fingers.count == 1 && !cancelled:
            let o = fingers.values.first!
            sink?.pointerMove(Int((o.stream.x + f.stream.x) / 2), Int((o.stream.y + f.stream.y) / 2))
            sink?.button(.right, down: true)
            sink?.button(.right, down: false)
            mode = .done
            return
        default: break
        }
        if fingers.isEmpty { mode = .idle }
    }
}

/// Keys as the protocol names them (the web's `KeyboardEvent.key`: "a", "A", "Enter", "ArrowLeft",
/// "Shift", " "), with their USB HID usage (UIKey's keyCode is one).
enum Keys {
    static let named: [UIKeyboardHIDUsage: String] = [
        .keyboardReturnOrEnter: "Enter", .keypadEnter: "Enter", .keyboardEscape: "Escape",
        .keyboardDeleteOrBackspace: "Backspace", .keyboardDeleteForward: "Delete", .keyboardTab: "Tab",
        .keyboardSpacebar: " ", .keyboardLeftArrow: "ArrowLeft", .keyboardRightArrow: "ArrowRight",
        .keyboardUpArrow: "ArrowUp", .keyboardDownArrow: "ArrowDown", .keyboardHome: "Home", .keyboardEnd: "End",
        .keyboardPageUp: "PageUp", .keyboardPageDown: "PageDown", .keyboardInsert: "Insert",
        .keyboardLeftShift: "Shift", .keyboardRightShift: "Shift", .keyboardLeftControl: "Control",
        .keyboardRightControl: "Control", .keyboardLeftAlt: "Alt", .keyboardRightAlt: "Alt",
        .keyboardLeftGUI: "Meta", .keyboardRightGUI: "Meta", .keyboardCapsLock: "CapsLock",
        .keyboardF1: "F1", .keyboardF2: "F2", .keyboardF3: "F3", .keyboardF4: "F4", .keyboardF5: "F5", .keyboardF6: "F6",
        .keyboardF7: "F7", .keyboardF8: "F8", .keyboardF9: "F9", .keyboardF10: "F10", .keyboardF11: "F11", .keyboardF12: "F12",
    ]

    /// The key's name: a named key, else the character it types (unshifted while Control, Alt or
    /// Command is held, as browsers give "c" for Ctrl+C).
    static func name(keyCode: UIKeyboardHIDUsage, characters: String, unmodified: String, modifiers: UIKeyModifierFlags) -> String? {
        if let n = named[keyCode] { return n }
        let chord = !modifiers.intersection([.control, .alternate, .command]).isEmpty
        let s = chord ? unmodified : characters
        return s.count == 1 ? s : (unmodified.count == 1 ? unmodified : nil)
    }

    static func name(_ k: UIKey) -> String? {
        name(keyCode: k.keyCode, characters: k.characters, unmodified: k.charactersIgnoringModifiers, modifiers: k.modifierFlags)
    }
}

/// A gamepad's state as the protocol has it (rr-proto's `Input::Gamepad`: the W3C standard layout).
struct GamepadState: Equatable {
    var pad: UInt8 = 0
    var buttons: UInt32 = 0
    var lx: Int16 = 0, ly: Int16 = 0, rx: Int16 = 0, ry: Int16 = 0
    var lt: UInt8 = 0, rt: UInt8 = 0

    static func axis(_ v: Float) -> Int16 { Int16((max(-1, min(1, v)) * 32767).rounded()) }
    static func trigger(_ v: Float) -> UInt8 { UInt8((max(0, min(1, v)) * 255).rounded()) }

    init() {}

    init(pad: UInt8, _ g: GCExtendedGamepad) {
        self.pad = pad
        let order: [GCControllerButtonInput?] = [
            g.buttonA, g.buttonB, g.buttonX, g.buttonY, g.leftShoulder, g.rightShoulder, g.leftTrigger, g.rightTrigger,
            g.buttonOptions, g.buttonMenu, g.leftThumbstickButton, g.rightThumbstickButton,
            g.dpad.up, g.dpad.down, g.dpad.left, g.dpad.right, g.buttonHome,
        ]
        for (i, b) in order.enumerated() where b?.isPressed == true { buttons |= 1 << UInt32(i) }
        // The triggers' bits: pressed past half way (their values go as well).
        buttons &= ~(1 << 6 | 1 << 7)
        if g.leftTrigger.value > 0.5 { buttons |= 1 << 6 }
        if g.rightTrigger.value > 0.5 { buttons |= 1 << 7 }
        // Up is positive in GameController, negative in the protocol (as on the web).
        lx = Self.axis(g.leftThumbstick.xAxis.value)
        ly = Self.axis(-g.leftThumbstick.yAxis.value)
        rx = Self.axis(g.rightThumbstick.xAxis.value)
        ry = Self.axis(-g.rightThumbstick.yAxis.value)
        lt = Self.trigger(g.leftTrigger.value)
        rt = Self.trigger(g.rightTrigger.value)
    }
}

/// Gamepads (MFi, Xbox, PlayStation, Switch…) as they connect: each one's state to the session when
/// it changes.
final class Gamepads {
    private var observers: [NSObjectProtocol] = []
    private var last: [ObjectIdentifier: GamepadState] = [:]
    var onState: ((GamepadState) -> Void)?
    private(set) var connected = 0

    init() {
        let nc = NotificationCenter.default
        observers.append(nc.addObserver(forName: .GCControllerDidConnect, object: nil, queue: .main) { [weak self] _ in self?.refresh() })
        observers.append(nc.addObserver(forName: .GCControllerDidDisconnect, object: nil, queue: .main) { [weak self] _ in self?.refresh() })
        GCController.startWirelessControllerDiscovery {}
        refresh()
    }

    deinit {
        observers.forEach(NotificationCenter.default.removeObserver)
        GCController.stopWirelessControllerDiscovery()
    }

    private func refresh() {
        let pads = GCController.controllers().filter { $0.extendedGamepad != nil }
        // A pad that went: once more at rest, so nothing stays held on the server.
        let here = Set(pads.map(ObjectIdentifier.init))
        for (id, s) in last where !here.contains(id) {
            var rest = GamepadState()
            rest.pad = s.pad
            onState?(rest)
            last[id] = nil
        }
        connected = pads.count
        for (i, c) in pads.enumerated() {
            let pad = UInt8(min(i, 255))
            c.playerIndex = GCControllerPlayerIndex(rawValue: i) ?? .indexUnset
            c.extendedGamepad?.valueChangedHandler = { [weak self] g, _ in
                guard let self else { return }
                let s = GamepadState(pad: pad, g)
                if self.last[ObjectIdentifier(c)] != s {
                    self.last[ObjectIdentifier(c)] = s
                    self.onState?(s)
                }
            }
        }
    }
}
