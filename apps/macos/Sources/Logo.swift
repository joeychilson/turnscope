// Account and agent logos, flat SVGs copied from `assets/logos/` into
// `Contents/Resources/logos/`, and Turnscope's mark: drawn in the color of the text next to them.

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

    /// Turnscope's own.
    init(size: CGFloat) {
        image = Logo.mark
        self.size = size
    }

    /// Two thirds of its box, as the menu bar item draws it beside providers' logos.
    @MainActor private static let mark = NSImage(size: NSSize(width: 21, height: 21), flipped: false) { rect in
        drawMark(in: rect.insetBy(dx: 3.5, dy: 3.5), color: .black)
        return true
    }

    /// Turnscope's mark as `Turnscope.icon` draws it: a ring around a 235° wedge in 530 units.
    nonisolated static func drawMark(in rect: NSRect, color: NSColor) {
        let unit = rect.width / 530
        let center = NSPoint(x: rect.midX, y: rect.midY)
        func circle(_ radius: CGFloat) -> NSBezierPath {
            let r = radius * unit
            return NSBezierPath(ovalIn: NSRect(x: center.x - r, y: center.y - r, width: r * 2, height: r * 2))
        }
        color.set()
        let ring = circle(265)
        ring.append(circle(207))
        ring.windingRule = .evenOdd
        ring.fill()

        let slice = NSBezierPath()
        slice.move(to: center)
        slice.appendArc(withCenter: center, radius: 169 * unit, startAngle: 90, endAngle: -145, clockwise: true)
        slice.close()
        slice.fill()
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
