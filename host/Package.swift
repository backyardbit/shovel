// swift-tools-version:5.10
import PackageDescription

let package = Package(
    name: "ShovelHost",
    platforms: [.macOS(.v14)],
    targets: [
        .target(name: "VNCShim", path: "Sources/VNCShim"),
        .executableTarget(
            name: "Shovel",
            dependencies: ["VNCShim"],
            path: "Sources/Shovel",
            linkerSettings: [.linkedFramework("Virtualization"), .linkedFramework("AppKit")]
        ),
    ]
)
