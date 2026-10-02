import SwiftUI
import AppKit
import Carbon.HIToolbox
import CCSBarCore

/// A live, app-owned check of the real panel window from a sanitized fixture: no sign-in, no network,
/// no account action, no desktop capture. It opens the panel the ways a person can (status item,
/// relaunch, shortcut toggle), toggles Settings with the gear and closes it with Escape, opens Details,
/// measures the process while idle, then checks Escape from a focused text field on the connect screen.
@MainActor
enum PanelSelfTest {
  static func run(input: String) -> Never {
    do {
      let dashboard = try JSONDecoder().decode(AccountDashboard.self, from: Data(contentsOf: URL(fileURLWithPath: input)))
      let app = NSApplication.shared
      app.setActivationPolicy(.accessory)
      // In-memory preferences: defaults only, and nothing is written to disk.
      let suite = "party.sittingmongoose.aac.selftest"
      let defaults = UserDefaults(suiteName: suite) ?? .standard
      let prefs = TrayPreferences(defaults: defaults, persist: false)
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
        if let panel { Self.escape(to: panel, viaApp: false) }
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

      // A reopen during the close fade must win: the fade's completion may not order the panel out.
      controller.close()
      controller.open()
      pump(0.6)
      record("reopening during the close fade keeps the panel open", panel?.isVisible == true
        && controller.isOpen && (panel?.alphaValue ?? 0) > 0.99, ["alpha": Double(panel?.alphaValue ?? 0)])
      pump(0.8)

      // Full-row Details from the nonactivating panel, then Escape: Details closes first, the panel stays.
      let before = Set(app.windows.map { ObjectIdentifier($0) })
      let row = panel?.contentView.flatMap { firstView(in: $0, of: DetailsRowButton.self) }
      row?.performClick(nil)
      pump(0.8)
      let popovers = app.windows.filter { $0.isVisible && !before.contains(ObjectIdentifier($0)) && $0 !== panel }
      record("a full-row click opens Details from the nonactivating panel", row != nil && !popovers.isEmpty,
        ["row": row?.identifier?.rawValue ?? "", "newWindows": popovers.map { String(describing: type(of: $0)) },
         "keyWindowIsPopover": app.keyWindow.map { win in popovers.contains { $0 === win } } ?? false])
      if let target = app.keyWindow ?? panel { Self.escape(to: target, viaApp: true) }
      pump(0.6)
      record("Escape closes Details first and keeps the panel open",
        popovers.allSatisfy { !$0.isVisible } && panel?.isVisible == true,
        ["popoversStillVisible": popovers.filter(\.isVisible).count])
      if popovers.contains(where: \.isVisible) { popovers.forEach { $0.orderOut(nil) } }

      // Idle cost: nothing may loop while the panel sits open or closed.
      controller.open()
      pump(2.5)
      let openIdle = idleCPU(seconds: 5)
      record("idle with the panel open stays under 2% CPU", openIdle < 2,
        ["cpuPercent": openIdle, "footprintMB": footprintMB()])
      controller.close()
      pump(1.0)
      let closedIdle = idleCPU(seconds: 5)
      record("idle with the panel closed stays under 0.5% CPU", closedIdle < 0.5, ["cpuPercent": closedIdle])
      // Each cycle runs in its own autorelease pool, as each event does inside the app's run loop.
      func cycles(_ count: Int) {
        for _ in 0..<count {
          autoreleasepool { controller.open(); pump(0.4) }
          autoreleasepool { controller.close(); pump(0.3) }
        }
        autoreleasepool { pump(1.0) }
      }
      cycles(4)
      let footprints = [footprintMB()] + (0..<3).map { _ in cycles(8); return footprintMB() }
      let growth = (footprints.last ?? 0) - (footprints.first ?? 0)
      record("24 more open-close cycles hold memory steady (under 10 MB growth)", growth < 10,
        ["footprintMBEvery8Cycles": footprints, "growthMB": (growth * 10).rounded() / 10])

      // The Carbon hot-key event (what Option-Command-A delivers) reaches the handler and toggles the panel.
      sendHotKeyPressed()
      pump(0.8)
      let openedByKey = panel?.isVisible == true
      sendHotKeyPressed()
      pump(0.8)
      record("the Option-Command-A hot-key event toggles the panel open and closed",
        openedByKey && panel?.isVisible == false)
      controller.hotKey?.unregister()
      NSStatusBar.system.removeStatusItem(controller.statusItem)

      // First run: Escape from a focused text field still closes the panel (the field editor would keep it).
      let connectPrefs = TrayPreferences(defaults: defaults, persist: false)
      connectPrefs.openShortcutEnabled = false
      let connect = PanelController(model: AccountsViewModel(previewWithoutConnection: true), prefs: connectPrefs)
      connect.open()
      pump(1.0)
      let field = connect.panel?.contentView.flatMap { firstView(in: $0, of: NSTextField.self) { $0.isEditable } }
      if let field { connect.panel?.makeFirstResponder(field) }
      pump(0.3)
      let focused = connect.panel?.firstResponder is NSTextView
      if let target = connect.panel { Self.escape(to: target, viaApp: true) }
      pump(0.6)
      record("Escape in a focused text field closes the panel", field != nil && focused && connect.panel?.isVisible == false,
        ["fieldFound": field != nil, "fieldFocused": focused])
      connect.open()
      pump(1.0)
      if let field = connect.panel?.contentView.flatMap({ firstView(in: $0, of: NSTextField.self) { $0.isEditable } }) {
        connect.panel?.makeFirstResponder(field)
      }
      pump(0.3)
      connect.state.setSettings(true)
      pump(0.6)
      let cleared = !(connect.panel?.firstResponder is NSTextView)
      if let target = connect.panel { Self.escape(to: target, viaApp: true) }
      pump(0.6)
      record("Settings takes focus from the covered field, and Escape then closes Settings only",
        cleared && !connect.state.settingsOpen && connect.panel?.isVisible == true)
      connect.close()
      pump(0.4)
      NSStatusBar.system.removeStatusItem(connect.statusItem)
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

  /// Escape as a real key event: straight to the window's responder chain, or through the application
  /// (its local event monitors first), the way a key press arrives.
  static func escape(to window: NSWindow, viaApp: Bool) {
    guard let event = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [],
      timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil,
      characters: "\u{1b}", charactersIgnoringModifiers: "\u{1b}", isARepeat: false, keyCode: 53) else { return }
    if viaApp { NSApplication.shared.sendEvent(event) } else { window.sendEvent(event) }
  }

  private static func firstView<T: NSView>(in root: NSView, of type: T.Type, where match: (T) -> Bool = { _ in true }) -> T? {
    if let view = root as? T, !view.isHiddenOrHasHiddenAncestor, match(view) { return view }
    for child in root.subviews { if let found = firstView(in: child, of: type, where: match) { return found } }
    return nil
  }

  /// The event Carbon delivers for a registered hot key, sent to the application target.
  static func sendHotKeyPressed() {
    var event: EventRef?
    guard CreateEvent(nil, OSType(kEventClassKeyboard), UInt32(kEventHotKeyPressed), 0,
      EventAttributes(kEventAttributeNone), &event) == noErr, let event else { return }
    var id = EventHotKeyID(signature: GlobalHotKey.signature, id: 1)
    SetEventParameter(event, EventParamName(kEventParamDirectObject), EventParamType(typeEventHotKeyID),
      MemoryLayout<EventHotKeyID>.size, &id)
    SendEventToEventTarget(event, GetApplicationEventTarget())
    ReleaseEvent(event)
  }

  private static func cpuSeconds() -> Double {
    var usage = rusage()
    getrusage(RUSAGE_SELF, &usage)
    func seconds(_ time: timeval) -> Double { Double(time.tv_sec) + Double(time.tv_usec) / 1_000_000 }
    return seconds(usage.ru_utime) + seconds(usage.ru_stime)
  }

  /// CPU percent of one core used by this process while the run loop idles for `seconds`.
  private static func idleCPU(seconds: Double) -> Double {
    let start = cpuSeconds(), clock = Date()
    RunLoop.main.run(until: Date().addingTimeInterval(seconds))
    let elapsed = Date().timeIntervalSince(clock)
    return ((cpuSeconds() - start) / max(0.001, elapsed) * 1000).rounded() / 10
  }

  /// The process's physical footprint (what Activity Monitor calls Memory), in MB.
  private static func footprintMB() -> Double {
    var info = task_vm_info_data_t()
    var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size)
    let result = withUnsafeMutablePointer(to: &info) { pointer in
      pointer.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
        task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
      }
    }
    guard result == KERN_SUCCESS else { return -1 }
    return (Double(info.phys_footprint) / 1_048_576 * 10).rounded() / 10
  }
}
