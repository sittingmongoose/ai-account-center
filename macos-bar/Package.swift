// swift-tools-version:6.2
import PackageDescription

// AI Account Center for macOS - the menu bar client.
//
// The panel is macOS 26+ Liquid Glass (SwiftUI glass API, NSGlassEffectView), so the deployment
// target is macOS 26 and the build needs Xcode 26 or later (Xcode 27 on the build Mac). The testable
// logic lives in the pure-Foundation `CCSBarCore` target and is exercised by the `ccs-bar-check`
// executable, an assert harness that needs no XCTest. The sources stay in the Swift 5 language mode.
let package = Package(
  name: "CCSBar",
  platforms: [.macOS(.v26)],
  products: [
    .executable(name: "CCSBar", targets: ["CCSBarApp"]),
    .executable(name: "ccs-bar-check", targets: ["CCSBarCheck"]),
  ],
  targets: [
    .target(name: "CCSBarCore"),
    .executableTarget(name: "CCSBarApp", dependencies: ["CCSBarCore"]),
    .executableTarget(name: "CCSBarCheck", dependencies: ["CCSBarCore"]),
  ],
  swiftLanguageModes: [.v5]
)
