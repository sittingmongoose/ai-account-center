import SwiftUI
import AppKit
import CCSBarCore

/// `--check-signin`: every sign-in state rendered offscreen and read back with on-device text recognition, then every
/// flow driven through the real `SignInModel`, `AccountsViewModel` and `ConnectionSession` against an in-process fake
/// dashboard. No network, no real connection file (a temporary folder), no screenshot written.
@MainActor
enum SignInCheck {
  private static var steps: [[String: Any]] = []

  private static func record(_ name: String, _ passed: Bool, _ detail: [String: Any] = [:]) {
    var entry = detail
    entry["step"] = name
    entry["passed"] = passed
    steps.append(entry)
  }

  private static func pump(_ seconds: Double) { RunLoop.main.run(until: Date().addingTimeInterval(seconds)) }

  @discardableResult
  private static func wait(_ seconds: Double = 5, until condition: () -> Bool) -> Bool {
    let deadline = Date().addingTimeInterval(seconds)
    while !condition() && Date() < deadline { pump(0.02) }
    return condition()
  }

  /// The text one rendered state shows.
  private static func read(_ model: SignInModel, appearance: String = "light") -> (text: String, size: CGSize) {
    guard let host = try? PreviewRenderer.signInHost(model: model, appearance: appearance) else { return ("", .zero) }
    let lines = (try? PreviewRenderer.recognizedText(host)) ?? []
    let size = host.bounds.size
    host.window?.orderOut(nil)
    return (lines.joined(separator: "\n"), size)
  }

  private static func shows(_ text: String, _ needles: [String]) -> Bool {
    let flat = text.replacingOccurrences(of: "\n", with: " ").lowercased()
    return needles.allSatisfy { flat.contains($0.lowercased()) }
  }

