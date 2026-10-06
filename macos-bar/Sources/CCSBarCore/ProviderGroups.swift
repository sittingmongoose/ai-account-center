import Foundation

public struct ProviderGroup: Identifiable, Sendable {
  public let id: String
  public let label: String
  public let accounts: [DashboardAccount]
  public let representative: DashboardAccount
  public let primaryWindows: [AccountQuotaWindow]
  public let supplementaryWindows: [AccountQuotaWindow]
  public let statusLabel: String
}

/// Builds `AccountDashboard.providerGroups` once at decode time (N4): as a computed property it reran
/// the grouping, the representative sort and its per-account ISO-8601 parses on every access,
/// including every refresh tick and every panel body evaluation.
enum ProviderGroupBuilder {
  static func build(shown: [DashboardAccount]) -> [ProviderGroup] {
    let grouped = Dictionary(grouping: shown, by: \.provider)
    // Today's tray order: the three account sections, then the other providers.
    let preferredOrder = [
      "claude", "codex", "antigravity", "cursor", "muse", "kimi-code", "qwen", "zai", "opencode-go",
    ]
    var providers = preferredOrder.filter { grouped[$0] != nil }
    for account in shown where !providers.contains(account.provider) {
      providers.append(account.provider)
    }

    return providers.compactMap { provider in
      guard let providerAccounts = grouped[provider],
        let representative = providerAccounts.enumerated().min(by: { left, right in
          if provider == "codex", left.element.isActive != right.element.isActive {
            return left.element.isActive
          }
          let leftRank = Self.statusRank(left.element.status)
          let rightRank = Self.statusRank(right.element.status)
          if leftRank != rightRank { return leftRank < rightRank }
          let leftDate = Self.sampleDate(left.element)
          let rightDate = Self.sampleDate(right.element)
          if leftDate != rightDate {
            guard let leftDate else { return false }
            guard let rightDate else { return true }
            return leftDate > rightDate
          }
          return left.offset < right.offset
        })?.element
      else { return nil }

      let visible = representative.visibleWindows
      let primary = visible.filter { !Self.isSupplementary($0) }
      return ProviderGroup(
        id: provider,
        label: Self.groupLabel(provider, fallback: representative.providerLabel),
        accounts: providerAccounts,
        representative: representative,
        primaryWindows: Array((primary.isEmpty ? visible : primary).prefix(3)),
        supplementaryWindows: visible.filter { Self.isSupplementary($0) },
        statusLabel: Self.statusLabel(representative.status)
      )
    }
  }

  private static func statusRank(_ status: String) -> Int {
    switch status {
    case "ok": return 0
    case "cached": return 1
    case "needs_sign_in": return 2
    case "unavailable": return 3
    case "error": return 4
    default: return 5
    }
  }

  private static func sampleDate(_ account: DashboardAccount) -> Date? {
    AccountFormatting.date(account.sampledAt) ?? AccountFormatting.date(account.fetchedAt)
  }

  private static func isSupplementary(_ window: AccountQuotaWindow) -> Bool {
    window.kind == "balance" || window.kind == "extra_usage"
  }

  private static func groupLabel(_ provider: String, fallback: String) -> String {
    switch provider {
    case "muse": return "Muse Code"
    case "antigravity": return "Antigravity"
    case "qwen": return "Qwen Token Plan"
    case "zai": return "Z.ai Coding Plan"
    case "opencode-go": return "OpenCode Go"
    default: return fallback
    }
  }

  private static func statusLabel(_ status: String) -> String {
    switch status {
    case "ok": return "Live"
    case "cached": return "Cached"
    case "needs_sign_in": return "Sign-in needed"
    case "error": return "Error"
    default: return "Usage unavailable"
    }
  }
}
