// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "AnyOSLauncher",
    platforms: [.macOS(.v11)],
    products: [
        .executable(name: "AnyOSLauncher", targets: ["AnyOSLauncher"]),
        .library(name: "LauncherCore", targets: ["LauncherCore"]),
    ],
    targets: [
        // Pure logic, no AppKit: importable by tests under `swift test`.
        .target(name: "LauncherCore"),
        .executableTarget(
            name: "AnyOSLauncher",
            dependencies: ["LauncherCore"]
        ),
        .testTarget(
            name: "LauncherCoreTests",
            dependencies: ["LauncherCore"]
        ),
    ]
)
