// Account and agent logos: flat SVGs drawn in the color of the text next to them.
// Copied from `assets/logos/` into `Contents/Resources/logos/`.

import AppKit
import SwiftUI

struct Logo: View {
    var image: NSImage
    var size: CGFloat

    init(provider: String, size: CGFloat) {
        image = Logo.image("providers/\(provider)", fallback: "key.fill")
        self.size = size
    }

    init(agent: String, size: CGFloat) {
        image = Logo.image("agents/\(agent)", fallback: "terminal.fill")
        self.size = size
    }

    @MainActor private static var cache: [String: NSImage] = [:]

    @MainActor static func providerImage(_ provider: String) -> NSImage? {
        let key = "providers/\(provider)"
        if let cached = cache[key] { return cached }
        guard let url = Bundle.main.resourceURL?.appending(path: "logos/\(key).svg"),
              let image = NSImage(contentsOf: url)
        else { return nil }
        image.isTemplate = true
        cache[key] = image
        return image
    }

    @MainActor private static func image(_ path: String, fallback: String) -> NSImage {
        if let cached = cache[path] { return cached }
        let loaded = Bundle.main.resourceURL
            .flatMap { NSImage(contentsOf: $0.appending(path: "logos/\(path).svg")) }
            ?? NSImage(systemSymbolName: fallback, accessibilityDescription: nil)
            ?? NSImage()
        loaded.isTemplate = true
        cache[path] = loaded
        return loaded
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
