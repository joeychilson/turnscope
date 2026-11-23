// Shared Settings card, setting row, and empty-tab message.

import SwiftUI

struct SettingGroup<Content: View>: View {
    var title: String
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionTitle(text: title)
            VStack(alignment: .leading, spacing: 0) { content }
                .padding(.vertical, 3)
                .background(
                    RoundedRectangle(cornerRadius: Row.radius, style: .continuous)
                        .fill(Color.primary.opacity(0.05))
                )
        }
    }
}

struct SettingRow<Control: View>: View {
    var title: String
    var help: String?
    @ViewBuilder var control: () -> Control

    var body: some View {
        HStack(spacing: Row.gap) {
            Text(title).font(.name)
            Spacer(minLength: 8)
            control()
        }
        .padding(Row.padding)
        .frame(minHeight: Row.setting)
        .accessibilityElement(children: .combine)
        .help(help ?? "")
    }
}

extension SettingRow where Control == Switch {
    init(_ title: String, help: String? = nil, isOn: Binding<Bool>) {
        self.init(title: title, help: help) { Switch(label: title, on: isOn) }
    }
}

struct EmptyTabMessage: View {
    var message: EmptyMessage

    var body: some View {
        VStack(spacing: 8) {
            Text(message.headline).font(.system(size: 13, weight: .semibold))
            Text(message.detail)
                .font(.line)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        }
        .padding(24)
        .frame(maxWidth: .infinity, minHeight: 140)
    }
}
