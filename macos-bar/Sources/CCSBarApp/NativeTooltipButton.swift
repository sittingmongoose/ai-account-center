import SwiftUI
import AppKit

/// A tracked AppKit help panel also works inside MenuBarExtra's window, where
/// the system tooltip timer does not reliably run for hosted SwiftUI controls.
struct NativeTooltipButton: NSViewRepresentable {
  var symbol: String
  var customImage: NSImage? = nil
  var title = ""
  let tooltip: String
  var enabled = true
  var identifier = ""
  var dismissWindowOnPress = false
  var menuActions: [NativeMenuAction] = []
  var isDetailsRow = false
  let action: () -> Void

  func makeCoordinator() -> Coordinator { Coordinator(action: action) }
  func makeNSView(context: Context) -> HoverHelpButton {
    let button: HoverHelpButton = isDetailsRow ? DetailsRowButton() : HoverHelpButton()
    button.isBordered = false
    button.bezelStyle = .regularSquare
    button.target = context.coordinator
    button.action = #selector(Coordinator.pressed(_:))
    updateNSView(button, context: context)
    return button
  }
  func updateNSView(_ button: HoverHelpButton, context: Context) {
    context.coordinator.action = action
    context.coordinator.dismissWindowOnPress = dismissWindowOnPress
    context.coordinator.menuActions = menuActions
    button.title = title
    button.image = customImage ?? (symbol.isEmpty ? nil : NSImage(systemSymbolName: symbol, accessibilityDescription: tooltip)?
      .withSymbolConfiguration(.init(pointSize: 15, weight: .medium))
    )
    button.imageScaling = .scaleProportionallyDown
    button.imagePosition = symbol.isEmpty && customImage == nil ? .noImage : (title.isEmpty ? .imageOnly : .imageLeading)
    button.contentTintColor = NSColor(AccountsPalette.muted)
    button.font = .systemFont(ofSize: 11)
    button.helpText = tooltip
    button.setAccessibilityHelp(tooltip)
    button.setAccessibilityLabel(tooltip)
    button.identifier = NSUserInterfaceItemIdentifier(identifier)
    button.isEnabled = enabled
  }
  final class Coordinator: NSObject {
    var action: () -> Void
    var dismissWindowOnPress = false
    var menuActions: [NativeMenuAction] = []
    init(action: @escaping () -> Void) { self.action = action }
    @objc func pressed(_ sender: NSButton) {
      if !menuActions.isEmpty {
        let menu = NSMenu()
        for entry in menuActions {
          let handler = MenuActionHandler(window: sender.window, action: entry.action)
          let item = NSMenuItem(title: entry.title, action: #selector(MenuActionHandler.performAction), keyEquivalent: "")
          item.target = handler
          item.representedObject = handler
          menu.addItem(item)
        }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: sender.bounds.minY), in: sender)
        return
      }
      // This button belongs to the account menu. Closing its own panel also
      // informs SwiftUI that presentation ended, so the next status click opens
      // it normally and its elevated window cannot cover native Settings.
      if dismissWindowOnPress { sender.window?.close() }
      action()
    }
  }
}

struct NativeMenuAction {
  let title: String
  let action: () -> Void
}

private final class MenuActionHandler: NSObject {
  weak var window: NSWindow?
  let action: () -> Void
  init(window: NSWindow?, action: @escaping () -> Void) { self.window = window; self.action = action }
  @objc func performAction() {
    window?.close()
    DispatchQueue.main.async(execute: action)
  }
}

class HoverHelpButton: NSButton {
  var helpText = ""
  private var hoverArea: NSTrackingArea?
  private var pendingHelp: DispatchWorkItem?
  private var helpPanel: NSPanel?

  override func updateTrackingAreas() {
    if let hoverArea { removeTrackingArea(hoverArea) }
    let area = NSTrackingArea(rect: .zero,
      options: [.mouseEnteredAndExited, .mouseMoved, .activeAlways, .inVisibleRect], owner: self)
    addTrackingArea(area)
    hoverArea = area
    super.updateTrackingAreas()
  }

  override func mouseEntered(with event: NSEvent) {
    hideHelp()
    scheduleHelp()
    super.mouseEntered(with: event)
  }

  override func mouseMoved(with event: NSEvent) {
    if !isPointerTarget { hideHelp() }
    else if pendingHelp == nil && helpPanel == nil { scheduleHelp() }
    super.mouseMoved(with: event)
  }

  private func scheduleHelp() {
    let work = DispatchWorkItem { [weak self] in self?.showHelp() }
    pendingHelp = work
    DispatchQueue.main.asyncAfter(deadline: .now() + 0.35, execute: work)
  }

