import Foundation

public struct AccountDashboard: Decodable, Sendable {
  public let schemaVersion: Int
  public let updatedAt: String
  public let accounts: [DashboardAccount]
  public let codexAutoSwitch: CodexAutoSwitch
  public let settings: AccountRefreshSettings?
  /// Antigravity's own automatic-switching policy (thresholdUsedPercent is % USED). A missing or
  /// malformed value is ignored rather than failing the whole dashboard.
  public let antigravityAutoSwitch: AntigravityAutoSwitch?
  /// Providers hidden on the dashboard ("Show on dashboard" off). The trays only report them: the dashboard and
  /// the trays have independent switches (Jared, 2026-10-02), so this list hides nothing here.
  public let hiddenProviders: Set<String>
  /// Providers hidden in the trays ("Show in tray" off): `providers[].trayVisible == false`, or a provider in
  /// `settings.trayHiddenProviders`. A missing field means visible.
  public let trayHiddenProviders: Set<String>
  /// The dashboard's own provider order and labels, when it sends them (`providers[]`).
  public let providers: [DashboardProvider]

  private enum CodingKeys: String, CodingKey {
    case schemaVersion, updatedAt, accounts, codexAutoSwitch, settings, antigravityAutoSwitch, hiddenProviders, providers
  }

  public init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    schemaVersion = try container.decode(Int.self, forKey: .schemaVersion)
    updatedAt = try container.decode(String.self, forKey: .updatedAt)
    accounts = try container.decode([DashboardAccount].self, forKey: .accounts)
    codexAutoSwitch = try container.decode(CodexAutoSwitch.self, forKey: .codexAutoSwitch)
    settings = try container.decodeIfPresent(AccountRefreshSettings.self, forKey: .settings)
    let antigravity = (try? container.decodeIfPresent(AntigravityAutoSwitch.self, forKey: .antigravityAutoSwitch)) ?? nil
    antigravityAutoSwitch = antigravity?.isValid == true ? antigravity : nil
    let topLevel = (try? container.decodeIfPresent([String].self, forKey: .hiddenProviders)) ?? nil
    hiddenProviders = Set((topLevel ?? []) + (settings?.hiddenProviders ?? []))
    // One malformed provider entry never fails the dashboard: the list is read entry by entry.
    let rawProviders = (try? container.decodeIfPresent([FailableProvider].self, forKey: .providers)) ?? nil
    providers = (rawProviders ?? []).compactMap(\.value)
    trayHiddenProviders = Set(providers.filter { $0.trayVisible == false }.map(\.id) + (settings?.trayHiddenProviders ?? []))
  }

  /// Accounts the trays show: every account whose provider is shown in the tray (`providers[].trayVisible`) and
  /// that is not hidden in the trays itself (`accounts[].trayHidden`). The dashboard's own switches
  /// (`providers[].visible`, `accounts[].hidden`) are never read: the two sets are independent (Jared, 2026-10-02).
  public var visibleAccounts: [DashboardAccount] {
    accounts.filter { !trayHiddenProviders.contains($0.provider) && $0.trayHidden != true }
  }

  /// Accounts hidden in the trays (`accounts[].trayHidden`), one by one or with their provider.
  public var trayHiddenAccounts: [DashboardAccount] { accounts.filter { $0.trayHidden == true } }

  /// Codex Activate is offered for an inactive saved profile while no automatic switch is running.
  public func canActivateCodex(_ account: DashboardAccount) -> Bool {
    account.canActivate && !codexAutoSwitch.activationInProgress
  }

  /// Every Antigravity account the dashboard reports, tray-hidden ones included. "Show in tray" for one account only
  /// changes what the panel lists; the server keeps that account as a switch candidate, so the "second account"
  /// rule counts it too.
  public var antigravityAccountCount: Int { accounts.filter { $0.provider == "antigravity" }.count }

  /// Antigravity Activate needs a second account to switch to (shown in the tray or not), a runtime-verified Ubuntu
  /// profile, no activation already running, manual or automatic, and the provider shown in the tray.
  public func canActivateAntigravity(_ account: DashboardAccount) -> Bool {
    account.canActivateAntigravity && antigravityAutoSwitch?.activationInProgress != true
      && !trayHiddenProviders.contains("antigravity") && antigravityAccountCount >= 2
  }
}

