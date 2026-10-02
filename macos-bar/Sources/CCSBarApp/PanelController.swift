import SwiftUI
import AppKit
import Combine
import CCSBarCore

/// The borderless panel under the status item. It can become key (for Escape and the connection
/// fields) without activating the app, like a system menu-bar panel.
final class TrayPanel: NSPanel {
  var onCancel: () -> Void = {}
  override var canBecomeKey: Bool { true }
  override var canBecomeMain: Bool { false }
  override func cancelOperation(_ sender: Any?) { onCancel() }
}

/// Owns the status item and the Liquid Glass panel: one regular NSGlassEffectView (radius 20) hosting
/// the SwiftUI content. Opens from the status item, the global shortcut, and a relaunch of the app.
@MainActor
final class PanelController: NSObject, NSWindowDelegate {
  let model: AccountsViewModel
  let prefs: TrayPreferences
  let state = PanelState()
  let statusItem: NSStatusItem
  private(set) var panel: TrayPanel?
  private var hosting: NSHostingView<PanelRootView>?
  private(set) var statusHosting: PassthroughHostingView<StatusItemLabel>?
  private(set) var hotKey: GlobalHotKey?
  private var monitors: [Any] = []
  private var observers: [NSObjectProtocol] = []
  private var cancellables: Set<AnyCancellable> = []

  init(model: AccountsViewModel, prefs: TrayPreferences) {
    self.model = model
    self.prefs = prefs
    statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    super.init()
    configureStatusItem()
    prefs.$openShortcutEnabled.removeDuplicates().sink { [weak self] enabled in
      self?.updateHotKey(enabled)
    }.store(in: &cancellables)
    state.$desiredHeight.removeDuplicates().sink { [weak self] _ in
      DispatchQueue.main.async { self?.layoutPanel() }
    }.store(in: &cancellables)
    state.$settingsOpen.removeDuplicates().sink { [weak self] open in
      if open { self?.panel?.makeKey() }
    }.store(in: &cancellables)
    model.objectWillChange.merge(with: prefs.objectWillChange).sink { [weak self] _ in
      DispatchQueue.main.async { self?.updateStatusLength() }
    }.store(in: &cancellables)
  }

  var isOpen: Bool { panel?.isVisible == true }

  private func configureStatusItem() {
    guard let button = statusItem.button else { return }
    button.target = self
    button.action = #selector(statusItemClicked(_:))
    button.sendAction(on: [.leftMouseDown, .rightMouseDown])
    button.setAccessibilityLabel("AI Account Center")
    button.imagePosition = .noImage
    let label = PassthroughHostingView(rootView: StatusItemLabel(model: model, prefs: prefs))
    label.translatesAutoresizingMaskIntoConstraints = true
    button.addSubview(label)
    statusHosting = label
    updateStatusLength()
  }

  private func updateStatusLength() {
    guard let label = statusHosting, let button = statusItem.button else { return }
    let size = label.fittingSize
    statusItem.length = ceil(size.width)
    label.frame = NSRect(x: 0, y: (button.bounds.height - size.height) / 2, width: ceil(size.width), height: size.height)
    button.toolTip = model.menuBarReading(prefs).map { "AI Account Center · \($0.detail)" } ?? "AI Account Center"
  }

  @objc private func statusItemClicked(_ sender: Any?) { toggle() }

  func toggle() { isOpen ? close() : open() }

  // MARK: Open and close

  func open() {
    let panel = self.panel ?? makePanel()
    model.pendingCodexSwitch = nil
    model.pendingAntigravitySwitch = nil
    state.settingsOpen = false
    var context = OpenContext()
    context.firstOpen = !model.hasOpenedThisSession
    context.from = model.lastShown
    context.animate = !state.reduceMotion
    for provider in ["codex", "antigravity"] {
      if let id = model.dashboard?.visibleAccounts.first(where: { $0.provider == provider && $0.isActive })?.id {
        context.activeAtOpen[provider] = id
      }
    }
    state.open = context
    model.hasOpenedThisSession = true
    state.openGeneration += 1
    layoutPanel()
    panel.alphaValue = 1
    panel.makeKeyAndOrderFront(nil)
    statusItem.button?.highlight(true)
    installDismissMonitors()
    Task { await model.refresh(force: true) }
  }

  func close() {
    guard let panel, panel.isVisible else { return }
    model.lastShown = model.currentReadings
    // An unanswered switch confirmation ends with the panel, so background refresh resumes.
    model.pendingCodexSwitch = nil
    model.pendingAntigravitySwitch = nil
    removeDismissMonitors()
    statusItem.button?.highlight(false)
    if state.reduceMotion {
      panel.orderOut(nil)
    } else {
      NSAnimationContext.runAnimationGroup({ context in
        context.duration = 0.12
        panel.animator().alphaValue = 0
      }, completionHandler: { [weak panel] in
        Task { @MainActor in
          panel?.orderOut(nil)
          panel?.alphaValue = 1
        }
      })
    }
  }

