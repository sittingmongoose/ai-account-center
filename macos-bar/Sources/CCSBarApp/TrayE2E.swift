import SwiftUI
import AppKit
import CCSBarCore

/// `--e2e <base URL> <phase>`: a real end-to-end run of the tray's sign-in, pairing and sign-out flows against a
/// SANDBOX dashboard (never the live one), driving the same `SignInModel`, `AccountsViewModel` and
/// `ConnectionSession` the panel uses, over real HTTP. Its tray state lives only in `AAC_TRAY_STATE_DIR`, which must be
/// set and must not be `~/.ccs/bar`. The dashboard's test login comes from `AAC_E2E_USER`, `AAC_E2E_PASSWORD` and
/// `AAC_E2E_NEW_PASSWORD`; nothing prints a password or a key. Screens are read back with text recognition in
/// memory; no screenshot is written.
@MainActor
enum TrayE2E {
  private static var steps: [[String: Any]] = []

  private static func record(_ name: String, _ passed: Bool, _ detail: [String: Any] = [:]) {
    var entry = detail
    entry["step"] = name
    entry["passed"] = passed
    steps.append(entry)
    print("\(passed ? "PASS" : "FAIL") \(name)")
  }

  private static func pump(_ seconds: Double) { RunLoop.main.run(until: Date().addingTimeInterval(seconds)) }

  @discardableResult
  private static func wait(_ seconds: Double = 15, until condition: () -> Bool) -> Bool {
    let deadline = Date().addingTimeInterval(seconds)
    while !condition() && Date() < deadline { pump(0.05) }
    return condition()
  }

  private static func screen(_ model: SignInModel) -> String {
    guard let host = try? PreviewRenderer.signInHost(model: model, appearance: "light") else { return "" }
    let text = ((try? PreviewRenderer.recognizedText(host)) ?? []).joined(separator: " ")
    host.window?.orderOut(nil)
    return text
  }

  private static func shows(_ text: String, _ needles: [String]) -> Bool {
    needles.allSatisfy { text.lowercased().contains($0.lowercased()) }
  }

  private static func finish() -> Never {
    let passed = !steps.isEmpty && steps.allSatisfy { $0["passed"] as? Bool == true }
    let result: [String: Any] = ["passed": passed, "steps": steps, "screenshotsWritten": false]
    print(String(decoding: (try? JSONSerialization.data(withJSONObject: result, options: [.sortedKeys, .prettyPrinted])) ?? Data(), as: UTF8.self))
    exit(passed ? 0 : 1)
  }

  private static func refuse(_ why: String) -> Never {
    fputs("End-to-end run refused: \(why)\n", stderr)
    exit(2)
  }

