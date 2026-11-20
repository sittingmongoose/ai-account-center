import AppKit
import SwiftUI
import CCSBarCore

/// Offline view checks of the Antigravity section and of the retired account switching (2026-11-20), against
/// `Tests/Fixtures/antigravity-cli-services.json`: the real panel and Details are rendered offscreen and read back with
/// on-device text recognition. No sign-in, network or screenshot of the desktop.
///
/// - The tray shows T3's Antigravity CLI quota only; the app's numbers appear nowhere in it (row, Details, menu bar).
/// - A failed T3 service and an older app-only dashboard read Unavailable.
/// - No switch control, auto-switch fact or "active account" prompt renders; the dashboard's Active mark stays.
@MainActor
enum AntigravityTrayCheck {
  static let fixture = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
    .deletingLastPathComponent().appendingPathComponent("Tests/Fixtures/antigravity-cli-services.json")
  /// Every app number in the fixture: the tray must never print one.
  static let appNumbers = ["11.11", "0.09", "22.22", "37.83", "44.44", "55.55", "66.66", "77.77", "88.88", "99.99",
    "51.51", "61.61", "71.71", "81.81", "91.91"]

  static func dashboard(_ edit: ([[String: Any]]) -> [[String: Any]] = { $0 }) throws -> AccountDashboard {
    var object = try JSONSerialization.jsonObject(with: Data(contentsOf: fixture)) as! [String: Any]
    object["accounts"] = edit(object["accounts"] as! [[String: Any]])
    return try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: object))
  }

  static func panelText(_ sample: AccountDashboard) throws -> String {
    AlignmentProbe.frames = [:]
    let (host, _, window) = PreviewRenderer.host(sample, options: PreviewRenderer.Options([]))
    defer { window.orderOut(nil) }
    return try PreviewRenderer.recognizedText(host).joined(separator: " ")
  }

  static func detailsText(_ sample: AccountDashboard, _ id: String) throws -> String {
    let host = try PreviewRenderer.detailsHost(sample, accountID: id, options: PreviewRenderer.Options([]))
    defer { host.window?.orderOut(nil) }
    return try PreviewRenderer.recognizedText(host).joined(separator: " ")
  }

  static func count(_ text: String, _ word: String) -> Int { text.components(separatedBy: word).count - 1 }

  static func run(_ check: (Bool, String) -> Void) throws {
    func noAppNumber(_ text: String) -> Bool { !appNumbers.contains { text.contains($0) } }
    let all = try dashboard()

    // CLI-only rows: T3's readings, the section says they are T3's, and no app number anywhere.
    let panel = try panelText(all)
    check(panel.contains("29.68%") && panel.contains("8.48%") && panel.contains("6.25%"), "CLI rows show T3's readings")
    check(noAppNumber(panel), "no app number renders in the panel")
    check(panel.contains("In T3"), "the Antigravity section says its quota is T3's")
    let bothDetails = try detailsText(all, "antigravity:t3-both")
    check(bothDetails.contains("29.68%") && noAppNumber(bothDetails), "Details shows T3's pools and no app number")

    // CLI unavailable: Unavailable in the row (five T3 cells, including Argon) and a notice in Details.
    let down = try dashboard { $0.filter { $0["provider"] as? String != "antigravity" || $0["id"] as? String == "antigravity:t3-down" } }
    let downPanel = try panelText(down)
    check(noAppNumber(downPanel) && count(downPanel, "Unavailable") >= 5, "a failed T3 service reads Unavailable, never the app's number")
    let downDetails = try detailsText(down, "antigravity:t3-down")
    check(noAppNumber(downDetails) && downDetails.contains("could not be read"), "Details names the failed T3 service, no app number")

    // An older app-only dashboard: T3's four columns read Unavailable; Details says the dashboard does not report T3's.
    let old = try dashboard { $0.filter { $0["provider"] as? String != "antigravity" || $0["id"] as? String == "antigravity:app-only" } }
    let oldPanel = try panelText(old)
    check(noAppNumber(oldPanel) && count(oldPanel, "Unavailable") >= 4, "an app-only dashboard reads Unavailable, never the app's numbers")
    let oldDetails = try detailsText(old, "antigravity:app-only")
    check(noAppNumber(oldDetails) && oldDetails.contains("does not report"), "Details says an older dashboard does not report T3's quota")

    // No switching UI: two Antigravity accounts and one, none reported active; then one active.
    let none = try dashboard { $0.map { account in var account = account; account["isActive"] = false; return account } }
    let noneText = try panelText(none)
    let single = try dashboard { accounts in
      accounts.filter { $0["provider"] as? String != "antigravity" || $0["id"] as? String == "antigravity:t3-both" }
        .map { account in var account = account; account["isActive"] = false; return account }
    }
    let singleText = try panelText(single)
    for (name, text) in [("two accounts", noneText), ("one account", singleText)] {
      let lower = text.lowercased()
      check(!lower.contains("activate") && !lower.contains("switch") && !lower.contains("active account")
        && !lower.contains("as active"),
        "\(name), none active: no switch control, auto-switch fact or active-account prompt renders")
    }
    check(!AlignmentProbe.frames.keys.contains { $0.lowercased().contains("activate") || $0.contains("auto") },
      "no switch control is laid out")
    let active = try dashboard { accounts in
      accounts.map { account in
        var account = account
        if account["provider"] as? String == "antigravity" { account["isActive"] = account["id"] as? String == "antigravity:t3-both" }
        return account
      }
    }
    let activeText = try panelText(active)
    check(!activeText.contains("agy-both active") && !activeText.lowercased().contains("switch"),
      "an active Antigravity account adds no header switch prompt")
    check(count(activeText, "Active") >= 2, "the dashboard's Active mark stays on the live Codex and Antigravity logins")
  }
}
