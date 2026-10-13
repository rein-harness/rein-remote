import SwiftUI

struct MenuBarLabel: View {
    let model: HostModel

    var body: some View {
        let sharing = model.isConnected && model.state?.sharing == true
        Image(systemName: sharing ? "dot.radiowaves.left.and.right" : "display")
            .accessibilityLabel("Rein Remote: \(model.statusText)")
    }
}

/// The menu bar's panel: status, start/stop, the source, the pairing code, viewers, and the way to
/// the window.
struct MenuBarContent: View {
    @Bindable var model: HostModel
    let open: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Rein Remote").font(.headline)
                Spacer()
                Text(model.statusText)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("menuStatus")
            }
            if model.isConnected, let s = model.state {
                Button {
                    Task { await model.toggleSharing() }
                } label: {
                    Label(s.sharing ? "Stop sharing" : "Start sharing", systemImage: s.sharing ? "stop.fill" : "play.fill")
                        .frame(maxWidth: .infinity)
                }
                .controlSize(.large)
                .buttonStyle(.borderedProminent)
                .tint(s.sharing ? .red : .accentColor)
                .disabled(model.busy)
                .accessibilityIdentifier("menuSharingToggle")
                if let e = s.error { ErrorText(message: e, identifier: "menuError") }
                Divider()
                LabeledContent("Source") {
                    Text(s.source).lineLimit(1).truncationMode(.middle)
                }
                .accessibilityIdentifier("menuSource")
                LabeledContent("Viewers") { Text("\(s.viewers)") }
                    .accessibilityIdentifier("menuViewers")
                if s.pairing.enabled, let code = s.pairing.code {
                    LabeledContent("Pairing code") {
                        Text(code).font(.body.monospaced().bold())
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("menuPairingCode")
                }
            } else {
                switch model.connection {
                case .notRunning(let why):
                    if let why { Text(why).font(.caption).foregroundStyle(.secondary).lineLimit(3) }
                    Button("Start the host") { model.startHost() }
                        .accessibilityIdentifier("menuStartHost")
                default:
                    ProgressView().controlSize(.small)
                }
            }
            Divider()
            Button("Open Rein Remote…", action: open)
                .keyboardShortcut("o")
                .accessibilityIdentifier("menuOpen")
            Button("Quit") { NSApp.terminate(nil) }
                .keyboardShortcut("q")
                .accessibilityIdentifier("menuQuit")
        }
        .buttonStyle(.borderless)
        .padding(14)
        .frame(width: 300)
    }
}
