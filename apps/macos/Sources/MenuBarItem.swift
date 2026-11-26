// The menu bar item: one AppKit drawing-handler image, redrawn only when `MenuBarModel`
// changes. Colors resolve dynamically at draw time in the menu bar's own appearance
// (light/dark wallpaper and Increase Contrast).

import AppKit

@MainActor
final class MenuBarItem {
    /// Item height and mark sizes (measured against other macOS menu bar items, Oct 2026).
    private static let height: CGFloat = 22
    private static let logoSize: CGFloat = 21
    private static let markSize: CGFloat = 14
    private static let dimmed: CGFloat = 0.45

    private var drawn: MenuBarModel?

    func image(for model: MenuBarModel) -> NSImage? {
        guard model != drawn else { return nil }
        drawn = model

        let isTemplate = model.slots.allSatisfy { $0.standing == .lasts }
        let fraction: CGFloat = model.outOfDate ? Self.dimmed : 1.0
        let font = NSFont.monospacedDigitSystemFont(
            ofSize: NSFont.menuBarFont(ofSize: 0).pointSize,
            weight: .semibold
        )
        let measureAttrs: [NSAttributedString.Key: Any] = [.font: font]

        struct Part: @unchecked Sendable {
            var logo: NSImage?
            var figure: NSString
            var standing: Standing
            var width: CGFloat
        }

        let parts: [Part] = model.slots.map { slot in
            let figure = slot.figure as NSString
            let width = ceil(figure.size(withAttributes: measureAttrs).width)
            return Part(
                logo: Logo.svg("providers/\(slot.provider)"),
                figure: figure,
                standing: slot.standing,
                width: width
            )
        }

        let lineHeight = font.ascender - font.descender
        let totalWidth: CGFloat
        if parts.isEmpty {
            totalWidth = Self.markSize + 2
        } else {
            let content = parts.reduce(CGFloat(0)) { sum, part in
                sum + (part.logo != nil ? Self.logoSize + 4 : 0) + part.width
            }
            totalWidth = content + 10 * CGFloat(parts.count - 1) + 2
        }

        let amberColor = Self.dynamicColor(UrgencyHex.amber, alpha: fraction)
        let redColor = Self.dynamicColor(UrgencyHex.red, alpha: fraction)

        let image = NSImage(size: NSSize(width: totalWidth, height: Self.height), flipped: false) { _ in
            func ink(_ standing: Standing) -> NSColor {
                if isTemplate {
                    return NSColor.black.withAlphaComponent(fraction)
                }
                return switch standing {
                case .lasts: NSColor.labelColor.withAlphaComponent(fraction)
                case .runningOut: amberColor
                case .usedUp: redColor
                }
            }

            if parts.isEmpty {
                Self.drawMark(
                    in: NSRect(
                        x: 1,
                        y: (Self.height - Self.markSize) / 2,
                        width: Self.markSize,
                        height: Self.markSize
                    ),
                    color: ink(.lasts)
                )
                return true
            }

            var x: CGFloat = 1
            for part in parts {
                if let logo = part.logo {
                    Self.drawTinted(
                        logo,
                        in: NSRect(
                            x: x,
                            y: (Self.height - Self.logoSize) / 2,
                            width: Self.logoSize,
                            height: Self.logoSize
                        ),
                        color: ink(.lasts)
                    )
                    x += Self.logoSize + 4
                }
                // Raised 1pt onto the menu bar clock's baseline (measured Oct 2026).
                let y = (Self.height - lineHeight) / 2 + 1
                part.figure.draw(
                    at: NSPoint(x: x, y: y),
                    withAttributes: [.font: font, .foregroundColor: ink(part.standing)]
                )
                x += part.width + 10
            }
            return true
        }
        image.isTemplate = isTemplate
        image.accessibilityDescription = model.spoken
        return image
    }

    private nonisolated static func drawTinted(_ image: NSImage, in rect: NSRect, color: NSColor) {
        let size = image.size
        guard size.width > 0, size.height > 0 else { return }
        let scale = min(rect.width / size.width, rect.height / size.height)
        let fitted = NSSize(width: size.width * scale, height: size.height * scale)
        let dest = NSRect(
            x: rect.minX + (rect.width - fitted.width) / 2,
            y: rect.minY + (rect.height - fitted.height) / 2,
            width: fitted.width,
            height: fitted.height
        )
        let tinted = NSImage(size: fitted, flipped: false) { bounds in
            image.draw(in: bounds)
            color.set()
            bounds.fill(using: .sourceAtop)
            return true
        }
        tinted.draw(in: dest)
    }

    /// Turnscope's mark as `Turnscope.icon` draws it: a ring around a 235° wedge in 530 units.
    private nonisolated static func drawMark(in rect: NSRect, color: NSColor) {
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

    private static func dynamicColor(_ hexes: [UInt32], alpha: CGFloat) -> NSColor {
        let colors = hexes.map { hex in
            NSColor(
                srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
                green: CGFloat((hex >> 8) & 0xFF) / 255,
                blue: CGFloat(hex & 0xFF) / 255,
                alpha: alpha
            )
        }
        return NSColor(name: nil) { appearance in
            switch appearance.bestMatch(from: [
                .aqua, .darkAqua, .accessibilityHighContrastAqua, .accessibilityHighContrastDarkAqua,
            ]) {
            case .accessibilityHighContrastDarkAqua: colors[3]
            case .accessibilityHighContrastAqua: colors[2]
            case .darkAqua: colors[1]
            default: colors[0]
            }
        }
    }
}
