import SwiftUI
import AppKit
import CCSBarCore

/// A live, app-owned check of the real panel window from a sanitized fixture: no sign-in, no network,
/// no account action, no desktop capture. It opens the panel the ways a person can (status item,
/// relaunch, shortcut toggle), toggles Settings with the gear and closes it with Escape, then quits.
@MainActor
enum PanelSelfTest {
  static func run(input: String) -> Never {
    do {
      let dashboard = try JSONDecoder().decode(AccountDashboard.self, from: Data(contentsOf: URL(fileURLWithPath: input)))
      let app = NSApplication.shared
      app.setActivationPolicy(.accessory)
      let suite = "party.sittingmongoose.aac.selftest"
      let defaults = UserDefaults(suiteName: suite) ?? .standard
      defaults.removePersistentDomain(forName: suite)
      let prefs = TrayPreferences(defaults: defaults)
      let delegate = AppDelegate()
      let controller = PanelController(model: AccountsViewModel(preview: dashboard), prefs: prefs)
      delegate.controller = controller
      var steps: [[String: Any]] = []
      func pump(_ seconds: Double) { RunLoop.main.run(until: Date().addingTimeInterval(seconds)) }
      func record(_ name: String, _ passed: Bool, _ detail: [String: Any] = [:]) {
        var entry = detail
        entry["step"] = name
        entry["passed"] = passed
        steps.append(entry)
      }
      pump(0.5)
      record("status item shows the Apex glyph and reading", controller.statusItem.length > 16
        && (controller.statusHosting?.fittingSize.width ?? 0) > 16,
        ["statusItemLength": Double(controller.statusItem.length),
         "reading": controller.model.menuBarReading(prefs)?.text ?? ""])
      record("open shortcut registered (Option-Command-A)", controller.hotKey?.isRegistered == true,
        ["problem": controller.state.shortcutProblem ?? ""])

      controller.open()
      pump(1.0)
      let panel = controller.panel
      let glass = panel?.contentView as? NSGlassEffectView
      record("panel opens as one regular glass", panel?.isVisible == true && glass != nil
        && glass?.style == .regular && glass?.cornerRadius == TrayMetrics.panelRadius && panel?.isOpaque == false,
        ["frame": panel.map { NSStringFromRect($0.frame) } ?? "", "contentView": panel?.contentView.map { String(describing: type(of: $0)) } ?? ""])
      let anchor = controller.statusItem.button?.window?.frame ?? .zero
      record("panel sits under the menu bar at its content height", (panel?.frame.maxY ?? 0) <= anchor.minY + 1
        && abs((panel?.frame.height ?? 0) - min(controller.state.desiredHeight, controller.state.maxHeight)) <= 2,
        ["desiredHeight": Double(controller.state.desiredHeight), "panelHeight": Double(panel?.frame.height ?? 0)])

      controller.state.setSettings(!controller.state.settingsOpen)
      pump(0.6)
      record("gear opens Settings in the panel", controller.state.settingsOpen && panel?.isVisible == true)
      controller.state.setSettings(!controller.state.settingsOpen)
      pump(0.6)
      record("gear closes Settings again", !controller.state.settingsOpen && panel?.isVisible == true)
      // Escape as a real key event through the panel's responder chain (SwiftUI first, then the panel).
      func escape() {
        guard let panel, let event = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [],
          timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: panel.windowNumber, context: nil,
          characters: "\u{1b}", charactersIgnoringModifiers: "\u{1b}", isARepeat: false, keyCode: 53) else { return }
        panel.sendEvent(event)
      }
      controller.state.setSettings(true)
      pump(0.5)
      record("Settings window is key for Escape", panel?.isKeyWindow == true)
      escape()
      pump(0.6)
      record("Escape closes Settings first", !controller.state.settingsOpen && panel?.isVisible == true)
      escape()
      pump(0.6)
      record("Escape then closes the panel", panel?.isVisible == false)

      _ = delegate.applicationShouldHandleReopen(app, hasVisibleWindows: false)
      pump(0.8)
      record("launching the app again opens the panel", panel?.isVisible == true)
      controller.toggle()
      pump(0.6)
      record("the shortcut action toggles it closed", panel?.isVisible == false)
      controller.toggle()
      pump(0.8)
      record("and open again", panel?.isVisible == true)
      // Appearance Light / Dark / Auto applies to the panel; the menu-bar item keeps the menu bar's own look.
      let menuBarBefore = controller.statusItem.button?.effectiveAppearance.name.rawValue ?? ""
      var appearances: [String: String] = [:]
      for choice in [TrayAppearance.dark, .light, .auto] {
        prefs.appearance = choice
        pump(0.3)
        appearances[choice.rawValue] = panel?.effectiveAppearance.name.rawValue ?? ""
      }
      let menuBarAfter = controller.statusItem.button?.effectiveAppearance.name.rawValue ?? ""
      record("Appearance switches the panel", appearances["dark"]?.contains("Dark") == true
        && appearances["light"]?.contains("Dark") == false, ["panelAppearances": appearances])
      record("menu-bar item keeps the menu bar's appearance", menuBarBefore == menuBarAfter,
        ["before": menuBarBefore, "after": menuBarAfter])
      controller.close()
      pump(0.4)

      defaults.removePersistentDomain(forName: suite)
      let passed = steps.allSatisfy { $0["passed"] as? Bool == true }
      let result: [String: Any] = ["passed": passed, "steps": steps, "isolatedPreviewModel": true,
        "networkOrAccountActions": false, "desktopCaptureOrExternalAutomation": false]
      print(String(decoding: try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys, .prettyPrinted]), as: UTF8.self))
      exit(passed ? 0 : 1)
    } catch {
      fputs("Panel self-test failed to start.\n", stderr)
      exit(1)
    }
  }
}
