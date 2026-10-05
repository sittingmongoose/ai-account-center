import SwiftUI
import AppKit
import CCSBarCore

/// The Apex Soft menu-bar template glyph (16 pt, tinted by macOS) and the status item's content.
enum MenuBarIcon {
  @MainActor static var template: NSImage {
    let image = TrayAssets.image("MenuBarTemplate")?.copy() as? NSImage
      ?? NSImage(systemSymbolName: "gauge.with.dots.needle.50percent", accessibilityDescription: "AI Account Center")
      ?? NSImage()
    image.size = NSSize(width: 16, height: 16)
    image.isTemplate = true
    return image
  }
}

/// The menu-bar label's whole visible state: the dashboard it reads its number from, and the
/// sign-in screen's visibility, which decides between the number and the logo alone. It lives apart
/// from AccountsViewModel so a background refresh while the panel is closed updates only this tiny
/// label instead of re-laying the whole invisible panel.
struct MenuBarSnapshot {
  var dashboard: AccountDashboard?
  var signInActive = false
  var signInRepair = false
  var signInHelp = ""
}

@MainActor
final class MenuBarState: ObservableObject {
  @Published var snapshot = MenuBarSnapshot()

  /// The number beside the glyph, or nil for the logo alone (section 9).
  func reading(_ prefs: TrayPreferences) -> MenuBarReading? {
    guard !snapshot.signInActive || snapshot.signInRepair else { return nil }
    return MenuBarReading.make(dashboard: snapshot.dashboard, provider: prefs.menuBarProvider, mode: prefs.menuBarMode,
      claudeAccountID: prefs.menuBarClaudeAccountID)
  }

  /// Signed out or not paired: the logo alone, and the help tag says so (section 9).
  var signedOut: Bool { snapshot.signInActive && !snapshot.signInRepair }
}

/// The glyph plus the chosen account's percentage. The number rolls to its new value with an ease-out,
/// so it never passes the reading.
struct StatusItemLabel: View {
  @ObservedObject var menuBar: MenuBarState
  @ObservedObject var prefs: TrayPreferences

  var body: some View {
    let reading = menuBar.reading(prefs)
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