  static func run() -> Never {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent("aac-signin-check-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: directory) }
    TrayFormat.referenceNow = nil
    renders()
    flows(directory)
    let passed = steps.allSatisfy { $0["passed"] as? Bool == true }
    let result: [String: Any] = ["passed": passed, "steps": steps, "networkOrAccountActions": false,
      "screenshotsWritten": false, "realConnectionFileTouched": false]
    print(String(decoding: (try? JSONSerialization.data(withJSONObject: result, options: [.sortedKeys, .prettyPrinted])) ?? Data(), as: UTF8.self))
    exit(passed ? 0 : 1)
  }

  // MARK: Every state, light and dark

  private static func renders() {
    let address = URL(string: "http://192.168.50.10:3000")!
    let expected: [SignInState: [String]] = [
      .firstRun: ["Connect this Mac", "Dashboard address", "Continue", "home network"],
      .password: ["Sign in to pair", "192.168.50.10:3000", "Change", "Username", "Password", "Pair this Mac", "mode 0600"],
      .setupCode: ["Set up sign-in", "has no sign-in yet", "Confirm password", "Setup code", "Create sign-in and pair"],
      .pairing: ["Pairing this Mac", "Password checked", "Device key issued", "Saving the key", "Forgetting the password"],
      .notLocal: ["This address isn't on your local network", "Use the dashboard's local address", "home VPN", "Try again", "Nothing was sent"],
      .pairingOff: ["Pairing is turned off for remote computers", "Trust this local network", "Try again", "dashboard machine"],
      .wrongPassword: ["Sign in to pair", "Username or password isn't right", "4 tries left"],
      .rateLimited: ["Try again in 15 minutes", "Too many tries", "left"],
      .unreachable: ["Can't reach that address", "didn't answer", "No answer after 10 seconds", "Retry"],
      .wrongAddress: ["That isn't a dashboard address", "answered, but not as", "Account Center", "Check the host name and the port", "Retry"],
      .securing: ["Securing this tray", "Rollback copy kept", "Checking that the key works", "Deleting the saved password"],
      .signedOut: ["This tray was signed out", "Revoked from the dashboard by owner", "Pair again"],
      .signedOutAll: ["This tray was signed out", "owner chose Sign out all devices", "Pair again"],
      .expired: ["This tray was signed out", "Not used for 90 days", "Pair again"],
      .success: ["Paired", "Opening your accounts", "The device key is saved"],
    ]
    for appearance in ["light", "dark"] {
      for state in SignInState.allCases {
        let owner = AccountsViewModel(previewWithoutConnection: true)
        let note: SignedOutNote? = [.signedOut, .signedOutAll, .expired].contains(state)
          ? SignedOutNote(reason: state == .expired ? "device_expired" : "device_revoked", at: ISO8601DateFormatter().string(from: Date()),
            revokedReason: state == .signedOutAll ? "revoke-all" : state == .expired ? "expired" : "dashboard", revokedBy: "owner")
          : nil
        let url = state == .notLocal ? URL(string: "http://home.example.net:3000")! : address
        owner.signIn.preview(state, verified: state == .firstRun ? nil : url, username: state == .firstRun ? "" : "owner", note: note)
        let (text, size) = read(owner.signIn, appearance: appearance)
        let needles = expected[state] ?? []
        let ok = shows(text, needles) && abs(size.height - SignInView.bodyHeight) < 0.5 && abs(size.width - 760) < 0.5
        record("\(appearance) \(state.rawValue): renders its copy at 760 x 736", ok,
          ok ? [:] : ["missing": needles.filter { !shows(text, [$0]) }, "size": "\(size)", "read": text])
      }
    }
  }

  // MARK: Flows

  private static func flows(_ directory: URL) {
    let fake = CheckDashboard()
    let file = directory.appendingPathComponent("first/accounts-connection.json")
    func makeSession(_ url: URL) -> ConnectionSession {
      let session = ConnectionSession(fileURL: url, makeTransport: { CheckDashboardTransport(fake) })
      session.resolver = { host in host == "home.example.net" ? ["203.0.113.9"] : [] }
      session.deviceName = "Check Mac"
      return session
    }
    let model = AccountsViewModel(preview: nil, session: makeSession(file))
    let signIn = model.signIn
    signIn.stepInterval = 0.02
    signIn.successHold = 0.05
    record("first run opens the sign-in screen with an empty address", signIn.active && signIn.state == .firstRun
      && signIn.address.isEmpty && model.menuBarReading(TrayPreferences(defaults: UserDefaults(suiteName: "aac.signin.check") ?? .standard, persist: false)) == nil)

    // The refresh timer never resets what the person is typing, and never polls behind the screen.
    signIn.address = "192.168.50.10:30"
    let ticked = Holder(false)
    Task { @MainActor in await model.tick(); ticked.value = true }
    wait { ticked.value }
    record("a refresh tick leaves the sign-in screen and its fields alone", signIn.active && signIn.state == .firstRun
      && signIn.address == "192.168.50.10:30" && fake.count == 0)

    // 4: a public address is refused by the tray itself; nothing is sent.
    let sent = fake.count
    signIn.address = "203.0.113.5:3000"
    signIn.submit()
    wait { !signIn.busy }
    record("4 a public address is refused before anything is sent", signIn.state == .notLocal && fake.count == sent
      && signIn.badField == .addr)
    signIn.submit()
    wait { !signIn.busy }
    record("4 the same refusal again shakes", signIn.state == .notLocal && signIn.shake >= 1)

    // 8: unreachable and wrong address.
    signIn.address = "refused.invalid:3000"
    signIn.submit()
    wait { !signIn.busy && signIn.state != .notLocal }
    record("8 an address that does not answer is unreachable", signIn.state == .unreachable
      && signIn.message?.bold == "No answer after 10 seconds.")
    signIn.address = "elsewhere.invalid:8080"
    signIn.submit()
    wait { !signIn.busy && signIn.state != .unreachable }
    record("8 an address that is not the dashboard is a wrong address", signIn.state == .wrongAddress)

    // 5: pairing turned off.
    fake.set { $0.trust = false }
    signIn.address = "192.168.50.10:3000"
    signIn.submit()
    wait { !signIn.busy && signIn.state != .wrongAddress }
    let off = read(signIn)
    record("5 trust off gives Pairing is turned off, read on screen", signIn.state == .pairingOff
      && shows(off.text, ["Pairing is turned off for remote computers", "192.168.50.10:3000"]))
    fake.set { $0.trust = true }
    signIn.submit()
    wait { !signIn.busy && signIn.state != .pairingOff }
    record("5 Try again after the owner turns it on goes to the password step", signIn.state == .password
      && signIn.verified?.absoluteString == "http://192.168.50.10:3000")

    // 6: wrong password with tries left.
    signIn.username = "owner"
    signIn.password = "not-it"
    signIn.submit()
    wait { !signIn.busy }
    let wrong = read(signIn)
    record("6 a wrong password says the tries left, on screen", signIn.state == .wrongPassword && signIn.badField == .pass
      && shows(wrong.text, ["Username or password isn't right", "4 tries left"]) && signIn.shake >= 2)

    // 3, 11: pairing, the steps, success and the hand-off.
    signIn.password = "check-pass-1"
    signIn.submit()
    let paired = wait(5) { !signIn.active }
    let saved = try? BarConnection.load(from: file)
    record("3 and 11 pairing ticks through its steps and hands off to the list", paired && saved?.isPaired == true
      && saved?.password == nil && model.connection?.isPaired == true && signIn.password.isEmpty)
    wait(3) { model.dashboard != nil }
    record("bearer reads fill the list after the hand-off", model.dashboard != nil && fake.lastAuthorization.hasPrefix("Bearer aacd_")
      && !fake.paths.contains("/api/auth/login"))

    // Re-pair and Cancel.
    model.beginRepair()
    record("Settings Re-pair opens the password step with Cancel", signIn.active && signIn.repair && signIn.state == .password
      && signIn.statusText == "Re-pairing · the current key still works")
    signIn.cancel()
    record("Cancel keeps the current key and returns to the list", !signIn.active
      && (try? BarConnection.load(from: file))?.deviceToken == saved?.deviceToken)

    // 10: signed out remotely, with who and when.
    fake.set { $0.revoke(reason: "dashboard"); $0.revokedFields = ["revokedBy": "owner", "revokedReason": "dashboard"] }
    Task { await model.refresh() }
    wait { signIn.active }
    let out = read(signIn)
    let outFile = try? BarConnection.load(from: file)
    record("10 a revoked key shows the signed-out screen with who", signIn.state == .signedOut
      && shows(out.text, ["This tray was signed out", "Revoked from the dashboard by owner", "Pair again"])
      && outFile?.isSignedOut == true && outFile?.username == "owner" && signIn.username == "owner"
      && model.menuBarReading(TrayPreferences(defaults: UserDefaults(suiteName: "aac.signin.check") ?? .standard, persist: false)) == nil)
    signIn.password = "check-pass-1"
    signIn.submit()
    wait(5) { !signIn.active }
    record("10 Pair again pairs a new key", !signIn.active && (try? BarConnection.load(from: file))?.isPaired == true)
    wait(3) { !model.isRefreshing }

    fake.set { $0.revoke(reason: "expired"); $0.revokedFields = [:] }
    Task { await model.refresh() }
    wait { signIn.active }
    record("10 an unused key shows Not used for 90 days", signIn.state == .expired
      && shows(read(signIn).text, ["Not used for 90 days"]))
    signIn.password = "check-pass-1"
    signIn.submit()
    wait(5) { !signIn.active }
    wait(3) { !model.isRefreshing }
    fake.set { $0.revoke(reason: "revoke-all"); $0.revokedFields = ["revokedBy": "owner", "revokedReason": "revoke-all"] }
    Task { await model.refresh() }
    wait { signIn.active }
    record("10 Sign out all devices names it", signIn.state == .signedOutAll
      && shows(read(signIn).text, ["owner chose Sign out all devices"]))
    signIn.password = "check-pass-1"
    signIn.submit()
    wait(5) { !signIn.active }
    wait(3) { !model.isRefreshing }

    // Disconnect.
    model.disconnect()
    wait { signIn.active }
    let gone = try? BarConnection.load(from: file)
    record("Disconnect revokes on the dashboard and shows the first run with the address", signIn.state == .firstRun
      && signIn.disconnectedAt != nil && signIn.address == "http://192.168.50.10:3000" && gone?.isSignedOut == true
      && fake.paths.last == "/api/auth/devices/me" && shows(read(signIn).text, ["Disconnected at"]))

    // 7: the fifth failure pauses pairing; the countdown returns to the password step.
    signIn.submit()
    wait { !signIn.busy && signIn.state != .firstRun }
    fake.set { $0.failures = 4 }
    signIn.username = "owner"
    signIn.password = "not-it"
    signIn.submit()
    wait { !signIn.busy && signIn.state == .rateLimited }
    let limited = read(signIn)
    record("7 the fifth failure pauses pairing with a countdown", signIn.state == .rateLimited && signIn.limitUntil != nil
      && shows(limited.text, ["Try again in", "Too many tries"]))
    signIn.limitEnded()
    record("7 the countdown ends on the password step", signIn.state == .password)
    fake.set { $0.failures = 0 }

    // 2: a fresh dashboard asks for the setup code, then pairs.
    fake.set { $0.mode = "setup" }
    signIn.changeAddress()
    signIn.address = "192.168.50.10:3000"
    signIn.submit()
    wait { !signIn.busy && signIn.state == .setupCode }
    record("2 a dashboard without a sign-in asks for the setup code", signIn.state == .setupCode && signIn.codeRequired)
    signIn.username = "owner"
    signIn.password = "summit-ledger-42"
    signIn.confirm = "summit-ledger-42"
    signIn.setupCode = "zz"
    signIn.submit()
    record("2 a short code is refused before anything is sent", signIn.badField == .code)
    signIn.setupCode = "k7qf-2mxd"
    signIn.submit()
    let setUp = wait(5) { !signIn.active }
    record("2 setup then pairing hands off", setUp && (try? BarConnection.load(from: file))?.isPaired == true && fake.mode == "login")
    wait(3) { !model.isRefreshing }

    // A connection deployed privately while the screen waits is picked up by the next tick.
    let deployedFile = directory.appendingPathComponent("deployed/accounts-connection.json")
    let waiting = AccountsViewModel(preview: nil, session: makeSession(deployedFile))
    let firstShown = waiting.signIn.active && waiting.signIn.state == .firstRun
    let writer = makeSession(deployedFile)
    let deployed = Holder(false)
    Task { @MainActor in
      if case .paired = await writer.pair(url: URL(string: "http://192.168.50.10:3000")!, username: "owner", password: "check-pass-1") {
        deployed.value = true
      }
    }
    wait { deployed.value }
    let tock = Holder(false)
    Task { @MainActor in await waiting.tick(); tock.value = true }
    wait { tock.value }
    wait(3) { !waiting.isRefreshing && waiting.dashboard != nil }
    record("a connection deployed while the screen waits is picked up by the next tick", firstShown && deployed.value
      && !waiting.signIn.active && waiting.connection?.isPaired == true)

    // 9: a stored version 1 password is traded for a key by itself.
    let migrated = directory.appendingPathComponent("v1/accounts-connection.json")
    try? ConnectionStore.save(BarConnection(baseURL: URL(string: "http://192.168.50.10:3000")!, username: "owner",
      password: "summit-ledger-42"), to: migrated)
    let older = AccountsViewModel(preview: nil, session: makeSession(migrated))
    older.signIn.stepInterval = 0.02
    older.signIn.successHold = 0.05
    let securingShown = older.signIn.active && older.signIn.state == .securing
    let secured = wait(5) { !older.signIn.active }
    let migratedFile = try? String(contentsOf: migrated, encoding: .utf8)
    record("9 a stored password is secured by itself and deleted", securingShown && secured
      && (try? BarConnection.load(from: migrated))?.isPaired == true && migratedFile?.contains("summit-ledger-42") == false
      && !FileManager.default.fileExists(atPath: BarConnection.rollbackURL(for: migrated).path))
  }
}

/// The fake dashboard for `--check-signin` (the same contract as CCSBarCheck's, smaller).
final class CheckDashboard: @unchecked Sendable {
  private let lock = NSLock()
  var trust = true
  var mode = "login"
  var failures = 0
  var revokedFields: [String: String] = [:]
  private(set) var tokens: [String: String?] = [:]
  private(set) var log: [String] = []
  private(set) var lastAuthorization = ""
  private var counter = 0