public struct AccountRefreshSettings: Decodable, Sendable {
  public let refreshIntervalSeconds: Int
  public let hiddenProviders: [String]?
  public let trayHiddenProviders: [String]?
  public var validatedInterval: TimeInterval {
    TimeInterval((30...3600).contains(refreshIntervalSeconds) ? refreshIntervalSeconds : 60)
  }

  private enum CodingKeys: String, CodingKey { case refreshIntervalSeconds, hiddenProviders, trayHiddenProviders }

  public init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    refreshIntervalSeconds = try container.decode(Int.self, forKey: .refreshIntervalSeconds)
    hiddenProviders = (try? container.decodeIfPresent([String].self, forKey: .hiddenProviders)) ?? nil
    trayHiddenProviders = (try? container.decodeIfPresent([String].self, forKey: .trayHiddenProviders)) ?? nil
  }
}

/// One entry of the dashboard's `providers[]` (CLIENT API SHEET 4.4), only what the trays read.
public struct DashboardProvider: Decodable, Sendable, Equatable {
  public let id: String
  public let label: String?
  public let order: Int?
  /// Shown on the dashboard. The trays do not follow it.
  public let visible: Bool?
  /// Shown in the trays; missing means shown.
  public let trayVisible: Bool?
}

private struct FailableProvider: Decodable {
  let value: DashboardProvider?
  init(from decoder: Decoder) throws { value = try? DashboardProvider(from: decoder) }
}

/// Antigravity automatic switching as the dashboard reports it (Ubuntu only).
public struct AntigravityAutoSwitch: Decodable, Sendable {
  public let enabled: Bool
  /// Percentage USED, unlike Codex's remaining-percent threshold.
  public let thresholdUsedPercent: Int
  public let pollIntervalSeconds: Int?
  public let requestedPoolId: String?
  public let outcome: String?
  public let message: String?
  public let activationInProgress: Bool?
  public let lastCheckedAt: String?
  public let lastSwitchedAt: String?

  public var isValid: Bool { (1...99).contains(thresholdUsedPercent) }
}

public struct CodexAutoSwitch: Decodable, Sendable {
  public let enabled: Bool
  public let thresholdPercent: Double
  public let pollIntervalSeconds: Int
  public let outcome: String
  public let message: String
  public let activationInProgress: Bool
  public let lastCheckedAt: String?
  public let lastSwitchedAt: String?
  /// Profile the monitor chose but could not switch to yet. Present only for waiting_idle.
  public let candidate: String?
}

public struct DashboardAccount: Decodable, Identifiable, Sendable {
  public let id: String
  public let provider: String
  public let providerLabel: String
  public let label: String
  public let email: String?
  public let plan: String?
  public let platform: String
  public let source: String
  public let status: String
  public let message: String?
  public let fetchedAt: String?
  public let sampledAt: String?
  public let isActive: Bool
  public let windows: [AccountQuotaWindow]
  public let capabilities: AccountCapabilities
  /// Hidden in the trays (`accounts[].trayHidden`: "Show in tray" off for this account or its provider). The
  /// dashboard's `accounts[].hidden` is not decoded at all: it never changes what a tray shows. Missing means shown.
  public let trayHidden: Bool?

