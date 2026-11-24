// Account and agent logos: flat SVGs drawn in the color of the text next to them.
// Copied from `assets/logos/` into `Contents/Resources/logos/`.

import AppKit
import SwiftUI

struct Logo: View {
    var image: NSImage
    var size: CGFloat

    init(provider: String, size: CGFloat) {
        image = Logo.svg("providers/\(provider)") ?? Logo.symbol("key.fill")
        self.size = size
    }

    init(agent: String, size: CGFloat) {
        image = Logo.svg("agents/\(agent)") ?? Logo.symbol("terminal.fill")
        self.size = size
    }

    @MainActor private static var svgs: [String: NSImage?] = [:]

    /// `logos/<path>.svg`, read once, or nil if there's none.
    @MainActor static func svg(_ path: String) -> NSImage? {
        if let image = svgs[path] { return image }
        let image = Bundle.main.resourceURL
            .flatMap { NSImage(contentsOf: $0.appending(path: "logos/\(path).svg")) }
        svgs.updateValue(image, forKey: path)
        return image
    }

    private static func symbol(_ name: String) -> NSImage {
        NSImage(systemSymbolName: name, accessibilityDescription: nil) ?? NSImage()
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
