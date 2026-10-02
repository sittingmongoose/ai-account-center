import SwiftUI
import AppKit
import Vision
import CoreImage
import CCSBarCore

/// Offline renders and interaction checks of the panel, from a sanitized fixture. Nothing here signs in,
/// makes a request, captures the desktop or controls another app.
@MainActor
enum PreviewRenderer {
  struct Options {
    var appearance = "light"
    var settings = false
    var width: CGFloat = 760
    var wallpaper = true
    var details: String?
    var connect = false

    init(_ arguments: [String]) {
      for argument in arguments {
        if argument == "--dark" { appearance = "dark" }
        if argument == "--light" { appearance = "light" }
        if argument == "--settings" { settings = true }
        if argument == "--plain" { wallpaper = false }
        if argument.hasPrefix("--width="), let value = Double(argument.dropFirst(8)) { width = CGFloat(value) }
        if argument.hasPrefix("--details=") { details = String(argument.dropFirst(10)) }
        if argument == "--connect" { connect = true }
      }
    }
  }

  private static func loadFixture(_ path: String) throws -> AccountDashboard {
    try JSONDecoder().decode(AccountDashboard.self, from: Data(contentsOf: URL(fileURLWithPath: path)))
  }

  /// A hosting view of the real panel content, laid out at its own height.
  private static func host(_ dashboard: AccountDashboard, options: Options) -> (NSHostingView<PanelRootView>, PanelState, NSWindow) {
    NSApplication.shared.setActivationPolicy(.prohibited)
    let appearance = NSAppearance(named: options.appearance == "dark" ? .darkAqua : .aqua)
    NSApplication.shared.appearance = appearance
    TrayFormat.referenceNow = AccountFormatting.date(dashboard.updatedAt)
    let model = AccountsViewModel(preview: dashboard)
    let prefs = TrayPreferences(defaults: UserDefaults(suiteName: "party.sittingmongoose.aac.preview") ?? .standard)
    let state = PanelState()
    state.staticRender = true
    state.panelWidth = options.width
    state.maxHeight = 4000
    var context = OpenContext()
    context.animate = false
    state.open = context
    state.settingsOpen = options.settings
    let host = NSHostingView(rootView: PanelRootView(model: model, prefs: prefs, state: state))
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: options.width, height: 900), styleMask: .borderless,
      backing: .buffered, defer: false)
    window.appearance = appearance
    window.isOpaque = false
    window.backgroundColor = .clear
    window.contentView = host
    host.frame = NSRect(x: 0, y: 0, width: options.width, height: 900)
    // SwiftUI scroll content draws only in an ordered-in window; park it far off every screen.
    window.setFrameOrigin(NSPoint(x: -30_000, y: -30_000))
    window.orderFrontRegardless()
    settle(host)
    // Size the window to the content's own height, as the panel does.
    let height = max(200, state.desiredHeight)
    window.setContentSize(NSSize(width: options.width, height: height))
    host.frame = NSRect(x: 0, y: 0, width: options.width, height: height)
    settle(host)
    return (host, state, window)
  }

  /// The Details popover's content for one account (or a provider's accounts), as the popover shows it.
  private static func detailsHost(_ dashboard: AccountDashboard, accountID: String, options: Options) throws -> NSView {
    NSApplication.shared.setActivationPolicy(.prohibited)
    let appearance = NSAppearance(named: options.appearance == "dark" ? .darkAqua : .aqua)
    NSApplication.shared.appearance = appearance
    TrayFormat.referenceNow = AccountFormatting.date(dashboard.updatedAt)
    let accounts = dashboard.accounts.filter { $0.id == accountID || $0.provider == accountID }
    guard !accounts.isEmpty else { throw BarClientError.decoding }
    let model = AccountsViewModel(preview: dashboard)
    let root = AccountDetailsPopover(model: model, accounts: accounts, maxHeight: 4000)
      .background(PreviewGlass())
      .environment(\.trayStaticRender, true)
      .modifier(PreviewActiveControls(enabled: true))
    let host = NSHostingView(rootView: root)
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 470, height: 600), styleMask: .borderless,
      backing: .buffered, defer: false)
    window.appearance = appearance
    window.contentView = host
    window.setFrameOrigin(NSPoint(x: -30_000, y: -30_000))
    window.orderFrontRegardless()
    settle(host)
    let size = host.fittingSize
    window.setContentSize(size)
    host.frame = NSRect(origin: .zero, size: size)
    settle(host)
    return host
  }

  private static func connectHost(options: Options) throws -> NSView {
    NSApplication.shared.setActivationPolicy(.prohibited)
    let appearance = NSAppearance(named: options.appearance == "dark" ? .darkAqua : .aqua)
    NSApplication.shared.appearance = appearance
    let root = ConnectView(model: AccountsViewModel(previewWithoutConnection: true)).frame(width: options.width)
      .background(PreviewGlass())
      .environment(\.trayStaticRender, true)
    let host = NSHostingView(rootView: root)
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: options.width, height: 600), styleMask: .borderless,
      backing: .buffered, defer: false)
    window.appearance = appearance
    window.contentView = host
    window.setFrameOrigin(NSPoint(x: -30_000, y: -30_000))
    window.orderFrontRegardless()
    settle(host)
    let size = host.fittingSize
    window.setContentSize(size)
    host.frame = NSRect(origin: .zero, size: size)
    settle(host)
    return host
  }

  private static func settle(_ view: NSView) {
    view.window?.makeFirstResponder(nil)
    for _ in 0..<4 {
      view.layoutSubtreeIfNeeded()
      RunLoop.main.run(until: Date().addingTimeInterval(0.08))
    }
  }

  // MARK: Render

  static func render(input: String, output: String, options arguments: [String]) -> Never {
    do {
      let options = Options(arguments)
      let dashboard = try loadFixture(input)
      let host: NSView
      if let id = options.details {
        host = try detailsHost(dashboard, accountID: id, options: options)
      } else if options.connect {
        // The first-run connect screen alone: inside the full panel its text fields move the content into
        // window-server layers that an offscreen render cannot read.
        host = try connectHost(options: options)
      } else {
        host = Self.host(dashboard, options: options).0
      }
      let size = host.bounds.size
      if ProcessInfo.processInfo.environment["AAC_PREVIEW_DEBUG"] == "1" {
        func dump(_ view: NSView, _ depth: Int) {
          print(String(repeating: "  ", count: depth) + "\(type(of: view)) \(view.frame) hidden=\(view.isHidden) alpha=\(view.alphaValue) layer=\(view.layer.map { "\(type(of: $0)) op=\($0.opacity) sub=\($0.sublayers?.count ?? 0)" } ?? "-")")
          if depth < 6 { view.subviews.forEach { dump($0, depth + 1) } }
        }
        dump(host, 0)
      }
      let content = try snapshot(host)
      let scale = CGFloat(content.pixelsWide) / max(1, size.width)
      let margin: CGFloat = 28
      let canvasSize = NSSize(width: size.width + margin * 2, height: size.height + margin * 2)
      let image = NSImage(size: canvasSize)
      image.lockFocus()
      let dark = options.appearance == "dark"
      drawBackdrop(canvasSize, dark: dark, wallpaper: options.wallpaper)
      // The panel's glass, baked as the concept does: the wallpaper diffused under the medium tint.
      let panelRect = NSRect(x: margin, y: margin, width: size.width, height: size.height)
      let path = NSBezierPath(roundedRect: panelRect, xRadius: TrayMetrics.panelRadius, yRadius: TrayMetrics.panelRadius)
      NSGraphicsContext.saveGraphicsState()
      let shadow = NSShadow()
      shadow.shadowColor = NSColor.black.withAlphaComponent(dark ? 0.5 : 0.22)
      shadow.shadowBlurRadius = 30
      shadow.shadowOffset = NSSize(width: 0, height: -14)
      shadow.set()
      (dark ? NSColor(srgbRed: 28 / 255, green: 28 / 255, blue: 32 / 255, alpha: 0.86)
        : NSColor(srgbRed: 250 / 255, green: 250 / 255, blue: 252 / 255, alpha: 0.88)).setFill()
      path.fill()
      NSGraphicsContext.restoreGraphicsState()
      NSColor.black.withAlphaComponent(dark ? 0.55 : 0.14).setStroke()
      path.lineWidth = 0.5
      path.stroke()
      NSGraphicsContext.saveGraphicsState()
      path.addClip()
      content.draw(in: panelRect)
      NSGraphicsContext.restoreGraphicsState()
      image.unlockFocus()
      guard let tiff = image.tiffRepresentation, let bitmap = NSBitmapImageRep(data: tiff) else { throw BarClientError.decoding }
      let pixels = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(canvasSize.width * scale), pixelsHigh: Int(canvasSize.height * scale),
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
      pixels.size = canvasSize
      NSGraphicsContext.saveGraphicsState()
      NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: pixels)
      bitmap.draw(in: NSRect(origin: .zero, size: canvasSize))
      NSGraphicsContext.restoreGraphicsState()
      guard let png = pixels.representation(using: .png, properties: [:]) else { throw BarClientError.decoding }
      try png.write(to: URL(fileURLWithPath: output), options: .atomic)
      print("Rendered isolated accounts preview (\(options.appearance)\(options.settings ? ", settings" : ""), \(Int(size.width))x\(Int(size.height)) pt).")
      exit(0)
    } catch {
      fputs("Unable to render the accounts preview.\n", stderr)
      exit(1)
    }
  }

  /// Draws the hosting view the way AppKit caches it. Scroll views are laid out flat in offline renders
  /// (PanelScroll), because their content is composited by the window server. System glass is composited
  /// there too, so the render shows the panel's content over a baked approximation of the glass.
  private static func snapshot(_ view: NSView) throws -> NSBitmapImageRep {
    guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { throw BarClientError.decoding }
    view.cacheDisplay(in: view.bounds, to: rep)
    return rep
  }

  /// A colourful macOS-style wallpaper, diffused like regular glass would see it.
  private static func drawBackdrop(_ size: NSSize, dark: Bool, wallpaper: Bool) {
    let rect = NSRect(origin: .zero, size: size)
    (dark ? NSColor(srgbRed: 0.10, green: 0.07, blue: 0.20, alpha: 1) : NSColor(srgbRed: 0.98, green: 0.86, blue: 0.80, alpha: 1)).setFill()
    rect.fill()
    guard wallpaper else { return }
    let blobs: [(CGFloat, CGFloat, CGFloat, NSColor)] = dark
      ? [(0.15, 0.85, 0.55, NSColor(srgbRed: 0.42, green: 0.16, blue: 0.62, alpha: 1)),
         (0.85, 0.65, 0.5, NSColor(srgbRed: 0.75, green: 0.22, blue: 0.45, alpha: 1)),
         (0.6, 0.15, 0.55, NSColor(srgbRed: 0.85, green: 0.40, blue: 0.18, alpha: 1)),
         (0.1, 0.2, 0.45, NSColor(srgbRed: 0.18, green: 0.25, blue: 0.70, alpha: 1))]
      : [(0.15, 0.85, 0.55, NSColor(srgbRed: 1.0, green: 0.78, blue: 0.55, alpha: 1)),
         (0.85, 0.7, 0.5, NSColor(srgbRed: 0.98, green: 0.55, blue: 0.48, alpha: 1)),
         (0.6, 0.15, 0.6, NSColor(srgbRed: 0.72, green: 0.52, blue: 0.95, alpha: 1)),
         (0.1, 0.2, 0.45, NSColor(srgbRed: 0.50, green: 0.70, blue: 1.0, alpha: 1))]
    for (x, y, r, color) in blobs {
      let center = NSPoint(x: size.width * x, y: size.height * y)
      let radius = max(size.width, size.height) * r
      NSGradient(colors: [color, color.withAlphaComponent(0)])?.draw(fromCenter: center, radius: 0, toCenter: center, radius: radius, options: [])
    }
  }

  // MARK: Interaction checks

  private static func collect(_ root: NSView) -> (help: [HoverHelpView], rows: [DetailsRowButton]) {
    var help: [HoverHelpView] = []
    var rows: [DetailsRowButton] = []
    func visit(_ view: NSView) {
      if let item = view as? HoverHelpView { help.append(item) }
      if let row = view as? DetailsRowButton { rows.append(row) }
      view.subviews.forEach(visit)
    }
    visit(root)
    return (help, rows)
  }

  /// Every icon control has a help tag equal to its accessibility label; every account and provider row is
  /// one full-width Details target; nested controls (Open, Activate, packs, footer) are never the row.
  static func checkNativeTooltips(input: String) -> Never {
    do {
      let dashboard = try loadFixture(input)
      let (host, _, _) = host(dashboard, options: Options([]))
      let (help, rows) = collect(host)
      let visible = dashboard.visibleAccounts
      let sectionAccounts = visible.filter { ["claude", "codex", "antigravity"].contains($0.provider) }
      let otherGroups = dashboard.providerGroups.filter { !["claude", "codex", "antigravity"].contains($0.id) }
      var expectedHelp = Set(["header-menu", "footer-dashboard", "footer-refresh", "footer-settings", "codex-auto-info"])
      for account in visible where account.provider == "claude" && account.capabilities.claudeProfileId != nil {
        for platform in account.capabilities.claudePlatforms where ["mac", "windows"].contains(platform) {
          expectedHelp.insert("claude-\(platform)-\(account.id)")
        }
      }
      for account in visible where account.provider == "codex" && account.canActivate { expectedHelp.insert("activate-\(account.id)") }
      if visible.contains(where: { $0.provider == "qwen" && !$0.creditPacks.isEmpty }) { expectedHelp.insert("qwen-packs") }
      let helpIDs = Set(help.map { $0.identifier?.rawValue ?? "" }.filter { !$0.isEmpty })
      let missingHelp = expectedHelp.subtracting(helpIDs).sorted()
      let labelled = help.allSatisfy { !$0.presenter.text.isEmpty && $0.accessibilityLabel() == $0.presenter.text }
      let accountRows = rows.filter { ($0.identifier?.rawValue ?? "").hasPrefix("account-row-") }
      let providerRows = rows.filter { ($0.identifier?.rawValue ?? "").hasPrefix("provider-row-") }
      let rowCountsPassed = accountRows.count == sectionAccounts.count && providerRows.count == otherGroups.count
      let geometryPassed = rows.allSatisfy { $0.bounds.width >= host.bounds.width * 0.8 && $0.bounds.height >= 40 }
      var hitTests: [[String: Any]] = []
      func hit(_ point: NSPoint, label: String) -> NSView? {
        let target = host.hitTest(host.convert(point, to: host.superview))
        hitTests.append(["target": label, "hitClass": target.map { String(describing: type(of: $0)) } ?? "nil",
          "hitId": target?.identifier?.rawValue ?? "", "x": point.x, "y": point.y])
        return target
      }
      let fullRowPassed = rows.allSatisfy { row in
        [CGFloat(0.03), CGFloat(0.30)].allSatisfy { fraction in
          let point = row.convert(NSPoint(x: row.bounds.width * fraction, y: row.bounds.midY), to: host)
          return hit(point, label: row.identifier?.rawValue ?? "row") === row
        }
      }
      let nestedPassed = help.filter { view in
        rows.contains { row in row.convert(row.bounds, to: nil).intersects(view.convert(view.bounds, to: nil)) }
      }.allSatisfy { view in
        let point = view.convert(NSPoint(x: view.bounds.midX, y: view.bounds.midY), to: host)
        let target = hit(point, label: view.identifier?.rawValue ?? "nested")
        return !(target is DetailsRowButton)
      }
      // Selected-row alignment: in every switchable section, the check's left edge must sit on the Activate
      // capsule's left edge and "Active" on the "Activate" label's x, within 0.5 pt.
      var alignment: [[String: Any]] = []
      var alignmentPassed = true
      for provider in ["codex", "antigravity"] {
        let ids = visible.filter { $0.provider == provider }.map(\.id)
        func edges(_ part: String) -> [CGFloat] {
          ids.compactMap { id in
            AlignmentProbe.frames["slot|activate-\(id)|\(part)"]?.minX ?? AlignmentProbe.frames["slot|active-\(id)|\(part)"]?.minX
          }
        }
        let icons = edges("icon"), labels = edges("label")
        let subs = ids.compactMap { AlignmentProbe.frames["slot|active-\($0)|sub"]?.minX }
        let spread = { (values: [CGFloat]) -> CGFloat in (values.max() ?? 0) - (values.min() ?? 0) }
        let ok = icons.count >= 2 ? spread(icons) <= 0.5 && spread(labels + subs) <= 0.5 : true
        alignmentPassed = alignmentPassed && ok
        alignment.append(["section": provider, "slots": icons.count, "iconLeftEdges": icons.map { Double($0) },
          "labelLeftEdges": labels.map { Double($0) }, "secondaryLeftEdges": subs.map { Double($0) }, "passed": ok])
      }
      let passed = missingHelp.isEmpty && labelled && rowCountsPassed && geometryPassed && fullRowPassed && nestedPassed && alignmentPassed
      let result: [String: Any] = [
        "passed": passed, "activeAlignment": alignment, "activeAlignmentPassed": alignmentPassed, "helpTags": help.map { ["id": $0.identifier?.rawValue ?? "", "text": $0.presenter.text] },
        "missingHelpTags": missingHelp, "helpTagsLabelled": labelled,
        "accountRows": accountRows.count, "expectedAccountRows": sectionAccounts.count,
        "providerRows": providerRows.count, "expectedProviderRows": otherGroups.count,
        "fullRowGeometryPassed": geometryPassed, "fullRowHitTestsPassed": fullRowPassed,
        "nestedControlHitTestsPassed": nestedPassed, "hitTests": hitTests,
        "hiddenAppOwnedInspection": true, "desktopCaptureOrAutomation": false, "accountActionsInvoked": false,
      ]
      print(String(decoding: try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]), as: UTF8.self))
      exit(passed ? 0 : 1)
    } catch { fputs("Native tooltip inspection failed.\n", stderr); exit(1) }
  }

  /// Opens the Qwen packs popover through the button's own action and reads its rendered text.
  static func checkNativePacks(input: String, output: String) -> Never {
    do {
      let dashboard = try loadFixture(input)
      let packs = dashboard.visibleAccounts.filter { $0.provider == "qwen" }.flatMap(\.creditPacks)
      guard !packs.isEmpty else { throw BarClientError.decoding }
      let (host, _, window) = host(dashboard, options: Options([]))
      NSApplication.shared.setActivationPolicy(.accessory)
      window.orderFront(nil)
      settle(host)
      guard let button = collect(host).help.first(where: { $0.identifier?.rawValue == "qwen-packs" }) else { throw BarClientError.decoding }
      let before = Set(NSApplication.shared.windows.map { ObjectIdentifier($0) })
      button.performAction()
      RunLoop.main.run(until: Date().addingTimeInterval(0.8))
      let presented = NSApplication.shared.windows.filter { $0.isVisible && !before.contains(ObjectIdentifier($0)) }
      guard let popup = presented.first, let content = popup.contentView,
        let bitmap = content.bitmapImageRepForCachingDisplay(in: content.bounds)
      else { throw BarClientError.decoding }
      content.layoutSubtreeIfNeeded()
      content.cacheDisplay(in: content.bounds, to: bitmap)
      guard let png = bitmap.representation(using: .png, properties: [:]) else { throw BarClientError.decoding }
      try png.write(to: URL(fileURLWithPath: output), options: .atomic)
      var renderedText: [String] = []
      if let cgImage = bitmap.cgImage {
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.recognitionLanguages = ["en-US"]
        request.usesLanguageCorrection = false
        try VNImageRequestHandler(cgImage: cgImage).perform([request])
        renderedText = request.results?.compactMap { $0.topCandidates(1).first?.string } ?? []
      }
      let text = renderedText.joined(separator: "\n")
      let matched = packs.filter { text.contains($0.label) }.count
      let passed = matched == packs.count
      let result: [String: Any] = ["passed": passed, "action": "qwen-packs button action", "expectedActualPackCount": packs.count,
        "renderedMatchedPackCount": matched, "nativePopoverWindowCount": presented.count, "renderedPopupTextOCR": renderedText,
        "isolatedPreviewModel": true, "networkOrAccountActions": false, "desktopCaptureOrExternalAutomation": false]
      print(String(decoding: try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]), as: UTF8.self))
      presented.forEach { $0.orderOut(nil) }
      window.orderOut(nil)
      exit(passed ? 0 : 1)
    } catch { fputs("Native pack interaction inspection failed.\n", stderr); exit(1) }
  }
}
