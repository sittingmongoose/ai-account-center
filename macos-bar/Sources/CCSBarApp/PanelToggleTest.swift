import SwiftUI
import AppKit
import Carbon.HIToolbox
import CCSBarCore

/// A live, app-owned check of the status-item toggle with real synthetic mouse events. A sanitized
/// fixture drives the panel (no sign-in, no network, no account action, no desktop capture), and
/// clicks posted through the system event stream reach the panel through the same paths a person's
/// clicks take: the menu-bar button action and the global dismiss monitor.
@MainActor
enum PanelToggleTest {
  static func run(input: String) -> Never {
    do {
      let dashboard = try JSONDecoder().decode(AccountDashboard.self, from: Data(contentsOf: URL(fileURLWithPath: input)))
      let app = NSApplication.shared
      app.setActivationPolicy(.accessory)
      // In-memory preferences: defaults only, and nothing is written to disk.
      let suite = "party.sittingmongoose.aac.toggletest"
      let defaults = UserDefaults(suiteName: suite) ?? .standard
      let prefs = TrayPreferences(defaults: defaults, persist: false)
      let controller = PanelController(model: AccountsViewModel(preview: dashboard), prefs: prefs)
      var steps: [[String: Any]] = []
      // A real app loop drains the event queue as well as the run loop; the button action arrives
      // through it, so a run-loop-only pump would leave clicks undelivered.
      func pump(_ seconds: Double) {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
          RunLoop.main.run(mode: .default, before: Date().addingTimeInterval(0.05))
          while let event = app.nextEvent(matching: .any, until: Date(), inMode: .default, dequeue: true) {
            app.sendEvent(event)
          }
        }
      }
      func record(_ name: String, _ passed: Bool, _ detail: [String: Any] = [:]) {
        var entry = detail
        entry["step"] = name
        entry["passed"] = passed
        steps.append(entry)
      }
      func buttonFrame() -> NSRect {
        guard let button = controller.statusItem.button, let window = button.window else { return .zero }
        return window.convertToScreen(button.convert(button.bounds, to: nil))
      }
      /// The button's centre as a Quartz-global point for a posted click.
      func buttonCenterCG() -> CGPoint? {
        let frame = buttonFrame()
        guard !frame.isNull, frame.width > 0 else { return nil }
        return Self.appKitToCG(NSPoint(x: frame.midX, y: frame.midY))
      }
      // Up to two presses per step: one posted click can be lost while the menu bar is busy,
      // and a retry still fails on a real regression (a broken toggle leaves the state unchanged
      // on every press).
      func clickIconUntil(open expectOpen: Bool) -> Bool {
        for _ in 0..<2 {
          guard let point = buttonCenterCG() else { return false }
          Self.click(at: point)
          pump(1.0)
          if (controller.panel?.isVisible == true) == expectOpen { return true }
        }
        return false
      }
      func clickOutsideUntilClosed() -> Bool {
        for _ in 0..<2 {
          if controller.panel?.isVisible == true,
            let point = Self.desktopPointCG(avoiding: controller.panel?.frame) {
            Self.click(at: point)
            pump(1.0)
          }
          if controller.panel?.isVisible == false { return true }
        }
        return false
      }
      pump(0.5)
      record("status item is in the menu bar", buttonFrame().width > 0,
        ["buttonFrame": NSStringFromRect(buttonFrame())])

      record("clicking the menu-bar icon opens the panel",
        clickIconUntil(open: true) && controller.isOpen,
        ["isOpen": controller.isOpen])

      record("clicking it again closes the panel", clickIconUntil(open: false),
        ["isOpen": controller.isOpen])

      record("clicking it a third time opens the panel again",
        clickIconUntil(open: true) && controller.isOpen,
        ["isOpen": controller.isOpen])

      // A click on the desktop, away from the panel, still dismisses it.
      let panelFrame = controller.panel?.frame
      record("clicking outside the panel closes it", clickOutsideUntilClosed(),
        ["panelFrame": panelFrame.map { NSStringFromRect($0) } ?? ""])

      // Escape as a real key event through the application, the way a key press arrives.
      controller.open()
      pump(0.8)
      if let target = controller.panel { PanelSelfTest.escape(to: target, viaApp: true) }
      pump(0.8)
      record("Escape closes the panel", controller.panel?.isVisible == false)

      // The Carbon hot-key event (what Option-Command-A delivers) reaches the handler and toggles.
      PanelSelfTest.sendHotKeyPressed()
      pump(0.8)
      let openedByKey = controller.panel?.isVisible == true
      PanelSelfTest.sendHotKeyPressed()
      pump(0.8)
      record("the Option-Command-A hot-key event toggles the panel open and closed",
        openedByKey && controller.panel?.isVisible == false)
      controller.hotKey?.unregister()
      NSStatusBar.system.removeStatusItem(controller.statusItem)

      let passed = steps.allSatisfy { $0["passed"] as? Bool == true }
      let result: [String: Any] = ["passed": passed, "steps": steps, "isolatedPreviewModel": true,
        "networkOrAccountActions": false, "syntheticMouseClicks": true,
        "desktopCaptureOrExternalAutomation": false]
      print(String(decoding: try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys, .prettyPrinted]), as: UTF8.self))
      exit(passed ? 0 : 1)
    } catch {
      fputs("Panel toggle test failed to start.\n", stderr)
      exit(1)
    }
  }

  /// AppKit-global (origin at the main display's lower left) to Quartz-global (origin at its
  /// upper left) for a posted event.
  private static func appKitToCG(_ point: NSPoint) -> CGPoint {
    CGPoint(x: point.x, y: CGDisplayBounds(CGMainDisplayID()).height - point.y)
  }

  /// A desktop point on the main screen, away from the panel, as a Quartz-global point.
  private static func desktopPointCG(avoiding panelFrame: NSRect?) -> CGPoint? {
    guard let visible = (NSScreen.main ?? NSScreen.screens.first)?.visibleFrame else { return nil }
    let candidates = [
      NSPoint(x: visible.minX + 60, y: visible.minY + 60),
      NSPoint(x: visible.maxX - 60, y: visible.minY + 60),
      NSPoint(x: visible.minX + 60, y: visible.maxY - 60),
      NSPoint(x: visible.maxX - 60, y: visible.maxY - 60),
    ]
    for candidate in candidates {
      if let frame = panelFrame, frame.contains(candidate) { continue }
      return appKitToCG(candidate)
    }
    return nil
  }

  /// A real click through the system event stream: down and up at a Quartz-global point.
  /// Posted by a short-lived helper child process, so the observing app never posts to itself.
  static func click(at point: CGPoint) {
    let task = Process()
    task.executableURL = URL(fileURLWithPath: CommandLine.arguments[0])
    task.arguments = ["--click-at", "\(Int(point.x.rounded()))", "\(Int(point.y.rounded()))"]
    try? task.run()
    task.waitUntilExit()
  }

  /// The helper child: post one down/up click and exit. No app, no windows.
  static func clickOnce(at point: CGPoint) {
    guard let down = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown,
        mouseCursorPosition: point, mouseButton: .left),
      let up = CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp,
        mouseCursorPosition: point, mouseButton: .left)
    else { return }
    down.setIntegerValueField(.mouseEventClickState, value: 1)
    up.setIntegerValueField(.mouseEventClickState, value: 1)
    down.post(tap: .cghidEventTap)
    up.post(tap: .cghidEventTap)
  }

}