  override func mouseExited(with event: NSEvent) {
    hideHelp()
    super.mouseExited(with: event)
  }

  override func mouseDown(with event: NSEvent) {
    hideHelp()
    super.mouseDown(with: event)
  }

  override func viewWillMove(toWindow newWindow: NSWindow?) {
    hideHelp()
    super.viewWillMove(toWindow: newWindow)
  }

  private func showHelp() {
    pendingHelp = nil
    guard let window, window.isVisible, !helpText.isEmpty, isPointerTarget else { return }
    let label = NSTextField(wrappingLabelWithString: helpText)
    label.font = .systemFont(ofSize: 11)
    label.textColor = NSColor(AccountsPalette.text)
    label.setAccessibilityIdentifier("account-center-tooltip-text")
    label.setAccessibilityLabel(helpText)
    let width = min(340, max(125, label.intrinsicContentSize.width + 20))
    label.frame = NSRect(x: 10, y: 7, width: width - 20, height: 32)
    let height = max(28, (label.cell?.cellSize(forBounds: NSRect(x: 0, y: 0, width: width - 20, height: 100)).height ?? 14) + 14)
    label.frame.size.height = height - 14
    let content = NSView(frame: NSRect(x: 0, y: 0, width: width, height: height))
    content.wantsLayer = true
    content.layer?.backgroundColor = NSColor(AccountsPalette.plate).cgColor
    content.layer?.cornerRadius = 7
    content.layer?.borderWidth = 1
    content.layer?.borderColor = NSColor(AccountsPalette.border).cgColor
    content.addSubview(label)
    let panel = NSPanel(contentRect: content.bounds, styleMask: [.borderless, .nonactivatingPanel],
      backing: .buffered, defer: false)
    panel.title = "AI Account Center help"
    panel.identifier = NSUserInterfaceItemIdentifier("account-center-hover-help")
    panel.contentView = content
    panel.isOpaque = false
    panel.backgroundColor = .clear
    panel.hasShadow = true
    panel.hidesOnDeactivate = false
    panel.ignoresMouseEvents = true
    panel.level = NSWindow.Level(rawValue: window.level.rawValue + 1)
    panel.collectionBehavior = [.transient, .fullScreenAuxiliary]
    let rect = window.convertToScreen(convert(bounds, to: nil))
    let screen = window.screen?.visibleFrame ?? NSScreen.main?.visibleFrame ?? rect
    let x = min(max(screen.minX + 4, rect.midX - width / 2), screen.maxX - width - 4)
    let below = rect.minY - height - 5
    let y = below >= screen.minY + 4 ? below : rect.maxY + 5
    panel.setFrameOrigin(NSPoint(x: x, y: y))
    helpPanel = panel
    window.addChildWindow(panel, ordered: .above)
    panel.orderFrontRegardless()
  }

  private func hideHelp() {
    pendingHelp?.cancel()
    pendingHelp = nil
    if let helpPanel {
      helpPanel.parent?.removeChildWindow(helpPanel)
      helpPanel.orderOut(nil)
    }
    helpPanel = nil
  }

  private var isPointerTarget: Bool {
    guard let window, let content = window.contentView else { return false }
    let windowPoint = window.convertPoint(fromScreen: NSEvent.mouseLocation)
    let point = content.superview?.convert(windowPoint, from: nil) ?? windowPoint
    return content.hitTest(point) === self
  }
}

/// The transparent row button yields to real nested controls, so a desktop-open,
/// Activate or Qwen-packs click cannot also open the account Details popover.
private final class DetailsRowButton: HoverHelpButton {
  override func hitTest(_ point: NSPoint) -> NSView? {
    guard let window, let root = window.contentView else { return super.hitTest(point) }
    // AppKit supplies hit-test points in the receiver's superview coordinates.
    let windowPoint = convert(convert(point, from: superview), to: nil)
    func hasControl(_ view: NSView) -> Bool {
      if view === self || view is DetailsRowButton { return false }
      if !view.isHiddenOrHasHiddenAncestor && (view is NSButton || view is RowActionExclusionView),
        view.bounds.contains(view.convert(windowPoint, from: nil)) { return true }
      return view.subviews.contains(where: hasControl)
    }
    return hasControl(root) ? nil : super.hitTest(point)
  }
}

struct RowActionExclusion: NSViewRepresentable {
  func makeNSView(context: Context) -> RowActionExclusionView { RowActionExclusionView() }
  func updateNSView(_ view: RowActionExclusionView, context: Context) {}
}

final class RowActionExclusionView: NSView {
  override func hitTest(_ point: NSPoint) -> NSView? { nil }
}
