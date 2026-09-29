// Turnscope, the menu bar app: an item in the menu bar, the panel it opens,
// and notifications, over the engine `turnscope watch` runs (`Engine`).
//
// The item and the panel are AppKit's, a status item and a borderless panel,
// with SwiftUI inside: the panel's window stays still and as tall as the
// screen allows, transparent, and the panel draws itself at its top, so its
// height moves with what is inside it. Clicks on the window's transparent
// part fall through to what is beneath, and close the panel, as a click
// anywhere else does.
//
// The engine is the `turnscope` binary in Contents/Helpers, or the one
// `TURNSCOPE_BINARY` names, as a build from a checkout runs; `--data <dir>`
// keeps its ledger in a scratch directory, as work in progress should, and
// `--open` opens the panel once the first feed comes, to look at it: `--open
// account` with the first account in use open, and `--open settings`,
// `--open agents` or `--open accounts` at that tab of Settings.
// `--fixture <feed.json>` shows a feed from a file, with no engine, as
// `contract/feed.json`, and `--snapshot <folder>` draws the panel over it,
// as `--open` would show it, in light and dark as PNGs, for the README and
// for review, and quits.

import AppKit
import SwiftUI
import TurnscopeKit

@main
enum Main {
    static func main() {
        let app = NSApplication.shared
        if let folder = argument("--snapshot") {
            MainActor.assumeIsolated { Snapshot.draw(fixture: argument("--fixture"), into: folder) }
            return
        }
        let shell = MainActor.assumeIsolated { Shell() }
        app.delegate = shell
        app.setActivationPolicy(.accessory)
        app.run()
    }
}

/// The value given after `option` on the command line.
func argument(_ option: String) -> String? {
    let arguments = CommandLine.arguments
    guard let index = arguments.firstIndex(of: option), index + 1 < arguments.count else { return nil }
    return arguments[index + 1]
}

/// A feed read from the file at `path`, as `--fixture` names one.
func fixture(_ path: String) -> Feed? {
    guard let data = FileManager.default.contents(atPath: path) else { return nil }
    return try? Contract.decoder.decode(Feed.self, from: data)
}

/// A panel that can take keys, as a borderless one can't by default.
private final class PanelWindow: NSPanel {
    override var canBecomeKey: Bool { true }
}

