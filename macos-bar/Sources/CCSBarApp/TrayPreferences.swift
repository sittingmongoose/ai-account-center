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
  /// False for the self-test: choices apply in memory and nothing is written to disk.
  private let persist: Bool

  @Published var appearance: TrayAppearance {
    didSet { if persist { defaults.set(appearance.rawValue, forKey: Keys.appearance) }; apply() }
  }
  /// The menu-bar provider: a provider id, or "none" for the icon alone. Stored under the earlier
  /// menuBarSource key, whose "codex"/"antigravity"/"none" values carry over unchanged.
  @Published var menuBarProvider: String {
    didSet { if persist { defaults.set(menuBarProvider, forKey: Keys.menuBarSource) } }
  }
  /// The Claude account the menu bar shows (Claude has no active account). Nil means the first one.
  @Published var menuBarClaudeAccountID: String? {
    didSet {
      if persist {
        if let id = menuBarClaudeAccountID, !id.isEmpty { defaults.set(id, forKey: Keys.menuBarClaudeAccount) }
        else { defaults.removeObject(forKey: Keys.menuBarClaudeAccount) }
      }
    }
  }
  @Published var menuBarMode: MenuBarMode {
    didSet { if persist { defaults.set(menuBarMode.rawValue, forKey: Keys.menuBarMode) } }
  }
  /// The global Option-Command-A shortcut that opens the panel. On by default.
  @Published var openShortcutEnabled: Bool {
    didSet { if persist { defaults.set(openShortcutEnabled, forKey: Keys.openShortcut) } }
  }

  enum Keys {
    static let appearance = "aac.tray.appearance"
    static let menuBarSource = "aac.tray.menuBarSource"
    static let menuBarClaudeAccount = "aac.tray.menuBarClaudeAccount"
    static let menuBarMode = "aac.tray.menuBarMode"
    static let openShortcut = "aac.tray.openShortcutEnabled"
  }

  init(defaults: UserDefaults = .standard, persist: Bool = true) {
    self.defaults = defaults
    self.persist = persist
    appearance = TrayAppearance(rawValue: defaults.string(forKey: Keys.appearance) ?? "") ?? .auto
    let storedProvider = defaults.string(forKey: Keys.menuBarSource) ?? ""
    menuBarProvider = storedProvider.isEmpty ? "codex" : storedProvider
    let storedClaude = defaults.string(forKey: Keys.menuBarClaudeAccount)
    menuBarClaudeAccountID = storedClaude?.isEmpty == false ? storedClaude : nil
    menuBarMode = MenuBarMode(rawValue: defaults.string(forKey: Keys.menuBarMode) ?? "") ?? .remaining
    openShortcutEnabled = defaults.object(forKey: Keys.openShortcut) as? Bool ?? true
  }

  /// Light, Dark or Auto for every window, popover and menu this app shows; Auto follows macOS.
  func apply() {
    NSApplication.shared.appearance = appearance.nsAppearance
  }
}
