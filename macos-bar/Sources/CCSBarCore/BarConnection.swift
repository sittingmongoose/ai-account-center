import Foundation

/// Private connection material stays in the user's home directory, outside the app/source bundle.
public struct BarConnection: Codable, Sendable {
  public let baseURL: URL
  public let username: String
  public let password: String

  public init(baseURL: URL, username: String, password: String) {
    self.baseURL = baseURL
    self.username = username
    self.password = password
  }

  public static var configURL: URL {
    FileManager.default.homeDirectoryForCurrentUser
      .appendingPathComponent(".ccs/bar/accounts-connection.json")
  }

  public static func load(from url: URL = configURL) throws -> BarConnection {
    let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
    let permissions = (attrs[.posixPermissions] as? NSNumber)?.intValue ?? 0o777
    guard permissions & 0o077 == 0 else { throw BarClientError.privateConfigRequired }
    let config = try JSONDecoder().decode(BarConnection.self, from: Data(contentsOf: url))
    guard ["http", "https"].contains(config.baseURL.scheme ?? ""),
      config.baseURL.host != nil, config.baseURL.user == nil, config.baseURL.password == nil,
      config.baseURL.query == nil, config.baseURL.fragment == nil,
      config.baseURL.path.isEmpty || config.baseURL.path == "/",
      !config.username.isEmpty, !config.password.isEmpty
    else { throw BarClientError.invalidConnection }
    return config
  }
}

public enum BarClientError: LocalizedError {
  case privateConfigRequired
  case invalidConnection
  case missingConnection
  case authentication
  case rateLimited
  case status(Int, String?)
  case codexConfirmation(CodexSwitchConfirmation)
  case nonHTTPResponse
  case decoding

  public var errorDescription: String? {
    switch self {
    case .privateConfigRequired: return "The CCS connection file must be readable only by your user."
    case .invalidConnection: return "The CCS connection configuration is invalid."
    case .missingConnection: return "The CCS connection is not configured."
    case .authentication: return "The CCS dashboard login was rejected."
    case .rateLimited: return "CCS login is temporarily limited. Try again in 15 minutes."
    case .status(let code, let message): return message ?? "CCS returned HTTP \(code)."
    case .codexConfirmation: return "Running Codex programs require confirmation before switching."
    case .nonHTTPResponse: return "CCS did not return an HTTP response."
    case .decoding: return "The CCS account data could not be read."
    }
  }
}
