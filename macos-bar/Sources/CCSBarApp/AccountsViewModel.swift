import SwiftUI
import AppKit
import CCSBarCore

struct PendingCodexSwitch: Identifiable {
  let accountID: String
  let profile: String
  let identity: String
  let confirmation: CodexSwitchConfirmation
  var id: String { confirmation.token }
  var warningText: String {
    let programs = confirmation.processes.map { "• \($0.label) (PID \($0.pid), \($0.role))" }.joined(separator: "\n")
    let legacyWarning = "Stopping these programs interrupts active Codex work. CCS will switch the account and restart the affected programs without replaying prompts."
    let requiredWarning = "Stopping these programs interrupts active Codex work. AI Account Center will switch the account and restart the affected programs without replaying prompts."
    let warning = [legacyWarning, requiredWarning].contains(confirmation.warning) ? requiredWarning : "\(requiredWarning)\n\n\(confirmation.warning)"
    return "Switch to \(identity)?\n\nAffected programs:\n\(programs)\n\n\(warning)"
  }
}

@MainActor
final class AccountsViewModel: ObservableObject {
  @Published private(set) var dashboard: AccountDashboard?
  @Published private(set) var isRefreshing = false
  @Published private(set) var busyAction: String?
  @Published private(set) var message: String?
  @Published private(set) var connection: BarConnection?
  @Published private(set) var connected = false
  @Published var pendingCodexSwitch: PendingCodexSwitch?
  private var client: AccountsClient?
  private var timer: Timer?

  init(preview: AccountDashboard? = nil) {
    if let preview {
      dashboard = preview
      connected = true
      return
    }
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

  var statusTitle: String {
    guard let row = dashboard?.accounts.first(where: { $0.provider == "codex" && $0.isActive }),
      let remaining = row.visibleWindows.compactMap(\.remainingPercent).min()
    else { return "" }
    return "\(remaining.formatted(.number.precision(.fractionLength(0...2))))%"
  }

  func configure() {
    pendingCodexSwitch = nil
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
        ? error.localizedDescription : "Connect to your AI Account Center dashboard to see your accounts."
    }
  }

  func refresh(force: Bool = false) async {
    guard !isRefreshing, busyAction == nil, pendingCodexSwitch == nil, let client else { return }
    isRefreshing = true
    defer { isRefreshing = false }
    do {
      let value = try await client.dashboard(refresh: force)
      guard value.schemaVersion == 1 else { throw BarClientError.decoding }
      dashboard = value
      configureRefreshTimer(value.settings?.validatedInterval ?? 60)
      connected = true
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

  var activeCodexIdentity: String? {
    dashboard?.accounts.first(where: { $0.provider == "codex" && $0.isActive })?.identity
  }

  func activate(_ account: DashboardAccount) {
    guard let profile = account.capabilities.codexProfile, account.canActivate else { return }
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
    guard busyAction == nil, pendingCodexSwitch == nil, !isRefreshing, let client else { return }
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

  func openClaude(_ account: DashboardAccount, platform: String = "mac") {
    guard account.provider == "claude", let profile = account.capabilities.claudeProfileId,
      account.capabilities.claudePlatforms.contains(platform) else { return }
    perform(id: account.id) { client in try await client.openClaude(profile: profile, platform: platform) }
  }

  func toggleAutomaticSwitching(_ enabled: Bool) {
    perform(id: "automatic") { client in try await client.setAutomaticSwitching(enabled: enabled) }
  }

  func setAutomaticThreshold(usedPercent: Int) {
    guard let status = dashboard?.codexAutoSwitch, (1...99).contains(usedPercent) else { return }
    perform(id: "automatic") { client in
      try await client.setAutomaticSwitching(enabled: status.enabled, thresholdPercent: 100 - usedPercent)
    }
  }

  private func perform(id: String, operation: @escaping (AccountsClient) async throws -> Void) {
    guard busyAction == nil, pendingCodexSwitch == nil, let client else { return }
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
