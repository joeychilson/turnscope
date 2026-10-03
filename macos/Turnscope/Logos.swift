// The logos accounts and agents are shown with: flat SVGs drawn in the color
// of the text beside them, as icons are, which the app keeps in
// Contents/Resources/Logos, from `macos/Logos`.

import AppKit
import SwiftUI
import TurnscopeKit

@MainActor
enum Logos {
    private static var cache: [String: NSImage] = [:]

    /// The logo at `path` within the logos, as "providers/anthropic"; a key
    /// for one there isn't.
    static func image(_ path: String) -> NSImage {
        if let image = cache[path] { return image }
        let image = Bundle.main.resourceURL
            .flatMap { NSImage(contentsOf: $0.appending(path: "Logos/\(path).svg")) }
            ?? NSImage(systemSymbolName: "key.fill", accessibilityDescription: nil)
            ?? NSImage()
        image.isTemplate = true
        cache[path] = image
        return image
    }

}

/// A logo, drawn in the text's color.
struct Logo: View {
    var image: NSImage
    var size: CGFloat = 16

    init(account: Account, size: CGFloat = 16) {
        image = Logos.image("providers/\(account.logo)")
        self.size = size
    }

    init(agent: AgentLink, size: CGFloat = 16) {
        image = Logos.image(agent.logo)
        self.size = size
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
