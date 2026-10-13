// Rein Remote for macOS: a menu-bar app (LSUIElement) on `rein-remote host`, with its window on
// demand. Everything goes through the control API (docs/control-api.md).
//
// For tests and screenshots, at launch: RR_OPEN_WINDOW=1 opens the window, RR_PAGE=<page> picks its
// page, RR_APPEARANCE=light|dark sets the appearance, RR_NO_ACTIVATE=1 leaves the app in the back.

import AppKit
import SwiftUI

@main
struct ReinRemoteHostApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var delegate

    var body: some Scene {
        MenuBarExtra {
            MenuBarContent(model: delegate.model, open: { delegate.showWindow() })
        } label: {
            MenuBarLabel(model: delegate.model)
        }
        .menuBarExtraStyle(.window)
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    let model = HostModel()
    private var window: NSWindow?

    func applicationDidFinishLaunching(_ note: Notification) {
        let env = ProcessInfo.processInfo.environment
        // Hosting unit tests: no host started, no window.
        let underUnitTests = env["XCTestConfigurationFilePath"] != nil || env["XCTestBundlePath"] != nil
        if underUnitTests, env["RR_UI_TEST"] != "1" {
            return
        }
        switch env["RR_APPEARANCE"] {
        case "dark": NSApp.appearance = NSAppearance(named: .darkAqua)
        case "light": NSApp.appearance = NSAppearance(named: .aqua)
        default: break
        }
        if let p = env["RR_PAGE"].flatMap(Page.init(rawValue:)) { model.page = p }
        model.begin()
        // The window on the first run (so a new user sees something), and when asked.
        let firstRun = !UserDefaults.standard.bool(forKey: "shownOnce")
        if env["RR_OPEN_WINDOW"] == "1" || firstRun {
            UserDefaults.standard.set(true, forKey: "shownOnce")
            showWindow(activate: env["RR_NO_ACTIVATE"] != "1")
        }
    }

    func applicationWillTerminate(_ note: Notification) {
        // A host this app started ends with it; one started otherwise (launchd, a terminal) stays.
        model.end()
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        showWindow()
        return true
    }

    func showWindow(activate: Bool = true) {
        if window == nil {
            let w = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 920, height: 640),
                styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                backing: .buffered, defer: false)
            w.title = "Rein Remote"
            w.identifier = NSUserInterfaceItemIdentifier("main")
            w.isReleasedWhenClosed = false
            w.contentMinSize = NSSize(width: 760, height: 520)
            let content = NSHostingView(rootView: MainView(model: model))
            // The window's size is the user's: not the content's ideal size (a long form would make
            // it taller than the screen).
            content.sizingOptions = []
            w.contentView = content
            // Where the user left it, if that's on a screen; tests always get the same size.
            let fixed = ProcessInfo.processInfo.environment["RR_UI_TEST"] == "1"
            let restored = !fixed && w.setFrameUsingName("ReinRemoteMain")
            let screen = (w.screen ?? NSScreen.main)?.visibleFrame
            if !restored || screen.map({ !$0.contains(w.frame) }) == true {
                w.setContentSize(NSSize(width: 920, height: 640))
                w.center()
            }
            if !fixed { w.setFrameAutosaveName("ReinRemoteMain") }
            w.delegate = self
            window = w
        }
        window?.makeKeyAndOrderFront(nil)
        if activate { NSApp.activate(ignoringOtherApps: true) }
    }
}
