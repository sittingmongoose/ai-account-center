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
  case insecureAddress
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
    case .insecureAddress: return SignInCopy.useNumericAddress
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
      case .appTransportSecurityRequiresSecureConnection: return .insecureAddress
      default: return .unreachable
      }
    }
    if case BarClientError.nonHTTPResponse = error { return .notDashboard }
    return .failed
  }

  /// The Mac refused a plain HTTP request to a host name (App Transport Security). Only a numeric address, a `.local`
  /// name or a one-word name reaches the dashboard over plain HTTP from the packaged app.
  public static func isInsecureAddressBlock(_ error: Error) -> Bool {
    (error as? URLError)?.code == .appTransportSecurityRequiresSecureConnection
  }
}

/// The connection file and its writers. The tray uses `BarConnection.configURL`; checks use temporary files.
public enum ConnectionStore {
  /// Entered details, validated exactly as `BarConnection.load` validates a saved file.
  public static func candidate(baseURL: String, username: String, password: String) -> BarConnection? {
    guard let url = URL(string: baseURL.trimmingCharacters(in: .whitespacesAndNewlines)),
      BarConnection.isDashboardURL(url), !username.isEmpty, !password.isEmpty
    else { return nil }
    return BarConnection(baseURL: url, username: username, password: password)
  }

  /// The members that say how this tray signs in. A writer never carries one of them over from the old file, so a
  /// password can never sit next to a key and a stale key never survives a new login.
  static let authKeys: Set<String> = ["version", "baseURL", "username", "password", "deviceId", "deviceToken",
    "installId", "pairedAt", "signedOut"]

  /// The version 1 writer: a 0700 directory and a 0600 file holding `BarConnection`'s JSON, written atomically. Any
  /// other members the saved file holds are kept exactly as they are; the sign-in members are always the new ones.
  public static func save(_ connection: BarConnection, to url: URL) throws {
    try write(connection, to: url)
  }

  /// Writes a connection privately and atomically: the directory is 0700, the temporary file is created 0600 in the
  /// same directory before anything is written to it, then renamed over the old file and read back.
  public static func write(_ connection: BarConnection, to url: URL) throws {
    // Sorted keys: the same connection always writes the same bytes.
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    var data = try encoder.encode(connection)
    if let existing = try? Data(contentsOf: url),
      let saved = try? JSONSerialization.jsonObject(with: existing) as? [String: Any] {
      let others = saved.filter { !authKeys.contains($0.key) }
      if !others.isEmpty, var merged = try JSONSerialization.jsonObject(with: data) as? [String: Any] {
        merged.merge(others) { own, _ in own }
        data = try JSONSerialization.data(withJSONObject: merged, options: [.sortedKeys])
      }
    }
    try writePrivately(data, to: url)
    guard let back = try? BarConnection.load(from: url), back.deviceToken == connection.deviceToken,
      back.password == connection.password, back.baseURL == connection.baseURL
    else { throw ConnectionCheckError.saveFailed }
  }

  /// Raw bytes, privately and atomically (the rollback copy and its restore use this too).
  public static func writePrivately(_ data: Data, to url: URL) throws {
    let directory = url.deletingLastPathComponent()
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
      attributes: [.posixPermissions: 0o700])
    try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
    let temp = directory.appendingPathComponent(".\(url.lastPathComponent).\(UUID().uuidString).tmp")
    let descriptor = open(temp.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0o600)
    guard descriptor >= 0 else { throw ConnectionCheckError.saveFailed }
    var written = 0
    let ok: Bool = data.withUnsafeBytes { buffer in
      guard let base = buffer.baseAddress else { return data.isEmpty }
      while written < buffer.count {
        let count = Darwin.write(descriptor, base + written, buffer.count - written)
        if count <= 0 { return false }
        written += count
      }
      return true
    }
    let synced = fchmod(descriptor, 0o600) == 0 && fsync(descriptor) == 0
    close(descriptor)
    guard ok, synced, rename(temp.path, url.path) == 0 else {
      unlink(temp.path)
      throw ConnectionCheckError.saveFailed
    }
  }

  public static func remove(_ url: URL) {
    try? FileManager.default.removeItem(at: url)
  }
}

