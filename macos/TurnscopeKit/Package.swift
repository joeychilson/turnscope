// swift-tools-version: 6.0
// TurnscopeKit: everything of the menu bar app that can be tested without a
// screen: the contract `turnscope serve` speaks, the client that talks to it, and how
// what it says is put into words. The app itself is the Xcode project in
// macos/, which uses this package; `swift test` runs these tests alone.
import PackageDescription

let package = Package(
    name: "TurnscopeKit",
    platforms: [.macOS("26.0")],
    products: [
        .library(name: "TurnscopeKit", targets: ["TurnscopeKit"]),
    ],
    targets: [
        .target(name: "TurnscopeKit"),
        .testTarget(name: "TurnscopeKitTests", dependencies: ["TurnscopeKit"]),
    ]
)
