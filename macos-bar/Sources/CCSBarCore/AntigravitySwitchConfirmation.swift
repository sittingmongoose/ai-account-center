import Foundation

/// The one-use consent offer the dashboard returns when Antigravity programs are running on Ubuntu
/// (`POST /api/antigravity/profiles/:id/activate` answers 409 `confirmation-required`). The token is
/// sent once, only by an explicit Confirm, and is never shown or stored.
public struct AntigravitySwitchConfirmation: Sendable {
  public static let warning =
    "Another Antigravity program is running on Ubuntu. Stop the listed programs, switch accounts, and restore their reviewed sessions?"
  static let processLabels = [
    "cli": "Antigravity CLI",
    "desktop": "Antigravity Desktop",
    "language-server": "Antigravity language server",
  ]

  public let token: String
  public let expiresAt: String
  public let profileId: String
  public let email: String
  public let processes: [CodexSwitchProcess]

  public func isValid(for profile: String, now: Date = Date()) -> Bool {
    profileId == profile && Self.isToken(token)
      && AccountFormatting.date(expiresAt).map { $0 > now } == true
  }

  static func isToken(_ value: String) -> Bool {
    value.range(of: "^[A-Za-z0-9_-]{16,256}\\z", options: .regularExpression) != nil
  }

  static func isEmail(_ value: String?) -> Bool {
    guard let value, value.count <= 254 else { return false }
    return value.range(of: "^[^\\s@]+@[^\\s@]+\\.[^\\s@]+\\z", options: .regularExpression) != nil
  }

  /// Rebuilds the offer from a 409 body, field by field. Anything unexpected yields nil, so the caller
  /// reports a fixed error instead of asking for consent it cannot describe exactly.
  static func parse(_ data: Data, profile: String, now: Date = Date()) -> AntigravitySwitchConfirmation? {
    guard let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
      body["status"] as? String == "confirmation-required",
      body["profileId"] as? String == profile, body["hostId"] as? String == "ubuntu",
      let offer = body["confirmation"] as? [String: Any],
      offer["profileId"] as? String == profile, offer["hostId"] as? String == "ubuntu",
      let token = offer["token"] as? String, isToken(token),
      let email = offer["email"] as? String, isEmail(email),
      (body["email"] as? String).map({ $0 == email }) ?? true,
      let expiresAt = offer["expiresAt"] as? String,
      let expiry = AccountFormatting.date(expiresAt), expiry > now,
      let rawProcesses = offer["processes"] as? [[String: Any]], rawProcesses.count <= 32
    else { return nil }
    var processes: [CodexSwitchProcess] = []
    for process in rawProcesses {
      guard let pid = process["pid"] as? Int, (1...2_147_483_647).contains(pid),
        let role = process["role"] as? String, let label = processLabels[role]
      else { return nil }
      processes.append(CodexSwitchProcess(label: label, pid: pid, role: role))
    }
    return AntigravitySwitchConfirmation(token: token, expiresAt: expiresAt, profileId: profile,
      email: email, processes: processes)
  }
}

/// A completed Antigravity activation, checked against the profile that was asked for.
public struct AntigravityActivationResult: Sendable {
  public let status: String
  public let profileId: String
  public let email: String?

  static func parse(_ data: Data, profile: String) -> AntigravityActivationResult? {
    guard let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
      let status = body["status"] as? String, ["active", "already-active"].contains(status),
      body["profileId"] as? String == profile, body["hostId"] as? String == "ubuntu"
    else { return nil }
    let email = body["email"] as? String
    if email != nil && !AntigravitySwitchConfirmation.isEmail(email) { return nil }
    return AntigravityActivationResult(status: status, profileId: profile, email: email)
  }
}