@MainActor
final class Shell: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private let store: Store
    private let engine: Engine?
    private let navigation = Navigation()
    private let notifier = Notifier()
    private let updater = Updater()
    private var item: NSStatusItem?
    private var panel: PanelWindow?
    /// Whether the panel is open, or opening: closing, it may still be seen.
    private var open = false
    private var outside: Any?
    /// The minute's tick, while something shown counts down with the clock.
    private var ticking: Timer?

    override init() {
        Preference.register()
        if let path = argument("--fixture") {
            engine = nil
            let feed = fixture(path)
            Words.frozen = feed?.at
            store = Store(engine: nil, feed: feed)
        } else {
            let engine = Engine(binary: Shell.binary, arguments: Shell.passed)
            self.engine = engine
            store = Store(engine: engine)
        }
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.target = self
        item.button?.action = #selector(toggle)
        self.item = item
        drawItem()
        panel = makePanel()
        notifier.onOpen = { [weak self] account in
            guard let self else { return }
            show()
            if store.inUse.contains(where: { $0.id == account }) { navigation.open = account }
        }
        notifier.start()
        store.onAlert = { [weak self] in self?.notifier.tell($0) }
        store.onRecap = { [weak self] in self?.notifier.tell($0) }
        var opening = CommandLine.arguments.contains("--open")
        let opened = argument("--open")
        store.onFeed = { [weak self] feed in
            self?.drawItem()
            self?.notifier.follow(feed)
            if opening {
                opening = false
                // Once the item has a place in the menu bar to hang it from.
                DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
                    guard let self else { return }
                    self.show()
                    if let tab = opened.flatMap(Navigation.Tab.init(opening:)) {
                        self.navigation.go(.settings, tab: tab)
                    } else if opened == "account" {
                        self.navigation.open = self.store.inUse.first?.id
                    }
                }
            }
        }
        if let engine {
            engine.start()
        } else if let feed = store.feed {
            store.onFeed(feed)
        }
        updater.start()
        DistributedNotificationCenter.default().addObserver(
            self, selector: #selector(drawItem),
            name: Notification.Name("AppleInterfaceThemeChangedNotification"), object: nil)
        // What the menu bar shows is a preference Settings changes.
        NotificationCenter.default.addObserver(
            self, selector: #selector(drawItem), name: UserDefaults.didChangeNotification, object: nil)
    }

    func applicationWillTerminate(_ notification: Notification) {
        engine?.stop()
    }

    // MARK: The item

    /// Draw the item, when what it shows has changed, and tick while it or
    /// the panel counts down.
    @objc private func drawItem() {
        let dark = NSApp.effectiveAppearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
        if let image = MenuBarItem.image(store.inUse, dark: dark) {
            item?.button?.image = image
        }
        tick()
    }

    /// Tick at each minute, the finest the words tell time to, only while a
    /// countdown is shown: in the menu bar, or in the open panel's verdict.
    private func tick() {
        let counting = open || MenuBarItem.counts(store.inUse)
        guard counting != (ticking != nil) else { return }
        ticking?.invalidate()
        ticking = nil
        guard counting else { return }
        let minute = Calendar.current.nextDate(after: .now, matching: DateComponents(second: 0),
                                               matchingPolicy: .nextTime) ?? .now.addingTimeInterval(60)
        let timer = Timer(fire: minute, interval: 60, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                if self.open { self.navigation.now = .now }
                self.drawItem()
            }
        }
        timer.tolerance = 2
        RunLoop.main.add(timer, forMode: .common)
        ticking = timer
    }

    // MARK: The panel

    private func makePanel() -> PanelWindow {
        let actions = PanelActions(
            checkForUpdates: { [weak self] in
                self?.close()
                self?.updater.check()
            },
            quit: { NSApp.terminate(nil) },
            close: { [weak self] in self?.close() },
            testNotification: { [weak self] in self?.notifier.test(self?.store.feed) }
        )
        let root = Floating()
            .environment(store)
            .environment(navigation)
            .environment(updater)
            .environment(\.panelActions, actions)
        let host = NSHostingView(rootView: root)
        host.wantsLayer = true
        host.layer?.backgroundColor = .clear
        let panel = PanelWindow(contentRect: .zero,
                                styleMask: [.borderless, .nonactivatingPanel, .fullSizeContentView],
                                backing: .buffered, defer: false)
        panel.isFloatingPanel = true
        panel.level = .popUpMenu
        panel.hasShadow = false
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.collectionBehavior = [.canJoinAllSpaces, .transient, .ignoresCycle]
        panel.contentView = host
        panel.delegate = self
        panel.setAccessibilityLabel("Turnscope")
        return panel
    }

    @objc private func toggle() {
        if open { close() } else { show() }
    }

    private func show() {
        guard let panel else { return }
        // Open already, as when a notification is clicked, it comes forward.
        guard !open else {
            panel.makeKeyAndOrderFront(nil)
            return
        }
        open = true
        navigation.reset()
        navigation.now = .now
        place(panel)
        if !panel.isVisible { panel.alphaValue = 0 }
        panel.makeKeyAndOrderFront(nil)
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.14
            panel.animator().alphaValue = 1
        }
        item?.button?.highlight(true)
        store.setPanelOpen(true)
        tick()
        outside = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown]) { [weak self] _ in
            MainActor.assumeIsolated { self?.close() }
        }
    }

    private func close() {
        guard let panel, open else { return }
        open = false
        if let outside { NSEvent.removeMonitor(outside) }
        outside = nil
        item?.button?.highlight(false)
        store.setPanelOpen(false)
        tick()
        NSAnimationContext.runAnimationGroup({ context in
            context.duration = 0.1
            panel.animator().alphaValue = 0
        }, completionHandler: {
            // Unless it was opened again while it faded.
            MainActor.assumeIsolated { if !self.open { panel.orderOut(nil) } }
        })
    }

    func windowDidResignKey(_ notification: Notification) {
        if (notification.object as? NSWindow) === panel { close() }
    }

    /// Hang the panel's window under the item: as wide as the panel and its
    /// shadow, from just under the menu bar to the bottom of the screen.
    private func place(_ panel: NSWindow) {
        guard let button = item?.button, let window = button.window,
              let screen = window.screen ?? NSScreen.main else { return }
        let at = window.convertToScreen(button.convert(button.bounds, to: nil))
        let visible = screen.visibleFrame
        let width = panelWidth + Floating.gutter * 2
        let x = min(max(at.midX - width / 2, visible.minX + 6 - Floating.gutter),
                    visible.maxX - width + Floating.gutter - 6)
        let top = at.minY - 6
        panel.setFrame(NSRect(x: x, y: visible.minY, width: width, height: top - visible.minY), display: true)
    }

    // MARK: The engine

    /// The `turnscope` binary: `TURNSCOPE_BINARY`, or the bundle's.
    private static var binary: URL {
        if let named = ProcessInfo.processInfo.environment["TURNSCOPE_BINARY"] {
            return URL(fileURLWithPath: named)
        }
        return Bundle.main.bundleURL.appending(path: "Contents/Helpers/turnscope")
    }

    /// What the app was started with to pass on: `--data <dir>` and `--home <dir>`.
    private static var passed: [String] {
        let arguments = CommandLine.arguments
        var passed: [String] = []
        for option in ["--data", "--home"] {
            if let index = arguments.firstIndex(of: option), index + 1 < arguments.count {
                passed += [option, arguments[index + 1]]
            }
        }
        return passed
    }
}
