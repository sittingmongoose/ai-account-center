import Foundation

public protocol BarHTTPTransport: Sendable {
  func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse)
}

public final class BarSessionTransport: BarHTTPTransport, @unchecked Sendable {
  private let session: URLSession

  public init() {
    let config = URLSessionConfiguration.ephemeral
    config.httpShouldSetCookies = true
    config.timeoutIntervalForRequest = 90
    session = URLSession(configuration: config)
  }

  public func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
    let (data, response) = try await session.data(for: request)
    guard let http = response as? HTTPURLResponse else { throw BarClientError.nonHTTPResponse }
    return (data, http)
  }
}

/// Authenticated client. It talks only to CCS; providers and account secrets remain server-side.
public actor AccountsClient {
  public let connection: BarConnection
  private let transport: BarHTTPTransport
  private var authenticated = false
  private var loginTask: Task<Void, Error>?
  private var loginBlockedUntil: Date?

  public init(connection: BarConnection, transport: BarHTTPTransport = BarSessionTransport()) {
    self.connection = connection
    self.transport = transport
  }

  private func login() async throws {
    if authenticated { return }
    if let block = loginBlockedUntil, block > Date() { throw BarClientError.rateLimited }
    if let task = loginTask { return try await task.value }
    let task = Task {
      let body = try JSONSerialization.data(withJSONObject: [
        "username": connection.username, "password": connection.password,
      ])
      let (_, response) = try await transport.send(makeRequest("api/auth/login", method: "POST", body: body))
      switch response.statusCode {
      case 200: return
      case 429: throw BarClientError.rateLimited
      case 401: throw BarClientError.authentication
      default: throw BarClientError.status(response.statusCode, nil)
      }
    }
    loginTask = task
    do {
      try await task.value
      authenticated = true
      loginTask = nil
    } catch {
      loginTask = nil
      // Automatic polling must not repeatedly consume the dashboard's login attempts.
      loginBlockedUntil = Date().addingTimeInterval(15 * 60)
      throw error
    }
  }

  private func makeRequest(_ path: String, method: String = "GET", body: Data? = nil) -> URLRequest {
    let parts = path.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)
    var components = URLComponents(url: connection.baseURL.appendingPathComponent(String(parts[0])), resolvingAgainstBaseURL: false)!
    if parts.count == 2 { components.percentEncodedQuery = String(parts[1]) }
    var request = URLRequest(url: components.url!)
    request.httpMethod = method
    request.httpBody = body
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    if method != "GET" {
      request.setValue("application/json", forHTTPHeaderField: "Content-Type")
      request.setValue(connection.baseURL.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/")), forHTTPHeaderField: "Origin")
    }
    return request
  }

  private func request(_ path: String, method: String = "GET", body: Data? = nil, confirmationProfile: String? = nil, retryUnauthorized: Bool = true) async throws -> Data {
    try await login()
    var (data, response) = try await transport.send(makeRequest(path, method: method, body: body))
    if response.statusCode == 401 {
      authenticated = false
      if retryUnauthorized {
        try await login()
        (data, response) = try await transport.send(makeRequest(path, method: method, body: body))
      }
    }
    if response.statusCode == 409, let profile = confirmationProfile,
      let conflict = try? JSONDecoder().decode(CodexSwitchConflict.self, from: data),
      conflict.code == "busy", conflict.reason == "running_processes",
      let confirmation = conflict.confirmation, confirmation.isValid(for: profile) {
      throw BarClientError.codexConfirmation(confirmation)
    }
    guard (200..<300).contains(response.statusCode) else {
      let publicCode = (try? JSONSerialization.jsonObject(with: data) as? [String: Any])
      throw BarClientError.status(response.statusCode, Self.publicError(status: response.statusCode, path: path,
        codexActivation: confirmationProfile != nil, code: publicCode?["code"] as? String,
        reason: publicCode?["reason"] as? String))
    }
    return data
  }

  /// Only fixed client copy and recognized public codes may reach the UI.
  /// Server error/message strings can contain private paths or credential data.
  private static func publicError(status: Int, path: String, codexActivation: Bool,
    code: String?, reason: String?) -> String {
    if codexActivation {
      if status == 409 && code == "busy" {
        if reason == "activation_running" { return "Another Codex account activation is already running. Wait for it to finish." }
        if reason == "unsupported_process" { return "A running Codex program cannot be restarted safely. Close it and try again." }
        return "Codex is busy. Try switching after its work finishes."
      }
      if status == 409 && code == "confirmation_stale" {
        return "The running Codex programs or account changed. Activate again to review a new warning."
      }
      if status == 400 && code == "invalid_profile" { return "The selected profile has no valid saved login." }
      if status == 400 && code == "invalid_codex_home" { return "Account activation needs the shared Codex configuration." }
      if status == 500 {
        switch code {
        case "restart_failed": return "Codex could not restart. Check its processes before retrying activation."
        case "verification_failed": return "The activated account could not be verified. Refresh accounts before retrying."
        case "auth_read_failed": return "The saved Codex login could not be read safely."
        case "auth_write_failed": return "The Codex login could not be installed safely."
        default: break
        }
      }
    }
    if path.hasPrefix("api/claude/desktop-profiles/") && path.hasSuffix("/open") {
      if [502, 503].contains(status) { return "Claude could not be opened on the selected computer. Check its profile setup and connection." }
      if status == 404 { return "The selected Claude profile is not available on that computer." }
    }
    if path == "api/codex/profiles/auto-switch" && status == 400 {
      return "The automatic switching settings were rejected. Refresh and try again."
    }
    switch status {
    case 401: return "Your dashboard session expired. Check the login in Settings."
    case 403: return "AI Account Center rejected the request. Check the dashboard address in Settings."
    case 404: return "The requested account operation is unavailable."
    case 429: return "AI Account Center is busy. Wait before trying again."
    case 408, 504: return "AI Account Center request timed out. Try again."
    case 502, 503: return "AI Account Center is temporarily unavailable. Try again."
    case 400, 415, 422: return "AI Account Center rejected this request. Refresh and try again."
    default: return path.hasPrefix("api/accounts/dashboard") ? "Usage could not be refreshed. Try Refresh." : "AI Account Center request failed. Try again."
    }
  }

  public func get<T: Decodable>(_ path: String, as type: T.Type) async throws -> T {
    let data = try await request(path)
    do { return try JSONDecoder().decode(type, from: data) }
    catch { throw BarClientError.decoding }
  }

  public func dashboard(refresh: Bool = false) async throws -> AccountDashboard {
    try await get("api/accounts/dashboard?platform=mac&refresh=\(refresh ? "true" : "false")", as: AccountDashboard.self)
  }

  public func activateCodex(profile: String, confirmationToken: String? = nil) async throws {
    guard isProfileIdentifier(profile) else { throw BarClientError.invalidConnection }
    var body: [String: String] = [:]
    if let confirmationToken {
      guard !confirmationToken.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, confirmationToken.count <= 512
      else { throw BarClientError.invalidConnection }
      body["confirmationToken"] = confirmationToken
    }
    _ = try await request("api/codex/profiles/\(profile)/activate", method: "POST",
      body: JSONSerialization.data(withJSONObject: body), confirmationProfile: profile, retryUnauthorized: confirmationToken == nil)
  }

  public func openClaude(profile: String, platform: String = "mac") async throws {
    guard ["platyr", "gmail", "party", "me"].contains(profile), ["mac", "windows"].contains(platform)
    else { throw BarClientError.invalidConnection }
    try await write("api/claude/desktop-profiles/\(profile)/open", body: ["platform": platform])
  }

  private func isProfileIdentifier(_ value: String) -> Bool {
    value.range(of: "^[A-Za-z0-9][A-Za-z0-9_-]{0,63}\\z", options: .regularExpression) != nil
  }

  @discardableResult
  public func write(_ path: String, method: String = "POST", body: [String: String] = [:]) async throws -> Data {
    try await request(path, method: method, body: JSONSerialization.data(withJSONObject: body))
  }

  @discardableResult
  public func setAutomaticSwitching(enabled: Bool, thresholdPercent: Int? = nil) async throws -> Data {
    var body: [String: Any] = ["enabled": enabled]
    if let thresholdPercent {
      guard (1...99).contains(thresholdPercent) else { throw BarClientError.invalidConnection }
      body["thresholdPercent"] = thresholdPercent
    }
    return try await request("api/codex/profiles/auto-switch", method: "PUT", body: JSONSerialization.data(withJSONObject: body))
  }
}
