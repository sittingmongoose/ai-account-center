import Foundation

extension DashboardAccount {
  /// The compact Claude row: core exact windows first, then scoped/other meters in reported order,
  /// Fable (exact `seven_day_fable`, Max only) in its existing slot. Surface windows `seven_day_fable_*`
  /// are ordinary scoped meters. Compact sizes stay documented: up to 3 for Claude, 2 exact for Codex.
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
    if provider == "claude" {
      let core = ["five_hour", "seven_day"].compactMap { key in candidates.first { $0.key == key } }
      if isMaxPlan, let fable = candidates.first(where: { $0.key == "seven_day_fable" }) {
        let others = candidates.filter { candidate in
          candidate.key != fable.key && !core.contains { $0.key == candidate.key }
        }
        return Array((core + others).prefix(2)) + [fable]
      } else {
        // Pro or no exact Fable: exclude the exact Fable window from compact (Max-gated cell only);
        // surface windows remain ordinary scoped meters in reported order.
        let others = candidates.filter { candidate in
          candidate.key != "seven_day_fable" && !core.contains { $0.key == candidate.key }
        }
        return Array((core + others).prefix(3))
      }
    }
    return Array(candidates.prefix(3))
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
            // A kind's roll-up stays while it holds or used a pack; an account with none hides it.
            return [window.remaining, window.used, window.limit].contains { $0.map { $0.isFinite && $0 > 0 } == true }
              || hasTiming
          }
          return hasTiming || [window.remaining, window.used, window.limit].contains { $0.map { $0.isFinite && $0 > 0 } == true }
            || (["grant", "record"].contains { Self.normalized(window.key).contains($0) } && Self.hasUsageValue(window))
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
