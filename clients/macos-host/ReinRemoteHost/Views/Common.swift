import AppKit
import CoreImage
import CoreImage.CIFilterBuiltins
import SwiftUI

/// A QR code made here with CoreImage (nothing leaves the Mac).
enum QRCode {
    static func image(for text: String, size: CGFloat = 220) -> NSImage? {
        let f = CIFilter.qrCodeGenerator()
        f.message = Data(text.utf8)
        f.correctionLevel = "M"
        guard let out = f.outputImage, out.extent.width > 0 else { return nil }
        let scale = max(1, (size / out.extent.width).rounded(.down))
        let scaled = out.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let ctx = CIContext(options: [.useSoftwareRenderer: false])
        guard let cg = ctx.createCGImage(scaled, from: scaled.extent) else { return nil }
        return NSImage(cgImage: cg, size: NSSize(width: scaled.extent.width, height: scaled.extent.height))
    }
}

struct QRCodeView: View {
    let text: String
    let label: String
    var size: CGFloat = 180

    var body: some View {
        Group {
            if let img = QRCode.image(for: text, size: size) {
                Image(nsImage: img)
                    .interpolation(.none)
                    .resizable()
                    .frame(width: size, height: size)
                    .padding(8)
                    .background(Color.white, in: RoundedRectangle(cornerRadius: 8))
            } else {
                Text("No QR code").foregroundStyle(.secondary)
            }
        }
        .accessibilityElement()
        .accessibilityLabel(label)
        .accessibilityAddTraits(.isImage)
    }
}

/// Copies text to the pasteboard, and says so for a moment.
struct CopyButton: View {
    let text: String
    let what: String
    var identifier: String
    @State private var copied = false

    var body: some View {
        Button {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
            copied = true
            Task { try? await Task.sleep(for: .seconds(1.5)); copied = false }
        } label: {
            Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
        }
        .accessibilityLabel(copied ? "Copied \(what)" : "Copy \(what)")
        .accessibilityIdentifier(identifier)
    }
}

/// The server's error, as it wrote it.
struct ErrorText: View {
    let message: String
    var identifier: String = "error"

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                .accessibilityHidden(true)
            Text(message)
                .font(message.contains("\n") ? .system(.body, design: .monospaced) : .body)
                .textSelection(.enabled)
                .lineLimit(nil)
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.red.opacity(0.1), in: RoundedRectangle(cornerRadius: 6))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Error: \(message)")
        .accessibilityIdentifier(identifier)
    }
}

/// A short note after a change ("Saved…").
struct InfoText: View {
    let message: String
    var identifier: String = "info"

    var body: some View {
        Label(message, systemImage: "checkmark.circle")
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(identifier)
    }
}

/// What a page shows when the host isn't there.
struct NotConnectedView: View {
    @Bindable var model: HostModel

    var body: some View {
        VStack(spacing: 12) {
            switch model.connection {
            case .starting, .connecting:
                ProgressView().accessibilityLabel("Starting the host")
                Text(model.statusText).font(.title3)
                    .accessibilityIdentifier("connectionStatus")
            case .notRunning(let why):
                Image(systemName: "bolt.horizontal.circle").font(.largeTitle).foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text("Not running").font(.title3)
                    .accessibilityIdentifier("connectionStatus")
                if let why { Text(why).foregroundStyle(.secondary).multilineTextAlignment(.center).textSelection(.enabled) }
                Button("Start the host") { model.startHost() }
                    .disabled(HostFiles.serverBinary == nil)
                    .accessibilityIdentifier("startHost")
            case .connected:
                ProgressView()
            }
        }
        .padding(40)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// A section's heading.
struct SectionTitle: View {
    let text: String
    var body: some View {
        Text(text).font(.headline).accessibilityAddTraits(.isHeader)
    }
}

extension Date {
    var shortTime: String { formatted(date: .abbreviated, time: .standard) }
}

/// "4:59" from seconds.
func countdown(_ s: Int) -> String {
    let s = max(0, s)
    return String(format: "%d:%02d", s / 60, s % 60)
}
