import Foundation

public struct AccountDashboard: Decodable, Sendable {
  public let schemaVersion: Int
  public let updatedAt: String
  public let accounts: [DashboardAccount]
  public let codexAutoSwitch: CodexAutoSwitch
  public let settings: AccountRefreshSettings?
}

public struct AccountRefreshSettings: Decodable, Sendable {
  public let refreshIntervalSeconds: Int
  public var validatedInterval: TimeInterval {
    TimeInterval((30...3600).contains(refreshIntervalSeconds) ? refreshIntervalSeconds : 60)
  }
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

  public var identity: String { email ?? label }
  public var canOpenOnMac: Bool {
    provider == "claude" && capabilities.claudeProfileId != nil && capabilities.claudePlatforms.contains("mac")
  }
  public var canActivate: Bool { provider == "codex" && capabilities.codexProfile != nil && !isActive }
}

public struct AccountCapabilities: Decodable, Sendable {
  public let codexProfile: String?
  public let claudeProfileId: String?
  public let claudePlatforms: [String]
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
