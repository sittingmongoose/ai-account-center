import Foundation

// Presentation rules for the tray panel, kept free of SwiftUI so the check harness can exercise them.
// They follow the approved tray concept (redesign-concepts-20261001/trays): the same visibility rules as
// the dashboard, no invented usage, at most two decimals, and value motion that never overshoots.

public enum QuotaPeriod: String, Sendable { case fiveHour, week, month, other }

public enum MeterSeverity: String, Sendable {
  case calm, warn, crit, over, unavailable

  /// Calm below 80 % used, warning from 80, critical from 95, over above 100.
  public static func of(_ used: Double?) -> MeterSeverity {
    guard let used, used.isFinite else { return .unavailable }
    if used > 100 { return .over }
    if used >= 95 { return .crit }
    if used >= 80 { return .warn }
    return .calm
  }
}

extension AccountQuotaWindow {
  /// Percent used as reported, or the exact complement of a reported remaining percent. Nil when the
  /// provider reported neither: a missing reading is unavailable, never zero.
  public var meterUsedPercent: Double? {
    if let used = clampedUsedPercent { return used }
    if let remaining = clampedRemainingPercent { return 100 - remaining }
    return nil
  }

  /// Balances, extra usage and spend are amounts, not meters.
  public var isMeter: Bool { !["balance", "extra_usage", "spend"].contains(kind ?? "rate_limit") }

  public var isFable: Bool { "\(key) \(label)".lowercased().contains("fable") }

  public var period: QuotaPeriod {
    if windowMinutes == 300 { return .fiveHour }
    if windowMinutes == 10080 { return .week }
    let text = "\(key) \(label)".lowercased()
    if text.range(of: "five.?hour|5.?hour|\\b5h\\b|5 hours|rolling", options: .regularExpression) != nil { return .fiveHour }
    if text.range(of: "seven.?day|weekly|\\bweek\\b|7.?day", options: .regularExpression) != nil { return .week }
    if text.contains("month") { return .month }
    return .other
  }
}

/// F6: once a window's reset has passed, a reading sampled before it no longer describes the window. The tray then
/// shows "Reset at <time> · new reading pending" with no number and no fill, never 0%.
public enum TrayReset {
  /// The reset time when `resetAt` is in the past and the reading was sampled before it (the window's own
  /// `sampledAt`, else the account's), or when its sample time is unknown. Nil when the reading stands. Amounts,
  /// unlimited and disabled windows are left alone.
  public static func pending(_ window: AccountQuotaWindow, accountSampledAt: String?, now: Date = TrayFormat.now) -> Date? {
    guard window.isMeter, window.unlimited != true, window.enabled != false,
      let reset = AccountFormatting.date(window.resetAt), reset <= now else { return nil }
    if let sampled = AccountFormatting.date(window.sampledAt ?? accountSampledAt), sampled >= reset { return nil }
    return reset
  }

  /// "10:15 AM" today, "Oct 1, 11:00 PM" on another day.
  public static func when(_ reset: Date, now: Date = TrayFormat.now) -> String {
    Calendar.current.isDate(reset, inSameDayAs: now)
      ? reset.formatted(date: .omitted, time: .shortened)
      : reset.formatted(.dateTime.month(.abbreviated).day().hour().minute())
  }

  /// "Reset at 10:15 AM".
  public static func at(_ reset: Date, now: Date = TrayFormat.now) -> String { "Reset at \(when(reset, now: now))" }

  /// Details and tooltips: "Reset at Thu, Oct 1, 10:15 AM · new reading pending".
  public static func long(_ reset: Date) -> String {
    "Reset at \(reset.formatted(.dateTime.weekday(.abbreviated).month(.abbreviated).day().hour().minute())) · new reading pending"
  }

  /// Compact cells, longest first: the whole sentence, then shorter forms for a narrow cell. The tooltip always has
  /// the whole sentence.
  public static func forms(_ reset: Date, now: Date = TrayFormat.now) -> [String] {
    let time = when(reset, now: now)
    return ["Reset at \(time) · new reading pending", "Reset at \(time) · pending", "Reset \(time) · pending",
      "Reset at \(time)", "New reading pending", "Pending"]
  }
}

extension DashboardAccount {
  /// F6 for one of this account's windows (see `TrayReset.pending`).
  public func pendingReset(_ window: AccountQuotaWindow, now: Date = TrayFormat.now) -> Date? {
    TrayReset.pending(window, accountSampledAt: sampledAt, now: now)
  }
}

