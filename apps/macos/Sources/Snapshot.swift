// Draws the panel off-screen as 2x PNGs (`panel-light.png` and `panel-dark.png`) and quits,
// for `scripts/screenshots.sh` and `scripts/check-app.sh`.

import AppKit
import SwiftUI

@MainActor
enum Snapshot {
    static func draw(_ launch: Launch, into folder: String) -> Int32 {
        _ = NSApplication.shared
        let out = URL(fileURLWithPath: folder)
        do {
            try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
        } catch {
            fputs("Turnscope: \(folder) can't be made: \(error.localizedDescription)\n", stderr)
            return 2
        }

        let panelModel: PanelModel
        switch snapshotPanel(launch) {
        case let .ok(model):
            panelModel = model
        case let .err(message, code):
            fputs("\(message)\n", stderr)
            return code
        }

        let bridge = Bridge(snapshot: panelModel)
        let ui = UIState()
        if let opening = launch.opening {
            ui.apply(opening, panel: panelModel)
        }
        let updater = Updater()
        let notifier = Notifier()
        let login = Login()

        for dark in [false, true] {
            let background = dark ? Color(hex: 0x1E1E1E) : Color.white
            let view = Panel()
                .environment(bridge)
                .environment(ui)
                .environment(updater)
                .environment(notifier)
                .environment(login)
                .environment(\.isSnapshot, true)
                .panelFrame(background)
                .padding(1)
                .environment(\.colorScheme, dark ? .dark : .light)

            guard let png = renderPNG(view, dark: dark) else {
                fputs("Turnscope: the panel can't be drawn\n", stderr)
                return 1
            }
            let file = out.appending(path: "panel-\(dark ? "dark" : "light").png")
            do {
                try png.write(to: file)
            } catch {
                fputs("Turnscope: \(file.path) can't be written: \(error.localizedDescription)\n", stderr)
                return 1
            }
        }
        return 0
    }

    private static func renderPNG(_ view: some View, dark: Bool) -> Data? {
        var png: Data?
        NSAppearance(named: dark ? .darkAqua : .aqua)?.performAsCurrentDrawingAppearance {
            let renderer = ImageRenderer(content: view)
            renderer.scale = 2
            guard let tiff = renderer.nsImage?.tiffRepresentation else { return }
            png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:])
        }
        return png
    }
}
