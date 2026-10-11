// What every Settings tab is made of: titled cards of rows, the row a setting
// is drawn as, and the switch most of them use.

import SwiftUI

/// A titled group of rows in a card. Where a row's name alone doesn't say
/// what its switch does, the title says it, as "Notify me when" does.
struct SettingGroup<Content: View>: View {
    var title: String
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionTitle(text: title)
            VStack(alignment: .leading, spacing: 0) { content }
                .padding(.vertical, 3)
                .background(RoundedRectangle(cornerRadius: Row.radius, style: .continuous)
                    .fill(Color.primary.opacity(0.05)))
        }
    }
}

/// A setting: its name and its control on one line. More about it, `help`,
/// shows on hover, and VoiceOver reads it too.
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
    /// A setting that is a switch, labeled with the row's title.
    init(_ title: String, help: String? = nil, isOn: Binding<Bool>) {
        self.init(title: title, help: help) { Switch(label: title, on: isOn) }
    }
}

/// An on and off switch, on in the system's accent. It's drawn here rather
/// than by AppKit, so it moves with the view it's in, and a snapshot draws
/// it as the screen does.
struct Switch: View {
    var label: String
    @Binding var on: Bool

    var body: some View {
        Button { on.toggle() } label: {
            Capsule()
                .fill(on ? Color.accentColor : Color.primary.opacity(0.16))
                .frame(width: 30, height: 18)
                .overlay(alignment: on ? .trailing : .leading) {
                    Circle().fill(.white)
                        .shadow(color: .black.opacity(0.18), radius: 1, y: 0.5)
                        .padding(2)
                }
                .animation(spring, value: on)
        }
        .buttonStyle(.plain)
        .accessibilityRepresentation { Toggle(label, isOn: $on) }
    }
}
