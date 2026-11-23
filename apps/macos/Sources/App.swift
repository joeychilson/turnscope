// Turnscope, the menu bar app: status item, floating panel, notifications, login item,
// and updates. All engine state, ranking, wording, and alert rules live in `turnscope-app`.

import AppKit
import SwiftUI

@main
@MainActor
enum Main {
    static func main() {
        let launch = parseLaunch(CommandLine.arguments)
        if let folder = launch.snapshot {
            exit(Snapshot.draw(launch, into: folder))
        }
        let app = NSApplication.shared
        let delegate = AppDelegate(launch)
        app.delegate = delegate
        app.setActivationPolicy(.accessory)
        app.run()
    }
}

/// Borderless NSPanel that can become key so keyboard shortcuts and focus work.
private final class PanelWindow: NSPanel {
    override var canBecomeKey: Bool { true }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private let bridge: Bridge
    private let notifier = Notifier()
    private let updater = Updater()
    private let login = Login()
    private let menuBar = MenuBarItem()

    private var item: NSStatusItem?
    private var panel: PanelWindow?
    private var startedAt = ContinuousClock.now
    private var panelOpen = false
    /// Ignore immediate status-item clicks right after resign-key/outside close so
    /// clicking the item closes the panel instead of reopening it.
    private var closedAt: ContinuousClock.Instant?
    private var outsideMonitor: Any?
    private var wakeObserver: (any NSObjectProtocol)?
    private var ticker: Timer?

    init(_ launch: Launch) {
        let prefs = launch.fixture == nil ? Preferences.read() : Preferences(menuShows: .all, dismissedConnect: [])
        self.bridge = Bridge(launch: launch, fromDiskImage: Self.fromDiskImage, preferences: prefs)
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        startedAt = .now
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.target = self
        item.button?.action = #selector(toggle)
        self.item = item

        panel = makePanel()

        bridge.onSync = { [weak self] sync in
            self?.handleSync(sync)
        }
        notifier.onOpen = { [weak self] account in
            self?.openFromNotification(account)
        }
        notifier.onAcknowledged = { [weak self] id, signInAccount in
            self?.bridge.send(.acknowledgeAlert(id: id, signInAccount: signInAccount))
        }

        if bridge.isLive {
            notifier.start()
            updater.start()
            observeWake()
        }
        bridge.sync()
    }

