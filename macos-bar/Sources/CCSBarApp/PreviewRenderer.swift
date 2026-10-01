import SwiftUI
import AppKit
import CCSBarCore

/// Renders an isolated fixture for layout review; no desktop capture or UI control.
@MainActor
enum PreviewRenderer {
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
