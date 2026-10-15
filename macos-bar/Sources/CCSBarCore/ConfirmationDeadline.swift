import Foundation

/// Date headers have one-second precision. Count the whole request against the offer and never renew it.
public struct ConfirmationDeadline: Sendable {
  public let uptime: TimeInterval

  public init?(expiresAt: String, serverDate: Date?, requestStarted: TimeInterval,
    now: TimeInterval = ProcessInfo.processInfo.systemUptime) {
    guard let serverDate, let expiry = AccountFormatting.date(expiresAt) else { return nil }
    uptime = now + max(0, expiry.timeIntervalSince(serverDate) - 1 - max(0, now - requestStarted))
  }

  public var expired: Bool { ProcessInfo.processInfo.systemUptime >= uptime }

  static func serverDate(_ response: HTTPURLResponse) -> Date? {
    guard let value = response.value(forHTTPHeaderField: "Date") else { return nil }
    let formatter = DateFormatter()
    formatter.locale = Locale(identifier: "en_US_POSIX")
    formatter.timeZone = TimeZone(secondsFromGMT: 0)
    formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss zzz"
    return formatter.date(from: value)
  }
}
