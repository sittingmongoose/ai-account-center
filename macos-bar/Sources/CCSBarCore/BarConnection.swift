import Foundation

/// Why this tray is signed out, kept in the connection file without a key so the sign-in screen can say it after a
/// restart (CONTRACT-auth-devices section 9). `reason` is the dashboard's code or the tray's own `disconnected`.
public struct SignedOutNote: Codable, Sendable, Equatable {
  /// `device_revoked`, `device_expired`, `invalid_token` or `disconnected`.
  public let reason: String
  /// When the tray noticed (or, when the dashboard says so, when it was revoked), ISO 8601.
  public let at: String
  /// The dashboard's own detail when it sends one: `dashboard`, `revoke-all`, `expired`, ...
  public let revokedReason: String?
  /// The dashboard username that signed the tray out, when the dashboard says so.
  public let revokedBy: String?

  public init(reason: String, at: String, revokedReason: String? = nil, revokedBy: String? = nil) {
    self.reason = reason
    self.at = at
    self.revokedReason = revokedReason
    self.revokedBy = revokedBy
  }
}

/// Private connection material stays in the user's home directory, outside the app/source bundle.
///
/// Version 1 (today's trays) holds `{ baseURL, username, password }`. Version 2 (paired, CONTRACT-auth-devices section
/// 8) holds `{ version: 2, baseURL, username, deviceId, deviceToken, installId, pairedAt }` and never a password. A
/// version 2 file without a key (signed out or disconnected) keeps the address, the username and the install id, so
/// the sign-in screen can fill them in.
public struct BarConnection: Codable, Sendable {
  public let version: Int?
  public let baseURL: URL
  public let username: String
  public let password: String?
  public let deviceId: String?
  public let deviceToken: String?
  public let installId: String?
  public let pairedAt: String?
  public let signedOut: SignedOutNote?

  /// Version 1: a dashboard password.
  public init(baseURL: URL, username: String, password: String) {
    self.init(version: nil, baseURL: baseURL, username: username, password: password, deviceId: nil,
      deviceToken: nil, installId: nil, pairedAt: nil, signedOut: nil)
  }

  /// Version 2: a device key from pairing, never a password.
  public init(baseURL: URL, username: String, deviceId: String, deviceToken: String, installId: String, pairedAt: String) {
    self.init(version: 2, baseURL: baseURL, username: username, password: nil, deviceId: deviceId,
      deviceToken: deviceToken, installId: installId, pairedAt: pairedAt, signedOut: nil)
  }

  private init(version: Int?, baseURL: URL, username: String, password: String?, deviceId: String?,
    deviceToken: String?, installId: String?, pairedAt: String?, signedOut: SignedOutNote?) {
    self.version = version
    self.baseURL = baseURL
    self.username = username
    self.password = password
    self.deviceId = deviceId
    self.deviceToken = deviceToken
    self.installId = installId
    self.pairedAt = pairedAt
    self.signedOut = signedOut
  }

  /// Version 2 without its key: the address, username and install id stay; the key is gone.
  public func signingOut(_ note: SignedOutNote) -> BarConnection {
    BarConnection(version: 2, baseURL: baseURL, username: username, password: nil, deviceId: nil, deviceToken: nil,
      installId: installId, pairedAt: nil, signedOut: note)
  }

  /// The same pairing with a rotated key (section 7).
  public func rotating(to token: String) -> BarConnection {
    BarConnection(version: 2, baseURL: baseURL, username: username, password: nil, deviceId: deviceId,
      deviceToken: token, installId: installId, pairedAt: pairedAt, signedOut: nil)
  }

  /// A device key that works: the tray sends it as `Authorization: Bearer` and never logs in.
  public var isPaired: Bool { deviceToken != nil }
  /// Today's password login (version 1).
  public var hasPassword: Bool { !(password ?? "").isEmpty }
  /// Neither a key nor a password: the sign-in screen is all this file can feed.
  public var isSignedOut: Bool { !isPaired && !hasPassword }