  /// Escape closes Settings first, then the panel.
  func cancel() {
    if state.settingsOpen { state.setSettings(false) } else { close() }
  }

  private func makePanel() -> TrayPanel {
    let panel = TrayPanel(contentRect: NSRect(x: 0, y: 0, width: 760, height: 400),
      styleMask: [.borderless, .nonactivatingPanel, .fullSizeContentView], backing: .buffered, defer: false)
    panel.title = "AI Account Center"
    panel.identifier = NSUserInterfaceItemIdentifier("ai-account-center-panel")
    panel.isOpaque = false
    panel.backgroundColor = .clear
    panel.hasShadow = true
    panel.level = .statusBar
    panel.collectionBehavior = [.moveToActiveSpace, .fullScreenAuxiliary, .transient, .ignoresCycle]
    panel.hidesOnDeactivate = false
    panel.isMovable = false
    panel.isReleasedWhenClosed = false
    panel.animationBehavior = .utilityWindow
    panel.delegate = self
    panel.onCancel = { [weak self] in self?.cancel() }
    panel.setAccessibilityRole(.popover)
    panel.setAccessibilityLabel("AI Account Center")

    let glass = NSGlassEffectView(frame: panel.contentRect(forFrameRect: panel.frame))
    glass.style = .regular
    glass.cornerRadius = TrayMetrics.panelRadius
    glass.tintColor = nil
    let host = NSHostingView(rootView: PanelRootView(model: model, prefs: prefs, state: state))
    host.sizingOptions = []
    glass.contentView = host
    panel.contentView = glass
    hosting = host
    self.panel = panel
    return panel
  }

  /// Width 760 (or the screen less a margin), height from the content, top edge under the menu bar.
  func layoutPanel() {
    guard let panel else { return }
    let anchor = statusAnchor()
    let screen = anchor.screen
    let visible = screen.visibleFrame
    let width = min(760, visible.width - 16)
    state.panelWidth = width
    state.maxHeight = visible.height - 14
    let desired = state.desiredHeight > 0 ? state.desiredHeight : 420
    let height = max(160, min(desired, state.maxHeight))
    var x = anchor.rect.midX - width / 2
    x = min(max(x, visible.minX + 8), visible.maxX - width - 8)
    let top = min(anchor.rect.minY - 6, visible.maxY - 4)
    let frame = NSRect(x: x.rounded(), y: (top - height).rounded(), width: width, height: height.rounded())
    if panel.frame != frame {
      panel.setFrame(frame, display: true)
      panel.invalidateShadow()
    }
  }

  private func statusAnchor() -> (rect: NSRect, screen: NSScreen) {
    if let button = statusItem.button, let window = button.window, let screen = window.screen {
      let rect = window.convertToScreen(button.convert(button.bounds, to: nil))
      if screen.frame.intersects(rect) { return (rect, screen) }
    }
    // Status item hidden (menu bar full or behind the notch): open at the top right of the active screen.
    let screen = NSScreen.screens.first(where: { $0.frame.contains(NSEvent.mouseLocation) }) ?? NSScreen.main ?? NSScreen.screens[0]
    let visible = screen.visibleFrame
    return (NSRect(x: visible.maxX - 220, y: visible.maxY, width: 1, height: 1), screen)
  }

  // MARK: Dismissal

  private func installDismissMonitors() {
    removeDismissMonitors()
    // A click in another app or on the desktop closes the panel, like a menu.
    if let monitor = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown], handler: { [weak self] _ in
      Task { @MainActor in self?.close() }
    }) { monitors.append(monitor) }
    observers.append(NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didActivateApplicationNotification,
      object: nil, queue: .main) { [weak self] note in
      let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
      guard app?.processIdentifier != ProcessInfo.processInfo.processIdentifier else { return }
      Task { @MainActor in self?.close() }
    })
    observers.append(NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.activeSpaceDidChangeNotification,
      object: nil, queue: .main) { [weak self] _ in
      Task { @MainActor in self?.close() }
    })
  }

  private func removeDismissMonitors() {
    monitors.forEach(NSEvent.removeMonitor)
    monitors.removeAll()
    observers.forEach(NSWorkspace.shared.notificationCenter.removeObserver)
    observers.removeAll()
  }

  // MARK: Open shortcut

  private func updateHotKey(_ enabled: Bool) {
    if !enabled {
      hotKey?.unregister()
      state.shortcutProblem = nil
      return
    }
    let key = hotKey ?? GlobalHotKey { [weak self] in self?.toggle() }
    hotKey = key
    state.shortcutProblem = key.register() ? nil
      : "Another app already uses Option-Command-A, so the shortcut is off. Open the panel from the menu bar or by opening the app again."
  }
}
