import SwiftUI
import AppKit

/// Offline checks only: a stand-in pointer location, so the hover-tag path can be driven in-process without
/// moving the real pointer. Nil in the app, where the real pointer is read.
@MainActor
enum HoverProbe {
  static var pointer: NSPoint?
  static var location: NSPoint { pointer ?? NSEvent.mouseLocation }
}

/// One layer of the panel that can be covered, fade out or close: the panel itself, the content Settings
/// slides over (the account list or the sign-in screen), and Settings. While a layer's gate is suppressed,
/// no hover tag inside it presents, and a tag already showing goes away. The gate is an object, read when a
/// tag would show, so it also holds for a layer SwiftUI has frozen while it animates out.
@MainActor
final class HoverGate: ObservableObject {
  @Published private(set) var suppressed: Bool
  private let presenters = NSHashTable<HelpPresenter>.weakObjects()

  init(suppressed: Bool = false) { self.suppressed = suppressed }

  func set(_ value: Bool) {
    guard value != suppressed else { return }
    suppressed = value
    for presenter in presenters.allObjects { presenter.gateChanged() }
  }

  func register(_ presenter: HelpPresenter) { presenters.add(presenter) }
}

private struct HoverGatesKey: EnvironmentKey { static let defaultValue: [HoverGate] = [] }
private struct HoverSuppressedKey: EnvironmentKey { static let defaultValue = false }
extension EnvironmentValues {
  /// The gates of every layer this view sits in.
  var trayHoverGates: [HoverGate] {
    get { self[HoverGatesKey.self] }
    set { self[HoverGatesKey.self] = newValue }
  }
  /// True while any layer this view sits in is covered, fading out or closed.
  var trayHoverSuppressed: Bool {
    get { self[HoverSuppressedKey.self] }
    set { self[HoverSuppressedKey.self] = newValue }
  }
}

private struct TrayHoverLayer: ViewModifier {
  @ObservedObject var gate: HoverGate
  func body(content: Content) -> some View {
    let gate = self.gate
    let suppressed = gate.suppressed
    return content
      .transformEnvironment(\.trayHoverGates) { $0.append(gate) }
      .transformEnvironment(\.trayHoverSuppressed) { $0 = $0 || suppressed }
  }
}

/// A native help tag (`.help`) that stays silent while its layer is covered, fading out or closed.
private struct TrayNativeHelp: ViewModifier {
  let text: String
  @Environment(\.trayHoverSuppressed) private var suppressed
  func body(content: Content) -> some View { content.help(suppressed ? "" : text) }
}

extension View {
  /// Marks a layer of the panel; its hover tags present only while `gate` is open.
  func trayHoverLayer(_ gate: HoverGate) -> some View { modifier(TrayHoverLayer(gate: gate)) }
  /// The system help tag, silenced while its layer is covered, fading out or closed.
  func trayHelp(_ text: String) -> some View { modifier(TrayNativeHelp(text: text)) }
}

/// A view that owns a hover tag and has its own state to drop when its layer is suppressed.
@MainActor
protocol HoverTagOwner: AnyObject {
  func hoverSuppressed()
}

/// Help tags that work inside the tray panel even while the app is inactive, where the system tooltip
/// timer does not reliably run for hosted SwiftUI controls. Each tag is a small regular-glass panel
/// (radius 10) under the control. A tag presents only when its layer is open, its view is actually
/// visible, no other window of this app covers the pointer, and the pointer is inside the view.
@MainActor
final class HelpPresenter {
  private weak var owner: NSView?
  private var pending: DispatchWorkItem?
  private var panel: NSPanel?
  private var clickMonitor: Any?
  /// A click or key press closed the tag: it stays closed until the pointer leaves the view.
  private var dismissed = false
  var text = ""
  /// The gates of the layers the owner sits in.
  var gates: [HoverGate] = [] {
    didSet { for gate in gates { gate.register(self) } }
  }

  init(owner: NSView) { self.owner = owner }

  /// True while any layer the owner sits in is covered, fading out or closed.
  var suppressed: Bool { gates.contains { $0.suppressed } }

  func gateChanged() {
    guard suppressed else { return }
    hide()
    (owner as? HoverTagOwner)?.hoverSuppressed()
  }

  func schedule() {
    hide()
    let work = DispatchWorkItem { [weak self] in self?.show() }
    pending = work
    DispatchQueue.main.asyncAfter(deadline: .now() + 0.35, execute: work)
  }

  /// Waiting out the hover delay, or showing.
  var isActive: Bool { pending != nil || panel != nil }

  func pointerInside() -> Bool {
    guard let owner, let window = owner.window else { return false }
    let point = owner.convert(window.convertPoint(fromScreen: HoverProbe.location), from: nil)
    return owner.bounds.contains(point)
  }

  /// The owner is drawn: no hidden view and no faded-out layer (SwiftUI's opacity reaches the hosted
  /// views as their alpha) between it and the window.
  private var ownerVisible: Bool {
    guard let owner, let window = owner.window, window.isVisible, window.alphaValue > 0.05 else { return false }
    var view: NSView? = owner
    while let current = view {
      if current.isHidden || current.alphaValue < 0.05 || (current.layer?.opacity ?? 1) < 0.05 { return false }
      view = current.superview
    }
    return true
  }