/// Runs `operation` for at most `seconds`; nil when it ran out of time. The caller gets its answer at the limit even
/// when the operation (a blocking name lookup, a stalled connection) does not stop at once.
public func bounded<T: Sendable>(_ seconds: TimeInterval, _ operation: @escaping @Sendable () async -> T) async -> T? {
  let gate = OnceGate()
  return await withCheckedContinuation { (continuation: CheckedContinuation<T?, Never>) in
    let work = Task {
      let value = await operation()
      if gate.claim() { continuation.resume(returning: value) }
    }
    Task {
      try? await Task.sleep(nanoseconds: UInt64(max(0, seconds) * 1_000_000_000))
      if gate.claim() {
        work.cancel()
        continuation.resume(returning: nil)
      }
    }
  }
}

final class OnceGate: @unchecked Sendable {
  private let lock = NSLock()
  private var claimed = false
  func claim() -> Bool {
    lock.lock()
    defer { lock.unlock() }
    if claimed { return false }
    claimed = true
    return true
  }
}

/// How the migration from a stored password ended (state 9, CONTRACT-auth-devices section 8).
public enum MigrationOutcome: Sendable, Equatable {
  /// Paired, the key worked once, and the password and its rollback copy are gone.
  case secured
  /// Paired and saved, but the first key check got no answer: the rollback copy stays and the check runs again on the
  /// next poll (at most 24 hours).
  case pending
  /// The dashboard has no pairing yet (404/405), refused it (403) or did not answer: today's password login stays.
  case keptPassword
  /// The stored password is wrong (401): the pairing screen, with the username filled in.
  case needsPassword(triesLeft: Int?)
  /// 429: today's 15-minute block.
  case rateLimited(until: Date)
}

/// The tray's live dashboard connection and client. Sign-in, pairing and Change go through here: a temporary client
/// built from the entered details proves them first, and only a verified connection is written (privately) and
/// replaces the live client. A wrong address, a rejected login, a timeout or Cancel leaves the saved file and the live
/// client exactly as they were.
@MainActor
public final class ConnectionSession {
  public private(set) var connection: BarConnection?
  public private(set) var client: AccountsClient?
  public let fileURL: URL
  /// How long a version 1 check may take before it counts as a failure.
  public var checkTimeout: TimeInterval = 15
  /// The address check's limit (state 8: "No answer after 10 seconds").
  public var addressTimeout: TimeInterval = 10
  /// Pairing's limit: the pair call and the first key check.
  public var pairTimeout: TimeInterval = 20
  /// How often the tray re-reads `devices/me` while it runs (for the rotation date and Settings).
  public var deviceCheckInterval: TimeInterval = 6 * 3600
  /// A migration's rollback copy lives at most this long (section 8).
  public var rollbackLifetime: TimeInterval = 24 * 3600
  /// Name resolution for the tray's own local-network check (a check fixture replaces it).
  public var resolver: @Sendable (String) -> [String] = { LocalNetwork.resolve($0) }
  public var now: () -> Date = { Date() }
  public lazy var deviceName: String = DashboardProbe.deviceName()
  public var appVersion: String? = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String
  /// The last `devices/me`, and when it was read.
  public private(set) var device: DeviceSelf?
  public private(set) var deviceCheckedAt: Date?
  /// How long the dashboard gets to revoke a key this tray will not keep (Cancel, an unconfirmed or unsaved key).
  public var discardTimeout: TimeInterval = 5
  /// How long a stored password waits before pairing is tried again after the dashboard refused it (403 with trust
  /// off, or no pairing yet) or did not answer. A key the dashboard refused right after a migration waits a day.
  public var migrationRetryInterval: TimeInterval = 3600
  private var rotationBlockedUntil: Date?
  private var migrationBlockedUntil: Date?
  private let makeTransport: @Sendable () -> BarHTTPTransport
  private var check: Task<AccountsClient, Error>?
  /// The install id of pair attempts not saved yet: one per sign-in session, so a retry after a key that was issued
  /// but never confirmed or saved replaces that key's record on the dashboard instead of adding another. Re-pair never
  /// uses the working key's own install id, because the dashboard revokes the old record of an install id the moment
  /// it pairs that id again.
  private var attemptInstallId: String?
  /// Bumped by Cancel and Escape: a pair still in flight then saves nothing and revokes any key it was issued.
  private var pairGeneration = 0
  private var pairsRunning = 0
  /// The keys a Re-pair replaced, until the dashboard confirms each is revoked (`DELETE /api/auth/devices/me` with
  /// that key). Maintenance tries again while the tray runs.
  private var retiredClients: [AccountsClient] = []
  private var retirement: Task<Void, Never>?