extension AccountDashboard {
  /// Every window now shown as "new reading pending", so a timer can tell when one flips.
  public func pendingResetKeys(now: Date = TrayFormat.now) -> Set<String> {
    Set(accounts.flatMap { account in
      account.windows.filter { account.pendingReset($0, now: now) != nil }.map { "\(account.id)|\($0.key)" }
    })
  }
}

/// A Claude row's Fable column: only Max plans have one, and an absent window is "Not reported yet".
public enum FableCell: Sendable {
  case notApplicable
  case notReported
  case window(AccountQuotaWindow)
}

extension DashboardAccount {
  public var isMaxPlan: Bool {
    let plan = (plan ?? "").lowercased().replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: "-", with: " ")
    return plan.range(of: "(^|\\s)max(\\s*(5|20)x)?($|\\s)", options: .regularExpression) != nil
  }

  private var meterWindows: [AccountQuotaWindow] { visibleWindows.filter(\.isMeter) }

  /// The 5-hour column: the exact `five_hour` window, else the first non-Fable 5-hour meter.
  public var fiveHourWindow: AccountQuotaWindow? {
    if provider == "codex" { return compactWindows.first { $0.key == "five_hour" } }
    return meterWindows.first { $0.key == "five_hour" }
      ?? meterWindows.first { !$0.isFable && $0.period == .fiveHour }
  }

  /// The weekly column: the exact `seven_day` window, else the first non-Fable weekly meter.
  public var weeklyWindow: AccountQuotaWindow? {
    if provider == "codex" { return compactWindows.first { $0.key == "seven_day" } }
    return meterWindows.first { $0.key == "seven_day" }
      ?? meterWindows.first { !$0.isFable && $0.period == .week }
  }

  /// Fable appears only on Claude Max rows, read from the provider-reported `seven_day_fable` window.
  public var fableCell: FableCell {
    guard provider == "claude", isMaxPlan else { return .notApplicable }
    if let window = visibleWindows.first(where: { $0.key == "seven_day_fable" }) ?? visibleWindows.first(where: \.isFable) {
      return .window(window)
    }
    return .notReported
  }

  /// Up to three meters for a one-row provider (Cursor, Muse Code, Kimi Code, Qwen, Z.ai, OpenCode Go).
  public var glanceMeters: [AccountQuotaWindow] { Array(meterWindows.prefix(3)) }

  /// Qwen add-on credit packs, listed one by one; credit is never summed across packs.
  public var creditPacks: [AccountQuotaWindow] { visibleWindows.filter { $0.key.hasPrefix("addon-pack-") } }
}

public enum TrayColumns {
  /// The concept's three Antigravity columns when the dashboard reports those buckets; otherwise the
  /// first account's own meters, so nothing is guessed from labels.
  public static func antigravity(_ accounts: [DashboardAccount]) -> [(key: String, label: String)] {
    let preferred: [(String, String)] = [
      ("gemini-5h", "Gemini 5-hour"), ("gemini-weekly", "Gemini weekly"), ("3p-weekly", "Claude and GPT weekly"),
    ]
    let present = preferred.filter { key, _ in accounts.contains { $0.visibleWindows.contains { $0.key == key } } }
    if !present.isEmpty { return present.map { (key: $0.0, label: $0.1) } }
    guard let first = accounts.first(where: { !$0.glanceMeters.isEmpty }) else { return [] }
    return first.glanceMeters.map { (key: $0.key, label: shortLabel(provider: "antigravity", $0)) }
  }

  /// Full window names for Details: the provider's label, with the terse Codex and Claude ones spelled out.
  public static func fullLabel(provider: String, _ window: AccountQuotaWindow) -> String {
    if window.isFable { return window.label.isEmpty ? "Weekly Fable usage" : window.label }
    if provider == "codex" { return ["week": "Weekly", "5h": "5-hour"][window.label] ?? window.label }
    if provider == "claude" { return ["Five-hour usage": "5-hour usage"][window.label] ?? window.label }
    return window.label.isEmpty ? "Usage" : window.label
  }

