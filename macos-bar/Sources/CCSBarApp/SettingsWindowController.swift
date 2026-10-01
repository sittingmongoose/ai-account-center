import AppKit
import SwiftUI

/// The menu bar can open connection settings even when a SwiftUI window action
/// is unavailable or the menu panel is losing focus.
@MainActor
final class SettingsWindowController: NSObject {
  static let shared = SettingsWindowController()
  private(set) var window: NSWindow?
  private weak var representedModel: AccountsViewModel?

  private override init() {
    super.init()
  }

  /// Only ConnectionSettingsView's typed registration view calls this method.
  /// Keeping the Scene's window controller intact preserves SwiftUI dismissal.
  func register(window: NSWindow, model: AccountsViewModel) {
    if self.window !== window {
      self.window?.close()
      self.window = window
    }
    window.isReleasedWhenClosed = false
    representedModel = model
  }

  func show(model: AccountsViewModel) {
    if window == nil || representedModel !== model {
      window?.close()
      let host = NSHostingController(rootView: ConnectionSettingsView(model: model, onClose: { [weak self] in
        self?.close()
      }))
      let settingsWindow = NSWindow(contentViewController: host)
      settingsWindow.title = "AI Account Center settings"
      settingsWindow.identifier = NSUserInterfaceItemIdentifier("ai-account-center-connection-settings")
      settingsWindow.styleMask = [.titled, .closable]
      settingsWindow.isReleasedWhenClosed = false
      settingsWindow.setContentSize(NSSize(width: 430, height: 330))
      settingsWindow.center()
      window = settingsWindow
      representedModel = model
    }

    // MenuBarExtra finishes processing the originating click before this window
    // becomes key, preventing its panel dismissal from swallowing the action.
    DispatchQueue.main.async { [weak self] in
      guard let self, let window = self.window else { return }
      NSApplication.shared.activate(ignoringOtherApps: true)
      window.makeKeyAndOrderFront(nil)
    }
  }

  func close() {
    window?.close()
  }
}