  func set(_ change: (CheckDashboard) -> Void) { lock.lock(); change(self); lock.unlock() }
  var count: Int { lock.lock(); defer { lock.unlock() }; return log.count }
  var paths: [String] { lock.lock(); defer { lock.unlock() }; return log }

  func revoke(reason: String) { for key in tokens.keys where tokens[key] == .some(nil) { tokens[key] = .some(reason) } }

  fileprivate func handle(_ request: URLRequest) -> (Int, [String: Any])? {
    lock.lock(); defer { lock.unlock() }
    let url = request.url!
    log.append(url.path)
    lastAuthorization = request.value(forHTTPHeaderField: "Authorization") ?? ""
    let body = (try? JSONSerialization.jsonObject(with: request.httpBody ?? Data())) as? [String: Any] ?? [:]
    let method = request.httpMethod ?? "GET"
    switch (method, url.path) {
    case ("GET", "/api/auth/check"), ("GET", "/api/auth/setup"):
      return (200, ["accessMode": mode, "configured": mode != "setup", "setupCodeRequired": mode == "setup",
        "secureTransport": false, "trustedLocalNetwork": trust, "connection": ["peer": "192.168.50.23", "trusted": trust]])
    case ("POST", "/api/auth/setup"):
      guard mode == "setup" else { return (409, ["code": "already_configured"]) }
      guard (body["setupCode"] as? String)?.uppercased() == "K7QF-2MXD" else { return (403, ["code": "setup_code_invalid", "triesLeft": 4]) }
      mode = "login"
      return (201, ["ok": true])
    case ("POST", "/api/auth/devices/pair"):
      guard trust else { return (403, ["code": "secure_transport_required"]) }
      if failures >= 5 { return (429, ["code": "rate_limited", "retryAfterSeconds": 900]) }
      guard body["password"] as? String == "check-pass-1" || body["password"] as? String == "summit-ledger-42" else {
        failures += 1
        return failures >= 5 ? (429, ["code": "rate_limited", "retryAfterSeconds": 900]) : (401, ["code": "invalid_credentials", "triesLeft": 5 - failures])
      }
      for key in tokens.keys where tokens[key] == .some(nil) { tokens[key] = .some("replaced") }
      counter += 1
      let token = "aacd_" + String(format: "%043d", counter).replacingOccurrences(of: "0", with: "Q")
      tokens[token] = .some(nil)
      return (201, ["deviceId": String(format: "dev_%016x", counter), "token": token, "pairedAt": "2026-10-02T15:00:00.000Z",
        "rotateAfter": "2099-01-01T00:00:00.000Z"])
    default: break
    }
    guard let auth = request.value(forHTTPHeaderField: "Authorization"), auth.hasPrefix("Bearer ") else {
      return (401, ["code": "auth_required"])
    }
    let token = String(auth.dropFirst(7))
    guard let state = tokens[token] else { return (401, ["code": "invalid_token"]) }
    if let reason = state {
      var object: [String: Any] = ["code": reason == "expired" ? "device_expired" : "device_revoked"]
      for (key, value) in revokedFields { object[key] = value }
      return (401, object)
    }
    switch (method, url.path) {
    case ("GET", "/api/auth/devices/me"):
      return (200, ["id": "dev_0000000000000001", "rotateAfter": "2099-01-01T00:00:00.000Z"])
    case ("DELETE", "/api/auth/devices/me"):
      tokens[token] = .some("self")
      return (204, [:])
    case ("GET", "/api/accounts/dashboard"):
      return nil
    default:
      return (200, [:])
    }
  }
}

actor CheckDashboardTransport: BarHTTPTransport {
  let fake: CheckDashboard
  init(_ fake: CheckDashboard) { self.fake = fake }
  func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
    let url = request.url!
    if url.host == "refused.invalid" { throw URLError(.cannotConnectToHost) }
    if url.host == "elsewhere.invalid" {
      return (Data("<html></html>".utf8), HTTPURLResponse(url: url, statusCode: 404, httpVersion: "HTTP/1.1", headerFields: nil)!)
    }
    guard let (status, object) = fake.handle(request) else {
      let dashboard = """
      {"schemaVersion":1,"updatedAt":"2026-10-02T15:00:00.000Z","accounts":[],"codexAutoSwitch":{"enabled":false,
      "thresholdPercent":10,"pollIntervalSeconds":60,"outcome":"idle","message":"Idle","activationInProgress":false},
      "settings":{"refreshIntervalSeconds":60}}
      """
      return (Data(dashboard.utf8), HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: nil)!)
    }
    let data = status == 204 ? Data() : ((try? JSONSerialization.data(withJSONObject: object)) ?? Data())
    return (data, HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: nil)!)
  }
}