  /// Short meter captions, as in the concept.
  public static func shortLabel(provider: String, _ window: AccountQuotaWindow) -> String {
    if window.isFable { return "Fable" }
    let periodLabel: String? = [.fiveHour: "5-hour", .week: "Weekly", .month: "Monthly"][window.period]
    switch provider {
    case "antigravity":
      let family = window.key.lowercased().hasPrefix("gemini") ? "Gemini" : "Claude and GPT"
      return "\(family) \(window.period == .fiveHour ? "5-hour" : "weekly")"
    case "cursor":
      return ["plan-reported": "Included", "autoPercentUsed": "Cursor models", "apiPercentUsed": "Other models"][window.key] ?? window.label
    case "zai":
      return ["usage-1": "5-hour tokens", "usage-2": "Weekly tokens", "usage-3": "Monthly requests"][window.key] ?? window.label
    case "opencode-go" where window.key.lowercased().hasPrefix("console"):
      return "Console " + (periodLabel ?? window.label).lowercased()
    default:
      return periodLabel ?? window.label
    }
  }
}

/// One status string for the panel header: providers reporting, live or cached.
public struct TrayStatusSummary: Sendable, Equatable {
  public let reporting: Int
  public let providers: Int
  public let allCached: Bool

  public init(dashboard: AccountDashboard) {
    let groups = Dictionary(grouping: dashboard.visibleAccounts, by: \.provider)
    providers = groups.count
    reporting = groups.values.filter { $0.contains { ["ok", "cached"].contains($0.status) } }.count
    allCached = !dashboard.visibleAccounts.contains { $0.status == "ok" }
  }
}

/// What the number means: % used, or % remaining (100 - used). Stored as "left"/"used" from the
/// earlier "% left" wording; the Settings labels read Used and Remaining.
public enum MenuBarMode: String, CaseIterable, Sendable { case remaining = "left", used = "used" }

/// What the menu bar shows next to the Apex glyph: one account's 5-hour window, or its weekly window
/// when no 5-hour window is reported.
public struct MenuBarReading: Sendable, Equatable {
  public let value: Double
  public let text: String
  /// For the tooltip, e.g. "Codex · codex-2 · Weekly used".
  public let detail: String
  /// The provider choice that shows the icon alone, with no number.
  public static let nothingProvider = "none"

  /// User-facing provider names, the same table the panel's provider marks use.
  public static func providerName(_ provider: String) -> String {
    [
      "claude": "Claude", "codex": "Codex", "antigravity": "Antigravity", "cursor": "Cursor", "muse": "Muse Code",
      "kimi-code": "Kimi Code", "qwen": "Qwen Token Plan", "zai": "Z.ai Coding Plan", "opencode-go": "OpenCode Go",
    ][provider] ?? provider
  }

  /// - `provider`: a provider id, or "none" for the icon alone. Codex and Antigravity follow the ACTIVE
  ///   account automatically; Claude uses `claudeAccountID` (Claude has no active account), or its first
  ///   account when none is picked; any other provider uses its single account, or the first of several.
  /// - The window is the account's 5-hour window when reported, otherwise its weekly window.
  /// - A reset-pending (F6) or unavailable reading is nil: the icon with no number, never 0%.
  public static func make(dashboard: AccountDashboard?, provider: String, mode: MenuBarMode,
    claudeAccountID: String? = nil) -> MenuBarReading? {
    guard let dashboard, provider != nothingProvider else { return nil }
    let accounts = dashboard.visibleAccounts.filter { $0.provider == provider }
    let account: DashboardAccount?
    switch provider {
    case "claude":
      account = claudeAccountID.flatMap { id in accounts.first { $0.id == id } } ?? accounts.first
    case "codex", "antigravity":
      account = accounts.first { $0.isActive }
    default:
      account = accounts.first
    }
    guard let account else { return nil }
    let fiveHour = account.fiveHourWindow
    guard let window = fiveHour ?? account.weeklyWindow else { return nil }
    guard account.pendingReset(window) == nil, let used = window.meterUsedPercent else { return nil }
    let value = mode == .used ? used : max(0, 100 - used)
    let text = "\(TrayFormat.number(value))%"
    let who = account.identity.split(separator: "@").first.map(String.init) ?? account.identity
    let span = window.key == fiveHour?.key ? "5-hour" : "Weekly"
    return MenuBarReading(value: value, text: text,
      detail: "\(providerName(provider)) · \(who) · \(span) \(mode == .used ? "used" : "remaining")")
  }
}

public enum TrayFormat {
  /// Offline renders pin "now" to the fixture's capture time so countdowns read as they did then.
  nonisolated(unsafe) public static var referenceNow: Date?
  public static var now: Date { referenceNow ?? Date() }

  /// Up to two decimals, never padded.
  public static func number(_ value: Double) -> String {
    value.formatted(.number.precision(.fractionLength(0...2)))
  }

