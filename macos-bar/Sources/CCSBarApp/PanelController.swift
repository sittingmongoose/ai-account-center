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
      // Focus never stays in a field that Settings now covers (or that leaves with Settings), so typing
      // cannot land in a hidden field and Escape always reaches the panel.
      self?.panel?.makeFirstResponder(nil)
      if open { self?.panel?.makeKey() }
    }.store(in: &cancellables)
    model.objectWillChange.merge(with: prefs.objectWillChange).merge(with: model.menuBar.objectWillChange).sink { [weak self] _ in
      DispatchQueue.main.async { self?.updateStatusLength() }
    }.store(in: &cancellables)
    // The panel starts closed: refreshes store silently and only the menu-bar label follows them.
    model.panelOpen = false
  }

  /// True while the close fade runs: a reopen during the fade cancels it, and the fade's completion then
  /// leaves the panel alone.
  private var closing = false
  private var closeGeneration = 0

  var isOpen: Bool { panel?.isVisible == true && !closing }

  private func configureStatusItem() {
    guard let button = statusItem.button else { return }
    button.target = self
    button.action = #selector(statusItemClicked(_:))
    button.sendAction(on: [.leftMouseDown, .rightMouseDown])
    button.setAccessibilityLabel("AI Account Center")
    button.imagePosition = .noImage
    let label = PassthroughHostingView(rootView: StatusItemLabel(menuBar: model.menuBar, prefs: prefs))
    label.translatesAutoresizingMaskIntoConstraints = true
    button.addSubview(label)
    statusHosting = label
    updateStatusLength()
  }

  /// The reading and help tag the status item now shows, and the button height they were laid out
  /// for. The menu-bar state syncs on every delivery; `fittingSize` forces a synchronous SwiftUI
  /// layout and `statusItem.length` re-lays the menu bar out, so both run only when what the
  /// status item shows actually changes (N4).
  private var appliedStatusReading: MenuBarReading?
  private var appliedStatusToolTip: String?
  private var appliedStatusButtonHeight: CGFloat?

  private func updateStatusLength() {
    guard let label = statusHosting, let button = statusItem.button else { return }
    let menuBar = model.menuBar
    let reading = menuBar.reading(prefs)
    // Signed out or not paired: the logo alone, and the help tag says so (section 9).
    let toolTip = reading.map { "AI Account Center · \($0.detail)" }
      ?? (menuBar.signedOut ? "AI Account Center · \(menuBar.snapshot.signInHelp)" : "AI Account Center")
    if appliedStatusToolTip != nil, reading == appliedStatusReading, toolTip == appliedStatusToolTip,
      button.bounds.height == appliedStatusButtonHeight { return }
    appliedStatusReading = reading
    appliedStatusToolTip = toolTip
    appliedStatusButtonHeight = button.bounds.height
    let size = label.fittingSize
    statusItem.length = ceil(size.width)
    label.frame = NSRect(x: 0, y: (button.bounds.height - size.height) / 2, width: ceil(size.width), height: size.height)
    button.toolTip = toolTip
  }

  @objc private func statusItemClicked(_ sender: Any?) { toggle() }

  func toggle() { isOpen ? close() : open() }

  // MARK: Open and close

  func open() {
    // First: the panel's publishes flow again, so it renders the silently stored refreshes (the sets
    // below already send, which is what re-renders it).
    model.panelOpen = true
    state.contentInstalled = true
    let panel = self.panel ?? makePanel()
    closing = false
    closeGeneration += 1
    model.pendingCodexSwitch = nil
    model.pendingAntigravitySwitch = nil
    state.settingsOpen = false
    model.panelOpened()
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
    // A zero-length animation replaces a close fade that may still be running.
    NSAnimationContext.runAnimationGroup { context in
      context.duration = 0
      panel.animator().alphaValue = 1
    }
    panel.alphaValue = 1
    panel.makeKeyAndOrderFront(nil)
    statusItem.button?.highlight(true)
    installDismissMonitors()
    Task { await model.refresh(force: true) }
  }

  func close() {
    guard let panel, panel.isVisible, !closing else { return }
    // First: later refreshes store silently again (the confirmation clears below go quietly with them).
    model.panelOpen = false
    model.lastShown = model.currentReadings
    // An unanswered switch confirmation ends with the panel, so background refresh resumes.
    model.pendingCodexSwitch = nil
    model.pendingAntigravitySwitch = nil
    removeDismissMonitors()
    statusItem.button?.highlight(false)
    if state.reduceMotion {
      panel.orderOut(nil)
      state.contentInstalled = false
    } else {
      closing = true
      let generation = closeGeneration
      NSAnimationContext.runAnimationGroup({ context in
        context.duration = 0.12
        panel.animator().alphaValue = 0
      }, completionHandler: { [weak self, weak panel] in
        Task { @MainActor in
          guard let self, self.closeGeneration == generation else { return }
          self.closing = false
          panel?.orderOut(nil)
          panel?.alphaValue = 1
          self.state.contentInstalled = false
        }
      })
    }
  }

  /// A Details, packs or info popover is showing. From the nonactivating panel it never becomes the key
  /// window, so its own Escape handling never runs; the panel closes it instead.
  var popoverShown: Bool {
    NSApplication.shared.windows.contains { window in
      window !== panel && window.isVisible && String(describing: type(of: window)).contains("Popover")
    }
  }

  /// Escape closes a popover first, then stops a running address check or pair (nothing is saved, and a key the
  /// dashboard already issued is revoked), then closes Settings, then an unanswered switch confirmation, then the panel.
  func cancel() {
    if popoverShown { state.popoverDismissal += 1 }
    else if model.signIn.active && model.signIn.cancelRunning() {}
    else if state.settingsOpen { state.setSettings(false) }
    else if model.pendingCodexSwitch != nil { model.cancelCodexSwitch() }
    else if model.pendingAntigravitySwitch != nil { model.cancelAntigravitySwitch() }
    else { close() }
  }

  /// Escape pressed in the panel, whatever has focus: a focused text field would otherwise swallow it
  /// (the field editor turns Escape into completion). Popovers and menus are other windows and keep
  /// their own Escape; text still being composed in an input method keeps it too.
  func handlePanelEscape(_ event: NSEvent) -> Bool {
    guard let panel, event.window === panel, event.keyCode == 53,
      event.modifierFlags.intersection(.deviceIndependentFlagsMask).subtracting([.function, .numericPad, .capsLock]).isEmpty
    else { return false }
    if let editor = panel.firstResponder as? NSTextView, editor.hasMarkedText() { return false }
    cancel()
    return true
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
    // A click in another app or on the desktop closes the panel, like a menu. A press on our own
    // status-item button is not an outside click: the menu bar lives in another process, so this
    // monitor sees the press before the button action runs. If it closed first, the action would see
    // a closed panel and reopen it, and the icon could open the panel but never close it.
    if let monitor = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown], handler: { [weak self] event in
      let location = NSEvent.mouseLocation
      let type = event.type
      Task { @MainActor in
        guard let self else { return }
        let buttonActs = type == .leftMouseDown || type == .rightMouseDown
        if !StatusItemClick.isOutsideClick(at: CGPoint(x: location.x, y: location.y),
          buttonFrame: self.statusButtonFrame, buttonActs: buttonActs) { return }
        self.close()
      }
    }) { monitors.append(monitor) }
    // Escape in the panel (only while it is open; the monitor is removed with it).
    if let monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown, handler: { [weak self] event in
      MainActor.assumeIsolated { self?.handlePanelEscape(event) == true } ? nil : event
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

  /// Our status-item button's frame in screen coordinates (.zero when it has no window yet,
  /// which dismisses as before).
  private var statusButtonFrame: CGRect {
    guard let button = statusItem.button, let window = button.window else { return .zero }
    let frame = window.convertToScreen(button.convert(button.bounds, to: nil))
    return CGRect(x: frame.minX, y: frame.minY, width: frame.width, height: frame.height)
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