    func applicationWillTerminate(_ notification: Notification) {
        ticker?.invalidate()
        ticker = nil
        if let wakeObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(wakeObserver)
            self.wakeObserver = nil
        }
        bridge.stop()
    }

    // MARK: - Sync & Effects

    private func handleSync(_ sync: AppSync) {
        drawItem(sync.menuBar)
        updateTicker(counting: sync.counting)
        for effect in sync.effects {
            switch effect {
            case let .openInitial(opening):
                whenPlaced { [self] in
                    show(opening: opening)
                }
            case let .postAlerts(alerts):
                notifier.post(alerts)
            case let .removeNotifications(identifiers):
                notifier.remove(identifiers)
            case let .savePreferences(menuShows, dismissedConnect):
                Preferences.save(menuShows: menuShows, dismissedConnect: dismissedConnect)
            }
        }
    }

    private func drawItem(_ model: MenuBarModel) {
        if let image = menuBar.image(for: model) {
            item?.button?.image = image
        }
    }

    private func updateTicker(counting: Bool) {
        guard counting != (ticker != nil) else { return }
        ticker?.invalidate()
        ticker = nil
        guard counting else { return }
        let nextMinute = Calendar.current.nextDate(
            after: .now,
            matching: DateComponents(second: 0),
            matchingPolicy: .nextTime
        ) ?? .now.addingTimeInterval(60)
        let timer = Timer(fire: nextMinute, interval: 60, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.bridge.send(.tick)
            }
        }
        timer.tolerance = 1
        RunLoop.main.add(timer, forMode: .common)
        ticker = timer
    }

    private func observeWake() {
        wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didWakeNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.bridge.send(.tick)
            }
        }
    }

    // MARK: - Status Item Placement

    /// The status item's window and screen once macOS has placed it in the top half of a screen.
    private var placedItemWindow: (NSWindow, NSScreen)? {
        guard let window = item?.button?.window, let screen = window.screen else { return nil }
        let frame = screen.frame
        guard window.frame.origin.y > frame.origin.y + frame.size.height / 2 else { return nil }
        return (window, screen)
    }

    /// Runs `action` once macOS has placed the status item, or 2s after launch at the latest
    /// (so a hidden status item never blocks notification clicks for 2s every time).
    private func whenPlaced(_ action: @escaping @MainActor () -> Void) {
        if placedItemWindow != nil || ContinuousClock.now - startedAt >= .seconds(2) {
            action()
            return
        }
        Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(50))
            self?.whenPlaced(action)
        }
    }

    // MARK: - Floating Panel

    private func makePanel() -> PanelWindow {
        let host = NSHostingView(rootView: makeRootView())
        // SwiftUI manages the panel's animated height inside the fixed transparent window;
        // disabling AppKit sizing options avoids constraint churn during spring frames.
        host.sizingOptions = []
        host.wantsLayer = true
        host.layer?.backgroundColor = .clear
        let panel = PanelWindow(
            contentRect: .zero,
            styleMask: [.borderless, .nonactivatingPanel, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
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

    private func makeRootView() -> some View {
        Floating()
            .environment(bridge)
            .environment(updater)
            .environment(notifier)
            .environment(login)
            .environment(\.closePanel, { [weak self] in self?.close() })
    }

    @objc private func toggle() {
        if panelOpen {
            close()
            return
        }
        if let closedAt, ContinuousClock.now - closedAt < .milliseconds(250) {
            return
        }
        show()
    }

    private func openFromNotification(_ account: String?) {
        whenPlaced { [self] in
            show(account: account)
        }
    }

    private func show(opening: Opening? = nil, account: String? = nil) {
        guard let panel else { return }
        let alreadyOpen = panelOpen
        if !alreadyOpen {
            panelOpen = true
            bridge.screenHeight = currentScreenHeight()
        }
        bridge.send(.panelOpened(opening: opening, account: account))
        if alreadyOpen {
            panel.makeKeyAndOrderFront(nil)
            return
        }
        bridge.panelVisible = true
        if bridge.isLive {
            notifier.check()
            login.check()
            updater.refreshAutomatic()
        }
        place(panel)
        if !panel.isVisible { panel.alphaValue = 0 }
        panel.makeKeyAndOrderFront(nil)
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.14
            panel.animator().alphaValue = 1
        }
        item?.button?.highlight(true)
        outsideMonitor = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown]) { [weak self] _ in
            MainActor.assumeIsolated { self?.close() }
        }
    }

    private func close() {
        guard let panel, panelOpen else { return }
        panelOpen = false
        closedAt = .now
        if let outsideMonitor {
            NSEvent.removeMonitor(outsideMonitor)
            self.outsideMonitor = nil
        }
        item?.button?.highlight(false)
        NSAnimationContext.runAnimationGroup({ context in
            context.duration = 0.1
            panel.animator().alphaValue = 0
        }, completionHandler: { [weak self] in
            MainActor.assumeIsolated {
                guard let self, !self.panelOpen else { return }
                panel.orderOut(nil)
                self.bridge.panelVisible = false
                self.bridge.send(.panelClosed)
            }
        })
    }

    func windowDidResignKey(_ notification: Notification) {
        if (notification.object as? NSWindow) === panel {
            close()
        }
    }

    private func currentScreenHeight() -> CGFloat {
        let screen = placedItemWindow?.1 ?? NSScreen.screens.first ?? NSScreen.main
        return screen?.visibleFrame.height ?? 800
    }

    private func place(_ panel: NSWindow) {
        let visible: NSRect
        let midX: CGFloat
        let underY: CGFloat
        if let (window, screen) = placedItemWindow, let button = item?.button {
            let at = window.convertToScreen(button.convert(button.bounds, to: nil))
            visible = screen.visibleFrame
            midX = at.midX
            underY = at.minY
        } else if let primary = NSScreen.screens.first ?? NSScreen.main {
            visible = primary.visibleFrame
            midX = visible.maxX
            underY = visible.maxY
        } else {
            return
        }
        let left = max(visible.minX + 6, min(midX - panelWidth / 2, visible.maxX - panelWidth - 6))
        let top = underY - 6
        let height = max(200, top - visible.minY)
        panel.setFrame(
            NSRect(
                x: left - Floating.gutter,
                y: visible.minY,
                width: panelWidth + Floating.gutter * 2,
                height: height
            ),
            display: true
        )
    }

    /// True when running from a mounted DMG or App Translocation path, whose bundled
    /// `turnscope` path vanishes after eject or quit.
    private static var fromDiskImage: Bool {
        let bundle = Bundle.main.bundleURL
        let readOnly = (try? bundle.resourceValues(forKeys: [.volumeIsReadOnlyKey]))?.volumeIsReadOnly
        return readOnly == true || bundle.path.contains("/AppTranslocation/")
    }
}