  public init(fileURL: URL = BarConnection.configURL,
    makeTransport: @escaping @Sendable () -> BarHTTPTransport = { BarSessionTransport() }) {
    self.fileURL = fileURL
    self.makeTransport = makeTransport
  }

  public var isChecking: Bool { check != nil }
  public var rollbackURL: URL { BarConnection.rollbackURL(for: fileURL) }
  public var hasPendingRollback: Bool { FileManager.default.fileExists(atPath: rollbackURL.path) }
  /// A pair or setup call is running (Cancel then revokes any key it is issued).
  public var isPairing: Bool { pairsRunning > 0 }
  /// A Re-pair's old key is still waiting for the dashboard to revoke it.
  public var isRetiringKey: Bool { !retiredClients.isEmpty }
  /// A stored password may try pairing again now (its last refusal or silence has waited long enough).
  public var migrationDue: Bool { migrationBlockedUntil.map { $0 <= now() } ?? true }

  /// Reads the saved connection. A signed-out file (no key, no password) feeds only the sign-in screen: no client.
  public func load() throws {
    let saved = try BarConnection.load(from: fileURL)
    connection = saved
    client = saved.isSignedOut ? nil : AccountsClient(connection: saved, transport: makeTransport())
    device = nil
    deviceCheckedAt = nil
  }

  /// No usable saved connection.
  public func clear() {
    connection = nil
    client = nil
    device = nil
    deviceCheckedAt = nil
  }

  /// Cancel (or Escape) while a check or a pair runs: nothing is saved and the live client stays. A pair whose key the
  /// dashboard already issued revokes that key instead of saving it, and the next attempt uses a new install id, so
  /// a late answer to the cancelled one can never replace it.
  public func cancelCheck() {
    check?.cancel()
    pairGeneration += 1
    if pairsRunning > 0 { attemptInstallId = UUID().uuidString }
  }

  /// Waits for a Re-pair's revoke of the old key to finish (checks and the end-to-end run).
  public func finishRetiring() async { await retirement?.value }

  /// The install id for the next pair attempt (see `attemptInstallId`).
  private func attemptInstall() -> String {
    let working = connection?.isPaired == true ? connection?.installId?.lowercased() : nil
    if let id = attemptInstallId, id.lowercased() != working { return id }
    let id = working == nil ? (connection?.installId ?? UUID().uuidString) : UUID().uuidString
    attemptInstallId = id
    return id
  }

  /// A key the dashboard issued that this tray will not keep: the dashboard is asked to revoke it at once, so it never
  /// stays active or counts toward the paired-tray limit. Best effort and short; a retry with the same install id
  /// replaces it anyway.
  private func discard(_ unused: AccountsClient) async {
    _ = await bounded(discardTimeout) { (try? await unused.disconnectDevice()) != nil }
  }

  /// Revokes the keys a Re-pair replaced, each with itself. A key the dashboard already refuses counts as revoked.
  private func revokeRetired() async {
    for old in retiredClients {
      let done = await bounded(pairTimeout) {
        do { try await old.disconnectDevice(); return true } catch BarClientError.signedOut { return true }
        catch { return false }
      } ?? false
      if done { retiredClients.removeAll { $0 === old } }
    }
  }

  /// The saved file exactly as it was, or nothing when there was none: a failed save leaves no half-changed file.
  private func restoreFile(_ previous: Data?) {
    if let previous { try? ConnectionStore.writePrivately(previous, to: fileURL) } else { ConnectionStore.remove(fileURL) }
  }

  private func iso(_ date: Date) -> String { ISO8601DateFormatter().string(from: date) }

