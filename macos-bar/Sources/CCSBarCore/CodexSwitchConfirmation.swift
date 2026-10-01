import Foundation

/// Server-bound one-use consent offer. Tokens are never shown or persisted.
public struct CodexSwitchConfirmation: Decodable, Sendable {
  public let token: String
  public let expiresAt: String
  public let targetProfile: String
  public let processes: [CodexSwitchProcess]
  public let warning: String

  public func isValid(for profile: String, now: Date = Date()) -> Bool {
    !token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && token.count <= 512
      && targetProfile == profile
      && AccountFormatting.date(expiresAt).map { $0 > now } == true
      && !processes.isEmpty
      && processes.allSatisfy { $0.pid > 0 && !$0.label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        && !$0.role.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
  }
}

public struct CodexSwitchProcess: Decodable, Sendable {
  public let label: String
  public let pid: Int
  public let role: String
}

struct CodexSwitchConflict: Decodable {
  let code: String
  let reason: String?
  let confirmation: CodexSwitchConfirmation?
}
