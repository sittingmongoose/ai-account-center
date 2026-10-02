import SwiftUI
import AppKit
import CCSBarCore

struct PendingCodexSwitch: Identifiable {
  let accountID: String
  let profile: String
  let identity: String
  let confirmation: CodexSwitchConfirmation
  var id: String { confirmation.token }
  static let requiredWarning = "Stopping these programs interrupts active Codex work. AI Account Center will switch the account and restart the affected programs without replaying prompts."
  /// The fixed warning, plus the server's own only when it says something else.
  var warning: String {
    let legacy = "Stopping these programs interrupts active Codex work. CCS will switch the account and restart the affected programs without replaying prompts."
    return [legacy, Self.requiredWarning].contains(confirmation.warning)
      ? Self.requiredWarning : "\(Self.requiredWarning)\n\n\(confirmation.warning)"
  }
  var processes: [CodexSwitchProcess] { confirmation.processes }
}

struct PendingAntigravitySwitch: Identifiable {
  let accountID: String
  let profile: String
  let identity: String
  let confirmation: AntigravitySwitchConfirmation
  var id: String { confirmation.token }
  var warning: String { AntigravitySwitchConfirmation.warning }
  var processes: [CodexSwitchProcess] { confirmation.processes }
}

@MainActor
final class AccountsViewModel: ObservableObject {
  @Published private(set) var dashboard: AccountDashboard?
  @Published private(set) var isRefreshing = false
  @Published private(set) var busyAction: String?
  @Published private(set) var message: String?
  @Published private(set) var connection: BarConnection?
  @Published private(set) var connected = false
  @Published private(set) var lastSyncedAt: Date?
  @Published var pendingCodexSwitch: PendingCodexSwitch?
  @Published var pendingAntigravitySwitch: PendingAntigravitySwitch?
  /// Meter readings as they were when the panel last closed: a later open animates only what changed.
  var lastShown: [String: Double] = [:]
  /// The first open of a session sweeps every meter from zero.
  var hasOpenedThisSession = false
  let isPreview: Bool
  private var client: AccountsClient?
  private var timer: Timer?

  /// The first-run connect screen with no connection file and no network, for offline renders.
  init(previewWithoutConnection: Bool) {
    isPreview = true
  }

  init(preview: AccountDashboard? = nil) {
    if let preview {
      isPreview = true
      dashboard = preview
      connected = true
      lastSyncedAt = AccountFormatting.date(preview.updatedAt)
      return
    }
    isPreview = false
    configure()
    configureRefreshTimer(60)
  }

  private func configureRefreshTimer(_ interval: TimeInterval) {
    if timer?.timeInterval == interval { return }
    timer?.invalidate()
    timer = Timer.scheduledTimer(withTimeInterval: interval, repeats: true) { [weak self] _ in
      Task { @MainActor in
        guard let self else { return }
        if self.client == nil { self.configure() }
        else { await self.refresh() }
      }
    }
  }

  func menuBarReading(_ prefs: TrayPreferences) -> MenuBarReading? {
    MenuBarReading.make(dashboard: dashboard, source: prefs.menuBarSource, mode: prefs.menuBarMode)
  }

  /// No saved connection and nothing to show: the panel offers the connect form.
  var needsConnection: Bool { connection == nil && dashboard == nil }

  var hasPendingConfirmation: Bool { pendingCodexSwitch != nil || pendingAntigravitySwitch != nil }

  func configure() {
    pendingCodexSwitch = nil
    pendingAntigravitySwitch = nil
    connected = false
    do {
      connection = try BarConnection.load()
      client = AccountsClient(connection: connection!)
      message = nil
      Task { await refresh() }
    } catch {
      connection = nil
      client = nil
      message = FileManager.default.fileExists(atPath: BarConnection.configURL.path)
        ? error.localizedDescription : nil
    }
  }