  /// The tray's private state folder: `~/.ccs/bar`, or `AAC_TRAY_STATE_DIR` when a check or an isolated end-to-end
  /// run points it elsewhere. The installed tray never sets it.
  public static var stateDirectory: URL {
    if let custom = ProcessInfo.processInfo.environment["AAC_TRAY_STATE_DIR"], !custom.isEmpty {
      return URL(fileURLWithPath: custom, isDirectory: true)
    }
    return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".ccs/bar", isDirectory: true)
  }

  public static var configURL: URL { stateDirectory.appendingPathComponent("accounts-connection.json") }

  /// The version 1 copy kept while a migration's first key check is pending (section 8).
  public static func rollbackURL(for url: URL = configURL) -> URL {
    url.deletingLastPathComponent().appendingPathComponent("accounts-connection.v1-rollback.json")
  }

  public static func load(from url: URL = configURL) throws -> BarConnection {
    let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
    let permissions = (attrs[.posixPermissions] as? NSNumber)?.intValue ?? 0o777
    guard permissions & 0o077 == 0 else { throw BarClientError.privateConfigRequired }
    let config: BarConnection
    do { config = try JSONDecoder().decode(BarConnection.self, from: Data(contentsOf: url)) }
    catch { throw BarClientError.invalidConnection }
    guard config.isValid else { throw BarClientError.invalidConnection }
    return config
  }

  /// One shape or the other, never both: version 1 needs a password and no key; version 2 needs a key and its
  /// device id, or a signed-out note, and never a password.
  public var isValid: Bool {
    guard Self.isDashboardURL(baseURL), !username.isEmpty else { return false }
    if let installId, !DeviceToken.isInstallId(installId) { return false }
    if version == 2 {
      guard password == nil else { return false }
      if let deviceToken {
        return DeviceToken.isWellFormed(deviceToken) && DeviceToken.isDeviceId(deviceId ?? "") && signedOut == nil
      }
      return signedOut != nil && deviceId == nil
    }
    return version == nil && hasPassword && deviceToken == nil && deviceId == nil && signedOut == nil
  }

  /// http or https, a host, and nothing else: no user, password, path, query or fragment.
  public static func isDashboardURL(_ url: URL) -> Bool {
    ["http", "https"].contains(url.scheme ?? "") && url.host != nil && url.user == nil && url.password == nil
      && url.query == nil && url.fragment == nil && (url.path.isEmpty || url.path == "/")
  }
}

/// The shapes a device key and its companions must have before they are stored or sent.
public enum DeviceToken {
  public static func isWellFormed(_ token: String) -> Bool {
    token.range(of: "^aacd_[A-Za-z0-9_-]{43}\\z", options: .regularExpression) != nil
  }
  public static func isDeviceId(_ id: String) -> Bool {
    id.range(of: "^dev_[0-9a-f]{16}\\z", options: .regularExpression) != nil
  }
  public static func isInstallId(_ id: String) -> Bool { UUID(uuidString: id) != nil }
}

public enum BarClientError: LocalizedError {
  case privateConfigRequired
  case invalidConnection
  case missingConnection
  case authentication
  case rateLimited
  case status(Int, String?)
  case codexConfirmation(CodexSwitchConfirmation)
  case antigravityConfirmation(AntigravitySwitchConfirmation)
  case nonHTTPResponse
  case decoding
  /// The dashboard refused this tray's device key: revoked, expired or no longer valid (section 9).
  case signedOut(SignedOutNote)

  public var errorDescription: String? {
    switch self {
    case .privateConfigRequired: return "The AI Account Center connection file must be readable only by your user."
    case .invalidConnection: return "The AI Account Center connection configuration is invalid."
    case .missingConnection: return "The AI Account Center connection is not configured."
    case .authentication: return "The AI Account Center dashboard login was rejected."
    case .rateLimited: return "AI Account Center login is temporarily limited. Try again in 15 minutes."
    case .status(let code, let message): return message ?? "AI Account Center returned HTTP \(code)."
    case .codexConfirmation: return "Running Codex programs require confirmation before switching."
    case .antigravityConfirmation: return "Running Antigravity programs require confirmation before switching."
    case .nonHTTPResponse: return "AI Account Center did not return an HTTP response."
    case .decoding: return "The AI Account Center account data could not be read."
    case .signedOut: return "This tray was signed out. Pair it again to see usage."
    }
  }
}
