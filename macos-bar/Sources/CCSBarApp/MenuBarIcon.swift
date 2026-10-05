import SwiftUI
import AppKit
import CCSBarCore

/// The Apex Soft menu-bar template glyph (16 pt, tinted by macOS) and the status item's content.
enum MenuBarIcon {
  /// Built once: the status label's body runs on every model publish, and copying the asset on
  /// each run was needless image work on every refresh tick (N4).
  @MainActor static let template: NSImage = {
    let image = TrayAssets.image("MenuBarTemplate")?.copy() as? NSImage
      ?? NSImage(systemSymbolName: "gauge.with.dots.needle.50percent", accessibilityDescription: "AI Account Center")
      ?? NSImage()
    image.size = NSSize(width: 16, height: 16)
    image.isTemplate = true
    return image
  }()
}

/// The glyph plus the chosen account's percentage. The number rolls to its new value with an ease-out,
/// so it never passes the reading.
struct StatusItemLabel: View {
  @ObservedObject var model: AccountsViewModel
  @ObservedObject var prefs: TrayPreferences

  var body: some View {
    let reading = model.menuBarReading(prefs)
    HStack(spacing: 4) {
      Image(nsImage: MenuBarIcon.template).renderingMode(.template)
      if let reading {
        Text(reading.text)
          .font(Font(NSFont.menuBarFont(ofSize: 0)).weight(.medium))
          .monospacedDigit()
          .contentTransition(.numericText(value: reading.value))
          .fixedSize()
      }
    }
    .foregroundStyle(.primary)
    .padding(.horizontal, 4)
    .frame(height: NSStatusBar.system.thickness)
    .animation(NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? nil : .trayValue(duration: 0.5), value: reading?.value)
    .fixedSize()
  }
}

/// A hosting view that leaves every click to the status item's own button.
final class PassthroughHostingView<Content: View>: NSHostingView<Content> {
  override func hitTest(_ point: NSPoint) -> NSView? { nil }
}