  /// Decimal places the reading needs (0 to 2), so a count-up keeps one width.
  public static func decimals(_ value: Double) -> Int {
    let rounded = (value * 100).rounded() / 100
    if rounded == rounded.rounded() { return 0 }
    if (rounded * 10) == (rounded * 10).rounded() { return 1 }
    return 2
  }

  public static func number(_ value: Double, decimals: Int) -> String {
    value.formatted(.number.precision(.fractionLength(decimals)))
  }

  public static func duration(_ seconds: TimeInterval) -> String {
    let total = max(0, seconds)
    let minutes = Int(total / 60), hours = minutes / 60, days = hours / 24
    if days >= 1 { return "\(days)d \(hours % 24)h" }
    if hours >= 1 { return "\(hours)h \(minutes % 60)m" }
    if minutes >= 1 { return "\(minutes)m" }
    return "\(max(1, Int(total)))s"
  }

  public static func relative(_ date: Date?, now: Date = TrayFormat.now) -> String {
    guard let date else { return "time unavailable" }
    let elapsed = now.timeIntervalSince(date)
    return elapsed < 10 ? "just now" : "\(duration(elapsed)) ago"
  }

  /// Row form: a countdown, or the clock time once the reset is under a day away.
  public static func shortReset(_ iso: String?, now: Date = TrayFormat.now) -> String? {
    guard let date = AccountFormatting.date(iso) else { return nil }
    let remaining = date.timeIntervalSince(now)
    if remaining <= 0 { return "due" }
    if remaining < 86_400 { return date.formatted(date: .omitted, time: .shortened) }
    return duration(remaining)
  }

  /// Details form: the date, time and countdown.
  public static func longReset(_ iso: String?, now: Date = TrayFormat.now) -> String {
    guard let date = AccountFormatting.date(iso) else { return "No reset reported" }
    let remaining = date.timeIntervalSince(now)
    let stamp = date.formatted(.dateTime.weekday(.abbreviated).month(.abbreviated).day().hour().minute())
    return "Resets \(stamp) · \(remaining <= 0 ? "due" : duration(remaining))"
  }

  public static func isSoon(_ iso: String?, now: Date = TrayFormat.now) -> Bool {
    guard let date = AccountFormatting.date(iso) else { return false }
    return date.timeIntervalSince(now) < 7200
  }

  public static func planLabel(_ plan: String?) -> String {
    guard let plan, !plan.isEmpty else { return "" }
    return plan.count <= 5 ? plan.prefix(1).uppercased() + plan.dropFirst() : plan
  }

  public static func platformName(_ platform: String) -> String {
    ["mac": "Mac", "windows": "Windows", "ubuntu": "Ubuntu", "linux": "Linux"][platform]
      ?? (platform.isEmpty ? "Unknown" : platform.prefix(1).uppercased() + platform.dropFirst())
  }

  public static func statusWord(_ status: String) -> String {
    ["ok": "Live", "cached": "Cached", "needs_sign_in": "Sign-in needed", "error": "Refresh failed"][status] ?? "Unavailable"
  }
}

/// Value motion: the shared ease-out curve (cubic-bezier(.2, .8, .2, 1)) whose control points never
/// leave 0...1 in y, so a meter width, a count-up or the active platter never passes its target.
public enum TrayMotion {
  public static let valueCurve = (x1: 0.2, y1: 0.8, x2: 0.2, y2: 1.0)
  public static let meterDuration = 0.9
  public static let platterDuration = 0.44
  public static let checkDuration = 0.38
  public static let settingsDuration = 0.34
  public static let firstOpenStagger = 0.026
  public static let laterOpenStagger = 0.007

  /// The curve's progress at time fraction `t` (0...1), solved on x by bisection.
  public static func progress(_ t: Double) -> Double {
    let c = valueCurve
    func bezier(_ s: Double, _ p1: Double, _ p2: Double) -> Double {
      let u = 1 - s
      return 3 * u * u * s * p1 + 3 * u * s * s * p2 + s * s * s
    }
    let target = min(1, max(0, t))
    var low = 0.0, high = 1.0
    for _ in 0..<40 {
      let mid = (low + high) / 2
      if bezier(mid, c.x1, c.x2) < target { low = mid } else { high = mid }
    }
    return bezier((low + high) / 2, c.y1, c.y2)
  }

  /// The share of the track a reading fills. Readings above 100 % stay at the end of the track (their
  /// text still says the real value); a missing reading fills nothing.
  public static func fillFraction(_ used: Double?) -> Double {
    guard let used, used.isFinite else { return 0 }
    return min(100, max(0, used)) / 100
  }
}
