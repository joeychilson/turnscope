// Account and agent logos: flat SVGs drawn in the color of the text next to
// them, like icons. They're copied from `macos/Logos` into
// Contents/Resources/Logos, as `providers/<provider id>.svg` and
// `agents/<agent id>.svg`, so a new one needs no code.

import AppKit
import SwiftUI
import TurnscopeKit

/// A logo, drawn in the text's color.
struct Logo: View {
    var image: NSImage
    var size: CGFloat = 16

    init(account: AccountStatus, size: CGFloat = 16) {
        image = Logo.image("providers/\(account.provider)", else: "key.fill")
        self.size = size
    }

    init(agent: AgentStatus, size: CGFloat = 16) {
        image = Logo.image("agents/\(agent.id)", else: "terminal.fill")
        self.size = size
    }

    @MainActor private static var cache: [String: NSImage] = [:]

    /// The logo at `path` within the logos, such as "providers/anthropic",
    /// or the symbol `generic` if there's no such logo.
    @MainActor private static func image(_ path: String, else generic: String) -> NSImage {
        if let image = cache[path] { return image }
        let image = Bundle.main.resourceURL
            .flatMap { NSImage(contentsOf: $0.appending(path: "Logos/\(path).svg")) }
            ?? NSImage(systemSymbolName: generic, accessibilityDescription: nil)
            ?? NSImage()
        image.isTemplate = true
        cache[path] = image
        return image
    }

    var body: some View {
        Image(nsImage: image)
            .renderingMode(.template)
            .resizable()
            .aspectRatio(contentMode: .fit)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}
