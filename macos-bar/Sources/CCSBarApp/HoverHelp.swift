import SwiftUI
import AppKit

/// Help tags that work inside the tray panel even while the app is inactive, where the system tooltip
/// timer does not reliably run for hosted SwiftUI controls. Each tag is a small regular-glass panel
/// (radius 10) under the control.
@MainActor
final class HelpPresenter {
  private weak var owner: NSView?
  private var pending: DispatchWorkItem?
  private var panel: NSPanel?
  private var clickMonitor: Any?
  var text = ""

  init(owner: NSView) { self.owner = owner }

  func schedule() {
    hide()
    let work = DispatchWorkItem { [weak self] in self?.show() }
    pending = work
    DispatchQueue.main.asyncAfter(deadline: .now() + 0.35, execute: work)
  }

  private func pointerInside() -> Bool {
    guard let owner, let window = owner.window else { return false }
    let point = owner.convert(window.convertPoint(fromScreen: NSEvent.mouseLocation), from: nil)
    return owner.bounds.contains(point)
  }

  func show() {
    pending = nil
    guard let owner, let window = owner.window, window.isVisible, !text.isEmpty, pointerInside() else { return }
    let label = NSTextField(wrappingLabelWithString: text)
    label.font = .systemFont(ofSize: 12)
    label.textColor = .labelColor
    label.setAccessibilityIdentifier("account-center-tooltip-text")
    label.setAccessibilityLabel(text)
    let width = min(340, max(110, label.intrinsicContentSize.width + 22))
    let textHeight = label.cell?.cellSize(forBounds: NSRect(x: 0, y: 0, width: width - 22, height: 200)).height ?? 15
    let height = max(28, textHeight + 14)
    label.frame = NSRect(x: 11, y: 7, width: width - 22, height: height - 14)
    let glass = NSGlassEffectView(frame: NSRect(x: 0, y: 0, width: width, height: height))
    glass.style = .regular
    glass.cornerRadius = 10
    let content = NSView(frame: glass.bounds)
    content.addSubview(label)
    glass.contentView = content
    let help = NSPanel(contentRect: glass.bounds, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
    help.title = "AI Account Center help"
    help.identifier = NSUserInterfaceItemIdentifier("account-center-hover-help")
    help.contentView = glass
    help.isOpaque = false
    help.backgroundColor = .clear
    help.hasShadow = true
    help.hidesOnDeactivate = false
    help.ignoresMouseEvents = true
    help.appearance = window.effectiveAppearance
    help.level = NSWindow.Level(rawValue: window.level.rawValue + 1)
    help.collectionBehavior = [.transient, .fullScreenAuxiliary]
    let rect = window.convertToScreen(owner.convert(owner.bounds, to: nil))
    let screen = window.screen?.visibleFrame ?? NSScreen.main?.visibleFrame ?? rect
    let x = min(max(screen.minX + 4, rect.midX - width / 2), screen.maxX - width - 4)
    let below = rect.minY - height - 6
    help.setFrameOrigin(NSPoint(x: x, y: below >= screen.minY + 4 ? below : rect.maxY + 6))
    panel = help
    window.addChildWindow(help, ordered: .above)
    help.orderFrontRegardless()
    clickMonitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown, .keyDown]) { [weak self] event in
      self?.hide()
      return event
    }
  }

  func hide() {
    pending?.cancel()
    pending = nil
    if let clickMonitor { NSEvent.removeMonitor(clickMonitor) }
    clickMonitor = nil
    if let panel {
      panel.parent?.removeChildWindow(panel)
      panel.orderOut(nil)
    }
    panel = nil
  }
}

/// A transparent help overlay for a SwiftUI control. It never takes clicks (they reach the control),
/// marks the control as a nested action for the full-row Details target, and carries the control's
/// identifier and action for the offline interaction checks.
struct HoverHelp: NSViewRepresentable {
  let text: String
  var identifier = ""
  var action: (() -> Void)? = nil

  func makeNSView(context: Context) -> HoverHelpView { HoverHelpView() }
  func updateNSView(_ view: HoverHelpView, context: Context) {
    view.presenter.text = text
    view.action = action
    view.identifier = NSUserInterfaceItemIdentifier(identifier)
    view.setAccessibilityLabel(text)
    view.setAccessibilityHelp(text)
  }
}

final class HoverHelpView: NSView {
  lazy var presenter = HelpPresenter(owner: self)
  var action: (() -> Void)?
  private var area: NSTrackingArea?

  override func hitTest(_ point: NSPoint) -> NSView? { nil }

