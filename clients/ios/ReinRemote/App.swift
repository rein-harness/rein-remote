import SwiftUI
import UIKit

@main
struct ReinRemoteApp: App {
    init() { Core.setUp() }

    var body: some Scene {
        WindowGroup { RootView() }
    }
}

/// What to connect to, and how.
struct Target: Identifiable, Hashable {
    let url: String
    let name: String
    var pairCode: String?
    var serverId: String?
    var id: String { url }
}

/// Launch arguments, for tests and measuring (`-RRConnect URL [-RRPair CODE] [-RRMeasure YES]
/// [-RRSelfTest YES] [-RRSeconds N] [-RRAudio NO] [-RRWebTransport NO] [-RRHEVC YES]`).
enum Launch {
    static let d = UserDefaults.standard
    static var connect: String? { d.string(forKey: "RRConnect") }
    static var pair: String? { d.string(forKey: "RRPair") }
    static var measure: Bool { d.bool(forKey: "RRMeasure") || selfTest }
    static var selfTest: Bool { d.bool(forKey: "RRSelfTest") }
    static var seconds: Double { d.object(forKey: "RRSeconds") == nil ? 10 : d.double(forKey: "RRSeconds") }
    static var audio: Bool { d.object(forKey: "RRAudio") == nil ? true : d.bool(forKey: "RRAudio") }
    static var webtransport: Bool { d.object(forKey: "RRWebTransport") == nil ? true : d.bool(forKey: "RRWebTransport") }
    /// `-RRHEVC YES`: offer HEVC even where it isn't decoded in hardware (the simulator).
    static var hevc: Bool { d.bool(forKey: "RRHEVC") }
}

struct RootView: View {
    @State private var target: Target?

    var body: some View {
        ServerList(open: { target = $0 })
            .fullScreenCover(item: $target) { t in StreamScreen(target: t) { target = nil } }
            .onAppear {
                if let url = Launch.connect, target == nil { target = Target(url: url, name: "launch", pairCode: Launch.pair) }
            }
    }
}

struct ServerList: View {
    let open: (Target) -> Void
    @StateObject private var discovery = Discovery()
    @StateObject private var saved = SavedServers()
    @State private var pairing: FoundServer?
    @State private var adding = false
    @State private var error: String?
    @State private var busy = false

