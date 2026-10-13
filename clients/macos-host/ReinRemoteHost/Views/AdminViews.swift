import SwiftUI

// MARK: Sign-in

struct SignInView: View {
    @Bindable var model: HostModel
    @State private var loaded: SignIn?
    @State private var kind = "off"
    @State private var fields: [String: String] = [:]
    @State private var secret = ""
    @State private var error: String?
    @State private var message: String?

    private static let oidcKeys = ["oidc-issuer", "oidc-client-id", "oidc-scopes", "oidc-user-claim", "oidc-groups-claim"]
    private static let samlKeys = ["saml-idp-metadata", "saml-entity-id", "saml-user-attr", "saml-groups-attr"]

    var body: some View {
        Form {
            if let error { Section { ErrorText(message: error, identifier: "signinError") } }
            if let message { Section { InfoText(message: message, identifier: "signinApplied") } }
            Section {
                Picker("Viewers sign in with", selection: $kind) {
                    Text("Off (links and paired devices only)").tag("off")
                    Text("OpenID Connect").tag("oidc")
                    Text("SAML").tag("saml")
                }
                .accessibilityIdentifier("signinKind")
                if kind == "saml", loaded?.samlAvailable == false {
                    Text("This build of rein-remote has no SAML (it's built with --features saml).")
                        .font(.caption).foregroundStyle(.orange)
                }
            }
            if kind != "off" {
                Section("This server") {
                    field("Public URL", "public-url", prompt: "https://remote.example.com")
                    Text("How browsers reach this Mac; the provider sends them back to it.").font(.caption).foregroundStyle(.secondary)
                }
            }
            if kind == "oidc" {
                Section("OpenID Connect") {
                    field("Issuer", "oidc-issuer", prompt: "https://example.okta.com")
                    field("Client ID", "oidc-client-id", prompt: "")
                    LabeledContent("Client secret") {
                        HStack {
                            Text(loaded?.oidcClientSecretSet == true ? "set" : "not set").foregroundStyle(.secondary)
                                .accessibilityIdentifier("oidcSecretStatus")
                            SecureField("", text: $secret, prompt: Text("new secret (write-only)"))
                                .frame(maxWidth: 220)
                                .accessibilityLabel("New client secret")
                                .accessibilityIdentifier("oidc-client-secret")
                        }
                    }
                    field("Scopes", "oidc-scopes", prompt: "openid email profile")
                    field("User claim", "oidc-user-claim", prompt: "email")
                    field("Groups claim", "oidc-groups-claim", prompt: "groups")
                }
            }
            if kind == "saml" {
                Section("SAML") {
                    field("IdP metadata (file or URL)", "saml-idp-metadata", prompt: "https://idp.example/metadata")
                    field("Entity ID", "saml-entity-id", prompt: "the public URL")
                    field("User attribute", "saml-user-attr", prompt: "NameID")
                    field("Groups attribute", "saml-groups-attr", prompt: "groups")
                }
            }
            Section {
                HStack {
                    Spacer()
                    Button("Revert") { if let loaded { fill(loaded) } }
                        .accessibilityIdentifier("signinRevert")
                    Button("Save") { Task { await save() } }
                        .keyboardShortcut("s")
                        .buttonStyle(.borderedProminent)
                        .accessibilityIdentifier("signinSave")
                }
            }
        }
        .formStyle(.grouped)
        .task { await load() }
    }

    private func field(_ title: String, _ key: String, prompt: String) -> some View {
        TextField(title, text: Binding(get: { fields[key] ?? "" }, set: { fields[key] = $0 }), prompt: Text(prompt))
            .accessibilityLabel(title)
            .accessibilityIdentifier(key)
    }

    private func load() async {
        switch await model.call(showError: false, { try await $0.signIn() }) {
        case .success(let s): loaded = s; fill(s)
        case .failure(let e): error = e.localizedDescription
        }
    }

