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
///
/// A paired connection (version 2) sends its device key as `Authorization: Bearer` on every request and never logs
/// in; a 401 with a device code ends it (`BarClientError.signedOut`). A version 1 connection keeps today's cookie login
/// until it is paired (CONTRACT-auth-devices sections 6, 8 and 9).
public actor AccountsClient {
  public let connection: BarConnection
  private let transport: BarHTTPTransport
  private var authenticated = false
  private var loginTask: Task<Void, Error>?
  private var loginBlockedUntil: Date?
  /// The device key now in use. Rotation replaces it here only after the new key is saved (section 7).
  private var deviceToken: String?

  public init(connection: BarConnection, transport: BarHTTPTransport = BarSessionTransport()) {
    self.connection = connection
    self.transport = transport
    deviceToken = connection.deviceToken
  }

  /// True when this client signs in with a device key.
  public var usesDeviceKey: Bool { deviceToken != nil }

  /// Rotation: the caller has written the new key privately; every later request uses it.
  public func adopt(token: String) {
    guard DeviceToken.isWellFormed(token) else { return }
    deviceToken = token
  }

  private func login() async throws {
    if authenticated { return }
    // A paired tray has no password to log in with.
    guard connection.hasPassword, let password = connection.password else { throw BarClientError.authentication }
    if let block = loginBlockedUntil, block > Date() { throw BarClientError.rateLimited }
    if let task = loginTask { return try await task.value }
    let task = Task {
      let body = try JSONSerialization.data(withJSONObject: [
        "username": connection.username, "password": password,
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

  /// Sign-in and Change: proves this connection before anything is saved. One login (no retry and no failure
  /// backoff), then GET /api/accounts/settings, a small read that always needs the session and answers with the
  /// dashboard's refresh settings. Success leaves the client signed in, ready to become the live client. Failures
  /// throw `ConnectionCheckError` (fixed text) or the transport's own error.
  public func verify() async throws {
    guard let password = connection.password, !password.isEmpty else { throw ConnectionCheckError.invalidDetails }
    let body = try JSONSerialization.data(withJSONObject: ["username": connection.username, "password": password])
    let (_, login) = try await transport.send(makeRequest("api/auth/login", method: "POST", body: body))
    guard (200..<300).contains(login.statusCode) else { throw ConnectionCheckError.login(status: login.statusCode) }
    let (data, read) = try await transport.send(makeRequest("api/accounts/settings"))
    if read.statusCode == 401 { throw ConnectionCheckError.sessionDropped }
    guard (200..<300).contains(read.statusCode),
      let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
      object["refreshIntervalSeconds"] is NSNumber
    else { throw ConnectionCheckError.notDashboard }
    authenticated = true
    loginBlockedUntil = nil
  }

  private func makeRequest(_ path: String, method: String = "GET", body: Data? = nil,
    headers: [String: String] = [:]) -> URLRequest {
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
    for (field, value) in headers { request.setValue(value, forHTTPHeaderField: field) }
    return request
  }

  private func request(_ path: String, method: String = "GET", body: Data? = nil, confirmationProfile: String? = nil,
    antigravityProfile: String? = nil, retryUnauthorized: Bool = true, headers: [String: String] = [:]) async throws -> Data {
    try await exchange(path, method: method, body: body, confirmationProfile: confirmationProfile,
      antigravityProfile: antigravityProfile, retryUnauthorized: retryUnauthorized, headers: headers).data
  }

  /// One request with its status, for the callers whose next step depends on it (the Claude Open's 200 or 202).
  private func exchange(_ path: String, method: String = "GET", body: Data? = nil, confirmationProfile: String? = nil,
    antigravityProfile: String? = nil, retryUnauthorized: Bool = true,
    headers: [String: String] = [:]) async throws -> (data: Data, status: Int) {
    var data: Data
    var response: HTTPURLResponse
    if let token = deviceToken {
      (data, response) = try await transport.send(bearer(makeRequest(path, method: method, body: body, headers: headers), token))
      // A key rotated while this request was in flight: the old key may already be gone, so the request goes
      // once more with the saved new key instead of signing the tray out.
      if response.statusCode == 401, let current = deviceToken, current != token {
        (data, response) = try await transport.send(bearer(makeRequest(path, method: method, body: body, headers: headers), current))
      }
      if response.statusCode == 401 {
        if let note = Self.signedOutNote(data) { throw BarClientError.signedOut(note) }
        // An older dashboard that ignores device keys: say so plainly; there is no password to retry with.
        throw BarClientError.status(401, "The dashboard did not accept this tray's device key. Pair it again in Settings.")
      }
    } else {
      try await login()
      (data, response) = try await transport.send(makeRequest(path, method: method, body: body, headers: headers))
      if response.statusCode == 401 {
        authenticated = false
        if retryUnauthorized {
          try await login()
          (data, response) = try await transport.send(makeRequest(path, method: method, body: body, headers: headers))
        }
      }
    }
    if response.statusCode == 409, let profile = confirmationProfile,
      let conflict = try? JSONDecoder().decode(CodexSwitchConflict.self, from: data),
      conflict.code == "busy", conflict.reason == "running_processes",
      let confirmation = conflict.confirmation, confirmation.isValid(for: profile) {
      throw BarClientError.codexConfirmation(confirmation)
    }
    if response.statusCode == 409, let profile = antigravityProfile,
      let offer = AntigravitySwitchConfirmation.parse(data, profile: profile) {
      throw BarClientError.antigravityConfirmation(offer)
    }
    guard (200..<300).contains(response.statusCode) else {
      let publicCode = (try? JSONSerialization.jsonObject(with: data) as? [String: Any])
      throw BarClientError.status(response.statusCode, Self.publicError(status: response.statusCode, path: path,
        codexActivation: confirmationProfile != nil, code: publicCode?["code"] as? String,
        reason: publicCode?["reason"] as? String,
        antigravityActivation: antigravityProfile != nil, activationStatus: publicCode?["status"] as? String))
    }
    return (data, response.statusCode)
  }

  /// Only fixed client copy and recognized public codes may reach the UI.
  /// Server error/message strings can contain private paths or credential data.
  private static func publicError(status: Int, path: String, codexActivation: Bool,
    code: String?, reason: String?, antigravityActivation: Bool = false, activationStatus: String? = nil) -> String {
    if antigravityActivation {
      // Only the dashboard's fixed activation status words select a message; its text never does.
      switch activationStatus {
      case "busy": return "Antigravity is busy on Ubuntu. Activate again when it is idle."
      case "deferred": return "Antigravity activation is deferred on Ubuntu. Refresh its native status before trying again."
      case "unsupported-runtime-probe": return "The Ubuntu Antigravity runtime could not be verified. Account switching is unavailable."
      case "stale-confirmation": return "This Antigravity confirmation is no longer valid. Activate again to review the running programs."
      case "confirmation-required": return "Antigravity programs are running on Ubuntu. Activate again to review them."
      case "invalid-profile": return "The selected Antigravity profile has no valid saved login."
      case "failed-rolled-back": return "Antigravity could not switch accounts on Ubuntu. The previous state was restored."
      case "recovery-required": return "Antigravity activation needs recovery on Ubuntu. Account switching is unavailable."
      default: break
      }
      if status == 500 { return "Antigravity account activation failed safely. Refresh the account list before retrying." }
    }
    if path == "api/antigravity/auto-switch" {
      if status == 400 { return "The Antigravity switching settings were rejected. Refresh and try again." }
      if status == 500 { return "Antigravity automatic switching settings could not be saved safely." }
    }
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
      // The guarded history copy could not be confirmed, so Claude was not opened (CONTRACT-serving-misc 4.4).
      if status == 409 && code == "history_unconfirmed" { return ClaudeOpenFlow.historyUnconfirmed }
      if [502, 503].contains(status) { return "Claude could not be opened on the selected computer. Check its profile setup and connection." }
      if status == 404 { return "The selected Claude profile is not available on that computer." }
    }
    if path == "api/codex/profiles/auto-switch" && status == 400 {
      return "The automatic switching settings were rejected. Refresh and try again."
    }
    if status == 503 && code == "auth_store_unavailable" {
      return "The dashboard cannot check paired trays right now. Try again shortly."
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

  /// Manual Antigravity activation on the shared Ubuntu runtime. Running programs come back as
  /// `BarClientError.antigravityConfirmation`, which only an explicit Confirm may answer.
  @discardableResult
  public func activateAntigravity(profile: String) async throws -> AntigravityActivationResult {
    guard isProfileIdentifier(profile) else { throw BarClientError.invalidConnection }
    let data = try await request("api/antigravity/profiles/\(profile)/activate", method: "POST",
      body: JSONSerialization.data(withJSONObject: ["hostId": "ubuntu"]), antigravityProfile: profile)
    guard let result = AntigravityActivationResult.parse(data, profile: profile) else { throw BarClientError.decoding }
    return result
  }

  /// Sends a reviewed one-use token once. It is never replayed after an authentication failure.
  @discardableResult
  public func confirmAntigravity(profile: String, confirmationToken: String) async throws -> AntigravityActivationResult {
    guard isProfileIdentifier(profile), AntigravitySwitchConfirmation.isToken(confirmationToken)
    else { throw BarClientError.invalidConnection }
    let data = try await request("api/antigravity/profiles/\(profile)/confirm", method: "POST",
      body: JSONSerialization.data(withJSONObject: ["hostId": "ubuntu", "confirmationToken": confirmationToken]),
      antigravityProfile: profile, retryUnauthorized: false)
    guard let result = AntigravityActivationResult.parse(data, profile: profile) else { throw BarClientError.decoding }
    return result
  }

  /// Antigravity's own policy: `thresholdUsedPercent` is percent USED (Codex stores percent remaining).
  @discardableResult
  public func setAntigravityAutomaticSwitching(enabled: Bool? = nil, thresholdUsedPercent: Int? = nil) async throws -> Data {
    var body: [String: Any] = [:]
    if let enabled { body["enabled"] = enabled }
    if let thresholdUsedPercent {
      guard (1...99).contains(thresholdUsedPercent) else { throw BarClientError.invalidConnection }
      body["thresholdUsedPercent"] = thresholdUsedPercent
    }
    guard !body.isEmpty else { throw BarClientError.invalidConnection }
    return try await request("api/antigravity/auto-switch", method: "PUT", body: JSONSerialization.data(withJSONObject: body))
  }

  /// Claude "Open on Mac" and "Open on Windows". `Prefer: respond-async` opts into the 202 progress answer; without
  /// it the server waits and answers 200. The POST is sent once: an expired session is never re-sent as a second
  /// Open, exactly as a one-use confirmation token is not.
  /// The profile id comes from the dashboard's own data (`capabilities.claudeProfileId`); the tray keeps no list of
  /// its own, and only checks that the id is a plain identifier before it goes into the path.
  @discardableResult
  public func openClaude(profile: String, platform: String = "mac") async throws -> ClaudeOpenStart {
    guard isProfileIdentifier(profile), ["mac", "windows"].contains(platform)
    else { throw BarClientError.invalidConnection }
    let (data, status) = try await exchange("api/claude/desktop-profiles/\(profile)/open", method: "POST",
      body: JSONSerialization.data(withJSONObject: ["platform": platform]), retryUnauthorized: false,
      headers: ["Prefer": "respond-async"])
    guard status == 202 else { return .opened }
    let accepted = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    return .accepted(operationId: accepted?["operationId"] as? String ?? "", state: accepted?["state"] as? String ?? "checking")
  }

  /// The Claude desktop profile list, read while an Open runs for its `openOperation` progress. It is on the tray's
  /// device-token allowlist and carries no UUIDs, titles, transcript text, ssh details or paths.
  public func claudeDesktopProfiles() async throws -> [ClaudeDesktopProfile] {
    try await get("api/claude/desktop-profiles", as: ClaudeDesktopProfileList.self).profiles
  }

  // MARK: This tray's own device (bearer only)

  /// `GET /api/auth/devices/me`: proves the key works, and says when to rotate it.
  public func deviceSelf() async throws -> DeviceSelf {
    guard deviceToken != nil else { throw BarClientError.missingConnection }
    let data = try await request("api/auth/devices/me", retryUnauthorized: false)
    guard let value = try? JSONDecoder().decode(DeviceSelf.self, from: data) else { throw BarClientError.decoding }
    return value
  }

  /// `POST /api/auth/devices/me/rotate`: a new key, which the caller saves before `adopt(token:)` (section 7).
  public func rotateKey() async throws -> RotatedKey {
    guard deviceToken != nil else { throw BarClientError.missingConnection }
    let data = try await request("api/auth/devices/me/rotate", method: "POST",
      body: Data("{}".utf8), retryUnauthorized: false)
    guard let value = try? JSONDecoder().decode(RotatedKey.self, from: data), DeviceToken.isWellFormed(value.token)
    else { throw BarClientError.decoding }
    return value
  }

  /// `DELETE /api/auth/devices/me`: Disconnect. The dashboard revokes this key at once (204).
  public func disconnectDevice() async throws {
    guard deviceToken != nil else { throw BarClientError.missingConnection }
    _ = try await request("api/auth/devices/me", method: "DELETE", body: Data("{}".utf8), retryUnauthorized: false)
  }

  private func bearer(_ request: URLRequest, _ token: String) -> URLRequest {
    var request = request
    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
    return request
  }

  /// A 401 that carries one of the dashboard's device codes, as the note the sign-in screen shows. The optional
  /// `revokedAt`, `revokedReason` and `revokedBy` say who signed the tray out and when, if the dashboard sends them.
  static func signedOutNote(_ data: Data, now: Date = Date()) -> SignedOutNote? {
    guard let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
      let code = object["code"] as? String, ["device_revoked", "device_expired", "invalid_token"].contains(code)
    else { return nil }
    func text(_ key: String, limit: Int) -> String? {
      guard let value = object[key] as? String, !value.isEmpty, value.count <= limit,
        value.rangeOfCharacter(from: .newlines) == nil else { return nil }
      return value
    }
    let at = text("revokedAt", limit: 40).flatMap { AccountFormatting.date($0) != nil ? $0 : nil }
      ?? ISO8601DateFormatter().string(from: now)
    let by = text("revokedBy", limit: 64).flatMap {
      $0.range(of: "^[A-Za-z][A-Za-z0-9_-]{2,63}\\z", options: .regularExpression) != nil ? $0 : nil
    }
    let reason = text("revokedReason", limit: 32).flatMap {
      $0.range(of: "^[a-z_-]{1,32}\\z", options: .regularExpression) != nil ? $0 : nil
    }
    return SignedOutNote(reason: code, at: at, revokedReason: reason, revokedBy: by)
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