  static func run(arguments: [String]) -> Never {
    NSApplication.shared.setActivationPolicy(.prohibited)
    let environment = ProcessInfo.processInfo.environment
    guard let raw = arguments.first, let base = DashboardProbe.normalize(raw), let port = base.port, (3901...3999).contains(port)
    else { refuse("the sandbox address must be an http address on a port from 3901 to 3999 (never the live 3000)") }
    guard let stateDir = environment["AAC_TRAY_STATE_DIR"], !stateDir.isEmpty else { refuse("AAC_TRAY_STATE_DIR is not set") }
    let real = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".ccs").standardizedFileURL.path
    guard !URL(fileURLWithPath: stateDir).standardizedFileURL.path.hasPrefix(real) else { refuse("the state folder is under ~/.ccs") }
    guard let user = environment["AAC_E2E_USER"], let first = environment["AAC_E2E_PASSWORD"],
      let second = environment["AAC_E2E_NEW_PASSWORD"] else { refuse("AAC_E2E_USER, AAC_E2E_PASSWORD and AAC_E2E_NEW_PASSWORD are needed") }
    let phase = arguments.count > 1 ? arguments[1] : "main"
    let folder = URL(fileURLWithPath: stateDir, isDirectory: true)
    try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
    record("isolated tray state folder (not ~/.ccs/bar)", BarConnection.configURL.deletingLastPathComponent().standardizedFileURL.path
      == folder.standardizedFileURL.path, ["folder": folder.path])
    switch phase {
    case "trust-off": trustOff(base: base, folder: folder, user: user, password: first)
    case "not-trusted": notTrusted(base: base, folder: folder)
    case "setup": setup(base: base, folder: folder, user: user, password: first, code: environment["AAC_E2E_SETUP_CODE"] ?? "")
    case "main": main(base: base, folder: folder, user: user, first: first, second: second)
    case "migrate": migrate(base: base, folder: folder, user: user, password: second)
    case "names": names(base: base, folder: folder, user: user, password: second)
    default: refuse("unknown phase")
    }
    finish()
  }

  private static func tray(_ file: URL) -> AccountsViewModel {
    // Each phase starts from its own empty folder inside the isolated state folder.
    try? FileManager.default.removeItem(at: file.deletingLastPathComponent())
    let session = ConnectionSession(fileURL: file)
    let model = AccountsViewModel(preview: nil, session: session)
    model.signIn.stepInterval = 0.2
    model.signIn.successHold = 0.3
    return model
  }

  /// State 5 over the real network: the owner switch is off on the sandbox.
  private static func trustOff(base: URL, folder: URL, user: String, password: String) {
    let file = folder.appendingPathComponent("trust-off/accounts-connection.json")
    let model = tray(file)
    let signIn = model.signIn
    record("first run shows the address step", signIn.active && signIn.state == .firstRun)
    signIn.address = base.absoluteString
    signIn.submit()
    wait { !signIn.busy }
    let text = screen(signIn)
    record("5 trust off: Pairing is turned off for remote computers, on screen", signIn.state == .pairingOff
      && shows(text, ["Pairing is turned off for remote computers", "Trust this local network"]), ["read": text])
    // The pair route itself refuses too (403 secure_transport_required), and nothing is saved.
    let session = ConnectionSession(fileURL: file)
    let refused = Holder<Bool?>(nil)
    Task { @MainActor in
      if case .refused(.pairingOff) = await session.pair(url: base, username: user, password: password) { refused.value = true }
      else { refused.value = false }
    }
    wait { refused.value != nil }
    record("5 pair is refused by the dashboard and nothing is saved", refused.value == true && !FileManager.default.fileExists(atPath: file.path))
  }

  /// State 4 from the dashboard's side: its trusted networks exclude this Mac.
  private static func notTrusted(base: URL, folder: URL) {
    let model = tray(folder.appendingPathComponent("not-trusted/accounts-connection.json"))
    let signIn = model.signIn
    signIn.address = base.absoluteString
    signIn.submit()
    wait { !signIn.busy }
    let text = screen(signIn)
    record("4 the dashboard sees this Mac outside its trusted networks", signIn.state == .notLocal && signIn.seenAs != nil
      && shows(text, ["This address isn't on your local network", "The dashboard sees this Mac at"]),
      ["seenAs": signIn.seenAs ?? "", "read": text])
    let refusedAddress = "203.0.113.5:3999"
    signIn.address = refusedAddress
    signIn.submit()
    wait { !signIn.busy }
    record("4 a public address is refused by the tray before anything is sent", signIn.state == .notLocal && signIn.seenAs == nil)
  }

  /// State 2 over the real network: a fresh sandbox dashboard with no sign-in yet, its one-time setup code read from
  /// the sandbox server's own file by the operator, then pairing.
  private static func setup(base: URL, folder: URL, user: String, password: String, code: String) {
    let file = folder.appendingPathComponent("setup/accounts-connection.json")
    let model = tray(file)
    let signIn = model.signIn
    signIn.address = base.absoluteString
    signIn.submit()
    wait { !signIn.busy }
    let text = screen(signIn)
    record("2 a dashboard with no sign-in asks for the one-time setup code, on screen", signIn.state == .setupCode
      && signIn.codeRequired && shows(text, ["Set up sign-in", "has no sign-in yet", "Setup code"]), ["read": text])
    signIn.username = user
    signIn.password = password
    signIn.confirm = password
    signIn.setupCode = "AAAA-AAAA"
    signIn.submit()
    wait { !signIn.busy }
    record("2 a wrong setup code is refused by the dashboard", signIn.state == .setupCode && signIn.badField == .code)
    signIn.setupCode = code
    signIn.submit()
    let done = wait(30) { !signIn.active }
    let saved = try? BarConnection.load(from: file)
    record("2 the right setup code creates the sign-in and pairs this tray", done && saved?.isPaired == true)
    wait(30) { model.dashboard != nil && !model.isRefreshing }
    model.disconnect()
    wait(20) { signIn.active }
    record("the setup phase's key is disconnected at the end", (try? BarConnection.load(from: file))?.isSignedOut == true)
  }

  private static func main(base: URL, folder: URL, user: String, first: String, second: String) {
    let file = folder.appendingPathComponent("main/accounts-connection.json")
    let model = tray(file)
    let signIn = model.signIn
    let admin = AdminBrowser(base: base)

    // First pairing.
    record("first run shows the address step", signIn.active && signIn.state == .firstRun && signIn.address.isEmpty)
    signIn.address = "\(base.host ?? ""):\(base.port ?? 0)"
    signIn.submit()
    wait { !signIn.busy }
    record("1 the address answers and the password step shows the dashboard", signIn.state == .password
      && signIn.verified == base, ["state": signIn.state.rawValue])
    signIn.username = user
    signIn.password = "not-the-password"
    signIn.submit()
    wait { !signIn.busy }
    let wrong = screen(signIn)
    record("6 a wrong password shows the tries left", signIn.state == .wrongPassword && signIn.triesLeft != nil
      && shows(wrong, ["Username or password isn't right", "tries left"]), ["triesLeft": signIn.triesLeft ?? -1])
    signIn.password = first
    signIn.submit()
    let sawPairing = wait(10) { signIn.state == .pairing || signIn.state == .success || !signIn.active }
    let handedOff = wait(20) { !signIn.active }
    let saved = try? BarConnection.load(from: file)
    let text = (try? String(contentsOf: file, encoding: .utf8)) ?? ""
    let mode = (try? FileManager.default.attributesOfItem(atPath: file.path)[.posixPermissions] as? NSNumber)?.intValue
    let folderMode = (try? FileManager.default.attributesOfItem(atPath: file.deletingLastPathComponent().path)[.posixPermissions] as? NSNumber)?.intValue
    record("3 and 11 first pairing: steps, success, hand-off; version 2 key at 0600 in 0700, no password",
      sawPairing && handedOff && saved?.isPaired == true && saved?.version == 2 && mode == 0o600 && folderMode == 0o700
      && !text.contains(first) && !text.contains("\"password\""), ["deviceId": saved?.deviceId ?? ""])

    // Bearer reads.
    wait(30) { model.dashboard != nil }
    let me = waitValue { try await model.session.client?.deviceSelf() }
    record("bearer reads: the dashboard and devices/me answer with the device key", model.dashboard != nil
      && me?.id == saved?.deviceId, ["accounts": model.dashboard?.accounts.count ?? -1])

    // Password change on the dashboard while the tray stays signed in.
    let loggedIn = admin.login(user, first)
    let changed = admin.json("POST", "/api/auth/password", ["currentPassword": first, "newPassword": second, "signOutOtherBrowsers": true])
    let before = model.lastSyncedAt
    Task { await model.refresh(force: false) }
    wait(30) { model.lastSyncedAt != before && !model.isRefreshing }
    record("a password change on the dashboard leaves the tray signed in", loggedIn && changed.status == 200
      && !signIn.active && model.connected && (try? BarConnection.load(from: file))?.deviceToken == saved?.deviceToken,
      ["passwordChange": changed.status, "pairedDevices": changed.object["pairedDevices"] ?? NSNull()])

    // Revoke from the dashboard: the signed-out screen.
    _ = admin.login(user, second)
    let listed = admin.json("GET", "/api/auth/devices", nil)
    let ours = (listed.object["devices"] as? [[String: Any]])?.first { $0["id"] as? String == saved?.deviceId }
    let revoked = admin.json("DELETE", "/api/auth/devices/\(saved?.deviceId ?? "none")", nil)
    Task { await model.refresh(force: false) }
    wait(30) { signIn.active }
    let out = screen(signIn)
    let outFile = try? BarConnection.load(from: file)
    record("10 revoke in the dashboard shows the signed-out screen", ours != nil && revoked.status == 204
      && signIn.state == .signedOut && shows(out, ["This tray was signed out", "Revoked from the dashboard", "Pair again"])
      && outFile?.isSignedOut == true && outFile?.username == user && signIn.username == user
      && model.menuBarReading(TrayPreferences(defaults: UserDefaults(suiteName: "aac.e2e") ?? .standard, persist: false)) == nil,
      ["listed": ours != nil, "revoke": revoked.status, "state": signIn.state.rawValue, "read": out])

    // Pair again.
    signIn.password = second
    signIn.submit()
    let again = wait(20) { !signIn.active }
    let repaired = try? BarConnection.load(from: file)
    wait(30) { model.dashboard != nil && !model.isRefreshing }
    record("re-pair after the sign-out gets a new key and reads again", again && repaired?.isPaired == true
      && repaired?.deviceId != saved?.deviceId && model.connected)

    // Settings › Re-pair: the new key gets its own install id, is confirmed and saved, then the old key is revoked
    // with itself, so exactly one key stays active.
    func activeIds() -> [String] {
      (admin.json("GET", "/api/auth/devices", nil).object["devices"] as? [[String: Any]])?.compactMap { $0["id"] as? String } ?? []
    }
    model.beginRepair()
    let repairShown = signIn.active && signIn.repair && signIn.state == .password
    signIn.password = second
    signIn.submit()
    let replacedOk = wait(20) { !signIn.active }
    wait(20) { !model.session.isRetiringKey }
    let third = try? BarConnection.load(from: file)
    let active = activeIds()
    record("Settings Re-pair issues a new key, then revokes the old one; one key stays active", repairShown && replacedOk
      && third?.deviceId != repaired?.deviceId && third?.installId != repaired?.installId && active == [third?.deviceId ?? "-"],
      ["active": active.count])

    // Re-pair, then Cancel while the pair call is on its way: nothing is saved, the key the dashboard issued is
    // revoked, and the working key still reads.
    model.beginRepair()
    signIn.password = second
    signIn.submit()
    wait(2) { model.session.isPairing }
    pump(0.08)
    let inFlight = model.session.isPairing
    signIn.cancel()
    wait(30) { !model.session.isPairing }
    let kept = try? BarConnection.load(from: file)
    let afterCancel = activeIds()
    let stillReads = waitValue { try await model.session.client?.deviceSelf() }
    record("Re-pair Cancel during the pair call saves nothing, revokes the issued key and keeps the working key", inFlight
      && !signIn.active && kept?.deviceToken == third?.deviceToken && model.session.connection?.deviceToken == third?.deviceToken
      && afterCancel == [third?.deviceId ?? "-"] && stillReads?.id == third?.deviceId,
      ["inFlight": inFlight, "active": afterCancel.count])

    // This connection as the dashboard sees it.
    model.readConnectionInfo()
    wait(10) { model.connectionCheck != nil }
    record("Settings This connection reads the dashboard's view of this Mac", model.connectionCheck?.connection?.trusted == true
      && model.connectionCheck?.peer != nil, ["peer": model.connectionCheck?.peer ?? ""])

    // Disconnect.
    model.disconnect()
    wait(20) { signIn.active && signIn.state == .firstRun }
    let disconnected = screen(signIn)
    let after = admin.json("GET", "/api/auth/devices", nil)
    let still = (after.object["devices"] as? [[String: Any]])?.compactMap { $0["id"] as? String } ?? []
    let gone = try? BarConnection.load(from: file)
    record("Disconnect revokes the key on the dashboard and shows the first run with the address", signIn.disconnectedAt != nil
      && signIn.disconnectTold && !still.contains(third?.deviceId ?? "-") && gone?.isSignedOut == true
      && signIn.address == base.absoluteString && shows(disconnected, ["Disconnected at", "Connect this Mac"]),
      ["read": disconnected])
  }

  /// Section 8 against the real dashboard: a fake version 1 file holding the test password.
  private static func migrate(base: URL, folder: URL, user: String, password: String) {
    let file = folder.appendingPathComponent("migrate/accounts-connection.json")
    try? FileManager.default.removeItem(at: file.deletingLastPathComponent())
    do { try ConnectionStore.save(BarConnection(baseURL: base, username: user, password: password), to: file) }
    catch { record("fake version 1 file written", false); return }
    record("fake version 1 password file written (0600)", (try? BarConnection.load(from: file))?.hasPassword == true)
    let session = ConnectionSession(fileURL: file)
    let model = AccountsViewModel(preview: nil, session: session)
    model.signIn.stepInterval = 0.2
    model.signIn.successHold = 0.3
    let signIn = model.signIn
    let securing = signIn.active && signIn.state == .securing
    let done = wait(30) { !signIn.active }
    let folderURL = file.deletingLastPathComponent()
    let names = ((try? FileManager.default.contentsOfDirectory(atPath: folderURL.path)) ?? []).sorted()
    // The password is in no file of the folder any more, and the rollback copy is gone.
    let leaked = names.contains { name in
      ((try? String(contentsOf: folderURL.appendingPathComponent(name), encoding: .utf8)) ?? "").contains(password)
    }
    record("9 a stored password is traded for a key by itself; the password and the rollback copy are gone", securing && done
      && (try? BarConnection.load(from: file))?.isPaired == true && !leaked
      && !names.contains("accounts-connection.v1-rollback.json"), ["files": names])
    wait(30) { model.dashboard != nil && !model.isRefreshing }
    record("bearer reads after the migration", model.dashboard != nil && model.connected)
    // Leave the sandbox clean: this key is disconnected too.
    model.disconnect()
    wait(20) { signIn.active }
    record("the migrated key is disconnected at the end", (try? BarConnection.load(from: file))?.isSignedOut == true)
  }

  /// The packaged app's App Transport Security against a host name: the address step refuses a dotted name with the
  /// numeric-address message before anything is sent, and a saved password connection to that name shows the same
  /// message instead of "could not reach". Run it with the packaged app's own binary (its Info.plist applies) against
  /// a name that resolves to the sandbox, such as `<lan address>.nip.io`.
  private static func names(base: URL, folder: URL, user: String, password: String) {
    guard let host = base.host, !LocalNetwork.plainHTTPReaches(host: host) else {
      record("the names phase needs a dotted host name (not a number or .local)", false)
      return
    }
    let packaged = Bundle.main.bundleURL.pathExtension == "app"
    record("names: run from the packaged app, so App Transport Security applies", packaged,
      ["bundle": Bundle.main.bundleURL.lastPathComponent])
    // Unbundled, nothing blocks the name, so the saved-password half would pair a key: stop here.
    guard packaged else { return }
    let file = folder.appendingPathComponent("names/accounts-connection.json")
    let model = tray(file)
    let signIn = model.signIn
    signIn.address = base.absoluteString
    signIn.submit()
    wait { !signIn.busy }
    let text = screen(signIn)
    record("a dotted host name asks for the numeric address, on screen; nothing saved", signIn.state == .firstRun
      && signIn.badField == .addr && signIn.message?.text == SignInCopy.useNumericAddress
      && shows(text, ["numeric address"]) && !FileManager.default.fileExists(atPath: file.path), ["read": text])
    // A saved password connection to that name (as an older tray might hold): no pairing and no reads get through,
    // and the panel says why.
    let saved = folder.appendingPathComponent("names-v1/accounts-connection.json")
    try? FileManager.default.removeItem(at: saved.deletingLastPathComponent())
    do { try ConnectionStore.save(BarConnection(baseURL: base, username: user, password: password), to: saved) }
    catch { record("names: version 1 file written", false); return }
    let older = AccountsViewModel(preview: nil, session: ConnectionSession(fileURL: saved))
    older.signIn.stepInterval = 0.2
    older.signIn.successHold = 0.3
    wait(40) { !older.signIn.active && !older.isRefreshing && older.message != nil }
    record("a saved connection to that name shows the numeric-address message, not 'could not reach'",
      older.message == SignInCopy.useNumericAddress && older.connection?.hasPassword == true && !older.connected,
      ["message": older.message ?? ""])
  }

  private static func waitValue<T: Sendable>(_ operation: @escaping @Sendable () async throws -> T?) -> T? {
    let result = Holder<T?>(nil), done = Holder(false)
    Task { @MainActor in
      result.value = (try? await operation()) ?? nil
      done.value = true
    }
    wait(20) { done.value }
    return result.value
  }
}

