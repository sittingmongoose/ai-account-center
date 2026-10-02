import AppKit
import Carbon.HIToolbox

/// A system-wide shortcut through Carbon's RegisterEventHotKey. It needs no Accessibility or Input
/// Monitoring permission, because the system delivers only this one key combination to the app.
@MainActor
final class GlobalHotKey {
  static let signature: OSType = 0x4141_4354 // "AACT"
  private var hotKey: EventHotKeyRef?
  var isRegistered: Bool { hotKey != nil }
  private var handler: EventHandlerRef?
  private let action: () -> Void

  init(action: @escaping () -> Void) {
    self.action = action
  }

  /// Option-Command-A by default. Returns false when the system refused the combination.
  @discardableResult
  func register(keyCode: UInt32 = UInt32(kVK_ANSI_A), modifiers: UInt32 = UInt32(optionKey | cmdKey)) -> Bool {
    unregister()
    var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
    let context = Unmanaged.passUnretained(self).toOpaque()
    let installed = InstallEventHandler(GetApplicationEventTarget(), { _, event, userData in
      guard let event, let userData else { return OSStatus(eventNotHandledErr) }
      var id = EventHotKeyID()
      let status = GetEventParameter(event, EventParamName(kEventParamDirectObject), EventParamType(typeEventHotKeyID),
        nil, MemoryLayout<EventHotKeyID>.size, nil, &id)
      guard status == noErr, id.signature == GlobalHotKey.signature else { return OSStatus(eventNotHandledErr) }
      let target = Unmanaged<GlobalHotKey>.fromOpaque(userData).takeUnretainedValue()
      DispatchQueue.main.async { target.action() }
      return noErr
    }, 1, &spec, context, &handler)
    guard installed == noErr else { return false }
    let registered = RegisterEventHotKey(keyCode, modifiers, EventHotKeyID(signature: Self.signature, id: 1),
      GetApplicationEventTarget(), 0, &hotKey)
    if registered != noErr {
      unregister()
      return false
    }
    return true
  }

  func unregister() {
    if let hotKey { UnregisterEventHotKey(hotKey) }
    hotKey = nil
    if let handler { RemoveEventHandler(handler) }
    handler = nil
  }
}
