// The panel drawn off-screen, from a feed, as PNGs in light and dark:
// `Turnscope --snapshot <folder> --fixture contract/feed.json`, as
// `scripts/screenshots.sh` runs it for the README. With `--open`, it is
// drawn as `--open` opens it (`Navigation.opening`). Glass needs a window
// behind it, so the panel is drawn on the window background instead.

import AppKit
import SwiftUI
import TurnscopeKit

@MainActor
enum Snapshot {
    static func draw(fixture path: String?, into folder: String) {
        Preference.register()
        let store = fixture(path)
        let navigation = Navigation()
        let tab = argument("--open").flatMap { navigation.opening($0, firstInUse: store.inUse.first?.id) }
        if let tab { navigation.tab = tab }
        let out = URL(fileURLWithPath: folder)
        try? FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
        for dark in [false, true] {
            let page: AnyView = tab == nil
                ? AnyView(Panel())
                : AnyView(SettingsPage().frame(width: panelWidth).fixedSize(horizontal: false, vertical: true))
            let view = page
                .environment(store)
                .environment(navigation)
                .environment(Updater())
                .panelFrame(Color(nsColor: .windowBackgroundColor))
                .padding(1)
            // Settings' native controls are drawn only by AppKit; the rest is
            // drawn sharper, and with its bars grown, by SwiftUI.
            let png = tab == nil ? rendered(view, dark: dark) : drawn(view, dark: dark)
            try? png?.write(to: out.appending(path: "panel-\(dark ? "dark" : "light").png"))
        }
    }

    /// `view` as a PNG, at twice its size, as SwiftUI renders it.
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

    /// `view` as a PNG, at twice its size, drawn by AppKit in a window that is
    /// never shown, so native controls, which `ImageRenderer` can't draw, are
    /// drawn as they are on screen.
    private static func drawn(_ view: some View, dark: Bool) -> Data? {
        let host = NSHostingView(rootView: view)
        let window = NSWindow(contentRect: .zero, styleMask: .borderless, backing: .buffered, defer: false)
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
