import SwiftUI

struct MainView: View {
    @Bindable var model: HostModel

    var body: some View {
        NavigationSplitView {
            List(selection: $model.page) {
                Section {
                    ForEach(Page.main) { row($0) }
                }
                Section("Admin") {
                    ForEach(Page.admin) { row($0) }
                }
            }
            .navigationSplitViewColumnWidth(min: 180, ideal: 200)
            .accessibilityIdentifier("sidebar")
            .safeAreaInset(edge: .bottom) { status.padding(10) }
        } detail: {
            VStack(spacing: 0) {
                if let e = model.lastError {
                    HStack {
                        ErrorText(message: e, identifier: "lastError")
                        Button {
                            model.lastError = nil
                        } label: { Image(systemName: "xmark") }
                            .buttonStyle(.borderless)
                            .accessibilityLabel("Dismiss the error")
                            .accessibilityIdentifier("dismissError")
                    }
                    .padding([.horizontal, .top])
                }
                if model.isConnected {
                    page.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                } else if model.page == .permissions {
                    PermissionsView(model: model)
                } else {
                    NotConnectedView(model: model)
                }
            }
            .navigationTitle(model.page.title)
        }
    }

    private func row(_ p: Page) -> some View {
        Label(p.title, systemImage: p.symbol)
            .badge(p == .viewers ? model.viewers.count : 0)
            .tag(p)
            .accessibilityIdentifier("nav.\(p.rawValue)")
    }

    private var status: some View {
        HStack(spacing: 6) {
            Circle().fill(dotColor).frame(width: 8, height: 8).accessibilityHidden(true)
            Text(model.statusText).font(.caption).foregroundStyle(.secondary)
            Spacer()
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Status: \(model.statusText)")
        .accessibilityIdentifier("sidebarStatus")
    }

    private var dotColor: Color {
        guard model.isConnected, let s = model.state else { return .gray }
        return s.sharing ? .green : (s.error == nil ? .secondary : .orange)
    }

    @ViewBuilder private var page: some View {
        switch model.page {
        case .sharing: SharingView(model: model)
        case .viewers: ViewersView(model: model)
        case .devices: DevicesView(model: model)
        case .settings: SettingsView(model: model)
        case .permissions: PermissionsView(model: model)
        case .signin: SignInView(model: model)
        case .policy: PolicyView(model: model)
        case .sessions: SessionsView(model: model)
        case .audit: AuditView(model: model)
        case .metrics: MetricsView(model: model)
        }
    }
}
