import SwiftUI
import AppKit
import CCSBarCore

enum TrayAppearance: String, CaseIterable, Identifiable {
  case light, dark, auto
  var id: String { rawValue }
  var title: String { ["light": "Light", "dark": "Dark", "auto": "Auto"][rawValue]! }
  var symbol: String { ["light": "sun.max", "dark": "moon", "auto": "circle.lefthalf.filled"][rawValue]! }
  var nsAppearance: NSAppearance? {
    switch self {
    case .light: return NSAppearance(named: .aqua)
    case .dark: return NSAppearance(named: .darkAqua)
    case .auto: return nil
    }
  }
}

/// Settings that belong to this Mac only, kept in the app's own defaults: Appearance, what the menu bar
/// shows and the open shortcut. Nothing here is shared with the dashboard or the Windows tray.
@MainActor
final class TrayPreferences: ObservableObject {
  static let shared = TrayPreferences()
  private let defaults: UserDefaults

  @Published var appearance: TrayAppearance {
    didSet { defaults.set(appearance.rawValue, forKey: Keys.appearance); apply() }
  }
  @Published var menuBarSource: MenuBarSource {
    didSet { defaults.set(menuBarSource.rawValue, forKey: Keys.menuBarSource) }
  }
  @Published var menuBarMode: MenuBarMode {
    didSet { defaults.set(menuBarMode.rawValue, forKey: Keys.menuBarMode) }
  }
  /// The global Option-Command-A shortcut that opens the panel. On by default.
  @Published var openShortcutEnabled: Bool {
    didSet { defaults.set(openShortcutEnabled, forKey: Keys.openShortcut) }
  }

  enum Keys {
    static let appearance = "aac.tray.appearance"
    static let menuBarSource = "aac.tray.menuBarSource"
    static let menuBarMode = "aac.tray.menuBarMode"
    static let openShortcut = "aac.tray.openShortcutEnabled"
  }

  init(defaults: UserDefaults = .standard) {
    self.defaults = defaults
    appearance = TrayAppearance(rawValue: defaults.string(forKey: Keys.appearance) ?? "") ?? .auto
    menuBarSource = MenuBarSource(rawValue: defaults.string(forKey: Keys.menuBarSource) ?? "") ?? .codex
    menuBarMode = MenuBarMode(rawValue: defaults.string(forKey: Keys.menuBarMode) ?? "") ?? .left
    openShortcutEnabled = defaults.object(forKey: Keys.openShortcut) as? Bool ?? true
  }

  /// Light, Dark or Auto for every window, popover and menu this app shows; Auto follows macOS.
  func apply() {
    NSApplication.shared.appearance = appearance.nsAppearance
  }
}