    private func fill(_ s: SignIn) {
        kind = s.kind ?? "off"
        fields = [
            "public-url": s.publicURL ?? "", "oidc-issuer": s.oidcIssuer ?? "", "oidc-client-id": s.oidcClientID ?? "",
            "oidc-scopes": s.oidcScopes ?? "", "oidc-user-claim": s.oidcUserClaim ?? "", "oidc-groups-claim": s.oidcGroupsClaim ?? "",
            "saml-idp-metadata": s.samlIdpMetadata ?? "", "saml-entity-id": s.samlEntityID ?? "",
            "saml-user-attr": s.samlUserAttr ?? "", "saml-groups-attr": s.samlGroupsAttr ?? "",
        ]
        secret = ""
    }

    /// What to send for the chosen kind (blank fields as null: their defaults).
    private func changes() -> [String: JSONValue] {
        func v(_ k: String) -> JSONValue {
            let t = (fields[k] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            return t.isEmpty ? .null : .string(t)
        }
        switch kind {
        case "oidc":
            var c: [String: JSONValue] = ["public-url": v("public-url"), "saml-idp-metadata": .null]
            for k in Self.oidcKeys { c[k] = v(k) }
            if !secret.isEmpty { c["oidc-client-secret"] = .string(secret) }
            return c
        case "saml":
            var c: [String: JSONValue] = ["public-url": v("public-url"), "oidc-issuer": .null]
            for k in Self.samlKeys { c[k] = v(k) }
            return c
        default:
            return ["oidc-issuer": .null, "saml-idp-metadata": .null]
        }
    }

    private func save() async {
        let c = changes()
        switch await model.call(showError: false, { try await $0.putSignIn(c) }) {
        case .success(let s):
            loaded = s
            fill(s)
            error = nil
            message = appliedText(s.applied) ?? "Saved."
        case .failure(let e):
            message = nil
            error = e.localizedDescription
        }
    }
}

// MARK: Policy

struct PolicyView: View {
    @Bindable var model: HostModel
    @State private var loaded: Policy?
    @State private var text = ""
    @State private var format = "toml"
    @State private var error: String?
    @State private var message: String?

    private static let example = """
        # Rules are matched in order; the first that matches decides.
        [[rule]]
        name = "admins"
        groups = ["rr-admins"]

        [[rule]]
        name = "everyone else, view only"
        view_only = true

        """

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                if let p = loaded {
                    Text(p.enabled ? "On: \(p.rules) rule\(p.rules == 1 ? "" : "s")" : "Off: everyone who gets in may watch and control.")
                        .accessibilityIdentifier("policyStatus")
                    if let path = p.path { Text(path).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle) }
                }
                Spacer()
                Picker("Format", selection: $format) {
                    Text("TOML").tag("toml")
                    Text("JSON").tag("json")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 140)
                .accessibilityLabel("Policy format")
                .accessibilityIdentifier("policyFormat")
            }
            Text("Rules by user, group or how a viewer came in (oidc, saml, token, paired, relay): view_only, clipboard, audio, sources, record, max_session_minutes. Checked by the server before it's used.")
                .font(.caption).foregroundStyle(.secondary)
                .lineLimit(3)
            TextEditor(text: $text)
                .font(.system(.body, design: .monospaced))
                .autocorrectionDisabled()
                .scrollContentBackground(.hidden)
                .padding(6)
                .background(Color(nsColor: .textBackgroundColor), in: RoundedRectangle(cornerRadius: 6))
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(.separator))
                .frame(minHeight: 160, idealHeight: 300, maxHeight: .infinity)
                .accessibilityLabel("Policy text")
                .accessibilityIdentifier("policyText")
            if let error { ErrorText(message: error, identifier: "policyError") }
            if let message { InfoText(message: message, identifier: "policyApplied") }
            HStack {
                if loaded?.enabled != true, text.isEmpty {
                    Button("Start from an example") { text = Self.example }
                        .accessibilityIdentifier("policyExample")
                }
                Spacer()
                if loaded?.enabled == true {
                    Button("Turn off") { Task { await save(nil) } }
                        .accessibilityIdentifier("policyOff")
                }
                Button("Revert") { if let loaded { fill(loaded) } }
                    .accessibilityIdentifier("policyRevert")
                Button("Check and save") { Task { await save(text) } }
                    .keyboardShortcut("s")
                    .buttonStyle(.borderedProminent)
                    .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityIdentifier("policySave")
            }
        }
        .padding(20)
        .task { await load() }
    }

    private func load() async {
        switch await model.call(showError: false, { try await $0.policy() }) {
        case .success(let p): loaded = p; fill(p)
        case .failure(let e): error = e.localizedDescription
        }
    }

    private func fill(_ p: Policy) {
        text = p.text ?? ""
        format = p.format ?? "toml"
    }

    private func save(_ t: String?) async {
        let f = format
        switch await model.call(showError: false, { try await $0.putPolicy(text: t, format: f) }) {
        case .success(let p):
            loaded = p
            fill(p)
            error = nil
            message = appliedText(p.applied) ?? "Saved."
        case .failure(let e):
            message = nil
            error = e.localizedDescription
        }
    }
}

