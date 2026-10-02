import SwiftUI
import AppKit

/// Bundled artwork. The packaged app reads Contents/Resources; a development or preview run can point
/// AAC_ASSETS_DIR at macos-bar/Resources/Assets.
enum TrayAssets {
  @MainActor private static var cache: [String: NSImage] = [:]

  @MainActor static func image(_ name: String, ext: String = "png") -> NSImage? {
    let key = "\(name).\(ext)"
    if let cached = cache[key] { return cached }
    var url = Bundle.main.url(forResource: name, withExtension: ext)
    if url == nil, let dir = ProcessInfo.processInfo.environment["AAC_ASSETS_DIR"] {
      let candidate = URL(fileURLWithPath: dir).appendingPathComponent(key)
      if FileManager.default.fileExists(atPath: candidate.path) { url = candidate }
    }
    guard let url, let image = NSImage(contentsOf: url) else { return nil }
    if ext == "png" {
      // Pick up an @2x companion so template glyphs stay crisp on Retina.
      let retina = url.deletingPathExtension().path + "@2x.png"
      if let hi = NSImage(contentsOfFile: retina) {
        hi.representations.forEach { rep in
          rep.size = image.size
          image.addRepresentation(rep)
        }
      }
    }
    cache[key] = image
    return image
  }
}

/// Official provider artwork, bare (never on a disc or plate), at one optical size. Light-surface
/// variants are used in the Light appearance where the provider ships one. Kimi Code is its official
/// app icon, plate included, by the owner's choice.
struct ProviderMark: View {
  let provider: String
  var size: CGFloat = 20
  @Environment(\.colorScheme) private var scheme

  /// Optical scales from the marks pipeline (AAC_MARK_META in marks.js), so one box looks equal across
  /// providers. Kimi Code is 0.9 for its app icon with the plate (sources.json's 0.97 measures the bare mark).
  static let scale: [String: CGFloat] = [
    "claude": 0.93, "codex": 0.95, "antigravity": 0.97, "cursor": 0.89, "muse": 1.0,
    "kimi-code": 0.9, "qwen": 0.93, "zai": 0.92, "opencode-go": 0.84,
  ]

  var body: some View {
    let inner = size * (Self.scale[provider] ?? 1)
    Group {
      if let image = (scheme == .light ? TrayAssets.image("provider-\(provider)-light") : nil)
        ?? TrayAssets.image("provider-\(provider)") {
        Image(nsImage: image).resizable().interpolation(.high).scaledToFit()
      } else {
        Image(systemName: "circle.dashed").resizable().scaledToFit().foregroundStyle(.secondary)
      }
    }
    .frame(width: inner, height: inner)
    .frame(width: size, height: size)
    .accessibilityLabel(Text(Self.name(provider)))
  }

  static func name(_ provider: String) -> String {
    [
      "claude": "Claude", "codex": "Codex", "antigravity": "Antigravity", "cursor": "Cursor", "muse": "Muse Code",
      "kimi-code": "Kimi Code", "qwen": "Qwen Token Plan", "zai": "Z.ai Coding Plan", "opencode-go": "OpenCode Go",
    ][provider] ?? provider
  }
}

/// Filled platform logos (Simple Icons, CC0), drawn as templates in the label colour.
struct PlatformGlyph: View {
  let platform: String
  var size: CGFloat = 14
  var body: some View {
    Group {
      if let image = TrayAssets.image("platform-\(platform == "mac" ? "apple" : platform)") {
        Image(nsImage: image).renderingMode(.template).resizable().interpolation(.high).scaledToFit()
      } else {
        Image(systemName: platform == "windows" ? "pc" : "apple.logo").resizable().scaledToFit()
      }
    }.frame(width: size, height: size)
  }
}

/// The Apex Soft mark: the "A" in the label colour with its blue meter end.
struct ApexMark: View {
  var size: CGFloat = 22
  var body: some View {
    ZStack {
      if let ink = TrayAssets.image("ApexInk", ext: "svg") {
        Image(nsImage: ink).renderingMode(.template).resizable().scaledToFit().foregroundStyle(.primary)
      } else {
        Image(systemName: "a.circle").resizable().scaledToFit()
      }
      if let meter = TrayAssets.image("ApexMeter", ext: "svg") {
        Image(nsImage: meter).resizable().scaledToFit()
      }
    }.frame(width: size, height: size)
    .accessibilityHidden(true)
  }
}

/// The app icon for About: the light or dark Apex Soft icon.
struct AppIconImage: View {
  var size: CGFloat = 44
  @Environment(\.colorScheme) private var scheme
  var body: some View {
    Group {
      if let image = TrayAssets.image(scheme == .dark ? "AppIconDark" : "AppIconLight") {
        Image(nsImage: image).resizable().interpolation(.high).scaledToFit()
      } else {
        Image(nsImage: NSApplication.shared.applicationIconImage).resizable().scaledToFit()
      }
    }.frame(width: size, height: size)
  }
}
