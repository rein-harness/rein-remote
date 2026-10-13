import ServiceManagement
import SwiftUI

struct SettingsView: View {
    @Bindable var model: HostModel
    @State private var message: String?
    @State private var error: String?
    @State private var fpsText = ""
    @State private var bitrateText = ""
    @State private var listenText = ""
    @State private var newRelay = ""
    @State private var relaySecret = ""
    @State private var recordText = ""
    @State private var auditText = ""
    @State private var loginItem = SMAppService.mainApp.status == .enabled
    @State private var loginError: String?
    @State private var serverLogin: StartAtLogin?

    var body: some View {
        if let all = model.settings {
            let s = all.settings
            Form {
                if let error { Section { ErrorText(message: error, identifier: "settingsError") } }
                if let message {
                    Section {
                        HStack {
                            InfoText(message: message, identifier: "settingsApplied")
                            if model.state?.restartNeeded == true {
                                Spacer()
                                Button("Restart sharing now") { Task { await model.restartSharing() } }
                                    .accessibilityIdentifier("settingsRestart")
                            }
                        }
                    }
                }
                Section("Picture") {
                    Picker("When the link is slow", selection: bind(s.prefer, key: "prefer") { .string($0) }) {
                        Text("Frame rate first").tag("framerate")
                        Text("Quality first").tag("quality")
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("prefer")
                    Text(s.prefer == "quality"
                        ? "The picture keeps its size and detail; frames give way first (text, detail)."
                        : "Frames keep coming; the bitrate, then the picture's size give way first (games, motion).")
                        .font(.caption).foregroundStyle(.secondary)
                    autoOrNumber("Frame rate", value: s.fps, key: "fps", text: $fpsText, unit: "fps", range: "1–240", id: "fps")
                    autoOrNumber("Most bitrate", value: s.bitrate, key: "bitrate", text: $bitrateText, unit: "kbit/s", range: "kbit/s", id: "bitrate")
                    Picker("Codec", selection: bind(s.codec, key: "codec") { .string($0) }) {
                        Text("H.264").tag("h264")
                        Text("HEVC (H.265)").tag("h265")
                        Text("Automatic").tag("auto")
                    }
                    .accessibilityIdentifier("codec")
                    Toggle("Send this Mac's sound", isOn: bind(s.audio, key: "audio") { .bool($0) })
                        .accessibilityIdentifier("audio")
                }
                Section("Network") {
                    Toggle("Pairing with a code", isOn: bind(s.pair, key: "pair") { .bool($0) })
                        .accessibilityIdentifier("pair")
                    Toggle("Found by name on this network (mDNS)", isOn: bind(s.mdns, key: "mdns") { .bool($0) })
                        .accessibilityIdentifier("mdns")
                    Toggle("WebTransport (QUIC)", isOn: bind(s.webtransport, key: "webtransport") { .bool($0) })
                        .accessibilityIdentifier("webtransport")
                    Toggle("A certificate from Tailscale", isOn: bind(s.tailscaleCert, key: "tailscale-cert") { .bool($0) })
                        .accessibilityIdentifier("tailscaleCert")
                    LabeledContent("Listen on") {
                        HStack {
                            TextField("Listen on", text: $listenText, prompt: Text("auto, or ADDR:PORT"))
                                .labelsHidden()
                                .frame(maxWidth: 200)
                                .onSubmit { applyListen() }
                                .accessibilityLabel("Listen on")
                                .accessibilityIdentifier("listen")
                            Button("Apply") { applyListen() }
                                .disabled(listenText == s.listen)
                                .accessibilityIdentifier("applyListen")
                        }
                    }
                }
                Section("Relays") {
                    if s.relay.isEmpty {
                        Text("None: viewers reach this Mac directly (or through your tailnet).").foregroundStyle(.secondary)
                    }
                    ForEach(Array(s.relay.enumerated()), id: \.offset) { i, r in
                        HStack {
                            Text(r).font(.body.monospaced())
                            Spacer()
                            Button {
                                var l = s.relay
                                l.remove(at: i)
                                apply(["relay": .array(l.map { .string($0) })])
                            } label: { Image(systemName: "minus.circle") }
                                .buttonStyle(.borderless)
                                .accessibilityLabel("Remove the relay \(r)")
                                .accessibilityIdentifier("removeRelay.\(i)")
                        }
                    }
                    HStack {
                        TextField("Add a relay", text: $newRelay, prompt: Text("wss://relay.example"))
                            .onSubmit { addRelay(s.relay) }
                            .accessibilityIdentifier("newRelay")
                        Button("Add") { addRelay(s.relay) }
                            .disabled(newRelay.trimmingCharacters(in: .whitespaces).isEmpty)
                            .accessibilityIdentifier("addRelay")
                    }
                    LabeledContent("Relays' secret") {
                        HStack {
                            Text(s.relaySecretSet ? "set" : "not set").foregroundStyle(.secondary)
                                .accessibilityIdentifier("relaySecretStatus")
                            SecureField("New secret", text: $relaySecret)
                                .frame(maxWidth: 180)
                                .accessibilityLabel("New relay secret")
                                .accessibilityIdentifier("relaySecret")
                            Button("Set") {
                                apply(["relay-secret": .string(relaySecret)])
                                relaySecret = ""
                            }
                            .disabled(relaySecret.isEmpty)
                            .accessibilityIdentifier("setRelaySecret")
                            if s.relaySecretSet {
                                Button("Clear") { apply(["relay-secret": .null]) }
                                    .accessibilityIdentifier("clearRelaySecret")
                            }
                        }
                    }
                }
                Section("Recording and the audit log") {
                    LabeledContent("Record sessions to") {
                        HStack {
                            TextField("Record sessions to", text: $recordText, prompt: Text("off (a folder to record to)"))
                                .labelsHidden()
                                .frame(maxWidth: 260)
                                .accessibilityLabel("Record sessions to")
                                .accessibilityIdentifier("record")
                            Button("Apply") {
                                let t = recordText.trimmingCharacters(in: .whitespaces)
                                apply(["record": t.isEmpty ? .null : .string(t)])
                            }
                            .disabled(recordText == (s.record ?? ""))
                            .accessibilityIdentifier("applyRecord")
                        }
                    }
                    LabeledContent("Audit log") {
                        HStack {
                            TextField("Audit log", text: $auditText, prompt: Text("a file, or off"))
                                .labelsHidden()
                                .frame(maxWidth: 260)
                                .accessibilityLabel("Audit log file")
                                .accessibilityIdentifier("auditPath")
                            Button("Apply") {
                                let t = auditText.trimmingCharacters(in: .whitespaces)
                                apply(["audit": t.isEmpty ? .null : .string(t)])
                            }
                            .disabled(auditText == (s.audit ?? ""))
                            .accessibilityIdentifier("applyAudit")
                        }
                    }
                }
                Section("Start at login") {
                    Toggle("Open Rein Remote at login", isOn: Binding(get: { loginItem }, set: setLoginItem))
                        .accessibilityIdentifier("loginItem")
                    if let loginError { ErrorText(message: loginError, identifier: "loginItemError") }
                    if let sl = serverLogin, sl.enabled {
                        HStack {
                            Text("rein-remote host is also installed to start at login on its own (\(sl.kind ?? "service")).")
                                .font(.caption).foregroundStyle(.secondary)
                            Button("Remove it") {
                                Task {
                                    if case .success(let v) = await model.call({ try await $0.setStartAtLogin(false) }) { serverLogin = v }
                                }
                            }
                            .accessibilityIdentifier("removeServerLogin")
                        }
                    }
                }
                Section {
                    LabeledContent("Settings file") {
                        Text(all.path).font(.caption.monospaced()).textSelection(.enabled)
                            .accessibilityIdentifier("settingsPath")
                    }
                    if !all.overridden.isEmpty {
                        Text("Set on the host's command line for this run: \(all.overridden.joined(separator: ", ")). A change here wins over it.")
                            .font(.caption).foregroundStyle(.secondary)
                            .accessibilityIdentifier("overridden")
                    }
                }
            }
            .formStyle(.grouped)
            .onAppear { sync(s) }
            .onChange(of: s) { _, n in sync(n) }
            .task {
                if case .success(let v) = await model.call(showError: false, { try await $0.startAtLogin() }) { serverLogin = v }
            }
        } else {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    /// A control's binding: reads the setting, writes it through the API.
    private func bind<V>(_ value: V, key: String, _ json: @escaping (V) -> JSONValue) -> Binding<V> {
        Binding(get: { value }, set: { apply([key: json($0)]) })
    }

    private func autoOrNumber(_ title: String, value: AutoOrNumber, key: String, text: Binding<String>, unit: String, range: String, id: String) -> some View {
        LabeledContent(title) {
            HStack {
                Toggle("Automatic", isOn: Binding(
                    get: { value == .auto },
                    set: { on in
                        if on { apply([key: .string("auto")]) } else {
                            let n = Int(text.wrappedValue) ?? (key == "fps" ? 60 : 20000)
                            apply([key: .number(Double(n))])
                        }
                    }))
                    .accessibilityIdentifier("\(id)Auto")
                if case .value = value {
                    TextField(range, text: text)
                        .labelsHidden()
                        .frame(width: 90)
                        .onSubmit { applyNumber(key, text.wrappedValue) }
                        .accessibilityLabel("\(title) in \(unit)")
                        .accessibilityIdentifier("\(id)Value")
                    Text(unit).foregroundStyle(.secondary)
                    Button("Apply") { applyNumber(key, text.wrappedValue) }
                        .accessibilityIdentifier("\(id)Apply")
                }
            }
        }
    }

    private func applyNumber(_ key: String, _ text: String) {
        let t = text.trimmingCharacters(in: .whitespaces)
        // The server checks the range and says what's wrong.
        apply([key: Int(t).map { .number(Double($0)) } ?? .string(t)])
    }

    private func applyListen() {
        let t = listenText.trimmingCharacters(in: .whitespaces)
        apply(["listen": t.isEmpty ? .null : .string(t)])
    }

    private func addRelay(_ current: [String]) {
        let t = newRelay.trimmingCharacters(in: .whitespaces)
        guard !t.isEmpty else { return }
        apply(["relay": .array((current + [t]).map { .string($0) })])
        newRelay = ""
    }

    private func apply(_ changes: [String: JSONValue]) {
        Task {
            switch await model.putSettings(changes) {
            case .success(let s):
                error = nil
                message = s.appliedText
            case .failure(let e):
                message = nil
                error = e.localizedDescription
            }
        }
    }

    private func sync(_ s: SettingsValues) {
        if case .value(let n) = s.fps { fpsText = String(n) }
        if case .value(let n) = s.bitrate { bitrateText = String(n) }
        listenText = s.listen
        recordText = s.record ?? ""
        auditText = s.audit ?? ""
    }

    private func setLoginItem(_ on: Bool) {
        do {
            if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
            loginError = nil
        } catch {
            loginError = "macOS didn't change the login item: \(error.localizedDescription)"
        }
        loginItem = SMAppService.mainApp.status == .enabled
    }
}