// MARK: Live sessions

struct SessionsView: View {
    @Bindable var model: HostModel
    @State private var sessions: [Session] = []
    @State private var error: String?
    @State private var toKick: Session?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let error { ErrorText(message: error, identifier: "sessionsError").padding() }
            Table(sessions) {
                TableColumn("Who") { s in
                    VStack(alignment: .leading, spacing: 1) {
                        Text(s.user ?? "–").accessibilityIdentifier("session.\(s.id)")
                        if let d = s.device { Text(d).font(.caption).foregroundStyle(.secondary) }
                    }
                }
                .width(min: 120, ideal: 170)
                TableColumn("Via") { s in Text([s.via, s.transport].compactMap { $0 }.joined(separator: ", ")) }
                    .width(min: 70, ideal: 120)
                TableColumn("Rule") { s in
                    VStack(alignment: .leading, spacing: 1) {
                        Text(s.rule ?? "–")
                        if let g = s.groups, !g.isEmpty { Text(g.joined(separator: ", ")).font(.caption).foregroundStyle(.secondary) }
                    }
                }
                .width(min: 60, ideal: 100)
                TableColumn("Allowed") { s in Text(allowed(s)).help(allowed(s)) }
                    .width(min: 80, ideal: 150)
                TableColumn("Since") { s in
                    VStack(alignment: .leading, spacing: 1) {
                        Text(s.since?.formatted(date: .omitted, time: .shortened) ?? "–")
                        if let e = s.endsAt { Text("ends \(e.formatted(date: .omitted, time: .shortened))").font(.caption).foregroundStyle(.secondary) }
                    }
                }
                .width(min: 60, ideal: 80)
                TableColumn("") { s in
                    Button("Kick…") { toKick = s }
                        .accessibilityLabel("Kick \(s.user ?? s.id)")
                        .accessibilityIdentifier("sessionKick.\(s.id)")
                }
                .width(64)
            }
            .accessibilityIdentifier("sessionsTable")
            .overlay {
                if sessions.isEmpty {
                    Text("No live sessions.").foregroundStyle(.secondary).accessibilityIdentifier("noSessions")
                }
            }
        }
        // The viewers event says when someone joins or leaves: read the sessions again then.
        .task(id: model.viewers.map(\.id)) { await load() }
        .alert(
            "End this session?",
            isPresented: Binding(get: { toKick != nil }, set: { if !$0 { toKick = nil } }),
            presenting: toKick
        ) { s in
            Button("Kick", role: .destructive) { Task { await model.kick(s.id); await load() } }
            Button("Cancel", role: .cancel) {}
        } message: { s in
            Text("\(s.user ?? s.id) is told the host ended it.")
        }
    }

    private func allowed(_ s: Session) -> String {
        var a: [String] = [s.viewOnly == true ? "view only" : "control"]
        if s.clipboard == true { a.append("clipboard") }
        if s.audio == true { a.append("sound") }
        if s.recorded == true { a.append("recorded") }
        return a.joined(separator: ", ")
    }

    private func load() async {
        switch await model.call(showError: false, { try await $0.sessions() }) {
        case .success(let s): sessions = s.sessions; error = nil
        case .failure(let e): error = e.localizedDescription
        }
    }
}

