import SwiftUI
import AppKit
import Vision
import CCSBarCore

/// Renders an isolated fixture for layout review; no desktop capture or UI control.
@MainActor
enum PreviewRenderer {
  static func checkNativeTooltips(input: String) -> Never {
    do {
      let dashboard = try JSONDecoder().decode(AccountDashboard.self, from: Data(contentsOf: URL(fileURLWithPath: input)))
      NSApplication.shared.setActivationPolicy(.prohibited)
      let host = NSHostingView(rootView: AccountsMenuView(model: AccountsViewModel(preview: dashboard)))
      let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: AccountsLayout.panelWidth, height: AccountsLayout.maximumContentHeight + 130), styleMask: .borderless, backing: .buffered, defer: false)
      window.contentView = host
      host.frame = window.contentView!.bounds
      host.layoutSubtreeIfNeeded()
      var buttons: [[String: Any]] = []
      var nativeButtons: [NSButton] = []
      var exclusions: [NSView] = []
      func visit(_ view: NSView) {
        if view is RowActionExclusionView { exclusions.append(view) }
        if let button = view as? NSButton, let id = button.identifier?.rawValue, !id.isEmpty {
          nativeButtons.append(button)
          buttons.append(["id": id, "tooltip": (button as? HoverHelpButton)?.helpText ?? button.toolTip ?? "", "accessibilityLabel": button.accessibilityLabel() ?? "", "title": button.title, "hasImage": button.image != nil, "width": button.bounds.width, "height": button.bounds.height])
        }
        view.subviews.forEach(visit)
      }
      visit(host)
      let packButton = dashboard.accounts.contains { account in
        account.provider == "qwen" && account.windows.contains { $0.key.hasPrefix("addon-pack-") }
      } ? 1 : 0
      let claudeOpenButtons = dashboard.accounts.filter { $0.provider == "claude" && $0.capabilities.claudeProfileId != nil }
        .reduce(0) { $0 + $1.capabilities.claudePlatforms.filter { ["mac", "windows"].contains($0) }.count }
      let expected = dashboard.accounts.filter { $0.provider == "claude" || $0.provider == "codex" }.count + dashboard.providerGroups.filter { $0.id != "claude" && $0.id != "codex" }.count + 5 + packButton + claudeOpenButtons
      let rows = nativeButtons.filter { ($0.identifier?.rawValue ?? "").hasPrefix("account-row-") || ($0.identifier?.rawValue ?? "").hasPrefix("provider-row-") }
      let rowGeometryPassed = rows.allSatisfy { $0.bounds.width >= AccountsLayout.panelWidth * 0.8 && $0.bounds.height >= 35 }
      let coreRows = rows.filter { ($0.identifier?.rawValue ?? "").hasPrefix("account-row-") }
      var hitTests: [[String: Any]] = []
      func hit(_ point: NSPoint, label: String) -> NSView? {
        let target = host.hitTest(host.convert(point, to: host.superview))
        hitTests.append(["target": label, "hitClass": target.map { String(describing: type(of: $0)) } ?? "nil", "hitId": target?.identifier?.rawValue ?? "", "x": point.x, "y": point.y])
        return target
      }
      let fullRowHitTestsPassed = coreRows.allSatisfy { row in
        [CGFloat(0.03), CGFloat(0.60)].allSatisfy { fraction in
          let point = row.convert(NSPoint(x: row.bounds.width * fraction, y: row.bounds.midY), to: host)
          return hit(point, label: row.identifier?.rawValue ?? "row") === row
        }
      }
      let nestedNativeControls = nativeButtons.filter { ($0.identifier?.rawValue ?? "").hasPrefix("claude-") || $0.identifier?.rawValue == "qwen-packs" }
      let nestedNativeControlsPassed = nestedNativeControls.allSatisfy { control in
        let point = control.convert(NSPoint(x: control.bounds.midX, y: control.bounds.midY), to: host)
        return hit(point, label: control.identifier?.rawValue ?? "control") === control
      }
      let activateExclusionPassed = exclusions.allSatisfy { region in
        let point = region.convert(NSPoint(x: region.bounds.midX, y: region.bounds.midY), to: host)
        let target = hit(point, label: "activate-exclusion")
        return target != nil && !rows.contains { target === $0 }
      }
      let passed = buttons.count == expected && buttons.allSatisfy { !(($0["tooltip"] as? String) ?? "").isEmpty && ($0["tooltip"] as? String) == ($0["accessibilityLabel"] as? String) } && rowGeometryPassed && fullRowHitTestsPassed && nestedNativeControlsPassed && activateExclusionPassed
      let result: [String: Any] = ["passed": passed, "buttonCount": buttons.count, "expectedCount": expected, "buttons": buttons, "hitTests": hitTests, "fullRowGeometryPassed": rowGeometryPassed, "fullCoreRowHitTestsPassed": fullRowHitTestsPassed, "nestedNativeControlHitTestsPassed": nestedNativeControlsPassed, "activateExclusionHitTestsPassed": activateExclusionPassed, "hiddenAppOwnedInspection": true, "desktopCaptureOrAutomation": false, "accountActionsInvoked": false]
      print(String(decoding: try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]), as: UTF8.self))
      exit(passed ? 0 : 1)
    } catch { fputs("Native tooltip inspection failed.\n", stderr); exit(1) }
  }

  /// Exercises only this process's fixture button and native popover. No client,
  /// login, account action, external app control, or desktop capture is involved.
  static func checkNativePacks(input: String, output: String) -> Never {
    do {
      let dashboard = try JSONDecoder().decode(AccountDashboard.self, from: Data(contentsOf: URL(fileURLWithPath: input)))
      let packs = dashboard.accounts.filter { $0.provider == "qwen" }
        .flatMap { $0.windows.filter { $0.key.hasPrefix("addon-pack-") } }
      guard !packs.isEmpty else { throw BarClientError.decoding }
      NSApplication.shared.setActivationPolicy(.accessory)
      let host = NSHostingView(rootView: AccountsMenuView(model: AccountsViewModel(preview: dashboard)))
      let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: AccountsLayout.panelWidth, height: AccountsLayout.maximumContentHeight + 115), styleMask: [.titled], backing: .buffered, defer: false)
      window.title = "AI Account Center — isolated pack interaction check"
      window.appearance = NSAppearance(named: .darkAqua)
      window.contentView = host
      host.frame = window.contentView!.bounds
      window.center()
      window.orderFront(nil)
      RunLoop.main.run(until: Date().addingTimeInterval(0.35))
      host.layoutSubtreeIfNeeded()
      var packButton: NSButton?
      func find(_ view: NSView) {
        if let button = view as? NSButton, button.identifier?.rawValue == "qwen-packs" { packButton = button }
        view.subviews.forEach(find)
      }
      find(host)
      guard let button = packButton, button.isEnabled else { throw BarClientError.decoding }
      let before = Set(NSApplication.shared.windows.map { ObjectIdentifier($0) })
      button.performClick(nil)
      RunLoop.main.run(until: Date().addingTimeInterval(0.6))
      let presented = NSApplication.shared.windows.filter {
        $0.isVisible && !before.contains(ObjectIdentifier($0))
      }
      var labels: [String] = []
      var visited: Set<ObjectIdentifier> = []
      func inspect(_ object: NSObject) {
        guard visited.insert(ObjectIdentifier(object)).inserted else { return }
        let label = NSSelectorFromString("accessibilityLabel")
        if object.responds(to: label), let value = object.perform(label)?.takeUnretainedValue() as? String, !value.isEmpty {
          labels.append(value)
        }
        let children = NSSelectorFromString("accessibilityChildren")
        if object.responds(to: children), let values = object.perform(children)?.takeUnretainedValue() as? [NSObject] {
          values.forEach(inspect)
        }
        if let view = object as? NSView { view.subviews.forEach(inspect) }
      }
      for popup in presented {
        popup.contentView?.layoutSubtreeIfNeeded()
        if let content = popup.contentView { inspect(content) }
      }
      guard let popup = presented.first, let content = popup.contentView,
        let bitmap = content.bitmapImageRepForCachingDisplay(in: content.bounds)
      else { throw BarClientError.decoding }
      content.cacheDisplay(in: content.bounds, to: bitmap)
      guard let png = bitmap.representation(using: .png, properties: [:]) else { throw BarClientError.decoding }
      try png.write(to: URL(fileURLWithPath: output), options: .atomic)
      // Hosted SwiftUI text is not always exposed by the in-process AppKit AX
      // bridge. Read the actually rendered, app-owned popup pixels as well.
      var renderedText: [String] = []
      if let cgImage = bitmap.cgImage {
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.recognitionLanguages = ["en-US"]
        request.usesLanguageCorrection = false
        try VNImageRequestHandler(cgImage: cgImage).perform([request])
        renderedText = request.results?.compactMap { $0.topCandidates(1).first?.string } ?? []
      }
      let text = (labels + renderedText).joined(separator: "\n")
      let matched = packs.filter { text.contains($0.label) }.count
      let passed = matched == packs.count
      let result: [String: Any] = ["passed": passed, "action": "native NSButton.performClick on qwen-packs", "expectedActualPackCount": packs.count, "renderedMatchedPackCount": matched, "nativePopoverWindowCount": presented.count, "visibleAccessibilityLabels": labels, "renderedPopupTextOCR": renderedText, "isolatedPreviewModel": true, "networkOrAccountActions": false, "desktopCaptureOrExternalAutomation": false]
      print(String(decoding: try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]), as: UTF8.self))
      presented.forEach { $0.orderOut(nil) }
      window.orderOut(nil)
      exit(passed ? 0 : 1)
    } catch { fputs("Native pack interaction inspection failed.\n", stderr); exit(1) }
  }

  static func render(input: String, output: String) -> Never {
    do {
      let dashboard = try JSONDecoder().decode(AccountDashboard.self, from: Data(contentsOf: URL(fileURLWithPath: input)))
      let model = AccountsViewModel(preview: dashboard)
      NSApplication.shared.setActivationPolicy(.prohibited)
      let view = NSHostingView(rootView: AccountsMenuView(model: model))
      let size = NSSize(width: AccountsLayout.panelWidth, height: AccountsLayout.maximumContentHeight + 130)
      let window = NSWindow(contentRect: NSRect(origin: .zero, size: size), styleMask: .borderless,
        backing: .buffered, defer: false)
      window.appearance = NSAppearance(named: .darkAqua)
      window.contentView = view
      view.frame = NSRect(origin: .zero, size: size)
      view.layoutSubtreeIfNeeded()
      guard let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { throw BarClientError.decoding }
      view.cacheDisplay(in: view.bounds, to: bitmap)
      guard let png = bitmap.representation(using: .png, properties: [:]) else { throw BarClientError.decoding }
      try png.write(to: URL(fileURLWithPath: output), options: .atomic)
      print("Rendered isolated accounts preview.")
      exit(0)
    } catch {
      fputs("Unable to render the accounts preview.\n", stderr)
      exit(1)
    }
  }
}
