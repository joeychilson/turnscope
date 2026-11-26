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
    var titleAccessibilityHidden = false
    @ViewBuilder var control: () -> Control

    var body: some View {
        HStack(spacing: Row.gap) {
            Text(title)
                .font(.name)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityHidden(titleAccessibilityHidden)
            control()
        }
        .padding(Row.padding)
        .frame(minHeight: Row.setting)
        .optionalHelp(help)
    }
}

extension SettingRow where Control == Switch {
    init(_ title: String, help: String? = nil, isOn: Binding<Bool>) {
        self.init(title: title, help: help, titleAccessibilityHidden: true) { Switch(label: title, on: isOn) }
    }
}

/// What a page says in place of what it can't show.
struct Message: View {
    var headline: String
    var detail: String
    var footnote: String?

    var body: some View {
        VStack(spacing: 8) {
            Text(headline).font(.system(size: 13, weight: .semibold))
            Text(detail)
                .font(.line)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
            if let footnote {
                Text(footnote)
                    .font(.small)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, minHeight: 140)
    }
}