  public var identity: String { email ?? label }
  public var canOpenOnMac: Bool {
    provider == "claude" && capabilities.claudeProfileId != nil && capabilities.claudePlatforms.contains("mac")
  }
  public var canActivate: Bool { provider == "codex" && capabilities.codexProfile != nil && !isActive }
  /// The Antigravity profile the switch routes address, when it is a safe identifier.
  public var antigravityProfile: String? {
    guard provider == "antigravity", let id = capabilities.antigravityProfileId,
      id.range(of: "^[A-Za-z0-9][A-Za-z0-9_-]{0,63}\\z", options: .regularExpression) != nil
    else { return nil }
    return id
  }
  /// Antigravity activation is offered only when the dashboard says the Ubuntu runtime can do it.
  public var canActivateAntigravity: Bool {
    antigravityProfile != nil && !isActive && status != "needs_sign_in"
      && capabilities.antigravityCanActivate == true
      && capabilities.antigravityHostIds == ["ubuntu"]
  }
}

public struct AccountCapabilities: Decodable, Sendable {
  public let codexProfile: String?
  public let claudeProfileId: String?
  public let claudePlatforms: [String]
  public let antigravityProfileId: String?
  public let antigravityHostIds: [String]?
  public let antigravityCanActivate: Bool?
}

public struct AccountQuotaWindow: Decodable, Identifiable, Sendable {
  public let key: String
  public let label: String
  public let usedPercent: Double?
  public let remainingPercent: Double?
  public let resetAt: String?
  public let windowMinutes: Double?
  public let used: Double?
  public let limit: Double?
  public let unit: String?
  public let kind: String?
  public let remaining: Double?
  public let expiresAt: String?
  public let unlimited: Bool?
  public let enabled: Bool?
  public let status: String?
  public let sampledAt: String?
  /// Antigravity quota pool this window belongs to, exactly as the adapter reported it.
  public let poolId: String?
  /// F6 from the dashboard: present (always true) when `resetAt` has passed and the reading was sampled before it.
  public let resetPassed: Bool?
  public var id: String { key }

  public var clampedUsedPercent: Double? {
    // Preserve real over-budget usage for text; only bar geometry is capped.
    guard let value = usedPercent, value.isFinite, value >= 0 else { return nil }
    return value
  }
  public var clampedRemainingPercent: Double? {
    guard let value = remainingPercent, value.isFinite, value >= 0, value <= 100 else { return nil }
    return value
  }
}

public enum AccountFormatting {
  public static func cachedSample(status: String?, sampledAt: String?) -> String? {
    guard status == "cached" else { return nil }
    guard let sample = date(sampledAt) else { return "Cached · Sample time unavailable" }
    let formatter = DateFormatter()
    formatter.dateFormat = "MMM d, yyyy h:mm:ss a z"
    return "Cached · Sampled \(formatter.string(from: sample))"
  }

  public static func date(_ iso: String?) -> Date? {
    guard let iso else { return nil }
    let fractional = ISO8601DateFormatter()
    fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    return fractional.date(from: iso) ?? ISO8601DateFormatter().date(from: iso)
  }

  public static func reset(_ iso: String?, now: Date = Date()) -> String {
    guard let date = date(iso) else { return "Reset time unavailable" }
    let formatter = DateFormatter()
    let daysAway = abs(date.timeIntervalSince(now)) / 86400
    formatter.dateFormat = Calendar.current.isDate(date, inSameDayAs: now) ? "h:mm a" : daysAway >= 7 ? "MMM d h:mm a" : "EEE h:mm a"
    let absolute = formatter.string(from: date)
    let seconds = date.timeIntervalSince(now)
    guard seconds > 0 else { return "Reset \(absolute)" }
    let minutes = Int(ceil(seconds / 60))
    let hours = minutes / 60
    let days = hours / 24
    let relative = days > 0 ? "\(days)d \(hours % 24)h" : hours > 0 ? "\(hours)h \(minutes % 60)m" : "\(minutes)m"
    return "Resets \(absolute) · \(relative)"
  }

  public static func expiration(_ iso: String?) -> String {
    guard let date = date(iso) else { return "Expiration unavailable" }
    let formatter = DateFormatter()
    formatter.dateFormat = "MMM d, yyyy h:mm a"
    return "Expires \(formatter.string(from: date))"
  }
}
