import Foundation

/// Why a sign-in or Change check failed. Every message is fixed client text; server strings never reach the screen.
public enum ConnectionCheckError: LocalizedError, Equatable, Sendable {
  case invalidDetails
  case unreachable
  case notDashboard
  case rejectedLogin
  case rateLimited
  case signInNotSetUp
  case forbidden
  case sessionDropped
  case timedOut
  case cancelled
  case saveFailed
  case failed

  public var errorDescription: String? {
    switch self {
    case .invalidDetails: return "Enter an http or https dashboard address without a path, plus the dashboard username and password."
    case .unreachable: return "Could not reach a dashboard at that address."
    case .notDashboard: return "That address answered, but not as an AI Account Center dashboard."
    case .rejectedLogin: return "The dashboard did not accept that username and password."
    case .rateLimited: return "The dashboard is limiting sign-in attempts. Wait 15 minutes, then try again."
    case .signInNotSetUp: return "The dashboard rejected this sign-in. Check that its password sign-in is set up."
    case .forbidden: return "The dashboard rejected a sign-in from this address. Check the address."
    case .sessionDropped: return "The dashboard accepted the sign-in but did not keep the session. Try again."
    case .timedOut: return "The dashboard took too long to answer."
    case .cancelled: return "Connection check cancelled."
    case .saveFailed: return "The connection could not be saved."
    case .failed: return "The dashboard could not check this sign-in. Try again."
    }
  }

  /// The fixed reason for a login status other than success.
  public static func login(status: Int) -> ConnectionCheckError {
    switch status {
    case 401: return .rejectedLogin
    case 429: return .rateLimited
    case 400: return .signInNotSetUp
    case 403: return .forbidden
    case 300..<400, 404, 405: return .notDashboard
    default: return .failed
    }
  }

  /// Any error from a check, as one fixed reason. `cancelled` is true when the person cancelled the check.
  public static func reason(_ error: Error, cancelled: Bool) -> ConnectionCheckError {
    if cancelled || error is CancellationError { return .cancelled }
    if let known = error as? ConnectionCheckError { return known }
    if let url = error as? URLError {
      switch url.code {
      case .cancelled: return .cancelled
      case .timedOut: return .timedOut
      default: return .unreachable
      }
    }
    if case BarClientError.nonHTTPResponse = error { return .notDashboard }
    return .failed
  }
}

/// The connection file and its writer. The tray uses `BarConnection.configURL`; checks use temporary files.
public enum ConnectionStore {
  /// Entered details, validated exactly as `BarConnection.load` validates a saved file.
  public static func candidate(baseURL: String, username: String, password: String) -> BarConnection? {
    guard let url = URL(string: baseURL.trimmingCharacters(in: .whitespacesAndNewlines)),
      ["http", "https"].contains(url.scheme ?? ""), url.host != nil,
      url.user == nil, url.password == nil, url.query == nil, url.fragment == nil,
      url.path.isEmpty || url.path == "/", !username.isEmpty, !password.isEmpty
    else { return nil }
    return BarConnection(baseURL: url, username: username, password: password)
  }

  /// The tray's writer, unchanged: a 0700 directory and a 0600 file holding `BarConnection`'s JSON, written
  /// atomically. Any other members the saved file holds (for example a device token from a later pairing) are kept
  /// exactly as they are.
  public static func save(_ connection: BarConnection, to url: URL) throws {
    var data = try JSONEncoder().encode(connection)
    if let existing = try? Data(contentsOf: url),
      let saved = try? JSONSerialization.jsonObject(with: existing) as? [String: Any] {
      let others = saved.filter { !["baseURL", "username", "password"].contains($0.key) }
      if !others.isEmpty, var merged = try JSONSerialization.jsonObject(with: data) as? [String: Any] {
        merged.merge(others) { own, _ in own }
        data = try JSONSerialization.data(withJSONObject: merged)
      }
    }
    let directory = url.deletingLastPathComponent()
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
      attributes: [.posixPermissions: 0o700])
    try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
    try data.write(to: url, options: .atomic)
    try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
  }
}

/// The tray's live dashboard connection and client. Sign-in and Change go through `change`: a temporary client built
/// from the entered details signs in and reads the dashboard first, and only a verified connection is written (the
/// same file, permissions and JSON) and replaces the live client. A wrong address, a rejected login, a timeout or
/// Cancel leaves the saved file and the live client exactly as they were.
@MainActor
public final class ConnectionSession {
  public private(set) var connection: BarConnection?
  public private(set) var client: AccountsClient?
  public let fileURL: URL
  /// How long a check may take before it counts as a failure.
  public var checkTimeout: TimeInterval = 15
  private let makeTransport: @Sendable () -> BarHTTPTransport
  private var check: Task<AccountsClient, Error>?

  public init(fileURL: URL = BarConnection.configURL,
    makeTransport: @escaping @Sendable () -> BarHTTPTransport = { BarSessionTransport() }) {
    self.fileURL = fileURL
    self.makeTransport = makeTransport
  }

  public var isChecking: Bool { check != nil }

  /// Reads the saved connection and builds its client.
  public func load() throws {
    let saved = try BarConnection.load(from: fileURL)
    connection = saved
    client = AccountsClient(connection: saved, transport: makeTransport())
  }

  /// No usable saved connection.
  public func clear() {
    connection = nil
    client = nil
  }

  /// Cancel (or Escape) while a check runs: nothing is saved and the live client stays.
  public func cancelCheck() { check?.cancel() }

  /// Verify, then save, then swap. Returns nil on success, or the message to show under the form.
  public func change(baseURL: String, username: String, password: String) async -> String? {
    guard check == nil else { return "A connection check is already running." }
    let unchanged = connection == nil ? "Nothing was saved." : "The saved connection was not changed."
    guard let candidate = ConnectionStore.candidate(baseURL: baseURL, username: username, password: password) else {
      return ConnectionCheckError.invalidDetails.errorDescription
    }
    let verifying = AccountsClient(connection: candidate, transport: makeTransport())
    let limit = UInt64(max(0, checkTimeout) * 1_000_000_000)
    let task = Task<AccountsClient, Error> {
      try await withThrowingTaskGroup(of: Void.self) { group in
        group.addTask { try await verifying.verify() }
        group.addTask {
          try await Task.sleep(nanoseconds: limit)
          throw ConnectionCheckError.timedOut
        }
        defer { group.cancelAll() }
        try await group.next()
      }
      return verifying
    }
    check = task
    defer { check = nil }
    do {
      let verified = try await task.value
      if task.isCancelled { throw CancellationError() }
      do { try ConnectionStore.save(candidate, to: fileURL) } catch {
        return "\(ConnectionCheckError.saveFailed.errorDescription ?? "") \(unchanged)"
      }
      connection = candidate
      client = verified
      return nil
    } catch {
      return "\(ConnectionCheckError.reason(error, cancelled: task.isCancelled).errorDescription ?? "") \(unchanged)"
    }
  }
}
