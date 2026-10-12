import AVFoundation
import UIKit

/// The remote screen: the decoded video (AVSampleBufferDisplayLayer), the remote pointer drawn here
/// from the server's Cursor messages, and every kind of input: touch gestures, a trackpad or mouse on
/// iPad (moves, clicks with their buttons, scrolling), the on-screen and hardware keyboards.
final class StreamUIView: UIView, UIPointerInteractionDelegate, UIGestureRecognizerDelegate {
    let display = AVSampleBufferDisplayLayer()
    private let cursorLayer = CALayer()
    let keyboard = KeyboardField()
    weak var sink: InputSink? {
        didSet { touches.sink = sink }
    }
    private(set) lazy var touches = TouchGestures(sink: sink)
    /// The stream's size in pixels (Config).
    var streamSize = CGSize(width: 1280, height: 720) {
        didSet { setNeedsLayout() }
    }
    private var cursorAt = CGPoint.zero
    private var cursorVisible = false
    private var hot = CGPoint.zero
    private var localPointer: (CGPoint, CFTimeInterval)?
    private var lastMove: CFTimeInterval = 0
    private var buttonsDown: Set<MouseButton> = []
    private var scrollLast = CGPoint.zero
    private let haptics = UIImpactFeedbackGenerator(style: .light)

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        isMultipleTouchEnabled = true
        display.videoGravity = .resizeAspect
        display.backgroundColor = UIColor.black.cgColor
        layer.addSublayer(display)
        cursorLayer.magnificationFilter = .nearest
        cursorLayer.isHidden = true
        cursorLayer.zPosition = 10
        layer.addSublayer(cursorLayer)
        keyboard.owner = self
        addSubview(keyboard)
        isAccessibilityElement = true
        accessibilityIdentifier = "stream"
        accessibilityLabel = "Remote screen"
        accessibilityTraits = [.allowsDirectInteraction]

