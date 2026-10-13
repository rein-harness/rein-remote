import SwiftUI

struct ViewersView: View {
    @Bindable var model: HostModel
    @State private var toKick: Viewer?

    var body: some View {
        Group {
            if model.viewers.isEmpty {
                VStack(spacing: 8) {
                    Image(systemName: "person.2.slash").font(.largeTitle).foregroundStyle(.secondary)
                        .accessibilityHidden(true)
                    Text("No one is connected.").font(.title3)
                    Text(model.state?.sharing == true ? "Viewers show here as they connect." : "Viewers connect while sharing.")
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("noViewers")
            } else {
                List {
                    ForEach(model.viewers) { v in row(v) }
                }
                .accessibilityIdentifier("viewerList")
            }
        }
        .alert(
            "End \(toKick.map(name) ?? "this viewer")'s session?",
            isPresented: Binding(get: { toKick != nil }, set: { if !$0 { toKick = nil } }),
            presenting: toKick
        ) { v in
            Button("Kick", role: .destructive) { Task { await model.kick(v.id) } }
                .accessibilityIdentifier("confirmKick")
            Button("Cancel", role: .cancel) {}
                .accessibilityIdentifier("cancelKick")
        } message: { _ in
            Text("They're told the host ended it. They can connect again with what they have: to keep a device out, unpair it.")
        }
    }

    private func name(_ v: Viewer) -> String { v.device ?? v.user ?? v.id }

    private func row(_ v: Viewer) -> some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: v.via == "paired" ? "iphone" : "person.crop.circle")
                .font(.title2).foregroundStyle(.secondary).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(name(v)).font(.headline)
                    if v.viewOnly == true {
                        Text("view only").font(.caption).padding(.horizontal, 5)
                            .background(.quaternary, in: Capsule())
                    }
                }
                Text([v.user, v.via.map { "via \($0)" }, v.transport, v.addr].compactMap { $0 }.joined(separator: " · "))
                    .font(.callout).foregroundStyle(.secondary)
                Text(stats(v)).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
            }
            Spacer()
            Button("Kick…") { toKick = v }
                .accessibilityLabel("Kick \(name(v))")
                .accessibilityIdentifier("kick.\(v.id)")
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Viewer \(name(v))")
        .accessibilityIdentifier("viewer.\(v.id)")
    }

    private func stats(_ v: Viewer) -> String {
        var parts: [String] = []
        if let fps = v.fps { parts.append("\(Int(fps)) fps") }
        if let w = v.width, let h = v.height { parts.append("\(w)×\(h)") }
        if let k = v.kbps { parts.append(k >= 1000 ? String(format: "%.1f Mbit/s", k / 1000) : "\(Int(k)) kbit/s") }
        if let r = v.rttMs { parts.append(String(format: "%.1f ms round trip", r)) }
        if let s = v.since { parts.append("since \(s.formatted(date: .omitted, time: .shortened))") }
        return parts.joined(separator: " · ")
    }
}

struct DevicesView: View {
    @Bindable var model: HostModel
    @State private var toUnpair: Device?

    var body: some View {
        Group {
            if model.devices.isEmpty {
                VStack(spacing: 8) {
                    Image(systemName: "iphone.slash").font(.largeTitle).foregroundStyle(.secondary).accessibilityHidden(true)
                    Text("No paired devices.").font(.title3)
                    Text("Pair a phone from the Sharing page with its code.").foregroundStyle(.secondary)
                    Button("Go to Sharing") { model.page = .sharing }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityIdentifier("noDevices")
            } else {
                List(model.devices) { d in
                    HStack {
                        Image(systemName: "iphone").font(.title2).foregroundStyle(.secondary).accessibilityHidden(true)
                        VStack(alignment: .leading) {
                            Text(d.name).font(.headline)
                            Text(d.id).font(.caption.monospaced()).foregroundStyle(.secondary)
                            if let s = d.since { Text("Paired \(s.shortTime)").font(.caption).foregroundStyle(.secondary) }
                        }
                        Spacer()
                        Button("Unpair…") { toUnpair = d }
                            .accessibilityLabel("Unpair \(d.name)")
                            .accessibilityIdentifier("unpair.\(d.id)")
                    }
                    .padding(.vertical, 4)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("device.\(d.id)")
                }
                .accessibilityIdentifier("deviceList")
            }
        }
        .alert(
            "Unpair \(toUnpair?.name ?? "this device")?",
            isPresented: Binding(get: { toUnpair != nil }, set: { if !$0 { toUnpair = nil } }),
            presenting: toUnpair
        ) { d in
            Button("Unpair", role: .destructive) { Task { await model.unpair(d.id) } }
                .accessibilityIdentifier("confirmUnpair")
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("Its sessions end, and it needs a new code to connect again.")
        }
    }
}