  /// Saves the dashboard address and login privately (0700 directory, 0600 file), then reconnects.
  func saveConnection(baseURL: String, username: String, password: String) throws {
    guard let url = URL(string: baseURL.trimmingCharacters(in: .whitespacesAndNewlines)),
      ["http", "https"].contains(url.scheme ?? ""), url.host != nil,
      url.user == nil, url.password == nil, url.query == nil, url.fragment == nil,
      url.path.isEmpty || url.path == "/", !username.isEmpty, !password.isEmpty
    else { throw BarClientError.invalidConnection }
    let config = BarConnection(baseURL: url, username: username, password: password)
    let directory = BarConnection.configURL.deletingLastPathComponent()
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
      attributes: [.posixPermissions: 0o700])
    try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
    try JSONEncoder().encode(config).write(to: BarConnection.configURL, options: .atomic)
    try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: BarConnection.configURL.path)
    configure()
  }

  func refresh(force: Bool = false) async {
    guard !isPreview, !isRefreshing, busyAction == nil, !hasPendingConfirmation, let client else { return }
    isRefreshing = true
    defer { isRefreshing = false }
    do {
      let value = try await client.dashboard(refresh: force)
      guard value.schemaVersion == 1 else { throw BarClientError.decoding }
      dashboard = value
      configureRefreshTimer(value.settings?.validatedInterval ?? 60)
      connected = true
      lastSyncedAt = Date()
      message = nil
      recordStatus(connected: true)
    } catch {
      connected = false
      message = error.localizedDescription
      recordStatus(connected: false)
    }
  }

  private func recordStatus(connected: Bool) {
    let file = BarConnection.configURL.deletingLastPathComponent().appendingPathComponent("native-status.json")
    let value: [String: Any] = [
      "connected": connected,
      "checkedAt": ISO8601DateFormatter().string(from: Date()),
      "accountCount": dashboard?.accounts.count ?? 0,
      "providers": Array(Set(dashboard?.accounts.map(\.provider) ?? [])).sorted(),
      "codexAutomaticEnabled": dashboard?.codexAutoSwitch.enabled as Any? ?? NSNull(),
      "codexThresholdRemaining": dashboard?.codexAutoSwitch.thresholdPercent as Any? ?? NSNull(),
    ]
    do {
      try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]).write(to: file, options: .atomic)
      try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
    } catch { /* Diagnostic persistence never affects account controls or prints private data. */ }
  }

  /// Every meter reading now on screen, keyed by account and window.
  var currentReadings: [String: Double] {
    var values: [String: Double] = [:]
    for account in dashboard?.visibleAccounts ?? [] {
      for window in account.visibleWindows {
        if let used = window.meterUsedPercent { values["\(account.id)|\(window.key)"] = used }
      }
    }
    return values
  }

  // MARK: Codex

  func activate(_ account: DashboardAccount) {
    guard let profile = account.capabilities.codexProfile, dashboard?.canActivateCodex(account) == true else { return }
    activateCodex(id: account.id, profile: profile, identity: account.identity)
  }

  func cancelCodexSwitch() { pendingCodexSwitch = nil }

  func confirmCodexSwitch(_ offer: PendingCodexSwitch) {
    pendingCodexSwitch = nil
    guard offer.confirmation.isValid(for: offer.profile) else {
      message = "The switching confirmation expired. Activate again to review the current programs."
      return
    }
    activateCodex(id: offer.accountID, profile: offer.profile, identity: offer.identity,
      confirmationToken: offer.confirmation.token)
  }

  private func activateCodex(id: String, profile: String, identity: String, confirmationToken: String? = nil) {
    guard busyAction == nil, !hasPendingConfirmation, !isRefreshing, let client else { return }
    busyAction = id
    Task {
      var succeeded = false
      do {
        try await client.activateCodex(profile: profile, confirmationToken: confirmationToken)
        message = nil
        succeeded = true
      } catch BarClientError.codexConfirmation(let confirmation) {
        pendingCodexSwitch = PendingCodexSwitch(accountID: id, profile: profile,
          identity: identity, confirmation: confirmation)
        message = nil
      } catch { message = error.localizedDescription }
      busyAction = nil
      if succeeded { await refresh() }
    }
  }

  // MARK: Antigravity (Ubuntu runtime, the routes from 9cf75fbe)

  func activateAntigravity(_ account: DashboardAccount) {
    guard let profile = account.antigravityProfile, dashboard?.canActivateAntigravity(account) == true else { return }
    runAntigravity(id: account.id, profile: profile, identity: account.identity, offer: nil)
  }

  func cancelAntigravitySwitch() { pendingAntigravitySwitch = nil }

  func confirmAntigravitySwitch(_ offer: PendingAntigravitySwitch) {
    pendingAntigravitySwitch = nil
    guard offer.confirmation.isValid(for: offer.profile) else {
      message = "This Antigravity confirmation expired. Activate again to review the running programs."
      return
    }
    runAntigravity(id: offer.accountID, profile: offer.profile, identity: offer.identity, offer: offer.confirmation)
  }

  static let antigravityApprovalFailed = "The Antigravity switch could not complete. Click Activate again to review the running programs."

  /// One Antigravity attempt. A reviewed approval is sent once: a fresh offer that comes back from it is
  /// never adopted (it needs a new review), and an account other than the reviewed one is reported, as
  /// the dashboard does. Any finished attempt refreshes, because the server may have changed state.
  private func runAntigravity(id: String, profile: String, identity: String, offer approval: AntigravitySwitchConfirmation?) {
    guard busyAction == nil, !hasPendingConfirmation, !isRefreshing, let client else { return }
    busyAction = id
    Task {
      var succeeded = false
      do {
        if let approval {
          let result = try await client.confirmAntigravity(profile: profile, confirmationToken: approval.token)
          if let email = result.email, email.lowercased() != approval.email.lowercased() {
            message = Self.antigravityApprovalFailed
          } else {
            message = nil
            succeeded = true
          }
        } else {
          try await client.activateAntigravity(profile: profile)
          message = nil
          succeeded = true
        }
      } catch BarClientError.antigravityConfirmation(let offer) {
        if approval == nil {
          pendingAntigravitySwitch = PendingAntigravitySwitch(accountID: id, profile: profile, identity: identity, confirmation: offer)
          message = nil
        } else {
          message = Self.antigravityApprovalFailed
        }
      } catch { message = error.localizedDescription }
      busyAction = nil
      // A consumed approval may have changed the Ubuntu runtime even when it failed; show the truth, but
      // keep the failure text that a successful refresh would otherwise clear.
      if pendingAntigravitySwitch == nil && (succeeded || approval != nil) {
        let note = message
        await refresh(force: succeeded)
        if let note { message = note }
      }
    }
  }

  /// Enabling needs the quota pool chosen in the dashboard; the server revalidates every write.
  var antigravityAutoCanEnable: Bool {
    guard let status = dashboard?.antigravityAutoSwitch else { return false }
    let accounts = dashboard?.visibleAccounts.filter { $0.provider == "antigravity" } ?? []
    return accounts.count >= 2 && status.requestedPoolId != nil && status.activationInProgress != true
  }

  func toggleAntigravityAutomatic(_ enabled: Bool) {
    guard dashboard?.antigravityAutoSwitch != nil, !enabled || antigravityAutoCanEnable else { return }
    perform(id: "antigravity-automatic") { client in try await client.setAntigravityAutomaticSwitching(enabled: enabled) }
  }

  func setAntigravityThreshold(usedPercent: Int) {
    guard dashboard?.antigravityAutoSwitch != nil, (1...99).contains(usedPercent) else { return }
    perform(id: "antigravity-automatic") { client in
      try await client.setAntigravityAutomaticSwitching(thresholdUsedPercent: usedPercent)
    }
  }

  // MARK: Claude and Codex automatic switching

  func openClaude(_ account: DashboardAccount, platform: String = "mac") {
    guard account.provider == "claude", let profile = account.capabilities.claudeProfileId,
      account.capabilities.claudePlatforms.contains(platform) else { return }
    perform(id: "\(account.id)|\(platform)") { client in try await client.openClaude(profile: profile, platform: platform) }
  }

  func toggleAutomaticSwitching(_ enabled: Bool) {
    perform(id: "automatic") { client in try await client.setAutomaticSwitching(enabled: enabled) }
  }

  /// The trays always show % used; Codex stores the threshold as % remaining.
  func setAutomaticThreshold(usedPercent: Int) {
    guard let status = dashboard?.codexAutoSwitch, (1...99).contains(usedPercent) else { return }
    perform(id: "automatic") { client in
      try await client.setAutomaticSwitching(enabled: status.enabled, thresholdPercent: 100 - usedPercent)
    }
  }

  private func perform(id: String, operation: @escaping (AccountsClient) async throws -> Void) {
    guard busyAction == nil, !hasPendingConfirmation, let client else { return }
    busyAction = id
    Task {
      var succeeded = false
      do {
        try await operation(client)
        message = nil
        succeeded = true
      } catch { message = error.localizedDescription }
      busyAction = nil
      if succeeded { await refresh() }
    }
  }

  func openDashboard() {
    guard let base = connection?.baseURL else { return }
    NSWorkspace.shared.open(base)
  }
}