  /// Another window of this app (a Details or packs popover, an open menu) is frontmost under the pointer.
  private func coveredByAnotherWindow(_ window: NSWindow) -> Bool {
    guard HoverProbe.pointer == nil else { return false }
    let number = NSWindow.windowNumber(at: NSEvent.mouseLocation, belowWindowWithWindowNumber: 0)
    guard number != 0, number != window.windowNumber,
      let top = NSApplication.shared.windows.first(where: { $0.windowNumber == number }) else { return false }
    return top.isVisible && top.identifier?.rawValue != "account-center-hover-help"
  }

  /// Every condition a tag needs to present right now, cheapest first.
  var canPresent: Bool {
    guard let owner, let window = owner.window, !text.isEmpty, !suppressed, pointerInside(), ownerVisible else { return false }
    return !coveredByAnotherWindow(window)
  }

  /// Called on every enter and move: a tag starts its delay when the pointer is over its view and may
  /// present, and goes away as soon as the pointer is not.
  func follow(blocked: Bool = false) {
    guard !blocked, !suppressed, pointerInside() else { pointerLeft(); return }
    if !isActive && !dismissed && canPresent { schedule() }
  }

  /// The pointer left the view (or the part of it this tag belongs to).
  func pointerLeft() {
    hide()
    dismissed = false
  }

  /// A click on the view closes its tag until the pointer leaves.
  func dismissForClick() {
    hide()
    dismissed = true
  }

  func show() {
    pending = nil
    guard canPresent, let owner, let window = owner.window else { return }
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
      self?.dismissForClick()
      return event
    }
  }

  /// True while this tag's glass panel is on screen.
  var isShowing: Bool { panel != nil }

  /// Offline checks only: runs a scheduled show now instead of after the hover delay.
  func flushPending() {
    guard let work = pending else { return }
    work.perform()
    work.cancel()
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
    view.presenter.gates = context.environment.trayHoverGates
    if view.presenter.suppressed { view.presenter.hide() }
    view.action = action
    view.identifier = NSUserInterfaceItemIdentifier(identifier)
    view.setAccessibilityLabel(text)
    view.setAccessibilityHelp(text)
  }
}

final class HoverHelpView: NSView, HoverTagOwner {
  lazy var presenter = HelpPresenter(owner: self)
  var action: (() -> Void)?
  private var area: NSTrackingArea?

  override func hitTest(_ point: NSPoint) -> NSView? { nil }

  // A hosted view's visible rect can reach past its bounds, so the tracking area may span far more than the
  // control: every enter and move re-checks where the pointer is, and the tag follows it.
  override func updateTrackingAreas() {
    if let area { removeTrackingArea(area) }
    let next = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .mouseMoved, .activeAlways, .inVisibleRect], owner: self)
    addTrackingArea(next)
    area = next
    super.updateTrackingAreas()
  }
  override func mouseEntered(with event: NSEvent) { presenter.follow() }
  override func mouseMoved(with event: NSEvent) { presenter.follow() }
  override func mouseExited(with event: NSEvent) { presenter.pointerLeft() }
  override func viewWillMove(toWindow newWindow: NSWindow?) {
    presenter.hide()
    super.viewWillMove(toWindow: newWindow)
  }
  func hoverSuppressed() {}
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
    button.presenter.gates = context.environment.trayHoverGates
    if button.presenter.suppressed { button.hoverSuppressed() }
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

final class DetailsRowButton: NSButton, HoverTagOwner {
  lazy var presenter = HelpPresenter(owner: self)
  var onHover: (Bool) -> Void = { _ in }
  private var area: NSTrackingArea?
  private var hovering = false

  override func draw(_ dirtyRect: NSRect) {}

  override func updateTrackingAreas() {
    if let area { removeTrackingArea(area) }
    let next = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .mouseMoved, .activeAlways, .inVisibleRect], owner: self)
    addTrackingArea(next)
    area = next
    super.updateTrackingAreas()
  }
  private func setHovering(_ value: Bool) {
    guard value != hovering else { return }
    hovering = value
    onHover(value)
  }
  /// The row highlight and its tag follow the pointer; a covered row neither highlights nor tags.
  private func follow() {
    let inside = !presenter.suppressed && presenter.pointerInside()
    setHovering(inside)
    presenter.follow(blocked: !inside || overNestedControl)
  }
  override func mouseEntered(with event: NSEvent) { follow() }
  override func mouseMoved(with event: NSEvent) { follow() }
  override func mouseExited(with event: NSEvent) { setHovering(false); presenter.pointerLeft() }
  override func mouseDown(with event: NSEvent) { presenter.dismissForClick(); super.mouseDown(with: event) }
  override func viewWillMove(toWindow newWindow: NSWindow?) {
    presenter.hide()
    super.viewWillMove(toWindow: newWindow)
  }
  func hoverSuppressed() {
    presenter.hide()
    setHovering(false)
  }

  private var overNestedControl: Bool {
    guard let window else { return false }
    return nestedControl(at: window.convertPoint(fromScreen: HoverProbe.location))
  }

  /// True when a nested control of a live layer sits under this window point.
  func nestedControl(at windowPoint: NSPoint) -> Bool {
    guard let root = window?.contentView else { return false }
    func visit(_ view: NSView) -> Bool {
      if view === self || view is DetailsRowButton { return false }
      if let help = view as? HoverHelpView, help.presenter.suppressed { return false }
      if !view.isHiddenOrHasHiddenAncestor && (view is HoverHelpView || view is RowActionExclusionView),
        view.bounds.contains(view.convert(windowPoint, from: nil)) { return true }
      return view.subviews.contains(where: visit)
    }
    return visit(root)
  }

  override func hitTest(_ point: NSPoint) -> NSView? {
    // A row under Settings never takes a click.
    if presenter.suppressed { return nil }
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
