import Foundation

extension DashboardAccount {
  /// The compact Claude Max row gives a genuinely reported Fable window its
  /// own place; other model-scoped quotas remain in the complete details.
  public var compactWindows: [AccountQuotaWindow] {
    if provider == "codex" {
      // Only the canonical account limits belong in the compact Codex row.
      // Additional windows stay available in details without substituting for
      // a missing core limit or changing the meaning of its cadence label.
      return ["five_hour", "seven_day"].compactMap { key in
        visibleWindows.first { $0.key == key }
      }
    }
    let primary = visibleWindows.filter { $0.kind != "balance" && $0.kind != "extra_usage" }
    let candidates = primary.isEmpty ? visibleWindows : primary
    guard provider == "claude", plan?.lowercased().contains("max") == true,
      let fable = candidates.first(where: { $0.key == "seven_day_fable" })
    else { return Array(candidates.prefix(3)) }
    let core = ["five_hour", "seven_day"].compactMap { key in candidates.first { $0.key == key } }
    let others = candidates.filter { candidate in
      candidate.key != fable.key && !core.contains { $0.key == candidate.key }
    }
    return Array((core + others).prefix(2)) + [fable]
  }

  /// Presentation-only windows. Raw provider data remains available in `windows`.
  public var visibleWindows: [AccountQuotaWindow] {
    windows.filter { window in
      switch provider {
      case "codex":
        if Self.normalized(window.key).contains("chatpass")
          || Self.normalized(window.label).contains("chatpass") { return false }
        if Self.isProPlan(plan), Self.isFiveHour(window), !Self.hasUsageValue(window) { return false }
      case "qwen":
        let key = Self.normalized(window.key)
        if key == "plansubscription" || key == "subscription"
          || Self.normalized(window.label) == "plansubscription" { return false }
      case "zai":
        if Self.isPack(window) {
          let hasTiming = AccountFormatting.date(window.resetAt) != nil
            || AccountFormatting.date(window.expiresAt) != nil
          if ["resetpacks5h", "resetpacksweekly"].contains(Self.normalized(window.key)) {
            // Zero summary counts have no individual pack details to display.
            return window.remaining.map { $0.isFinite && $0 > 0 } == true || hasTiming
          }
          return Self.hasUsageValue(window) || hasTiming
        }
      default: break
      }
      return true
    }
  }

  private static func normalized(_ value: String) -> String {
    value.lowercased().filter { $0.isLetter || $0.isNumber }
  }

  private static func isProPlan(_ plan: String?) -> Bool {
    guard let plan else { return false }
    return ["pro", "chatgptpro"].contains(normalized(plan))
  }

  private static func isPack(_ window: AccountQuotaWindow) -> Bool {
    normalized(window.key).contains("pack") || normalized(window.label).contains("pack")
      || window.unit.map { ["pack", "packs"].contains(normalized($0)) } == true
  }

  private static func isFiveHour(_ window: AccountQuotaWindow) -> Bool {
    if window.windowMinutes == 300 { return true }
    return [window.key, window.label].contains { value in
      let value = normalized(value)
      return value == "5h" || value.contains("5hour") || value.contains("fivehour")
    }
  }

  private static func hasUsageValue(_ window: AccountQuotaWindow) -> Bool {
    window.clampedUsedPercent != nil || window.clampedRemainingPercent != nil
      || [window.used, window.limit, window.remaining].contains { value in
        value.map { $0.isFinite && $0 >= 0 } == true
      }
      || window.unlimited == true
  }
}