  override func updateTrackingAreas() {
    if let area { removeTrackingArea(area) }
    let next = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
    addTrackingArea(next)
    area = next
    super.updateTrackingAreas()
  }
  override func mouseEntered(with event: NSEvent) { presenter.schedule() }
  override func mouseExited(with event: NSEvent) { presenter.hide() }
  override func viewWillMove(toWindow newWindow: NSWindow?) {
    presenter.hide()
    super.viewWillMove(toWindow: newWindow)
  }
  /// Used only by the offline interaction check: the same action the control performs.
  func performAction() { action?() }
}

extension View {
  /// A help tag for a nested control; `identifier` names it for the offline checks.
  func hoverHelp(_ text: String, id: String = "", action: (() -> Void)? = nil) -> some View {
    overlay(HoverHelp(text: text, identifier: id, action: action))
  }
}

/// The full-row Details target. It yields to nested controls (anything carrying a HoverHelp or a
/// RowActionExclusion), so an Activate, Open or packs click never also opens Details.
struct DetailsRowTarget: NSViewRepresentable {
  let tooltip: String
  let identifier: String
  var onHover: (Bool) -> Void = { _ in }
  let action: () -> Void

  func makeCoordinator() -> Coordinator { Coordinator() }
  func makeNSView(context: Context) -> DetailsRowButton {
    let button = DetailsRowButton()
    button.isBordered = false
    button.title = ""
    button.bezelStyle = .regularSquare
    button.imagePosition = .noImage
    button.focusRingType = .none
    button.target = context.coordinator
    button.action = #selector(Coordinator.pressed(_:))
    updateNSView(button, context: context)
    return button
  }
  func updateNSView(_ button: DetailsRowButton, context: Context) {
    context.coordinator.action = action
    button.onHover = onHover
    button.presenter.text = tooltip
    button.identifier = NSUserInterfaceItemIdentifier(identifier)
    button.setAccessibilityLabel(tooltip)
    button.setAccessibilityHelp(tooltip)
    button.toolTip = nil
  }
  final class Coordinator: NSObject {
    var action: () -> Void = {}
    @objc func pressed(_ sender: NSButton) { action() }
  }
}

final class DetailsRowButton: NSButton {
  lazy var presenter = HelpPresenter(owner: self)
  var onHover: (Bool) -> Void = { _ in }
  private var area: NSTrackingArea?

  override func draw(_ dirtyRect: NSRect) {}

  override func updateTrackingAreas() {
    if let area { removeTrackingArea(area) }
    let next = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .mouseMoved, .activeAlways, .inVisibleRect], owner: self)
    addTrackingArea(next)
    area = next
    super.updateTrackingAreas()
  }
  override func mouseEntered(with event: NSEvent) { onHover(true); if !overNestedControl { presenter.schedule() } }
  override func mouseMoved(with event: NSEvent) {
    if overNestedControl { presenter.hide() }
  }
  override func mouseExited(with event: NSEvent) { onHover(false); presenter.hide() }
  override func mouseDown(with event: NSEvent) { presenter.hide(); super.mouseDown(with: event) }
  override func viewWillMove(toWindow newWindow: NSWindow?) {
    presenter.hide()
    super.viewWillMove(toWindow: newWindow)
  }

  private var overNestedControl: Bool {
    guard let window else { return false }
    return nestedControl(at: window.convertPoint(fromScreen: NSEvent.mouseLocation))
  }

  /// True when a nested control sits under this window point.
  func nestedControl(at windowPoint: NSPoint) -> Bool {
    guard let root = window?.contentView else { return false }
    func visit(_ view: NSView) -> Bool {
      if view === self || view is DetailsRowButton { return false }
      if !view.isHiddenOrHasHiddenAncestor && (view is HoverHelpView || view is RowActionExclusionView),
        view.bounds.contains(view.convert(windowPoint, from: nil)) { return true }
      return view.subviews.contains(where: visit)
    }
    return visit(root)
  }

  override func hitTest(_ point: NSPoint) -> NSView? {
    // AppKit supplies hit-test points in the receiver's superview coordinates.
    guard let superview else { return super.hitTest(point) }
    let windowPoint = superview.convert(point, to: nil)
    return nestedControl(at: windowPoint) ? nil : super.hitTest(point)
  }
}

/// Marks a nested control that has no help tag (a toggle or a menu) so the row target yields to it.
struct RowActionExclusion: NSViewRepresentable {
  func makeNSView(context: Context) -> RowActionExclusionView { RowActionExclusionView() }
  func updateNSView(_ view: RowActionExclusionView, context: Context) {}
}

final class RowActionExclusionView: NSView {
  override func hitTest(_ point: NSPoint) -> NSView? { nil }
}
