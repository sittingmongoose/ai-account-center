import CoreGraphics
import Foundation

/// Menu-bar click routing for the tray panel, shared by the app and the offline checks.
///
/// A press that our own status-item button will act on must not count as an outside click: the
/// menu bar lives in another process, so the panel's global mouse monitor sees the press as well
/// as the button action. If the monitor closed first, the action would see a closed panel and
/// reopen it, and the icon could open the panel but never close it. Presses off the button, and
/// presses the button ignores, still dismiss the panel.
public enum StatusItemClick {
  /// Whether a mouse-down at `point` (screen coordinates) dismisses the panel when the button
  /// occupies `buttonFrame`. `buttonActs` is true for the press types the button sends its
  /// action on (left and right); other buttons keep their old dismiss behaviour.
  public static func isOutsideClick(at point: CGPoint, buttonFrame: CGRect, buttonActs: Bool) -> Bool {
    if buttonActs && buttonFrame.contains(point) { return false }
    return true
  }
}
