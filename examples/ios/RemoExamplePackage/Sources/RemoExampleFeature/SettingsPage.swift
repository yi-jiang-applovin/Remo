import SwiftUI
import RemoSwift
#if canImport(AppKit)
import AppKit
#elseif canImport(UIKit)
import UIKit
#endif

struct SettingsPage: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        NavigationStack {
            Form {
                Section("Profile") {
                    @Bindable var s = store
                    TextField("Username", text: $s.username)
                }

                Section("Appearance") {
                    let colors = ["blue", "purple", "red", "green", "orange", "pink", "teal", "mint"]
                    LazyVGrid(columns: Array(repeating: GridItem(.flexible()), count: 4), spacing: 12) {
                        ForEach(colors, id: \.self) { name in
                            ColorDot(
                                name: name,
                                isSelected: store.accentColorName == name
                            ) {
                                withAnimation { store.accentColorName = name }
                            }
                        }
                    }
                    .padding(.vertical, 4)
                }

                Section("Remo") {
                    LabeledContent("Status") {
                        HStack(spacing: 6) {
                            Circle()
                                .fill(Remo.port > 0 ? .green : .red)
                                .frame(width: 8, height: 8)
                            Text(Remo.port > 0 ? "Running" : "Stopped")
                        }
                    }
                    LabeledContent("Port", value: "\(Remo.port)")
                    LabeledContent("Capabilities") {
                        Text("\(Remo.listCapabilities().count)")
                    }
                }

                Section("Network") {
                    LabeledContent("Last request", value: store.networkStatus)
                    Button("Fetch Demo Data") {
                        Task { await store.performDemoFetch() }
                    }
                }

                Section("Try It") {
                    CopyableCommand(
                        label: "Toast",
                        command: "remo call -a 127.0.0.1:\(Remo.port) ui.toast '{\"message\": \"Hello!\"}'"
                    )
                    CopyableCommand(
                        label: "Confetti",
                        command: "remo call -a 127.0.0.1:\(Remo.port) ui.confetti '{}'"
                    )
                    CopyableCommand(
                        label: "Recolor",
                        command: "remo call -a 127.0.0.1:\(Remo.port) ui.setAccentColor '{\"color\": \"purple\"}'"
                    )
                }
            }
            .navigationTitle("Settings")
        }
    }
}

// MARK: - Supporting Views

struct ColorDot: View {
    let name: String
    let isSelected: Bool
    let action: () -> Void

    private var color: Color {
        switch name {
        case "red": .red
        case "green": .green
        case "orange": .orange
        case "purple": .purple
        case "pink": .pink
        case "yellow": .yellow
        case "mint": .mint
        case "teal": .teal
        default: .blue
        }
    }

    var body: some View {
        Button(action: action) {
            Circle()
                .fill(color.gradient)
                .frame(width: 36, height: 36)
                .overlay {
                    if isSelected {
                        Image(systemName: "checkmark")
                            .font(.caption.bold())
                            .foregroundStyle(.white)
                    }
                }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(name)
    }
}

struct CopyableCommand: View {
    let label: String
    let command: String
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label)
                .font(.subheadline.weight(.medium))
            Text(command)
                .font(.caption2.monospaced())
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .onTapGesture {
            copyToPasteboard(command)
            withAnimation { copied = true }
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                withAnimation { copied = false }
            }
        }
        .overlay(alignment: .trailing) {
            if copied {
                Text("Copied!")
                    .font(.caption2.bold())
                    .foregroundStyle(.green)
                    .transition(.opacity.combined(with: .scale))
            }
        }
    }
}

private func copyToPasteboard(_ value: String) {
    #if canImport(AppKit)
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(value, forType: .string)
    #elseif canImport(UIKit)
    UIPasteboard.general.string = value
    #endif
}
