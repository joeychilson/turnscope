// The row every setting is drawn as, and the small switch most of them use.

import SwiftUI

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
            // The row is as tall as its name: a control taller than that, as
            // a pill is, reaches into the row's padding. That way a row with
            // a pill is as tall as one with a switch.
            control().frame(height: 0)
        }
        .padding(Row.padding)
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

/// A switch, small enough for the rows it sits in.
struct Switch: View {
    var label: String
    @Binding var on: Bool

    var body: some View {
        Toggle(label, isOn: $on).toggleStyle(.switch).controlSize(.mini).labelsHidden()
    }
}