    var body: some View {
        NavigationStack {
            List {
                Section {
                    if discovery.found.isEmpty {
                        Label("Looking for computers running rein-remote…", systemImage: "antenna.radiowaves.left.and.right")
                            .foregroundStyle(.secondary)
                    }
                    ForEach(discovery.found) { s in
                        Button { choose(s) } label: {
                            HStack {
                                Image(systemName: "desktopcomputer")
                                VStack(alignment: .leading) {
                                    Text(s.name)
                                    Text(Core.isPaired(s.id) ? "paired" : "pair with its code").font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                        .accessibilityIdentifier("server-\(s.name)")
                    }
                } header: { Text("On this network") } footer: {
                    if let e = discovery.error { Text(e) }
                }
                let away = saved.list.filter { s in !discovery.found.contains { $0.id == s.serverId } }
                if !away.isEmpty {
                    Section("Paired") {
                        ForEach(away) { s in
                            Button { open(Target(url: s.url, name: s.name, serverId: s.serverId)) } label: {
                                VStack(alignment: .leading) {
                                    Text(s.name)
                                    Text(s.url).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                        .onDelete { idx in idx.map { away[$0] }.forEach(saved.remove) }
                    }
                }
                if let error { Section { Text(error).foregroundStyle(.red) } }
            }
            .navigationTitle("Rein Remote")
            .toolbar {
                Button { adding = true } label: { Image(systemName: "plus") }.accessibilityLabel("Add a computer")
            }
            .overlay { if busy { ProgressView() } }
            .sheet(item: $pairing) { s in
                PairSheet(title: "Pair with \(s.name)", link: nil) { code, _ in
                    pairing = nil
                    connect(s, code: code)
                }
            }
            .sheet(isPresented: $adding) {
                PairSheet(title: "Add a computer", link: "") { code, link in
                    adding = false
                    guard let link, let p = PairLink(link) else { return }
                    open(Target(url: p.url, name: URLComponents(string: p.url)?.host ?? "server", pairCode: code ?? p.code, serverId: p.id))
                }
            }
        }
        .onAppear { discovery.start() }
    }

    private func choose(_ s: FoundServer) {
        if Core.isPaired(s.id) { connect(s, code: nil) } else { pairing = s }
    }

    private func connect(_ s: FoundServer, code: String?) {
        busy = true
        error = nil
        Task {
            do {
                let url = try await Discovery.resolve(s)
                await MainActor.run {
                    busy = false
                    saved.add(SavedServer(name: s.name, url: url, serverId: s.id))
                    open(Target(url: url, name: s.name, pairCode: code, serverId: s.id))
                }
            } catch {
                await MainActor.run {
                    busy = false
                    self.error = "\(s.name): \(error.localizedDescription)"
                }
            }
        }
    }
}

/// The code a server shows (typed, or scanned from its QR code), or a link (with `link` non-nil).
struct PairSheet: View {
    let title: String
    let link: String?
    let done: (String?, String?) -> Void
    @State private var code = ""
    @State private var url = ""
    @State private var scanning = false

    var body: some View {
        NavigationStack {
            Form {
                if link != nil {
                    Section {
                        TextField("http://computer:7979/?token=…", text: $url)
                            .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
                            .accessibilityIdentifier("link")
                    } header: { Text("A link it printed") } footer: {
                        Text("Any link `rein-remote serve` prints, a relay's link, or the computer's address to pair with.")
                    }
                }
                Section {
                    TextField("123-456", text: $code).keyboardType(.numbersAndPunctuation).accessibilityIdentifier("code")
                    Button("Scan its QR code") { scanning = true }
                } header: { Text(link == nil ? "The code it shows" : "Its pairing code (if no token)") }
            }
            .navigationTitle(title)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Connect") { done(code.isEmpty ? nil : code, link == nil ? nil : url) }
                        .disabled(link == nil ? code.isEmpty : url.isEmpty)
                }
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { done(nil, nil) } }
            }
            .sheet(isPresented: $scanning) {
                QRScanner { s in
                    scanning = false
                    guard let p = PairLink(s) else { return }
                    code = p.code ?? code
                    if link != nil { url = s }
                    if link == nil || !url.isEmpty { done(p.code, link == nil ? nil : s) }
                }
            }
        }
    }
}

/// The remote screen, full screen, with a small bar: the keyboard, the numbers, disconnect.
struct StreamScreen: View {
    let target: Target
    let dismiss: () -> Void
    @StateObject private var c = StreamController()
    @State private var showStats = Launch.measure
    @State private var selfTest = ""
    @State private var failed: String?

    var body: some View {
        ZStack(alignment: .topLeading) {
            StreamRepresentable(view: c.view).ignoresSafeArea()
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 14) {
                    Button { dismiss() } label: { Image(systemName: "xmark.circle.fill") }.accessibilityLabel("Disconnect")
                    Button { c.view.toggleKeyboard() } label: { Image(systemName: "keyboard") }.accessibilityLabel("Keyboard")
                    Button { showStats.toggle() } label: { Image(systemName: "speedometer") }.accessibilityLabel("Stats")
                    Text(c.serverName.isEmpty ? target.name : c.serverName).font(.caption.bold())
                }
                .font(.title3)
                .padding(8)
                .background(.ultraThinMaterial, in: Capsule())
                if showStats || failed != nil || c.state != "streaming" {
                    Text(failed ?? (c.state == "streaming" ? c.overlay : c.state))
                        .font(.caption.monospaced())
                        .padding(6)
                        .background(.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 6))
                        .foregroundStyle(.white)
                        .accessibilityIdentifier("status")
                }
                if !c.inputSeen.isEmpty && showStats {
                    Text(c.inputSeen).font(.caption2.monospaced()).foregroundStyle(.green).accessibilityIdentifier("input")
                }
                if !selfTest.isEmpty {
                    Text(selfTest).font(.caption2.monospaced()).foregroundStyle(.yellow).accessibilityIdentifier("selftest")
                }
            }
            .padding(.leading, 12)
            .padding(.top, 4)
        }
        .statusBarHidden()
        .persistentSystemOverlays(.hidden)
        .task {
            c.measure = Launch.measure
            var o = ConnectOptions()
            o.audio = Launch.audio
            o.webtransport = Launch.webtransport
            if Launch.hevc { o.codecs = [.h265, .h264] }
            do {
                try await c.connect(target: target.url, pairCode: target.pairCode, options: o)
                if Launch.selfTest { selfTest = await SelfTest.run(c, seconds: Launch.seconds) }
            } catch {
                failed = error.localizedDescription
                NSLog("rein-remote: couldn't connect: %@", error.localizedDescription)
                if Launch.selfTest { selfTest = "selftest: FAIL couldn't connect: \(error.localizedDescription)" }
            }
        }
        .onDisappear { c.close() }
    }
}

struct StreamRepresentable: UIViewRepresentable {
    let view: StreamUIView
    func makeUIView(context: Context) -> StreamUIView {
        DispatchQueue.main.async { view.becomeFirstResponder() }
        return view
    }

    func updateUIView(_ v: StreamUIView, context: Context) {}
}