// MARK: Audit log

struct AuditView: View {
    @Bindable var model: HostModel
    @State private var query = AuditQuery()
    @State private var page: AuditPage?
    /// The `before` of each page seen, to go back to newer ones.
    @State private var stack: [String?] = []
    @State private var error: String?

    struct Row: Identifiable {
        let id: Int
        let entry: [String: JSONValue]
        var ts: String { entry["ts"]?.string ?? "" }
        var event: String { entry["event"]?.string ?? "" }
        var user: String { entry["user"]?.display ?? "" }
        var details: String {
            entry.keys.filter { !["ts", "event", "user"].contains($0) }.sorted()
                .map { "\($0): \(entry[$0]!.display)" }.joined(separator: "  ")
        }
    }

    private var rows: [Row] { (page?.entries ?? []).enumerated().map { Row(id: $0.offset, entry: $0.element) } }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            filters.padding([.horizontal, .top], 16).padding(.bottom, 8)
            if let error { ErrorText(message: error, identifier: "auditError").padding(.horizontal) }
            Table(rows) {
                TableColumn("Time") { r in
                    Text(ControlJSON.date(r.ts)?.formatted(date: .numeric, time: .standard) ?? r.ts).monospacedDigit()
                }
                .width(min: 140, ideal: 160)
                TableColumn("Event") { r in Text(r.event).accessibilityIdentifier("auditEvent.\(r.id)") }
                    .width(min: 100, ideal: 130)
                TableColumn("User") { r in Text(r.user) }
                    .width(min: 80, ideal: 160)
                TableColumn("Details") { r in Text(r.details).font(.caption.monospaced()).lineLimit(2).help(r.details) }
            }
            .accessibilityIdentifier("auditTable")
            .overlay {
                if page != nil, rows.isEmpty {
                    Text(page?.path == nil ? "The audit log is off (Settings)." : "No entries match.")
                        .foregroundStyle(.secondary).accessibilityIdentifier("auditEmpty")
                }
            }
            HStack {
                Text(page?.path ?? "").font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                Spacer()
                Text("Page \(stack.count + 1)").foregroundStyle(.secondary).accessibilityIdentifier("auditPage")
                Button("Newer") {
                    query.before = stack.popLast() ?? nil
                    Task { await load() }
                }
                .disabled(stack.isEmpty)
                .accessibilityIdentifier("auditNewer")
                Button("Older") {
                    stack.append(query.before)
                    query.before = page?.next
                    Task { await load() }
                }
                .disabled(page?.next == nil)
                .accessibilityIdentifier("auditOlder")
            }
            .padding(12)
        }
        .task { await load() }
    }

    private var filters: some View {
        HStack(spacing: 10) {
            Menu {
                Button("All events") { query.events = []; reload() }
                Divider()
                ForEach(AuditQuery.eventTypes, id: \.self) { e in
                    Toggle(e, isOn: Binding(
                        get: { query.events.contains(e) },
                        set: { on in
                            if on { query.events.append(e) } else { query.events.removeAll { $0 == e } }
                            reload()
                        }))
                }
            } label: {
                Text(query.events.isEmpty ? "All events" : query.events.joined(separator: ", ")).lineLimit(1)
            }
            .frame(maxWidth: 260)
            .accessibilityLabel("Event types")
            .accessibilityIdentifier("auditEvents")
            TextField("User", text: $query.user, prompt: Text("User contains…"))
                .frame(maxWidth: 160)
                .onSubmit(reload)
                .accessibilityIdentifier("auditUser")
            TextField("Since", text: $query.since, prompt: Text("Since (2026-10-12)"))
                .frame(maxWidth: 150)
                .onSubmit(reload)
                .accessibilityIdentifier("auditSince")
            Picker("Per page", selection: Binding(get: { query.limit }, set: { query.limit = $0; reload() })) {
                ForEach([25, 50, 100, 500], id: \.self) { Text("\($0)").tag($0) }
            }
            .frame(maxWidth: 130)
            .accessibilityIdentifier("auditLimit")
            Button("Search", action: reload)
                .accessibilityIdentifier("auditSearch")
        }
    }

    /// From the newest, with the filters as they are.
    private func reload() {
        query.before = nil
        stack = []
        Task { await load() }
    }

    private func load() async {
        let q = query
        switch await model.call(showError: false, { try await $0.audit(q) }) {
        case .success(let p): page = p; error = nil
        case .failure(let e): error = e.localizedDescription
        }
    }
}

