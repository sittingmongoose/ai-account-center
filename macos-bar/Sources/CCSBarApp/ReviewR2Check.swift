import AppKit
import SwiftUI
import CCSBarCore

/// Offline checks of the actual row layout, tooltip views and sign-out hand-off.
@MainActor
enum ReviewR2Check {
  static func run() -> Never {
    var failures: [String] = []
    func check(_ ok: Bool, _ name: String) { if !ok { failures.append(name) } }
    do {
      func account(_ id: String, provider: String = "antigravity", hidden: Bool = false) -> [String: Any] {
        ["id": id, "provider": provider, "providerLabel": provider, "label": id,
          "email": "\(id)@example.com", "plan": "pro", "platform": "ubuntu", "host": "nas1",
          "source": "Fixture", "status": hidden ? "unavailable" : "ok", "isActive": false,
          "trayHidden": hidden, "sampledAt": "2026-10-15T12:00:00Z",
          "windows": [["key": "gemini-5h", "label": "Gemini", "usedPercent": 20, "poolId": "watched"]],
          "capabilities": ["antigravityProfileId": id, "antigravityHostIds": ["ubuntu"],
            "antigravityCanActivate": true, "claudePlatforms": []]]
      }
      func dashboard(_ accounts: [[String: Any]]) throws -> AccountDashboard {
        let object: [String: Any] = ["schemaVersion": 1, "updatedAt": "2026-10-15T12:00:00Z", "accounts": accounts,
          "codexAutoSwitch": ["enabled": false, "thresholdPercent": 5, "pollIntervalSeconds": 60,
            "outcome": "disabled", "message": "", "activationInProgress": false],
          "antigravityAutoSwitch": ["enabled": true, "thresholdUsedPercent": 95, "requestedPoolId": "watched"]]
        return try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: object))
      }
      let sample = try dashboard([account("shown"), account("hidden", hidden: true)])
      AlignmentProbe.frames = [:]
      let (host, _, window) = PreviewRenderer.host(sample, options: PreviewRenderer.Options([]))
      defer { window.orderOut(nil) }
      let text = try PreviewRenderer.recognizedText(host).joined(separator: " ")
      check(AlignmentProbe.frames["antigravity-auto|header"] != nil, "hidden second account keeps header controls")
      check(AlignmentProbe.frames["meter|shown|gemini-5h|notch"] != nil, "hidden second account keeps watched-pool notch")
      check(AlignmentProbe.frames["slot|activate-shown|label"] != nil || text.contains("Activate"), "hidden second account keeps Activate")
      check(!text.contains("hidden@example.com") && text.lowercased().contains("nas1"), "only shown rows render with host provenance")

      var pro = account("claude", provider: "claude")
      let proSample = try dashboard([pro])
      check(!SectionLayout.make("claude", accounts: proSample.accounts).columns.contains { $0.key == "fable" }, "Pro-only header has no Fable column")
      pro["plan"] = "max"
      let maxSample = try dashboard([pro])
      check(SectionLayout.make("claude", accounts: maxSample.accounts).columns.contains { $0.key == "fable" }, "Max header has Fable without a reported window")

      let cached = try JSONDecoder().decode(AccountQuotaWindow.self, from: Data("""
        {"key":"credits","label":"Credits","kind":"balance","remaining":4,"status":"cached","sampledAt":"2026-09-01T12:00:00Z","usedPercent":20}
        """.utf8))
      let marker = AccountFormatting.cachedSample(status: cached.status, sampledAt: cached.sampledAt)!
      check(AmountRow(window: cached).sub?.contains(marker) == true, "amount view shows retained window sample")
      check(MeterView(key: "cached", window: cached, sampledAt: "2026-10-15T12:00:00Z").helpText.contains(marker), "compact tooltip uses retained window time")
      let undated = try JSONDecoder().decode(AccountQuotaWindow.self, from: Data("{\"key\":\"credits\",\"label\":\"Credits\",\"status\":\"cached\"}".utf8))
      check(AmountRow(window: undated).sub?.contains("Sample time unavailable") == true
        && MeterView(key: "undated", window: undated, sampledAt: sample.updatedAt).helpText.contains("Sample time unavailable"), "cached views never borrow account time")

      check(MeterView(key: "pending", window: undated, sampledAt: sample.updatedAt, pendingReset: Date()).helpText.contains("Sample time unavailable"), "reset-pending cached tooltip keeps unknown window sample")

      let prefs = TrayPreferences(defaults: UserDefaults(suiteName: "aac-review-r2")!, persist: false)
      prefs.menuBarProvider = "antigravity"
      let menu = MenuBarState()
      var active = account("active")
      active["isActive"] = true
      menu.snapshot = MenuBarSnapshot(dashboard: try dashboard([active]), refreshFailed: true)
      check(menu.reading(prefs) != nil && menu.toolTip(prefs).contains("last sample"), "failed refresh retains number and marks tooltip")
      menu.snapshot.signInActive = true
      menu.snapshot.signInHelp = "Signed out"
      check(menu.reading(prefs) == nil && menu.toolTip(prefs).contains("Signed out") && !menu.toolTip(prefs).contains("last sample"), "sign-out removes old number and sample tooltip")

      let folder = FileManager.default.temporaryDirectory.appendingPathComponent("aac-r2-\(UUID().uuidString)")
      defer { try? FileManager.default.removeItem(at: folder) }
      let model = AccountsViewModel(preview: sample, session: ConnectionSession(fileURL: folder.appendingPathComponent("connection.json")))
      model.handleSignedOut(SignedOutNote(reason: "device_revoked", at: sample.updatedAt))
      check(model.dashboard == nil && model.menuBar.snapshot.dashboard == nil, "final remote sign-out clears Settings facts and menu sample")
      let disconnected = BarConnection(baseURL: URL(string: "http://127.0.0.1")!, username: "fixture", password: "fixture-only")
        .signingOut(SignedOutNote(reason: "disconnected", at: sample.updatedAt, disconnectTold: false, credentialKind: "password"))
      let file = folder.appendingPathComponent("connection.json")
      try ConnectionStore.write(disconnected, to: file)
      let relaunched = AccountsViewModel(session: ConnectionSession(fileURL: file))
      check(relaunched.signIn.disconnectedCredential == "password" && !relaunched.signIn.disconnectTold, "relaunch restores Disconnect credential kind and outcome")
      let signInHost = try PreviewRenderer.signInHost(model: relaunched.signIn, appearance: "light")
      let disconnectedText = try PreviewRenderer.recognizedText(signInHost).joined(separator: " ").lowercased()
      signInHost.window?.orderOut(nil)
      check(disconnectedText.contains("saved password") && !disconnectedText.contains("forgot its device key") && !disconnectedText.contains("revoked it"), "password Disconnect uses password wording")
    } catch { failures.append("view check error: \(error)") }
    if failures.isEmpty { print("PASS native review row, tooltip and sign-out checks") }
    else { for failure in failures { print("FAIL \(failure)") } }
    exit(failures.isEmpty ? 0 : 1)
  }
}