  // MARK: Version 1 (a dashboard without pairing)

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
      device = nil
      return nil
    } catch {
      return "\(ConnectionCheckError.reason(error, cancelled: task.isCancelled).errorDescription ?? "") \(unchanged)"
    }
  }

  // MARK: Pairing (states 1 to 8, 11)

  /// The address step: nothing is saved, whatever the answer.
  public func checkAddress(_ raw: String) async -> AddressCheck {
    guard let url = DashboardProbe.normalize(raw) else { return .invalid }
    let probe = DashboardProbe(baseURL: url, transport: makeTransport())
    let resolver = self.resolver
    return await bounded(addressTimeout) { await probe.checkAddress(resolver: resolver) } ?? .unreachable(url)
  }

  /// Pair with a username and password, prove the new key once (`devices/me` 200), then save it. The saved
  /// connection and the live client change only after both succeed. Re-pair pairs under a new install id, so the
  /// working key stays valid until the new one is saved; only then is the old key revoked (with itself). A key that
  /// is never saved (no confirmation, a failed save, Cancel) is revoked instead. A dashboard without pairing (404/405)
  /// gets today's verified password login instead (`passwordLogin`).
  public func pair(url: URL, username: String, password: String) async -> PairOutcome {
    await pair(url: url, username: username, password: password, generation: pairGeneration)
  }

  private func pair(url: URL, username: String, password: String, generation: Int) async -> PairOutcome {
    guard generation == pairGeneration else { return .cancelled }
    pairsRunning += 1
    defer { pairsRunning -= 1 }
    let probe = DashboardProbe(baseURL: url, transport: makeTransport())
    let installId = attemptInstall()
    let name = deviceName, version = appVersion
    guard let answer = await bounded(pairTimeout, {
      do { return Optional(try await probe.pair(username: username, password: password, installId: installId,
        deviceName: name, appVersion: version)) } catch { return nil }
    }) ?? nil else { return generation == pairGeneration ? .refused(.unreachable(url)) : .cancelled }
    switch answer {
    case .success(let paired):
      let candidate = BarConnection(baseURL: url, username: username, deviceId: paired.deviceId,
        deviceToken: paired.token, installId: installId, pairedAt: paired.pairedAt ?? iso(now()))
      let verifying = AccountsClient(connection: candidate, transport: makeTransport())
      guard generation == pairGeneration else {
        await discard(verifying)
        return .cancelled
      }
      guard let me = await bounded(pairTimeout, { try? await verifying.deviceSelf() }) ?? nil else {
        await discard(verifying)
        return generation == pairGeneration ? .failed(SignInCopy.notConfirmed, field: nil) : .cancelled
      }
      guard generation == pairGeneration else {
        await discard(verifying)
        return .cancelled
      }
      // From here to the return nothing waits, so Cancel lands either before the save or after the swap.
      let previousFile = try? Data(contentsOf: fileURL)
      do { try ConnectionStore.write(candidate, to: fileURL) } catch {
        restoreFile(previousFile)
        await discard(verifying)
        return .failed(SignInCopy.saveFailed, field: nil)
      }
      let replaced = connection?.isPaired == true ? client : nil
      ConnectionStore.remove(rollbackURL)
      connection = candidate
      client = verifying
      device = me
      deviceCheckedAt = now()
      attemptInstallId = nil
      if let replaced {
        retiredClients.append(replaced)
        let earlier = retirement
        retirement = Task {
          await earlier?.value
          await self.revokeRetired()
        }
      }
      return .paired(candidate, me)
    case .failure(let refusal):
      guard generation == pairGeneration else { return .cancelled }
      if [404, 405].contains(refusal.status) {
        if let failure = await change(baseURL: url.absoluteString, username: username, password: password) {
          return .failed(failure, field: nil)
        }
        return .unsupported
      }
      return await outcome(refusal, url: url)
    }
  }

  /// State 2: create the dashboard's sign-in with the one-time setup code, then pair with it.
  /// Cancel during setup stops before pairing; the sign-in the dashboard already created stays (Pair then finds it).
  public func setupAndPair(url: URL, username: String, password: String, setupCode: String?) async -> PairOutcome {
    let generation = pairGeneration
    pairsRunning += 1
    defer { pairsRunning -= 1 }
    let probe = DashboardProbe(baseURL: url, transport: makeTransport())
    guard let answer = await bounded(pairTimeout, {
      do { return Result<PairRefusal?, Error>.success(try await probe.setup(username: username, password: password,
        setupCode: setupCode)) } catch { return .failure(error) }
    }), case .success(let refusal) = answer else {
      return generation == pairGeneration ? .refused(.unreachable(url)) : .cancelled
    }
    guard generation == pairGeneration else { return .cancelled }
    guard let refusal else { return await pair(url: url, username: username, password: password, generation: generation) }
    switch (refusal.status, refusal.code) {
    case (403, "setup_code_required"), (403, "setup_code_invalid"): return .setupCode(triesLeft: refusal.triesLeft)
    case (409, "already_configured"): return .alreadyConfigured
    case (409, "managed_by_env"): return .failed(SignInCopy.managedByEnv, field: nil)
    case (400, "invalid_username"): return .failed(SignInCopy.usernameRule, field: "user")
    case (400, "weak_password"):
      return .failed(refusal.reason == "too_long" ? SignInCopy.passwordLong : SignInCopy.passwordShort, field: "pass")
    default: return await outcome(refusal, url: url)
    }
  }

  private func outcome(_ refusal: PairRefusal, url: URL) async -> PairOutcome {
    switch (refusal.status, refusal.code) {
    case (401, _): return .wrongPassword(triesLeft: refusal.triesLeft)
    case (429, _): return .rateLimited(until: now().addingTimeInterval(max(1, refusal.retryAfter ?? 900)))
    case (403, "secure_transport_required"):
      let again = await checkAddress(url.absoluteString)
      if case .ready = again { return .refused(.pairingOff(url)) }
      return .refused(again)
    case (409, "auth_not_configured"): return .failed(SignInCopy.signInOff, field: nil)
    case (409, "too_many_devices"): return .failed(SignInCopy.tooManyDevices, field: nil)
    case (503, _): return .failed(SignInCopy.storeUnavailable, field: nil)
    case (403, _): return .refused(.notDashboard(url))
    default: return .failed(SignInCopy.failed, field: nil)
    }
  }

  // MARK: Migration from a stored password (state 9, section 8)

  /// Pairs with the stored version 1 login: the version 1 file is copied aside (0600), version 2 replaces it, and the
  /// first `devices/me` 200 deletes the copy. A 401 device code puts version 1 back; no answer keeps both. It runs at
  /// launch and again from the refresh path whenever `migrationDue` (an hour after a refusal or no answer, a day after
  /// a key the dashboard refused), so turning on "Trust this local network" later needs no restart.
  public func migrate() async -> MigrationOutcome {
    guard let old = connection, old.hasPassword, let password = old.password else { return .keptPassword }
    guard migrationDue else { return .keptPassword }
    let probe = DashboardProbe(baseURL: old.baseURL, transport: makeTransport())
    let installId = attemptInstall()
    let name = deviceName, version = appVersion
    let retryLater = now().addingTimeInterval(migrationRetryInterval)
    guard let answer = await bounded(pairTimeout, {
      do { return Optional(try await probe.pair(username: old.username, password: password, installId: installId,
        deviceName: name, appVersion: version)) } catch { return nil }
    }) ?? nil else {
      migrationBlockedUntil = retryLater
      return .keptPassword
    }
    switch answer {
    case .failure(let refusal):
      switch refusal.status {
      case 401:
        // The pairing screen takes over; the refresh path does not spend another try on the same password.
        migrationBlockedUntil = retryLater
        return .needsPassword(triesLeft: refusal.triesLeft)
      case 429:
        let until = now().addingTimeInterval(max(1, refusal.retryAfter ?? 900))
        migrationBlockedUntil = until
        return .rateLimited(until: until)
      default:
        // No pairing yet (404/405), or refused over this transport (403, trust off): keep the password login and try
        // again from the refresh path after `migrationRetryInterval`.
        migrationBlockedUntil = retryLater
        return .keptPassword
      }
    case .success(let paired):
      let next = BarConnection(baseURL: old.baseURL, username: old.username, deviceId: paired.deviceId,
        deviceToken: paired.token, installId: installId, pairedAt: paired.pairedAt ?? iso(now()))
      let issued = AccountsClient(connection: next, transport: makeTransport())
      guard let v1 = try? Data(contentsOf: fileURL), (try? ConnectionStore.writePrivately(v1, to: rollbackURL)) != nil else {
        migrationBlockedUntil = retryLater
        await discard(issued)
        return .keptPassword
      }
      do { try ConnectionStore.write(next, to: fileURL) } catch {
        try? ConnectionStore.writePrivately(v1, to: fileURL)
        ConnectionStore.remove(rollbackURL)
        migrationBlockedUntil = retryLater
        await discard(issued)
        return .keptPassword
      }
      connection = next
      client = issued
      device = nil
      deviceCheckedAt = nil
      attemptInstallId = nil
      return await confirmMigration()
    }
  }

  /// The first key check after a migration, and again on each poll while the rollback copy exists.
  @discardableResult
  public func confirmMigration() async -> MigrationOutcome {
    guard hasPendingRollback else { return .secured }
    guard let client, connection?.isPaired == true else { return restoreRollback() }
    let attrs = try? FileManager.default.attributesOfItem(atPath: rollbackURL.path)
    let age = (attrs?[.modificationDate] as? Date).map { now().timeIntervalSince($0) } ?? 0
    enum Check: Sendable { case confirmed(DeviceSelf), signedOut, noAnswer }
    let answer: Check = await bounded(pairTimeout) {
      do { return Check.confirmed(try await client.deviceSelf()) }
      catch let error as BarClientError {
        if case .signedOut = error { return Check.signedOut }
        return Check.noAnswer
      } catch { return Check.noAnswer }
    } ?? .noAnswer
    switch answer {
    case .confirmed(let me):
      ConnectionStore.remove(rollbackURL)
      device = me
      deviceCheckedAt = now()
      return .secured
    case .signedOut:
      return restoreRollback()
    case .noAnswer:
      if age > rollbackLifetime {
        // The copy never lives longer than 24 hours; the key that still cannot be checked stays.
        ConnectionStore.remove(rollbackURL)
      }
      return .pending
    }
  }

  /// Puts the version 1 file back and keeps today's password login.
  private func restoreRollback() -> MigrationOutcome {
    guard let v1 = try? Data(contentsOf: rollbackURL) else { return .keptPassword }
    do {
      try ConnectionStore.writePrivately(v1, to: fileURL)
      let restored = try BarConnection.load(from: fileURL)
      connection = restored
      client = AccountsClient(connection: restored, transport: makeTransport())
    } catch { return .keptPassword }
    ConnectionStore.remove(rollbackURL)
    device = nil
    deviceCheckedAt = nil
    migrationBlockedUntil = now().addingTimeInterval(24 * 3600)
    return .keptPassword
  }

  // MARK: Signed out, Disconnect, rotation (sections 6, 7 and 9)

  /// A 401 device code: the key is deleted, the address, username and install id stay, and polling stops. During a
  /// migration's first check, version 1 comes back instead (section 8).
  public func signOut(_ note: SignedOutNote) {
    guard let current = connection else { return }
    if hasPendingRollback, case .keptPassword = restoreRollback(), connection?.hasPassword == true { return }
    let out = current.signingOut(note)
    try? ConnectionStore.write(out, to: fileURL)
    connection = out
    client = nil
    device = nil
    deviceCheckedAt = nil
  }

  /// Disconnect: the dashboard revokes this key (`DELETE /api/auth/devices/me`), then the tray forgets it and keeps
  /// the address for the first-run screen. Returns false when the dashboard could not be told.
  @discardableResult
  public func disconnect() async -> Bool {
    var told = false
    if let client, connection?.isPaired == true {
      told = await bounded(pairTimeout) {
        do { try await client.disconnectDevice(); return true } catch BarClientError.signedOut { return true }
        catch { return false }
      } ?? false
    }
    ConnectionStore.remove(rollbackURL)
    if let current = connection {
      let out = current.signingOut(SignedOutNote(reason: "disconnected", at: iso(now())))
      try? ConnectionStore.write(out, to: fileURL)
      connection = out
    }
    client = nil
    device = nil
    deviceCheckedAt = nil
    return told
  }

  /// Run on each poll while paired: the pending migration check, `devices/me` every few hours, and rotation once
  /// `rotateAfter` has passed. The new key is saved before it is first used. Throws only a sign-out.
  public func maintain() async throws {
    guard connection?.isPaired == true, let client else { return }
    if !retiredClients.isEmpty {
      await retirement?.value
      await revokeRetired()
    }
    if hasPendingRollback { await confirmMigration() }
    guard let current = connection, current.isPaired, let live = self.client, live === client else { return }
    if deviceCheckedAt.map({ now().timeIntervalSince($0) >= deviceCheckInterval }) ?? true {
      do {
        device = try await client.deviceSelf()
        deviceCheckedAt = now()
      } catch BarClientError.signedOut(let note) { throw BarClientError.signedOut(note) }
      catch { /* A network error is not a sign-out; the next poll tries again. */ }
    }
    guard let due = AccountFormatting.date(device?.rotateAfter), due <= now(),
      (rotationBlockedUntil.map { $0 <= now() } ?? true) else { return }
    do {
      let key = try await client.rotateKey()
      let next = current.rotating(to: key.token)
      do { try ConnectionStore.write(next, to: fileURL) } catch {
        rotationBlockedUntil = now().addingTimeInterval(3600)
        return
      }
      await client.adopt(token: key.token)
      connection = next
      device = nil
      deviceCheckedAt = nil
    } catch BarClientError.signedOut(let note) {
      throw BarClientError.signedOut(note)
    } catch {
      // 403 secure_transport_required (trust turned off) or no answer: keep the current key and try later.
      rotationBlockedUntil = now().addingTimeInterval(3600)
    }
  }
}
