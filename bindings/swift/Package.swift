// swift-tools-version:5.9
import PackageDescription
import Foundation

// AgentStateGraph — Swift binding for macOS and iOS.
//
// Two build modes, selected at configuration time:
//
//   • XCFramework mode (default): consumes a prebuilt
//     `artifacts/AgentStateGraphFFI.xcframework` that carries static
//     `agentstategraph-ffi` slices for macOS (arm64 + x86_64), iOS device
//     (arm64), and iOS simulator (arm64 + x86_64). This is the only mode
//     that links and runs on a physical iPhone/iPad. Build the framework
//     first with `scripts/build-swift-xcframework.sh`.
//
//   • Local-dylib mode: set the environment variable
//     `AGENTSTATEGRAPH_SWIFT_LOCAL=1`. The package then links directly
//     against a host-built `target/release/libagentstategraph_ffi`
//     (mirrors the Go binding). Fast for macOS/simulator dev — no
//     cross-compilation — but will NOT run on a device.
//
// In both modes the C surface is exposed as the Clang module
// `CAgentStateGraph`, so the Swift sources are identical.

let useLocal = ProcessInfo.processInfo.environment["AGENTSTATEGRAPH_SWIFT_LOCAL"] == "1"

// Repo root is two levels up from bindings/swift.
let packageDir = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
let repoRoot = packageDir.deletingLastPathComponent().deletingLastPathComponent()
let releaseDir = repoRoot.appendingPathComponent("target/release").path

var targets: [Target] = []
var swiftDeps: [Target.Dependency] = []

if useLocal {
    // Link the host-built dynamic/static library directly.
    targets.append(
        .systemLibrary(
            name: "CAgentStateGraph",
            path: "Sources/CAgentStateGraph"
        )
    )
    swiftDeps.append("CAgentStateGraph")
} else {
    // Consume the prebuilt fat framework.
    targets.append(
        .binaryTarget(
            name: "AgentStateGraphFFI",
            path: "artifacts/AgentStateGraphFFI.xcframework"
        )
    )
    swiftDeps.append("AgentStateGraphFFI")
}

// Linker settings needed when pulling in the static/dynamic Rust lib.
// getrandom + rusqlite(bundled) reference these system pieces on Apple.
var swiftLinkerSettings: [LinkerSetting] = [
    .linkedLibrary("c++"),
    .linkedFramework("Security"),
    .linkedFramework("CoreFoundation"),
]

if useLocal {
    swiftLinkerSettings.append(
        .unsafeFlags([
            "-L", releaseDir,
            "-lagentstategraph_ffi",
            "-Xlinker", "-rpath", "-Xlinker", releaseDir,
        ])
    )
}

targets.append(
    .target(
        name: "AgentStateGraph",
        dependencies: swiftDeps,
        path: "Sources/AgentStateGraph",
        linkerSettings: swiftLinkerSettings
    )
)

targets.append(
    .testTarget(
        name: "AgentStateGraphTests",
        dependencies: ["AgentStateGraph"],
        path: "Tests/AgentStateGraphTests"
    )
)

let package = Package(
    name: "AgentStateGraph",
    platforms: [
        .macOS(.v11),
        .iOS(.v14),
    ],
    products: [
        .library(name: "AgentStateGraph", targets: ["AgentStateGraph"]),
    ],
    targets: targets
)
