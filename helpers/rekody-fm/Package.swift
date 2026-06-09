// swift-tools-version:6.0
import PackageDescription

// rekody-fm — a tiny Swift helper that exposes Apple's on-device Foundation
// Models LLM (macOS 26+) to the rekody Rust binary over a stdin/stdout/exit-code
// contract. rekody shells out to this; it is NOT part of the cargo build, so
// CI/release (which may lack the macOS 26 SDK) stay green without it.
let package = Package(
    name: "rekody-fm",
    platforms: [.macOS("26.0")],
    targets: [
        .executableTarget(name: "rekody-fm", path: "Sources/rekody-fm")
    ]
)
