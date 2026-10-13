import AppKit
import ApplicationServices
import CoreGraphics
import SwiftUI

/// Screen Recording and Accessibility: read without making macOS prompt (never on launch, never
/// here), with a button each that opens the right pane of System Settings.
struct PermissionsView: View {
    @Bindable var model: HostModel
    @State private var perms: Permissions?
    @State private var error: String?

    private static let panes = [
        "screen_recording": "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
        "accessibility": "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
    ]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text("macOS asks for these once, in System Settings › Privacy & Security. Turn on **\(who)** there.")
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("permissionsWho")
                if let error { ErrorText(message: error, identifier: "permissionsError") }
                row(
                    "Screen Recording", which: "screen_recording", value: current?.screenRecording,
                    why: "Needed to share a display, a window or an app (a test pattern or VNC needs none).")
                row(
                    "Accessibility", which: "accessibility", value: current?.accessibility,
                    why: "Needed for viewers to control this Mac: clicks and keys.")
                Divider()
                Text("macOS applies a new Screen Recording grant when the host restarts.")
                    .foregroundStyle(.secondary)
                HStack {
                    Button("Check again") { Task { await load() } }
                        .accessibilityIdentifier("checkPermissions")
                    if model.weStartedIt {
                        Button("Restart the host") { model.restartHost() }
                            .accessibilityIdentifier("restartHost")
                    }
                }
                if perms == nil {
                    Text("The host isn't connected: this is what macOS says for this app itself.")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .padding(20)
            .frame(maxWidth: 640, alignment: .leading)
        }
        .task(id: model.isConnected) { await load() }
    }

    /// Who macOS asks about: the app that started the host (its responsible process).
    private var who: String {
        if model.weStartedIt || !model.isConnected { return "Rein Remote" }
        return "the app that started rein-remote host (pid \(model.state?.pid ?? 0)), or rein-remote itself when it starts at login"
    }

    /// The host's answer, else this app's own (both only read; neither prompts).
    private var current: Permissions? {
        if let perms { return perms }
        return Permissions(
            platform: "macos",
            screenRecording: CGPreflightScreenCaptureAccess() ? "granted" : "denied",
            accessibility: AXIsProcessTrusted() ? "granted" : "denied")
    }

    private func row(_ title: String, which: String, value: String?, why: String) -> some View {
        let granted = value == "granted" || value == "not_needed"
        return HStack(alignment: .top, spacing: 12) {
            Image(systemName: granted ? "checkmark.circle.fill" : "xmark.circle.fill")
                .font(.title2)
                .foregroundStyle(granted ? .green : .orange)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.headline)
                Text(label(value))
                    .foregroundStyle(granted ? .green : .orange)
                    .accessibilityIdentifier("\(which)Status")
                Text(why).font(.callout).foregroundStyle(.secondary)
            }
            Spacer()
            if value != "not_needed" {
                Button("Open System Settings") { Task { await open(which) } }
                    .accessibilityLabel("Open System Settings at \(title)")
                    .accessibilityIdentifier("open.\(which)")
            }
        }
        .padding(12)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(title): \(label(value))")
    }

    private func label(_ v: String?) -> String {
        switch v {
        case "granted": return "Allowed"
        case "denied": return "Not allowed"
        case "not_needed": return "Not needed here"
        default: return "Unknown"
        }
    }

    private func load() async {
        guard model.isConnected else { perms = nil; return }
        switch await model.call(showError: false, { try await $0.permissions() }) {
        case .success(let p): perms = p; error = nil
        case .failure(let e): error = e.localizedDescription
        }
    }

    /// Asks the host where the pane is (it knows its platform), and opens it from here so System
    /// Settings comes forward over this app.
    private func open(_ which: String) async {
        var url = Self.panes[which]
        if model.isConnected, case .success(let r) = await model.call(showError: false, { try await $0.permissionURL(which) }) {
            url = r.url ?? url
        }
        if let s = url, let u = URL(string: s) { NSWorkspace.shared.open(u) }
    }
}
