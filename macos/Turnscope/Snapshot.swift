// Draws the panel off-screen as PNGs, light and dark, from a status file:
// `Turnscope --snapshot <folder> --fixture contract/status.notification.json`.
// `scripts/screenshots.sh` runs it for the README. `--open` opens a row or
// Settings first (`Navigation.apply`). Glass needs a window behind it, so the
// snapshot uses the window background instead.

import AppKit
import SwiftUI
import TurnscopeKit

@MainActor
enum Snapshot {
    static func draw(_ launch: Launch, into folder: String) {
        Preference.register()
        let store = Store(fixture: launch.fixture)
        let navigation = Navigation()
        let tab = launch.opening.flatMap { navigation.apply($0, firstInUse: store.inUse.first?.id) }
        if let tab { navigation.tab = tab }
        let out = URL(fileURLWithPath: folder)
        try? FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
        for dark in [false, true] {
            // Only AppKit draws Settings' native controls. SwiftUI draws the
            // rest sharper, and with its bars grown.
            let png = if tab == nil {
                rendered(framed(Panel(), store: store, navigation: navigation), dark: dark)
            } else {
                drawn(framed(SettingsPage().frame(width: panelWidth).fixedSize(horizontal: false, vertical: true),
                             store: store, navigation: navigation), dark: dark)
            }
            try? png?.write(to: out.appending(path: "panel-\(dark ? "dark" : "light").png"))
        }
    }

    /// `page` in the panel's frame, showing what `store` and `navigation`
    /// hold.
    private static func framed(_ page: some View, store: Store, navigation: Navigation) -> some View {
        page
            .environment(store)
            .environment(navigation)
            .environment(Updater())
            .panelFrame(Color(nsColor: .windowBackgroundColor))
            .padding(1)
    }

    /// `view` as a PNG at twice its size, rendered by SwiftUI.
    private static func rendered(_ view: some View, dark: Bool) -> Data? {
        var png: Data?
        NSAppearance(named: dark ? .darkAqua : .aqua)?.performAsCurrentDrawingAppearance {
            let renderer = ImageRenderer(content: view.environment(\.colorScheme, dark ? .dark : .light))
            renderer.scale = 2
            guard let tiff = renderer.nsImage?.tiffRepresentation else { return }
            png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:])
        }
        return png
    }

    /// `view` as a PNG at twice its size, drawn by AppKit in a window that's
    /// never shown. That way native controls, which `ImageRenderer` can't
    /// draw, look as they do on screen.
    private static func drawn(_ view: some View, dark: Bool) -> Data? {
        let host = NSHostingView(rootView: view)
        let window = Retina(contentRect: .zero, styleMask: .borderless, backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        window.backgroundColor = .clear
        window.contentView = host
        let size = host.fittingSize
        window.setContentSize(size)
        host.frame = NSRect(origin: .zero, size: size)
        host.layoutSubtreeIfNeeded()
        let pixels = NSSize(width: size.width * 2, height: size.height * 2)
        guard let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(pixels.width),
                                            pixelsHigh: Int(pixels.height), bitsPerSample: 8,
                                            samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)
        else { return nil }
        bitmap.size = size
        host.cacheDisplay(in: host.bounds, to: bitmap)
        return bitmap.representation(using: .png, properties: [:])
    }
}

/// A window that draws at twice its size on any screen, so a snapshot drawn
/// by AppKit is just as sharp on a display that isn't Retina.
private final class Retina: NSWindow {
    override var backingScaleFactor: CGFloat { 2 }
}
