import Foundation

public struct ProviderGroup: Identifiable, Sendable {
  /// The provider for a provider group; `provider|accountID` for one account's own tray row.
  public let id: String
  public let label: String
  public let accounts: [DashboardAccount]
  public let representative: DashboardAccount
  public let primaryWindows: [AccountQuotaWindow]
  public let supplementaryWindows: [AccountQuotaWindow]
  public let statusLabel: String
  /// The provider these accounts belong to (`id` names the row, which may be one account's).
  public var provider: String = ""
  /// True for one account's own row: its meters and secondary line are that account's.
  public var isAccountRow = false
  /// The tray's rows for this provider: one per account for providers listed account by account
  /// (OpenCode Go, like Codex and Claude), otherwise this one representative row.
  public var trayRows: [ProviderGroup] = []

  /// The row's secondary line. One account's row names it, its plan, status and reading time; a
  /// provider row with several accounts names the shown one ("1 of N", the others in Details).
  /// One account's row line in two parts: who (the name before an address's "@", cut in the middle
  /// when long so its distinguishing end stays) and its state (status and reading time, always whole).
  /// The plan is in the row's tooltip and Details: the 158 pt line cannot also hold it. Nil for a
  /// provider row.
  public func accountMeta(now: Date = TrayFormat.now) -> (who: String, state: String)? {
    guard isAccountRow else { return nil }
    let rep = representative
    var short = rep.identity.split(separator: "@").first.map(String.init) ?? rep.identity
    // The title already names the provider: a name that repeats it ("OpenCode Go 3d794f2c") keeps only the part
    // that tells the rows apart, so the line never spends its width on the title again.
    if short.count > label.count + 1, short.hasPrefix(label + " ") { short = String(short.dropFirst(label.count + 1)) }
    // A stored key's row ("Key 3d794f2c", card OCG1) shows its digest alone: "Key" would cut it on the 158 pt line.
    if short.count == 12, short.hasPrefix("Key "), short.dropFirst(4).allSatisfy({ $0.isHexDigit && !$0.isUppercase }) {
      short = String(short.dropFirst(4))
    }
    let read = TrayFormat.relative(AccountFormatting.date(rep.sampledAt ?? rep.fetchedAt), now: now)
    let state = rep.status == "ok" || rep.status == "cached" ? "\(TrayFormat.statusWord(rep.status)) \(read)"
      : TrayFormat.statusWord(rep.status)
    return (short, state)
  }

  /// One account's row tooltip: the provider, the full identity and the plan.
  public var accountHelp: String {
    let rep = representative
    let plan = TrayFormat.planLabel(rep.plan, provider: provider)
    return "\(label) \(rep.identity)\(plan.isEmpty ? "" : " (\(plan))") usage details: windows, balances and reset times"
  }

  public func meta(now: Date = TrayFormat.now) -> String {
    let rep = representative
    let read = TrayFormat.relative(AccountFormatting.date(rep.sampledAt ?? rep.fetchedAt), now: now)
    // The name before an address's "@", so the status and reading time still fit the line.
    let short = rep.identity.split(separator: "@").first.map(String.init) ?? rep.identity
    if let parts = accountMeta(now: now) {
      return [parts.who, parts.state].filter { !$0.isEmpty }.joined(separator: " · ")
    }
    if accounts.count > 1 {
      return "\(short) · 1 of \(accounts.count)"
    }
    return "\(TrayFormat.statusWord(rep.status)) \(read)"
  }
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

      var group = Self.group(id: provider, provider: provider, accounts: providerAccounts, representative: representative)
      group.trayRows = Self.perAccountProviders.contains(provider) && providerAccounts.count > 1
        ? providerAccounts.map { account in
          var row = Self.group(id: "\(provider)|\(account.id)", provider: provider, accounts: [account], representative: account)
          row.isAccountRow = true
          return row
        }
        : [group]
      return group
    }
  }

  /// Providers whose accounts are each their own tray row, listed the way Codex and Claude accounts
  /// are (owner, 2026-11-21): every OpenCode Go account has its own meters, reset times and status.
  static let perAccountProviders: Set<String> = ["opencode-go"]

  private static func group(
    id: String, provider: String, accounts: [DashboardAccount], representative: DashboardAccount
  ) -> ProviderGroup {
    let visible = representative.visibleWindows
    let primary = visible.filter { !Self.isSupplementary($0) }
    var group = ProviderGroup(
      id: id,
      label: Self.groupLabel(provider, fallback: representative.providerLabel),
      accounts: accounts,
      representative: representative,
      primaryWindows: Array((primary.isEmpty ? visible : primary).prefix(3)),
      supplementaryWindows: visible.filter { Self.isSupplementary($0) },
      statusLabel: Self.statusLabel(representative.status)
    )
    group.provider = provider
    return group
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
