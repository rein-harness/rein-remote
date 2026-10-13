import SwiftUI

struct SharingView: View {
    @Bindable var model: HostModel
    @State private var sources: Sources?
    @State private var loadingSources = false
    @State private var qrLink: String?

    var body: some View {
        if let s = model.state {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    header(s)
                    Divider()
                    sourceSection(s)
                    Divider()
                    linksSection(s)
                    Divider()
                    pairingSection(s)
                }
                .padding(20)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .task { await loadSources() }
        }
    }

    // MARK: Sharing on and off

    private func header(_ s: HostState) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 12) {
                Image(systemName: s.sharing ? "dot.radiowaves.left.and.right" : "pause.circle")
                    .font(.system(size: 28))
                    .foregroundStyle(s.sharing ? .green : .secondary)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(s.sharing ? "Sharing" : "Not sharing").font(.title2.bold())
                        .accessibilityIdentifier("sharingStatus")
                    Text(s.sharing ? "Viewers connect at \(s.listen)" : "Start sharing to let viewers in.")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    Task { await model.toggleSharing() }
                } label: {
                    Text(s.sharing ? "Stop sharing" : "Start sharing").frame(minWidth: 110)
                }
                .controlSize(.large)
                .buttonStyle(.borderedProminent)
                .tint(s.sharing ? .red : .accentColor)
                .disabled(model.busy)
                .accessibilityIdentifier("sharingToggle")
            }
            if let e = s.error { ErrorText(message: e, identifier: "stateError") }
            if s.restartNeeded {
                HStack {
                    Label("Settings changed while viewers were connected. They apply when sharing restarts.", systemImage: "arrow.clockwise")
                    Spacer()
                    Button("Restart sharing") { Task { await model.restartSharing() } }
                        .accessibilityIdentifier("restartSharing")
                }
                .accessibilityIdentifier("restartNeeded")
            }
        }
    }

    // MARK: Source

    private func sourceSection(_ s: HostState) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionTitle(text: "What's shared")
            HStack {
                Picker("Source", selection: Binding(
                    get: { s.source },
                    set: { id in if id != s.source { Task { await model.setSource(id) } } }
                )) {
                    if !known(s.source) {
                        Text(sourceName(s.source)).tag(s.source)
                    }
                    if let src = sources {
                        if !src.displays.isEmpty {
                            Section("Displays") {
                                ForEach(src.displays, id: \.id) { d in
                                    Text(d.width.map { "\(d.name) (\($0)×\(d.height ?? 0))" } ?? d.name).tag(d.id)
                                }
                            }
                        }
                        if !src.apps.isEmpty {
                            Section("Apps") {
                                ForEach(src.apps, id: \.id) { a in Text(a.name).tag(a.id) }
                            }
                        }
                        if !src.windows.isEmpty {
                            Section("Windows") {
                                ForEach(src.windows, id: \.id) { w in
                                    Text(w.title.isEmpty ? w.app : "\(w.app): \(w.title)").tag(w.id)
                                }
                            }
                        }
                        if !src.other.isEmpty {
                            Section("Other") {
                                ForEach(src.other, id: \.id) { o in Text(o.name).tag(o.id) }
                            }
                        }
                    }
                }
                .labelsHidden()
                .fixedSize()
                .disabled(model.busy)
                .accessibilityLabel("Source")
                .accessibilityIdentifier("sourcePicker")
                Button {
                    Task { await loadSources() }
                } label: {
                    if loadingSources { ProgressView().controlSize(.small) } else { Image(systemName: "arrow.clockwise") }
                }
                .help("List the displays, windows and apps again")
                .accessibilityLabel("Refresh the sources")
                .accessibilityIdentifier("refreshSources")
            }
            Text("Now: \(s.source)")
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .accessibilityLabel("Current source: \(s.source)")
                .accessibilityIdentifier("currentSource")
            if let e = sources?.error {
                // Not an error for what's shared now (a test pattern needs nothing): a note.
                HStack(alignment: .firstTextBaseline) {
                    Image(systemName: "info.circle").foregroundStyle(.orange).accessibilityHidden(true)
                    Text(e).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier("sourcesError")
                    Spacer()
                    Button("Permissions…") { model.page = .permissions }
                        .accessibilityIdentifier("goPermissions")
                }
                .font(.callout)
            }
        }
    }

    private func known(_ id: String) -> Bool {
        guard let s = sources else { return false }
        return s.displays.contains { $0.id == id } || s.windows.contains { $0.id == id }
            || s.apps.contains { $0.id == id } || s.other.contains { $0.id == id }
    }

    private func sourceName(_ id: String) -> String {
        if id.hasPrefix("test-pattern:") { return "Test pattern (\(id.dropFirst("test-pattern:".count)))" }
        if id.hasPrefix("display:") { return "Display \(id.dropFirst("display:".count))" }
        if id == "display" { return "The main display" }
        if id.hasPrefix("app:") { return String(id.dropFirst(4)) }
        return id
    }

    private func loadSources() async {
        loadingSources = true
        defer { loadingSources = false }
        if case .success(let s) = await model.call(showError: false, { try await $0.sources() }) { sources = s }
    }

    // MARK: Links

    private func linksSection(_ s: HostState) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionTitle(text: "Links")
            if s.links.isEmpty {
                Text(s.sharing ? "No links." : "Links appear while sharing.").foregroundStyle(.secondary)
                    .accessibilityIdentifier("noLinks")
            } else {
                Text("Each link is a key to this screen: anyone who has it can watch and control it.")
                    .font(.callout).foregroundStyle(.secondary)
                HStack(alignment: .top, spacing: 20) {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(Array(s.links.enumerated()), id: \.element.id) { i, link in
                            linkRow(link, index: i, shown: (qrLink ?? s.links[0].qr) == link.qr)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    let shown = s.links.first { $0.qr == qrLink } ?? s.links[0]
                    VStack(spacing: 4) {
                        QRCodeView(text: shown.qr, label: "QR code for the link: \(shown.label)", size: 160)
                            .accessibilityIdentifier("linkQR")
                        Text(shown.label).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
    }

    private func linkRow(_ link: Link, index: Int, shown: Bool) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(link.label.capitalized).font(.subheadline.bold())
            HStack {
                Text(link.url)
                    .font(.callout.monospaced())
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                    .accessibilityLabel("Link for \(link.label)")
                    .accessibilityValue(link.url)
                    .accessibilityIdentifier("link.\(index)")
                Spacer(minLength: 8)
                CopyButton(text: link.url, what: "the link for \(link.label)", identifier: "copyLink.\(index)")
                Button {
                    qrLink = link.qr
                } label: { Image(systemName: "qrcode") }
                    .disabled(shown)
                    .accessibilityLabel("Show the QR code for \(link.label)")
                    .accessibilityIdentifier("showQR.\(index)")
            }
        }
        .padding(8)
        .background(shown ? Color.accentColor.opacity(0.08) : .clear, in: RoundedRectangle(cornerRadius: 6))
    }

    // MARK: Pairing

    private func pairingSection(_ s: HostState) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionTitle(text: "Pair a device")
            if !s.pairing.enabled {
                Text(s.sharing ? "Pairing is off (Settings › Pairing)." : "Pairing works while sharing.")
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("pairingOff")
            } else {
                HStack(alignment: .top, spacing: 20) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Enter this code in Rein Remote on the phone, or scan the QR code with its camera. A code pairs one device.")
                            .font(.callout).foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                        if let code = s.pairing.code {
                            Text(code)
                                .font(.system(size: 40, weight: .semibold, design: .monospaced))
                                .textSelection(.enabled)
                                .accessibilityLabel("Pairing code")
                                .accessibilityValue(code)
                                .accessibilityIdentifier("pairingCode")
                            if let exp = s.pairing.expiresAt {
                                TimelineView(.periodic(from: .now, by: 1)) { ctx in
                                    let left = Int(exp.timeIntervalSince(ctx.date).rounded())
                                    Text(left > 0 ? "Expires in \(countdown(left))" : "Expired")
                                        .monospacedDigit()
                                        .foregroundStyle(left < 60 ? .orange : .secondary)
                                        .accessibilityIdentifier("pairingCountdown")
                                }
                            }
                        } else {
                            Text("No open code (used, expired, or too many wrong tries).")
                                .foregroundStyle(.secondary)
                                .accessibilityIdentifier("noPairingCode")
                        }
                        Button("New code") { Task { await model.newCode() } }
                            .disabled(model.busy)
                            .accessibilityLabel("Make a new pairing code")
                            .accessibilityIdentifier("newCode")
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    if let qr = s.pairing.qr, s.pairing.code != nil {
                        QRCodeView(text: qr, label: "QR code to pair a phone", size: 140)
                            .accessibilityIdentifier("pairingQR")
                    }
                }
            }
        }
    }
}