        // A trackpad or mouse on iPad: the pointer moves without a press; the system's own pointer is
        // hidden over the stream, where the remote one is drawn.
        addGestureRecognizer(UIHoverGestureRecognizer(target: self, action: #selector(hover(_:))))
        addInteraction(UIPointerInteraction(delegate: self))
        // Two fingers on a trackpad, or a mouse's wheel: scrolling (no touches on the screen).
        let scroll = UIPanGestureRecognizer(target: self, action: #selector(scroll(_:)))
        scroll.allowedScrollTypesMask = .all
        scroll.allowedTouchTypes = []
        scroll.delegate = self
        addGestureRecognizer(scroll)
        haptics.prepare()
        touches.onDragStart = { [weak self] in self?.haptics.impactOccurred() }
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override var canBecomeFirstResponder: Bool { true }

    override func layoutSubviews() {
        super.layoutSubviews()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        display.frame = bounds
        placeCursor()
        CATransaction.commit()
        keyboard.frame = CGRect(x: 0, y: bounds.height - 1, width: 1, height: 1)
    }

    /// Where the stream is drawn in this view (aspect fit).
    var videoRect: CGRect { AVMakeRect(aspectRatio: streamSize, insideRect: bounds) }

    /// A point in the view, in the stream's pixels.
    func streamPoint(_ p: CGPoint) -> CGPoint {
        let r = videoRect
        guard r.width > 0, r.height > 0 else { return .zero }
        let x = ((p.x - r.minX) * streamSize.width / r.width).rounded()
        let y = ((p.y - r.minY) * streamSize.height / r.height).rounded()
        return CGPoint(x: max(0, min(streamSize.width - 1, x)), y: max(0, min(streamSize.height - 1, y)))
    }

    /// A stream pixel's place in the view.
    func viewPoint(_ s: CGPoint) -> CGPoint {
        let r = videoRect
        return CGPoint(x: r.minX + s.x * r.width / streamSize.width, y: r.minY + s.y * r.height / streamSize.height)
    }

    // ---- the pointer, drawn here ----

    func cursor(x: Int, y: Int, visible: Bool, shape: CursorShape?) {
        if let s = shape { setShape(s) }
        cursorAt = CGPoint(x: x, y: y)
        cursorVisible = visible
        placeCursor()
    }

    private func setShape(_ s: CursorShape) {
        guard s.width > 0, s.height > 0, s.rgba.count >= s.width * s.height * 4,
              let provider = CGDataProvider(data: s.rgba as CFData),
              let img = CGImage(width: s.width, height: s.height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: s.width * 4,
                                space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue),
                                provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
        else {
            cursorLayer.contents = nil
            return
        }
        cursorLayer.contents = img
        cursorLayer.bounds = CGRect(x: 0, y: 0, width: s.width, height: s.height)
        hot = CGPoint(x: s.hotX, y: s.hotY)
    }

    private func placeCursor() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        defer { CATransaction.commit() }
        guard cursorLayer.contents != nil, cursorVisible, streamSize.width > 0 else {
            cursorLayer.isHidden = true
            return
        }
        // Drawn at its size on the stream (not smaller than 0.6 of its pixels).
        let k = max(videoRect.width / streamSize.width, 0.6)
        let size = cursorLayer.bounds.size
        // At once where the local pointer is, else where the server says.
        let at: CGPoint
        if let (p, t) = localPointer, CACurrentMediaTime() - t < 0.25 { at = p } else { at = viewPoint(cursorAt) }
        cursorLayer.anchorPoint = CGPoint(x: hot.x / max(size.width, 1), y: hot.y / max(size.height, 1))
        cursorLayer.setAffineTransform(CGAffineTransform(scaleX: k, y: k))
        cursorLayer.position = at
        cursorLayer.isHidden = false
    }

    // ---- touch, and a trackpad's or mouse's clicks ----

    override func touchesBegan(_ set: Set<UITouch>, with event: UIEvent?) {
        if !keyboard.isFirstResponder && !isFirstResponder { becomeFirstResponder() }
        for t in set {
            let p = t.location(in: self)
            if t.type == .indirectPointer {
                let b: MouseButton = event?.buttonMask.contains(.secondary) == true ? .right : .left
                buttonsDown.insert(b)
                pointerTo(p, force: true)
                sink?.button(b, down: true)
            } else {
                touches.down(id: ObjectIdentifier(t), at: p, stream: streamPoint(p))
            }
        }
    }

    override func touchesMoved(_ set: Set<UITouch>, with event: UIEvent?) {
        let scale = videoRect.width > 0 ? streamSize.width / videoRect.width : 1
        for t in set {
            let p = t.location(in: self)
            if t.type == .indirectPointer {
                pointerTo(p)
            } else {
                touches.move(id: ObjectIdentifier(t), at: p, stream: streamPoint(p), scale: scale)
            }
        }
    }

    override func touchesEnded(_ set: Set<UITouch>, with event: UIEvent?) { ended(set, cancelled: false) }
    override func touchesCancelled(_ set: Set<UITouch>, with event: UIEvent?) { ended(set, cancelled: true) }

    private func ended(_ set: Set<UITouch>, cancelled: Bool) {
        for t in set {
            let p = t.location(in: self)
            if t.type == .indirectPointer {
                pointerTo(p, force: true)
                for b in buttonsDown { sink?.button(b, down: false) }
                buttonsDown.removeAll()
            } else {
                touches.up(id: ObjectIdentifier(t), stream: streamPoint(p), cancelled: cancelled)
            }
        }
    }

    /// The local pointer moved: drawn there at once, sent at most every 8 ms.
    private func pointerTo(_ p: CGPoint, force: Bool = false) {
        localPointer = (p, CACurrentMediaTime())
        placeCursor()
        let now = CACurrentMediaTime()
        if !force && now - lastMove < 0.008 { return }
        lastMove = now
        let s = streamPoint(p)
        sink?.pointerMove(Int(s.x), Int(s.y))
    }

    @objc private func hover(_ g: UIHoverGestureRecognizer) {
        switch g.state {
        case .began, .changed: pointerTo(g.location(in: self))
        default:
            localPointer = nil
            placeCursor()
        }
    }

    @objc private func scroll(_ g: UIPanGestureRecognizer) {
        let t = g.translation(in: self)
        if g.state == .began { scrollLast = .zero }
        let scale = videoRect.width > 0 ? streamSize.width / videoRect.width : 1
        // Content follows the fingers, as on a Mac's trackpad.
        let dx = Int(((scrollLast.x - t.x) * scale).rounded()), dy = Int(((scrollLast.y - t.y) * scale).rounded())
        scrollLast = t
        if dx != 0 || dy != 0 { sink?.wheel(dx: dx, dy: dy) }
    }

    func pointerInteraction(_ interaction: UIPointerInteraction, styleFor region: UIPointerRegion) -> UIPointerStyle? {
        // The remote pointer is drawn instead (when the server sends its shape).
        cursorLayer.contents != nil ? .hidden() : nil
    }

    func gestureRecognizer(_ g: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool { true }

    // ---- a hardware keyboard ----

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if !handle(presses, down: true) { super.pressesBegan(presses, with: event) }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if !handle(presses, down: false) { super.pressesEnded(presses, with: event) }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if !handle(presses, down: false) { super.pressesCancelled(presses, with: event) }
    }

    /// Keys to the server, by name and HID usage; false when none of them was a key.
    @discardableResult
    func handle(_ presses: Set<UIPress>, down: Bool) -> Bool {
        var any = false
        for p in presses {
            guard let k = p.key, let name = Keys.name(k) else { continue }
            sink?.key(hid: UInt16(truncatingIfNeeded: k.keyCode.rawValue), down: down, name: name)
            any = true
        }
        return any
    }

    // ---- the on-screen keyboard ----

    func toggleKeyboard() {
        if keyboard.isFirstResponder { _ = becomeFirstResponder() } else { _ = keyboard.becomeFirstResponder() }
    }
}

/// Takes the on-screen keyboard's typing: text as text (any language, emoji, autocorrect off), a
/// newline as Enter, delete as Backspace. A hardware keyboard's keys go as keys (presses) instead.
final class KeyboardField: UIView, UIKeyInput {
    weak var owner: StreamUIView?

    override var canBecomeFirstResponder: Bool { true }
    var hasText: Bool { true }
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    var keyboardType: UIKeyboardType = .asciiCapable

    func insertText(_ text: String) {
        guard let sink = owner?.sink else { return }
        for (i, line) in text.components(separatedBy: "\n").enumerated() {
            if i > 0 { tap(sink, "Enter", .keyboardReturnOrEnter) }
            if !line.isEmpty { sink.text(line) }
        }
    }

    func deleteBackward() {
        if let sink = owner?.sink { tap(sink, "Backspace", .keyboardDeleteOrBackspace) }
    }

    private func tap(_ sink: InputSink, _ name: String, _ hid: UIKeyboardHIDUsage) {
        sink.key(hid: UInt16(hid.rawValue), down: true, name: name)
        sink.key(hid: UInt16(hid.rawValue), down: false, name: name)
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if owner?.handle(presses, down: true) != true { super.pressesBegan(presses, with: event) }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if owner?.handle(presses, down: false) != true { super.pressesEnded(presses, with: event) }
    }
}