// MARK: Metrics

struct MetricsView: View {
    @Bindable var model: HostModel
    @State private var m: Metrics?
    @State private var error: String?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if let error { ErrorText(message: error, identifier: "metricsError") }
                if let m {
                    Grid(alignment: .leading, horizontalSpacing: 24, verticalSpacing: 10) {
                        metric("Version", m.version)
                        metric("Up for", uptime(m.uptimeS))
                        metric("Viewers now", "\(m.viewers.direct) direct, \(m.viewers.relayed) relayed")
                        metric("Sessions", "\(m.sessionsTotal)")
                        metric("Frames sent", m.framesSentTotal.formatted())
                        metric("Keyframes sent", m.keyframesSentTotal.formatted())
                        metric("Video sent", ByteCountFormatter.string(fromByteCount: Int64(m.videoBytesSentTotal), countStyle: .binary))
                        metric("Frames skipped", m.framesSkippedTotal.formatted())
                        metric("Direct paths offered / answered", "\(m.directOffersTotal) / \(m.directAnswersTotal)")
                    }
                    .accessibilityIdentifier("metricsGrid")
                    if !m.relays.isEmpty {
                        SectionTitle(text: "Relays")
                        ForEach(m.relays) { r in
                            HStack {
                                Circle().fill(r.up ? .green : .red).frame(width: 8, height: 8).accessibilityHidden(true)
                                Text(r.relay).font(.body.monospaced())
                                Text(r.up ? "up" : "down")
                                if let rtt = r.rttMs { Text(String(format: "%.1f ms", rtt)).foregroundStyle(.secondary) }
                                if let n = r.registrations { Text("\(n) registrations").foregroundStyle(.secondary) }
                            }
                            .accessibilityElement(children: .combine)
                        }
                    }
                    Text("Counted since the host started (across stops and starts of sharing).")
                        .font(.caption).foregroundStyle(.secondary)
                } else if error == nil {
                    ProgressView()
                }
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task {
            while !Task.isCancelled {
                switch await model.call(showError: false, { try await $0.metrics() }) {
                case .success(let v): m = v; error = nil
                case .failure(let e): error = e.localizedDescription
                }
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    private func metric(_ k: String, _ v: String) -> some View {
        GridRow {
            Text(k).foregroundStyle(.secondary)
            Text(v).monospacedDigit().textSelection(.enabled)
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("metric.\(k)")
    }

    private func uptime(_ s: Double) -> String {
        let f = DateComponentsFormatter()
        f.allowedUnits = s >= 3600 ? [.hour, .minute] : [.minute, .second]
        f.unitsStyle = .abbreviated
        return f.string(from: s) ?? "\(Int(s)) s"
    }
}
