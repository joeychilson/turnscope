// Turnscope, the menu bar app: the menu bar item, the panel it opens, and
// notifications. It talks to `turnscope serve` through `Client`.
//
// The item is an AppKit status item. The panel is a borderless AppKit window
// with SwiftUI inside. The window doesn't move or resize: it's transparent and
// as tall as the screen allows, and the panel draws itself at the top, so its
// height follows what's in it. A click on the transparent part goes through to
// whatever is underneath and closes the panel, like a click anywhere else.
//
// The engine is the `turnscope` binary in Contents/Helpers, or the one named
// by `TURNSCOPE_BINARY` when run from a checkout. Launch options such as
// `--data <dir>` are read by `Launch`.

import AppKit
import SwiftUI
import TurnscopeKit

@main
@MainActor
enum Main {
    static func main() {
        let launch = Launch(CommandLine.arguments)
        if let folder = launch.snapshot {
            Snapshot.draw(launch, into: folder)
            return
        }
        let app = NSApplication.shared
        let shell = AppDelegate(launch)
        app.delegate = shell
        app.setActivationPolicy(.accessory)
        app.run()
    }
}

/// A panel that can become key, which a borderless one can't by default.
private final class PanelWindow: NSPanel {
    override var canBecomeKey: Bool { true }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private let store: Store
    private let client: Client?
    private let navigation = Navigation()
    private let notifier = Notifier()
    private let updater = Updater()
    private let menuBar = MenuBarItem()
    private var item: NSStatusItem?
    private var panel: PanelWindow?
    /// What to open once the first status arrives, as `--open` says. Nil
    /// once it has.
    private var opening: Opening?
    /// Whether the panel is open or opening. A closing panel may still be
    /// visible, but isn't open.
    private var open = false
    private var outside: Any?
    /// The timer that ticks each minute, while something shown counts down
    /// with the clock.
    private var ticking: Timer?
    /// Watches the menu bar's appearance, which the item's colors follow.
    private var inked: NSKeyValueObservation?
    /// Watches the preferences, since what the menu bar shows is one.
    private var preferences: (any NSObjectProtocol)?

    init(_ launch: Launch) {
        Preference.register()
        opening = launch.opening
        if launch.fixture != nil {
            client = nil
            store = Store(fixture: launch.fixture)
        } else {
            let client = Client(binary: AppDelegate.binary, arguments: launch.passed)
            self.client = client
            store = Store(client: client)
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
        notifier.onOpen = { [weak self] account in self?.show(account) }
        notifier.start()
        // An alert is acknowledged only once it's posted. That way one macOS
        // refused, such as before notifications were allowed, comes again.
        store.onAlert = { [weak self] alert in
            guard let self else { return }
            Task {
                if await self.notifier.tell(alert, accounts: self.store.all, words: self.store.words()) {
                    self.store.acknowledge([alert.id])
                }
            }
        }
        store.onChange = { [weak self] in
            guard let self else { return }
            drawItem()
            notifier.renewed(store.all)
            if let opening, store.status != nil {
                self.opening = nil
                openOnce(opening)
            }
        }
        if let client {
            client.start()
        } else if store.status != nil {
            store.onChange()
        }
        updater.start()
        // Redraw when the menu bar turns light or dark, as the system's
        // appearance or the wallpaper behind it changes. This runs on the
        // main thread, since a view's appearance changes there.
        inked = item.button?.observe(\.effectiveAppearance) { [weak self] _, _ in
            MainActor.assumeIsolated { self?.drawItem() }
        }
        // What the menu bar shows is a preference that Settings changes.
        // Others, such as the updater's, may change on another thread.
        preferences = NotificationCenter.default.addObserver(
            forName: UserDefaults.didChangeNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.drawItem() }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        client?.stop()
    }

    /// Open the panel as `--open` asked, once the item has a place in the
    /// menu bar to hang it from.
    private func openOnce(_ opening: Opening) {
        Task {
            try? await Task.sleep(for: .seconds(1))
            show()
            if let tab = navigation.apply(opening, firstInUse: store.inUse.first?.id) {
                navigation.go(.settings, tab: tab)
            }
        }
    }

    // MARK: The item

    /// Draw the item if what it shows has changed, and tick while it or the
    /// panel counts down. While serve is away, it's out of date.
    private func drawItem() {
        // The menu bar's own appearance, not the app's. It follows the
        // wallpaper behind it.
        let appearance = item?.button?.effectiveAppearance ?? NSApp.effectiveAppearance
        let dark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
        if let image = menuBar.image(store.inUse, words: store.words(), dark: dark,
                                     outOfDate: store.failure != nil) {
            item?.button?.image = image
        }
        tick()
    }

    /// Tick each minute, the finest unit the words use for time, but only
    /// while a countdown is shown: in the menu bar, or in the open panel's
    /// verdict.
    private func tick() {
        let counting = open || MenuBarItem.urgent(store.inUse)
        guard counting != (ticking != nil) else { return }
        ticking?.invalidate()
        ticking = nil
        guard counting else { return }
        let minute = Calendar.current.nextDate(after: .now, matching: DateComponents(second: 0),
                                               matchingPolicy: .nextTime) ?? .now.addingTimeInterval(60)
        // On the main run loop, so it fires on the main thread.
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
            quit: { NSApp.terminate(nil) },
            close: { [weak self] in self?.close() },
            notificationsOff: { [weak self] in await self?.notifier.off() ?? false }
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

    /// Open the panel on its first page with `account` open in place, as a
    /// notification about it asks. If it's among the accounts not used this
    /// week, those are unfolded.
    private func show(_ account: String?) {
        show()
        guard let account, store.all.contains(where: { $0.id == account && !$0.hidden }) else { return }
        navigation.go(.main)
        navigation.open = account
        navigation.unusedOpen = store.unused.contains { $0.id == account }
        store.watch(account)
    }

    private func show() {
        guard let panel else { return }
        // If it's already open (say, a notification was clicked), bring it
        // forward.
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
        store.askWhatUsedEach()
        tick()
        // Event monitors run on the main thread.
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
        store.watch(nil)
        tick()
        NSAnimationContext.runAnimationGroup({ context in
            context.duration = 0.1
            panel.animator().alphaValue = 0
        }, completionHandler: { [weak self] in
            // This runs on the main thread, since AppKit's animations end
            // there. Skip it if the panel was opened again while fading.
            MainActor.assumeIsolated { if self?.open == false { panel.orderOut(nil) } }
        })
    }

    func windowDidResignKey(_ notification: Notification) {
        if (notification.object as? NSWindow) === panel { close() }
    }

    /// Place the panel's window under the item. It's as wide as the panel
    /// and its shadow, and runs from just under the menu bar to the bottom of
    /// the screen.
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
}