/// A browser-like dashboard session for the end-to-end run's owner actions (password change, device list, revoke).
@MainActor
final class AdminBrowser {
  let base: URL
  private let session: URLSession

  init(base: URL) {
    self.base = base
    let config = URLSessionConfiguration.ephemeral
    config.httpShouldSetCookies = true
    config.httpCookieAcceptPolicy = .always
    config.timeoutIntervalForRequest = 20
    session = URLSession(configuration: config)
  }

  struct Answer { var status: Int; var object: [String: Any] }

  func login(_ user: String, _ password: String) -> Bool {
    json("POST", "/api/auth/login", ["username": user, "password": password]).status == 200
  }

  func json(_ method: String, _ path: String, _ body: [String: Any]?) -> Answer {
    var request = URLRequest(url: base.appendingPathComponent(String(path.dropFirst())))
    request.httpMethod = method
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    if method != "GET" {
      request.setValue(base.absoluteString, forHTTPHeaderField: "Origin")
      request.setValue("application/json", forHTTPHeaderField: "Content-Type")
      request.httpBody = try? JSONSerialization.data(withJSONObject: body ?? [String: Any]())
    }
    let answer = Holder(Answer(status: 0, object: [:])), done = Holder(false)
    let task = session.dataTask(with: request) { data, response, _ in
      let status = (response as? HTTPURLResponse)?.statusCode ?? 0
      let object = ((data.flatMap { try? JSONSerialization.jsonObject(with: $0) }) as? [String: Any]) ?? [:]
      answer.value = Answer(status: status, object: object)
      done.value = true
    }
    task.resume()
    let deadline = Date().addingTimeInterval(25)
    while !done.value && Date() < deadline { RunLoop.main.run(until: Date().addingTimeInterval(0.05)) }
    return answer.value
  }
}

/// A value shared with a callback for the end-to-end run's waits.
final class Holder<T>: @unchecked Sendable {
  private let lock = NSLock()
  private var stored: T
  init(_ value: T) { stored = value }
  var value: T {
    get { lock.lock(); defer { lock.unlock() }; return stored }
    set { lock.lock(); stored = newValue; lock.unlock() }
  }
}
