import Foundation
import CCSBarCore

// Credential-free, network-free contract checks. Buildable with Command Line Tools.
private struct CheckFailure: Error, CustomStringConvertible {
  let description: String
}

private func expect(_ condition: @autoclosure () -> Bool, _ description: String) throws {
  guard condition() else { throw CheckFailure(description: description) }
}

private enum ExpectedError {
  case authentication, rateLimited, invalidConnection, privateConfigRequired, decoding
  case status(Int)

  func matches(_ error: Error) -> Bool {
    guard let clientError = error as? BarClientError else { return false }
    switch (self, clientError) {
    case (.authentication, .authentication), (.rateLimited, .rateLimited),
      (.invalidConnection, .invalidConnection), (.privateConfigRequired, .privateConfigRequired),
      (.decoding, .decoding): return true
    case (.status(let expected), .status(let actual, _)): return expected == actual
    default: return false
    }
  }
}

private func expectError(
  _ expected: ExpectedError, _ description: String,
  operation: () async throws -> Void
) async throws {
  do {
    try await operation()
  } catch {
    try expect(expected.matches(error), "\(description): unexpected error type")
    return
  }
  throw CheckFailure(description: "\(description): operation unexpectedly succeeded")
}

private let dashboardJSON = Data("""
{
  "schemaVersion": 1,
  "updatedAt": "2026-10-01T02:13:45.466Z",
  "settings": {"refreshIntervalSeconds": 120},
  "accounts": [
    {
      "id": "qwen-token-plan", "provider": "qwen", "providerLabel": "Qwen",
      "label": "Token plan", "email": null, "plan": "Token plan", "platform": "mac",
      "source": "Fixture", "status": "unavailable", "message": null,
      "fetchedAt": null, "sampledAt": null, "isActive": false,
      "windows": [
        {"key":"unknown","label":"Weekly","usedPercent":null,"remainingPercent":null,"resetAt":null,"windowMinutes":null,"used":null,"limit":null,"unit":null},
        {"key":"zero","label":"Five-hour","usedPercent":0,"remainingPercent":100,"resetAt":"2026-10-01T03:14:15.250Z","windowMinutes":300,"used":0,"limit":100,"unit":"requests"},
        {"key":"full","label":"Full","usedPercent":100,"remainingPercent":0,"resetAt":"2026-10-01T03:14:15Z","windowMinutes":300,"used":100,"limit":100,"unit":"requests"},
        {"key":"negative","label":"Bad negative","usedPercent":-1,"remainingPercent":101,"resetAt":null,"windowMinutes":null,"used":null,"limit":null,"unit":null},
        {"key":"over","label":"Over quota","usedPercent":101,"remainingPercent":-1,"resetAt":null,"windowMinutes":null,"used":null,"limit":null,"unit":null},
        {"key":"balance","label":"Credits","usedPercent":null,"remainingPercent":null,"resetAt":"2026-10-02T03:14:15Z","windowMinutes":null,"used":9.75,"limit":50,"unit":"credits","kind":"balance","remaining":40.25,"expiresAt":"2026-12-31T23:59:59.250Z","unlimited":false,"enabled":true},
        {"key":"extra","label":"Extra usage","usedPercent":null,"remainingPercent":null,"resetAt":null,"windowMinutes":null,"used":7.5,"limit":null,"unit":"USD","kind":"extra_usage","remaining":0,"expiresAt":"2026-10-03T01:02:03Z","unlimited":true,"enabled":false},
        {"key":"spend","label":"Monthly spend","usedPercent":12.5,"remainingPercent":87.5,"resetAt":"2026-11-01T00:00:00Z","windowMinutes":null,"used":12.5,"limit":100,"unit":"USD","kind":"spend","remaining":null,"expiresAt":null,"unlimited":false,"enabled":true},
        {"key":"over120","label":"Over quota usage","usedPercent":120,"remainingPercent":0,"resetAt":null,"windowMinutes":null,"used":120,"limit":100,"unit":"requests"}
      ],
      "capabilities": {"codexProfile":"gmail","claudeProfileId":"gmail","claudePlatforms":["mac"]}
    }
  ],
  "codexAutoSwitch": {
    "enabled":true,"thresholdPercent":5,"pollIntervalSeconds":60,"outcome":"healthy",
    "message":"Fixture account healthy","activationInProgress":false,
    "lastCheckedAt":"2026-10-01T02:13:45.466Z","lastSwitchedAt":null
  }
}
""".utf8)

private struct MockReply: Sendable {
  let status: Int
  let data: Data
  let delayNanoseconds: UInt64

  init(_ status: Int, _ data: Data = Data("{}".utf8), delayNanoseconds: UInt64 = 0) {
    self.status = status
    self.data = data
    self.delayNanoseconds = delayNanoseconds
  }
}

private struct RecordedRequest: Sendable {
  let url: URL
  let method: String
  let headers: [String: String]
  let body: Data?

  init(_ request: URLRequest) {
    url = request.url!
    method = request.httpMethod ?? "GET"
    headers = Dictionary(uniqueKeysWithValues: (request.allHTTPHeaderFields ?? [:]).map {
      ($0.key.lowercased(), $0.value)
    })
    body = request.httpBody
  }

  func jsonBody() throws -> [String: Any] {
    guard let body,
      let object = try JSONSerialization.jsonObject(with: body) as? [String: Any]
    else { throw CheckFailure(description: "Request body must be a JSON object") }
    return object
  }
}

private actor MockTransport: BarHTTPTransport {
  private var calls: [RecordedRequest] = []
  private var replies: [String: [MockReply]]
  private let loginDelay: UInt64

  init(replies: [String: [MockReply]] = [:], loginDelay: UInt64 = 0) {
    self.replies = replies
    self.loginDelay = loginDelay
  }

  func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
    let record = RecordedRequest(request)
    calls.append(record)
    let key = "\(record.method) \(record.url.path)"
    let reply: MockReply
    if var pending = replies[key], !pending.isEmpty {
      reply = pending.removeFirst()
      replies[key] = pending
    } else if record.url.path == "/api/accounts/dashboard" {
      reply = MockReply(200, dashboardJSON)
    } else if record.url.path == "/api/auth/login" {
      reply = MockReply(200, delayNanoseconds: loginDelay)
    } else {
      reply = MockReply(200)
    }
    if reply.delayNanoseconds > 0 { try await Task.sleep(nanoseconds: reply.delayNanoseconds) }
    let response = HTTPURLResponse(url: record.url, statusCode: reply.status,
      httpVersion: "HTTP/1.1", headerFields: nil)!
    return (reply.data, response)
  }

  func recorded() -> [RecordedRequest] { calls }
}

private func testConnection() -> BarConnection {
  BarConnection(baseURL: URL(string: "http://ccs.invalid:3000")!,
    username: "offline-test", password: "not-a-real-credential")
}

private func checkSessionAndQueries() async throws {
  // Delayed mock login keeps both callers in flight and exercises actor reentrancy.
  let transport = MockTransport(loginDelay: 50_000_000)
  let client = AccountsClient(connection: testConnection(), transport: transport)
  let settings = try JSONDecoder().decode(AccountDashboard.self, from: dashboardJSON).settings
  try expect(settings?.validatedInterval == 120, "Saved usage refresh interval was not decoded")
  let invalid = try JSONDecoder().decode(AccountRefreshSettings.self, from: Data("{\"refreshIntervalSeconds\":1}".utf8))
  try expect(invalid.validatedInterval == 60, "Invalid refresh interval must retain default")
  async let cached = client.dashboard()
  async let refreshed = client.dashboard(refresh: true)
  let (cachedDashboard, freshDashboard) = try await (cached, refreshed)
  try expect(cachedDashboard.schemaVersion == 1 && freshDashboard.schemaVersion == 1,
    "Concurrent dashboard requests must decode the contract")
  _ = try await client.dashboard()
  let calls = await transport.recorded()
  let loginCalls = calls.filter { $0.url.path == "/api/auth/login" }
  try expect(loginCalls.count == 1, "Concurrent requests and later polling must share one login")
  let login = loginCalls[0]
  try expect(login.method == "POST", "Login must use POST")
  try expect(login.headers["origin"] == "http://ccs.invalid:3000", "Login must send same-origin header")
  try expect(login.headers["content-type"] == "application/json", "Login must send JSON content type")
  let loginBody = try login.jsonBody()
  try expect(Set(loginBody.keys) == Set(["username", "password"]), "Login must send only credential fields")
  try expect(loginBody["username"] as? String == "offline-test", "Login username must come from private connection")
  try expect(loginBody["password"] as? String == "not-a-real-credential", "Login password must come from private connection")
  let reads = calls.filter { $0.url.path == "/api/accounts/dashboard" }
  try expect(reads.count == 3, "Every dashboard request must use account endpoint")
  var refreshFlags: [String] = []
  for read in reads {
    try expect(read.method == "GET" && read.body == nil, "Dashboard must use body-free GET")
    try expect(read.headers["accept"] == "application/json", "Dashboard must accept JSON")
    try expect(read.url.host == "ccs.invalid", "Requests must remain on configured CCS host")
    let items = URLComponents(url: read.url, resolvingAgainstBaseURL: false)!.queryItems ?? []
    try expect(items.count == 2, "Dashboard query must have exactly platform and refresh")
    try expect(items.filter { $0.name == "platform" }.map(\.value) == ["mac"], "Mac tray must request Mac usage")
    let values = items.filter { $0.name == "refresh" }.compactMap(\.value)
    try expect(values.count == 1, "Each dashboard request must explicitly select refresh")
    refreshFlags.append(values[0])
  }
  try expect(refreshFlags.sorted() == ["false", "false", "true"], "Only requested refresh may ask for live data")
}

private func checkWrites() async throws {
  let transport = MockTransport()
  let client = AccountsClient(connection: testConnection(), transport: transport)
  try await client.activateCodex(profile: "gmail")
  _ = try await client.setAutomaticSwitching(enabled: true)
  _ = try await client.setAutomaticSwitching(enabled: false)
  // Profile ids come from the dashboard's data; the tray keeps no list of its own.
  for profile in ["alpha", "gmail", "work-2", "me"] { try await client.openClaude(profile: profile) }
  try await client.openClaude(profile: "gmail", platform: "windows")
  _ = try await client.setAutomaticSwitching(enabled: true, thresholdPercent: 15)
  let allCalls = await transport.recorded()
  let calls = allCalls.filter { $0.url.path != "/api/auth/login" }
  try expect(calls.count == 9, "Controls must make one request each")
  for call in calls {
    try expect(call.url.host == "ccs.invalid", "Controls must never contact a provider directly")
    try expect(call.headers["origin"] == "http://ccs.invalid:3000", "All writes require exact configured Origin")
    try expect(call.headers["content-type"] == "application/json", "All writes require JSON content type")
    try expect(call.headers["accept"] == "application/json", "All writes require JSON response type")
    try expect(call.url.query == nil, "Controls must not pass paths or credentials in query strings")
  }
  try expect(calls[0].url.path == "/api/codex/profiles/gmail/activate" && calls[0].method == "POST",
    "Codex activation must reuse guarded POST endpoint")
  let activationBody = try calls[0].jsonBody()
  try expect(activationBody.isEmpty, "Codex activation body must not select host or path")
  for (index, enabled) in [(1, true), (2, false)] {
    let call = calls[index]
    try expect(call.url.path == "/api/codex/profiles/auto-switch" && call.method == "PUT",
      "Automatic switching must use guarded PUT endpoint")
    let body = try call.jsonBody()
    try expect(Set(body.keys) == Set(["enabled"]), "Automatic settings body must contain only enabled")
    try expect(body["enabled"] as? Bool == enabled, "Enabled setting must remain a JSON boolean")
    // NSNumber distinguishes JSON booleans from a mistakenly encoded string.
    try expect(body["enabled"] is NSNumber, "Enabled setting must not be encoded as text")
  }
  for (offset, profile) in ["alpha", "gmail", "work-2", "me"].enumerated() {
    let call = calls[offset + 3]
    try expect(call.url.path == "/api/claude/desktop-profiles/\(profile)/open" && call.method == "POST",
      "Claude launcher must select an allowlisted profile through server endpoint")
    let body = try call.jsonBody()
    try expect(Set(body.keys) == Set(["platform"]) && body["platform"] as? String == "mac",
      "Mac launcher body must only select Mac platform")
  }
  let windowsBody = try calls[7].jsonBody()
  try expect(calls[7].url.path == "/api/claude/desktop-profiles/gmail/open" && calls[7].method == "POST",
    "Windows glyph must use the guarded server launcher rather than a local Mac URI")
  try expect(Set(windowsBody.keys) == Set(["platform"]) && windowsBody["platform"] as? String == "windows",
    "Windows launch must select only the allowlisted Windows platform")
  let thresholdBody = try calls[8].jsonBody()
  try expect(calls[8].method == "PUT" && calls[8].url.path == "/api/codex/profiles/auto-switch",
    "Threshold picker must reuse the guarded automatic switching endpoint")
  try expect(Set(thresholdBody.keys) == Set(["enabled", "thresholdPercent"]),
    "Threshold update must select only automatic switching settings")
  try expect(thresholdBody["enabled"] as? Bool == true && thresholdBody["thresholdPercent"] as? Int == 15,
    "A used threshold of 85 percent must be encoded as 15 percent remaining with enabled preserved")
}

private func checkRejectedProfileIdentifiers() async throws {
  let transport = MockTransport()
  let client = AccountsClient(connection: testConnection(), transport: transport)
  let invalidCodex = ["", ".", "..", "../gmail", "gmail/activate", "gmail?x=y", "gmail#fragment",
    "gmail%2Factivate", " gmail", "gmail ", "gmail\n", "gmail\r\n", "-gmail", "_gmail", "gmaíl",
    String(repeating: "a", count: 65)]
  for profile in invalidCodex {
    try await expectError(.invalidConnection, "Unsafe Codex profile must be rejected locally") {
      try await client.activateCodex(profile: profile)
    }
  }
  for profile in ["", ".", "../gmail", "gmail/open", "gmail\n", "gmail?platform=windows", "gmail#x", "-gmail",
    "gmail%2Fopen", " gmail", String(repeating: "a", count: 65)] {
    try await expectError(.invalidConnection, "Claude profile id must be a plain identifier before it enters the path") {
      try await client.openClaude(profile: profile)
    }
  }
  for platform in ["ubuntu", "Windows", "../windows", "windows\n", ""] {
    try await expectError(.invalidConnection, "Claude launch must reject arbitrary platform input") {
      try await client.openClaude(profile: "gmail", platform: platform)
    }
  }
  for threshold in [-1, 0, 100, 101] {
    try await expectError(.invalidConnection, "Remaining threshold must stay in range 1 through 99") {
      _ = try await client.setAutomaticSwitching(enabled: true, thresholdPercent: threshold)
    }
  }
  let calls = await transport.recorded()
  try expect(calls.isEmpty, "Invalid profile input must fail before login or any network request")
}

private func checkExpiredSession() async throws {
  let transport = MockTransport(replies: ["GET /api/accounts/dashboard": [MockReply(401), MockReply(200, dashboardJSON)]])
  let client = AccountsClient(connection: testConnection(), transport: transport)
  _ = try await client.dashboard(refresh: true)
  let calls = await transport.recorded()
  try expect(calls.filter { $0.url.path == "/api/auth/login" }.count == 2, "An expired session must reauthenticate once")
  let reads = calls.filter { $0.url.path == "/api/accounts/dashboard" }
  try expect(reads.count == 2 && reads[0].url == reads[1].url, "Session retry must preserve original GET query")
  let deniedTransport = MockTransport(replies: ["GET /api/accounts/dashboard": [MockReply(401), MockReply(401)]])
  let deniedClient = AccountsClient(connection: testConnection(), transport: deniedTransport)
  try await expectError(.status(401), "Repeated authorization failure must not loop") {
    _ = try await deniedClient.dashboard()
  }
  let deniedCalls = await deniedTransport.recorded()
  try expect(deniedCalls.count == 4, "A request may make only one session-expiry retry")
}

private func confirmationObject(
  token: String = "offline-confirmation-token",
  targetProfile: String = "party",
  expiresAt: String? = nil,
  processes: [[String: Any]]? = nil
) -> [String: Any] {
  let expiration = expiresAt ?? ISO8601DateFormatter().string(from: Date().addingTimeInterval(300))
  return [
    "token": token,
    "expiresAt": expiration,
    "targetProfile": targetProfile,
    "processes": processes ?? [
      ["label": "Codex desktop", "pid": 123, "role": "main"],
      ["label": "Codex server", "pid": 246, "role": "daemon"],
    ],
    "warning": "Running Codex processes will stop. Unfinished requests may be interrupted.",
  ]
}

private func confirmationResponse(_ confirmation: [String: Any]) throws -> Data {
  try JSONSerialization.data(withJSONObject: [
    "error": "Codex processes are running.",
    "code": "busy",
    "reason": "running_processes",
    "confirmation": confirmation,
  ])
}

private func expectCodexConfirmation(
  _ client: AccountsClient, profile: String
) async throws -> CodexSwitchConfirmation {
  do {
    try await client.activateCodex(profile: profile)
  } catch BarClientError.codexConfirmation(let confirmation) {
    return confirmation
  } catch {
    throw CheckFailure(description: "A valid busy response must expose typed Codex confirmation")
  }
  throw CheckFailure(description: "A busy activation must require confirmation rather than succeed")
}

private func checkConfirmedCodexSwitch() async throws {
  let fixture = confirmationObject()
  let transport = MockTransport(replies: [
    "POST /api/codex/profiles/party/activate": [MockReply(409, try confirmationResponse(fixture)), MockReply(200)],
  ])
  let client = AccountsClient(connection: testConnection(), transport: transport)
  let confirmation = try await expectCodexConfirmation(client, profile: "party")
  try expect(confirmation.token == "offline-confirmation-token" && confirmation.targetProfile == "party",
    "Confirmation token must retain its exact target profile")
  try expect(confirmation.warning == fixture["warning"] as? String,
    "Confirmation must preserve the warning for the consent dialog")
  try expect(confirmation.processes.count == 2 && confirmation.processes[0].pid == 123 &&
    confirmation.processes[0].label == "Codex desktop" && confirmation.processes[0].role == "main",
    "Confirmation must retain the actual processes affected by the switch")
  try expect(confirmation.isValid(for: "party") && !confirmation.isValid(for: "gmail"),
    "Confirmation consent must be bound to the requested profile")
  let expiration = AccountFormatting.date(confirmation.expiresAt)
  try expect(expiration != nil, "Confirmation expiration must retain a parseable exact server timestamp")
  if let expiration {
    try expect(confirmation.isValid(for: "party", now: expiration.addingTimeInterval(-1)),
      "A confirmation must remain valid before its exact expiration")
    try expect(!confirmation.isValid(for: "party", now: expiration) &&
      !confirmation.isValid(for: "party", now: expiration.addingTimeInterval(1)),
      "Confirmation must be revalidated at consent and rejected at or after expiration")
  }
  let beforeConsent = await transport.recorded()
  let initialActivations = beforeConsent.filter { $0.url.path == "/api/codex/profiles/party/activate" }
  try expect(initialActivations.count == 1,
    "A 409 confirmation or dialog cancellation must not automatically retry activation")
  let initialBody = try initialActivations[0].jsonBody()
  try expect(initialBody.isEmpty, "First activation must not pre-authorize process interruption")
  // This second invocation represents the user explicitly confirming the dialog.
  try await client.activateCodex(profile: "party", confirmationToken: confirmation.token)
  let afterConsent = await transport.recorded()
  let activations = afterConsent.filter { $0.url.path == "/api/codex/profiles/party/activate" }
  try expect(activations.count == 2, "Explicit consent must submit one guarded activation request")
  let body = try activations[1].jsonBody()
  try expect(Set(body.keys) == Set(["confirmationToken"]) && body["confirmationToken"] as? String == confirmation.token,
    "Confirmed activation must send only the exact server-issued confirmation token")
  try expect(activations[1].headers["origin"] == "http://ccs.invalid:3000" &&
    activations[1].headers["content-type"] == "application/json",
    "Confirmed activation must retain same-origin JSON protections")
  try expect(afterConsent.filter { $0.url.path == "/api/auth/login" }.count == 1,
    "Confirmation resubmission must reuse the authenticated session")

  let malformed: [[String: Any]] = [
    confirmationObject(token: ""),
    confirmationObject(token: " \n\t"),
    confirmationObject(token: String(repeating: "a", count: 513)),
    confirmationObject(targetProfile: "gmail"),
    confirmationObject(expiresAt: "not-a-date"),
    confirmationObject(expiresAt: "2020-01-01T00:00:00Z"),
    confirmationObject(processes: []),
    confirmationObject(processes: [["label": "Codex", "pid": 0, "role": "main"]]),
    confirmationObject(processes: [["label": "Codex", "pid": -2, "role": "main"]]),
    confirmationObject(processes: [["label": " \n", "pid": 123, "role": "main"]]),
    confirmationObject(processes: [["label": "Codex", "pid": 123, "role": " \t"]]),
    confirmationObject(processes: [
      ["label": "Codex", "pid": 123, "role": "main"],
      ["label": "Broken process", "pid": 0, "role": "daemon"],
    ]),
  ]
  for fixture in malformed {
    let transport = MockTransport(replies: [
      "POST /api/codex/profiles/party/activate": [MockReply(409, try confirmationResponse(fixture))],
    ])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    try await expectError(.status(409), "Invalid, mismatched, or expired confirmation must not authorize a dialog") {
      try await client.activateCodex(profile: "party")
    }
    let calls = await transport.recorded()
    try expect(calls.filter { $0.url.path == "/api/codex/profiles/party/activate" }.count == 1,
      "Rejected confirmation must never trigger an automatic activation retry")
  }
  for payload in [
    ["error": "Another activation is running.", "code": "busy", "reason": "activation_running"],
    ["error": "Confirmation expired or processes changed.", "code": "confirmation_stale"],
  ] {
    let transport = MockTransport(replies: [
      "POST /api/codex/profiles/party/activate": [MockReply(409, try JSONSerialization.data(withJSONObject: payload))],
    ])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    try await expectError(.status(409), "Busy or stale response without a confirmation must remain a generic conflict") {
      try await client.activateCodex(profile: "party", confirmationToken: "offline-confirmation-token")
    }
    let calls = await transport.recorded()
    try expect(calls.filter { $0.url.path == "/api/codex/profiles/party/activate" }.count == 1,
      "A stale confirmation must require a fresh user action rather than resubmit itself")
  }
  let noNetwork = MockTransport()
  let tokenClient = AccountsClient(connection: testConnection(), transport: noNetwork)
  for token in ["", " \n\t", String(repeating: "a", count: 513)] {
    try await expectError(.invalidConnection, "Invalid explicit confirmation token must fail before login") {
      try await tokenClient.activateCodex(profile: "party", confirmationToken: token)
    }
  }
  let invalidCalls = await noNetwork.recorded()
  try expect(invalidCalls.isEmpty, "Invalid explicit confirmation tokens must not make network requests")

  // A one-use consent token must not be replayed even if authentication expires.
  let expiredSession = MockTransport(replies: [
    "POST /api/codex/profiles/party/activate": [
      MockReply(401),
      MockReply(409, try confirmationResponse(confirmationObject(token: "fresh-offline-confirmation-token"))),
    ],
  ])
  let expiredClient = AccountsClient(connection: testConnection(), transport: expiredSession)
  try await expectError(.status(401), "Confirmed request with expired authentication must not replay its consent token") {
    try await expiredClient.activateCodex(profile: "party", confirmationToken: "offline-confirmation-token")
  }
  let afterUnauthorized = await expiredSession.recorded()
  try expect(afterUnauthorized.filter { $0.url.path == "/api/auth/login" }.count == 1 &&
    afterUnauthorized.filter { $0.url.path == "/api/codex/profiles/party/activate" }.count == 1,
    "A confirmed 401 must make only the initial login and one activation request")
  // A later user action starts again without consent and receives a fresh offer.
  let freshConfirmation = try await expectCodexConfirmation(expiredClient, profile: "party")
  try expect(freshConfirmation.token == "fresh-offline-confirmation-token",
    "A fresh activation after authorization failure must require fresh confirmation")
  let afterFreshAction = await expiredSession.recorded()
  let freshActivations = afterFreshAction.filter { $0.url.path == "/api/codex/profiles/party/activate" }
  try expect(afterFreshAction.filter { $0.url.path == "/api/auth/login" }.count == 2 && freshActivations.count == 2,
    "Only a fresh user action may reauthenticate and make a new unconfirmed activation")
  let freshBody = try freshActivations[1].jsonBody()
  try expect(freshBody.isEmpty, "A fresh action must not retain or replay the expired-session confirmation token")
}

private func checkSafeHTTPFailures() async throws {
  enum Context: CaseIterable {
    case refresh, claudeOpen, codexActivate

    var requestKey: String {
      switch self {
      case .refresh: return "GET /api/accounts/dashboard"
      case .claudeOpen: return "POST /api/claude/desktop-profiles/gmail/open"
      case .codexActivate: return "POST /api/codex/profiles/party/activate"
      }
    }
  }
  let canary = "FIXTURE_ONLY_PRIVATE_ERROR_CANARY fixture-token /fixture/private/profile"
  let canaryJSON = try JSONSerialization.data(withJSONObject: [
    "error": canary, "message": canary, "reason": canary,
    "code": "unknown-fixture-code", "context": "codex-activate",
  ])

  func failure(_ context: Context, _ status: Int, _ data: Data) async throws -> String {
    // Authentication retry is intentionally bounded to one extra request. A Claude Open is never repeated, so an
    // expired session on it is reported instead of re-sent (CONTRACT-serving-misc 4.4).
    let replies = Array(repeating: MockReply(status, data), count: status == 401 && context != .claudeOpen ? 2 : 1)
    let transport = MockTransport(replies: [context.requestKey: replies])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    do {
      switch context {
      case .refresh: _ = try await client.dashboard(refresh: true)
      case .claudeOpen: try await client.openClaude(profile: "gmail")
      case .codexActivate: try await client.activateCodex(profile: "party")
      }
    } catch BarClientError.status(let actual, let message) {
      try expect(actual == status && message?.isEmpty == false,
        "HTTP failure must retain its status and useful public guidance")
      let visible = BarClientError.status(actual, message).localizedDescription
      try expect(!visible.contains("FIXTURE_ONLY_PRIVATE_ERROR_CANARY") &&
        !visible.contains("fixture-token") && !visible.contains("/fixture/private/profile"),
        "Raw private server error text must never reach the UI")
      let calls = await transport.recorded()
      try expect(calls.allSatisfy { $0.url.host == "ccs.invalid" } &&
        calls.filter { "\($0.method) \($0.url.path)" == context.requestKey }.count == replies.count,
        "Sanitization must preserve the bounded request/authentication flow")
      return visible
    } catch {
      throw CheckFailure(description: "HTTP failure changed its public error type")
    }
    throw CheckFailure(description: "An HTTP error unexpectedly succeeded")
  }

  // Unknown bodies, including a spoofed action context, must produce the same
  // guidance as an empty body for every supported status and actual local action.
  let statuses = [400, 401, 403, 404, 408, 409, 415, 422, 429, 500, 502, 503, 504]
  for context in Context.allCases {
    for status in statuses {
      let baseline = try await failure(context, status, Data("{}".utf8))
      let polluted = try await failure(context, status, canaryJSON)
      try expect(baseline == polluted, "Unknown server strings must not change public guidance")
    }
  }

  let publicCodes: [(Int, String, String?, String)] = [
    (409, "busy", "activation_running", "Another Codex account activation is already running. Wait for it to finish."),
    (409, "busy", "unsupported_process", "A running Codex program cannot be restarted safely. Close it and try again."),
    (409, "busy", "running_processes", "Codex is busy. Try switching after its work finishes."),
    (409, "confirmation_stale", nil, "The running Codex programs or account changed. Activate again to review a new warning."),
    (400, "invalid_profile", nil, "The selected profile has no valid saved login."),
    (400, "invalid_codex_home", nil, "Account activation needs the shared Codex configuration."),
    (500, "restart_failed", nil, "Codex could not restart. Check its processes before retrying activation."),
    (500, "verification_failed", nil, "The activated account could not be verified. Refresh accounts before retrying."),
    (500, "auth_read_failed", nil, "The saved Codex login could not be read safely."),
    (500, "auth_write_failed", nil, "The Codex login could not be installed safely."),
  ]
  for (status, code, reason, expected) in publicCodes {
    var body: [String: Any] = ["error": canary, "message": canary, "code": code]
    if let reason { body["reason"] = reason }
    let actual = try await failure(.codexActivate, status, JSONSerialization.data(withJSONObject: body))
    try expect(actual == expected, "Recognized public Codex codes must retain useful fixed guidance")
  }
  for data in [
    Data("{malformed \(canary)".utf8),
    Data("<html>\(canary)</html>".utf8),
    try JSONSerialization.data(withJSONObject: ["error": canary + String(repeating: "x", count: 256 * 1024)]),
  ] {
    let actual = try await failure(.refresh, 500, data)
    try expect(actual == "Usage could not be refreshed. Try Refresh.",
      "Malformed, HTML, and large server failures must use fixed refresh guidance")
  }

  let spoofed = try JSONSerialization.data(withJSONObject: [
    "error": canary, "message": canary, "code": "auth_write_failed", "context": "codex-activate",
  ])
  for context in [Context.refresh, .claudeOpen] {
    let baseline = try await failure(context, 500, Data("{}".utf8))
    let actual = try await failure(context, 500, spoofed)
    try expect(actual == baseline, "The server cannot select a different local action's error mapping")
  }

  let settingsTransport = MockTransport(replies: [
    "PUT /api/codex/profiles/auto-switch": [MockReply(400, canaryJSON)],
  ])
  let settingsClient = AccountsClient(connection: testConnection(), transport: settingsTransport)
  do {
    _ = try await settingsClient.setAutomaticSwitching(enabled: true, thresholdPercent: 5)
    throw CheckFailure(description: "Rejected automatic settings unexpectedly succeeded")
  } catch BarClientError.status(let status, let message) {
    try expect(status == 400 && message == "The automatic switching settings were rejected. Refresh and try again.",
      "Settings failures must use fixed, action-specific guidance")
  }

  let fixture = confirmationObject()
  let transport = MockTransport(replies: [
    "POST /api/codex/profiles/party/activate": [MockReply(409, try JSONSerialization.data(withJSONObject: [
      "error": canary, "message": canary, "code": "busy", "reason": "running_processes", "confirmation": fixture,
    ]))],
  ])
  let confirmation = try await expectCodexConfirmation(AccountsClient(connection: testConnection(), transport: transport), profile: "party")
  try expect(confirmation.token == fixture["token"] as? String &&
    confirmation.warning == fixture["warning"] as? String &&
    confirmation.processes.count == 2 && confirmation.processes[0].pid == 123,
    "Valid structured busy consent must survive removal of arbitrary top-level server error text")
  let calls = await transport.recorded()
  try expect(calls.filter { $0.url.path == "/api/codex/profiles/party/activate" }.count == 1,
    "A sanitized busy response must still wait for explicit user consent")
}

private func checkLoginBackoffAndBadPayload() async throws {
  for (status, error) in [(401, ExpectedError.authentication), (429, .rateLimited), (503, .status(503))] {
    let transport = MockTransport(replies: ["POST /api/auth/login": [MockReply(status)]])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    try await expectError(error, "Login failure must preserve useful error type") {
      _ = try await client.dashboard()
    }
    try await expectError(.rateLimited, "Repeated polling must not consume failed login attempts") {
      _ = try await client.dashboard(refresh: true)
    }
    let calls = await transport.recorded()
    try expect(calls.count == 1, "Failed login must suppress repeated automatic login attempts")
  }
  let malformed = MockTransport(replies: ["GET /api/accounts/dashboard": [MockReply(200, Data("{broken".utf8))]])
  let client = AccountsClient(connection: testConnection(), transport: malformed)
  try await expectError(.decoding, "Malformed dashboard JSON must produce a safe decoding error") {
    _ = try await client.dashboard()
  }
}

private func checkUnknownQuotaAndExactReset() throws {
  let dashboard = try JSONDecoder().decode(AccountDashboard.self, from: dashboardJSON)
  let account = dashboard.accounts[0]
  let windows = account.windows
  try expect(windows[0].usedPercent == nil && windows[0].remainingPercent == nil,
    "Unknown quota must remain unknown rather than zero")
  try expect(windows[0].clampedUsedPercent == nil, "Unknown quota must not draw an empty usage bar")
  try expect(windows[1].clampedUsedPercent == 0 && windows[2].clampedUsedPercent == 100,
    "Known zero and full usage must retain real values")
  try expect(windows[3].clampedUsedPercent == nil,
    "Negative usage must remain unavailable rather than clamp into a plausible zero")
  try expect(windows[4].clampedUsedPercent == 101 && windows[8].clampedUsedPercent == 120,
    "Finite over-quota usage must preserve actual percentages instead of hiding or clipping them")
  try expect(windows[1].clampedRemainingPercent == 100 && windows[2].clampedRemainingPercent == 0 &&
    windows[8].clampedRemainingPercent == 0,
    "Valid remaining percentages, including real zero, must keep their independent values")
  try expect(windows[3].clampedRemainingPercent == nil && windows[4].clampedRemainingPercent == nil,
    "Out-of-range remaining percentages must remain unavailable independently of actual used usage")
  let nonfiniteDecoder = JSONDecoder()
  nonfiniteDecoder.nonConformingFloatDecodingStrategy = .convertFromString(
    positiveInfinity: "Infinity", negativeInfinity: "-Infinity", nan: "NaN")
  for value in ["NaN", "Infinity", "-Infinity"] {
    let data = Data("""
    {"key":"invalid","label":"Invalid usage","usedPercent":"\(value)","remainingPercent":"\(value)"}
    """.utf8)
    let window = try nonfiniteDecoder.decode(AccountQuotaWindow.self, from: data)
    try expect(window.clampedUsedPercent == nil && window.clampedRemainingPercent == nil,
      "Nonfinite usage and remaining values must never be displayed or used for bar geometry")
  }
  try expect(!account.canActivate && !account.canOpenOnMac,
    "Usage-only providers must not expose activation even if a payload contains capabilities")
  try expect(windows[0].resetAt == nil && AccountFormatting.date(nil) == nil,
    "Unknown reset must not be inferred from usage or window length")
  try expect(AccountFormatting.reset(nil) == "Reset time unavailable", "Unknown reset must be labelled unavailable")
  let fractional = AccountFormatting.date(windows[1].resetAt)
  let whole = AccountFormatting.date(windows[2].resetAt)
  try expect(fractional != nil && whole != nil, "ISO UTC reset timestamps must parse with and without fractional seconds")
  if let fractional, let whole {
    try expect(abs(fractional.timeIntervalSince(whole) - 0.250) < 0.00001,
      "Reset parser must preserve server-provided fractional seconds")
    let offset = AccountFormatting.date("2026-09-30T23:14:15.250-04:00")
    try expect(offset == fractional, "Reset parser must respect timezone offsets")
    try expect(AccountFormatting.reset(windows[1].resetAt, now: fractional.addingTimeInterval(-90)).hasSuffix(" · 2m"),
      "Reset countdown must round upward from exact reset rather than infer a new period")
    try expect(AccountFormatting.reset(windows[1].resetAt, now: fractional.addingTimeInterval(90)).hasPrefix("Reset "),
      "Past server reset must not be rolled forward into a fictional future window")
  }
  try expect(AccountFormatting.date("not-a-date") == nil, "Invalid reset timestamp must remain unavailable")
}

private func checkPrivateConnectionFile() async throws {
  let directory = FileManager.default.temporaryDirectory.appendingPathComponent("ccs-bar-core-check-\(UUID().uuidString)")
  try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
    attributes: [.posixPermissions: 0o700])
  defer { try? FileManager.default.removeItem(at: directory) }
  let file = directory.appendingPathComponent("connection.json")
  let data = try JSONEncoder().encode(testConnection())
  try data.write(to: file, options: .atomic)
  try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
  let loaded = try BarConnection.load(from: file)
  try expect(loaded.baseURL == testConnection().baseURL && loaded.username == "offline-test",
    "Private connection file must load configured endpoint")
  for mode in [0o644, 0o640, 0o604, 0o620, 0o666] {
    try FileManager.default.setAttributes([.posixPermissions: mode], ofItemAtPath: file.path)
    try await expectError(.privateConfigRequired, "Connection file must forbid group/other access") {
      _ = try BarConnection.load(from: file)
    }
  }
  try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
  for base in ["ftp://ccs.invalid", "file:///tmp/ccs", "http://user:password@ccs.invalid",
    "http://ccs.invalid?query=true", "http://ccs.invalid#fragment", "http://ccs.invalid/provider/path"] {
    let object = ["baseURL": base, "username": "offline-test", "password": "not-a-real-credential"]
    try JSONSerialization.data(withJSONObject: object).write(to: file)
    try await expectError(.invalidConnection, "Connection endpoint must be a plain HTTP origin") {
      _ = try BarConnection.load(from: file)
    }
  }
  for field in ["username", "password"] {
    var object = ["baseURL": "https://ccs.invalid", "username": "offline-test", "password": "not-a-real-credential"]
    object[field] = ""
    try JSONSerialization.data(withJSONObject: object).write(to: file)
    try await expectError(.invalidConnection, "Connection credentials must not be empty") {
      _ = try BarConnection.load(from: file)
    }
  }
}

private func checkUsageMetadata() throws {
  let dashboard = try JSONDecoder().decode(AccountDashboard.self, from: dashboardJSON)
  let windows = dashboard.accounts[0].windows
  let legacy = windows[0]
  try expect(legacy.kind == nil && legacy.remaining == nil && legacy.expiresAt == nil,
    "Old responses without optional usage metadata must still decode")
  try expect(legacy.unlimited == nil && legacy.enabled == nil,
    "Missing optional flags must remain unknown rather than false")
  let balance = windows[5]
  try expect(balance.kind == "balance" && balance.unit == "credits",
    "Provider balance kind and unit must retain their exact meaning")
  try expect(balance.used == 9.75 && balance.limit == 50 && balance.remaining == 40.25,
    "Numeric balances must preserve fractional precision independently of percentages")
  try expect(balance.usedPercent == nil && balance.remainingPercent == nil,
    "Numeric balances must not invent a quota percentage")
  try expect(balance.unlimited == false && balance.enabled == true,
    "Explicit limited and enabled flags must survive decoding")
  try expect(balance.resetAt == "2026-10-02T03:14:15Z" && balance.expiresAt == "2026-12-31T23:59:59.250Z",
    "Quota reset and credit expiration must remain separate exact server timestamps")
  try expect(AccountFormatting.date(balance.resetAt) != AccountFormatting.date(balance.expiresAt),
    "Expiration must never be substituted for an earlier quota reset")
  let expirationLabel = AccountFormatting.expiration(balance.expiresAt)
  try expect(expirationLabel.hasPrefix("Expires ") && !expirationLabel.contains("Reset"),
    "Expiration display must identify entitlement expiration rather than quota reset")
  let extra = windows[6]
  try expect(extra.kind == "extra_usage" && extra.used == 7.5 && extra.limit == nil && extra.remaining == 0,
    "Extra usage must distinguish real zero remaining from unknown limit")
  try expect(extra.unlimited == true && extra.enabled == false,
    "Unlimited and disabled flags must be preserved independently")
  try expect(extra.resetAt == nil && extra.expiresAt == "2026-10-03T01:02:03Z",
    "An expiration-only balance must retain an unknown quota reset")
  try expect(AccountFormatting.reset(extra.resetAt) == "Reset time unavailable",
    "Expiration-only balance must not display its expiration as a reset")
  let spend = windows[7]
  try expect(spend.kind == "spend" && spend.unit == "USD" && spend.used == 12.5 && spend.limit == 100,
    "Spend must retain provider-supplied counters and currency")
  try expect(spend.remaining == nil && spend.expiresAt == nil && spend.resetAt != nil,
    "Missing spend balance or expiration must not be inferred from a reported reset")
  try expect(AccountFormatting.expiration(spend.expiresAt) == "Expiration unavailable",
    "Unknown expiration must be labelled unavailable independently of known reset")
  try expect(AccountFormatting.expiration("not-a-date") == "Expiration unavailable",
    "Malformed expiration must not create a plausible expiration date")
}

private func safeLiveError(_ error: Error) -> String {
  guard let clientError = error as? BarClientError else {
    return "The AI Account Center connection or account request could not be completed."
  }
  switch clientError {
  case .status(let status, _): return "AI Account Center returned HTTP \(status)."
  case .codexConfirmation: return "The Codex switch requires user confirmation."
  default: return clientError.errorDescription ?? "The AI Account Center account request could not be completed."
  }
}

private func checkCachedWindowProvenance() throws {
  let legacy = try JSONDecoder().decode(AccountDashboard.self, from: dashboardJSON)
  try expect(legacy.accounts[0].windows.allSatisfy { $0.status == nil && $0.sampledAt == nil },
    "Older quota responses must decode missing per-window provenance as unknown")
  var object = try JSONSerialization.jsonObject(with: dashboardJSON) as! [String: Any]
  var account = (object["accounts"] as! [[String: Any]])[0]
  let originalWindows = account["windows"] as! [[String: Any]]
  let originalSample = "2026-09-30T13:20:06.123Z"
  var retained = originalWindows[5]
  retained["status"] = "cached"
  retained["sampledAt"] = originalSample
  account["provider"] = "claude"
  account["status"] = "ok"
  account["fetchedAt"] = "2026-10-01T17:00:00Z"
  account["sampledAt"] = "2026-10-01T17:00:00Z"
  account["windows"] = [originalWindows[1], retained]
  object["accounts"] = [account]
  let decoded = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: object))
  let fresh = decoded.accounts[0].windows[0], cached = decoded.accounts[0].windows[1]
  try expect(fresh.status == nil && fresh.sampledAt == nil && fresh.usedPercent == 0,
    "Fresh core quota must not inherit cached optional-window metadata")
  try expect(cached.status == "cached" && cached.sampledAt == originalSample && cached.remaining == 40.25 && cached.used == 9.75,
    "Retained extras must preserve the original sample and actual fractional values")
  try expect(cached.resetAt == legacy.accounts[0].windows[5].resetAt && cached.expiresAt == legacy.accounts[0].windows[5].expiresAt,
    "Cached provenance must not change actual reset or expiration timestamps")
  let label = AccountFormatting.cachedSample(status: cached.status, sampledAt: cached.sampledAt)
  try expect(label?.hasPrefix("Cached · Sampled ") == true &&
    label != AccountFormatting.cachedSample(status: "cached", sampledAt: decoded.accounts[0].fetchedAt),
    "Cached Details must use the original window sample rather than the fresh account fetch")
  try expect(AccountFormatting.cachedSample(status: nil, sampledAt: originalSample) == nil &&
    AccountFormatting.cachedSample(status: "ok", sampledAt: originalSample) == nil,
    "Only explicit per-window cached status may display a Cached label")
  try expect(AccountFormatting.cachedSample(status: "cached", sampledAt: nil) == "Cached · Sample time unavailable" &&
    AccountFormatting.cachedSample(status: "cached", sampledAt: "invalid-date") == "Cached · Sample time unavailable",
    "Missing or malformed original samples must remain unavailable, without a fresh-date fallback")
}

private func checkProviderGrouping() throws {
  var object = try JSONSerialization.jsonObject(with: dashboardJSON) as! [String: Any]
  let prototype = (object["accounts"] as! [[String: Any]])[0]
  func account(_ id: String, _ provider: String, _ status: String, _ date: String?, active: Bool = false) -> [String: Any] {
    var value = prototype
    value["id"] = id
    value["provider"] = provider
    value["status"] = status
    value["sampledAt"] = "invalid-date"
    value["fetchedAt"] = date.map { $0 as Any } ?? NSNull()
    value["isActive"] = active
    return value
  }
  let accounts = [
    account("qwen", "qwen", "cached", nil),
    account("codex-inactive", "codex", "ok", "2026-12-01T00:00:00Z"),
    account("claude-old-live", "claude", "ok", "2026-01-01T00:00:00Z"),
    account("claude-cached", "claude", "cached", "2027-01-01T00:00:00Z"),
    account("claude-signin", "claude", "needs_sign_in", nil),
    account("claude-current-live", "claude", "ok", "2026-09-01T00:00:00Z"),
    account("codex-active", "codex", "cached", "2026-01-01T00:00:00Z", active: true),
    account("future", "future-provider", "unavailable", nil),
  ]
  object["accounts"] = accounts
  let decoded = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: object))
  let groups = decoded.providerGroups
  try expect(groups.map(\.id) == ["claude", "codex", "qwen", "future-provider"],
    "Provider grouping must order only real groups without inventing absent providers")
  try expect(groups[0].accounts.count == 4 && groups[0].representative.id == "claude-current-live",
    "Claude grouping must preserve four accounts and prefer the freshest live sample over a newer cache")
  try expect(groups[1].representative.id == "codex-active",
    "Codex glance must represent the actually active runtime even when another account has newer quota")
  try expect(groups.flatMap(\.accounts).count == accounts.count,
    "Grouping must preserve every original account")
  try expect(groups[0].primaryWindows.map(\.key) == ["unknown", "zero", "full"],
    "Primary glance must preserve actual server window order and cap only the glance")
  try expect(groups[0].supplementaryWindows.map(\.key) == ["balance", "extra"],
    "Balances and extra usage must remain available separately")
  try expect(groups[0].representative.windows.count == 9,
    "Collapsed provider selection must not drop detail windows")
  try expect(groups[1].statusLabel == "Cached" && groups[0].statusLabel == "Live",
    "Provider status must describe the selected sample rather than claim every account is online")
}

private func checkCodexAutoStatusText() throws {
  let object = try JSONSerialization.jsonObject(with: dashboardJSON) as! [String: Any]
  let prototype = (object["accounts"] as! [[String: Any]])[0]
  func account(_ id: String, _ email: String, _ profile: String) -> [String: Any] {
    var value = prototype
    value["id"] = id
    value["provider"] = "codex"
    value["email"] = email
    value["label"] = email
    var caps = value["capabilities"] as! [String: Any]
    caps["codexProfile"] = profile
    value["capabilities"] = caps
    return value
  }
  func status(_ outcome: String, _ message: String, enabled: Bool = true, candidate: String? = nil) -> [String: Any] {
    var value: [String: Any] = [
      "enabled": enabled, "thresholdPercent": 5, "pollIntervalSeconds": 60,
      "outcome": outcome, "message": message, "activationInProgress": false,
    ]
    if let candidate { value["candidate"] = candidate }
    return value
  }
  func text(auto: [String: Any], accounts: [[String: Any]]) throws -> String? {
    var payload = object
    payload["accounts"] = accounts
    payload["codexAutoSwitch"] = auto
    let decoded = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: payload))
    return AccountFormatting.codexAutoStatusText(status: decoded.codexAutoSwitch, accounts: decoded.accounts)
  }
  let accounts = [account("codex:a", "a@example.test", "a"), account("codex:b", "b@example.test", "b")]
  let waiting = try text(auto: status("waiting_idle", "Waiting.", candidate: "b"), accounts: accounts)
  try expect(waiting == "Waiting. Will switch to b@example.test when Codex goes idle. Activate b@example.test to switch now.",
    "A blocked Codex switch must name the vetted candidate account, resolved to its identity")
  let unknown = try text(auto: status("waiting_idle", "Waiting.", candidate: "ghost"), accounts: accounts)
  try expect(unknown == "Waiting. Will switch to ghost when Codex goes idle. Activate ghost to switch now.",
    "An unknown candidate profile must fall back to the profile name, never vanish")
  let plain = try text(auto: status("no_quota", "The reading is out of date."), accounts: accounts)
  try expect(plain == "The reading is out of date.",
    "A blocked switch without a candidate must show the plain reason")
  let hiddenHealthy = try text(auto: status("healthy", "Healthy."), accounts: accounts)
  let shownWaiting = try text(auto: status("waiting_idle", "Waiting.", candidate: "b"), accounts: accounts)
  let hiddenDisabled = try text(auto: status("waiting_idle", "Waiting.", enabled: false, candidate: "b"), accounts: accounts)
  try expect(hiddenHealthy == nil && shownWaiting != nil && hiddenDisabled == nil,
    "The stuck-switch line must hide for healthy or disabled switching")
}

private func checkVisibleUsageWindows() throws {
  let original = try JSONSerialization.jsonObject(with: dashboardJSON) as! [String: Any]
  let prototype = (original["accounts"] as! [[String: Any]])[0]
  func window(_ key: String, _ label: String, _ values: [String: Any] = [:]) -> [String: Any] {
    var result: [String: Any] = [
      "key": key, "label": label,
      "usedPercent": NSNull(), "remainingPercent": NSNull(), "resetAt": NSNull(),
      "windowMinutes": NSNull(), "used": NSNull(), "limit": NSNull(), "unit": NSNull(),
    ]
    for (key, value) in values { result[key] = value }
    return result
  }
  func accountObject(_ id: String, _ provider: String, _ windows: [[String: Any]], plan: String? = nil) -> [String: Any] {
    var result = prototype
    result["id"] = id
    result["provider"] = provider
    result["plan"] = plan.map { $0 as Any } ?? NSNull()
    result["windows"] = windows
    return result
  }
  func decodeAccount(_ object: [String: Any]) throws -> DashboardAccount {
    try JSONDecoder().decode(DashboardAccount.self, from: JSONSerialization.data(withJSONObject: object))
  }
  let common = [
    window("chat_pass", "Quota", ["usedPercent": 20]),
    window("label-matched", "Chat Pass", ["usedPercent": 30]),
    window("five_hour", "5-hour", ["windowMinutes": 300]),
    window("weekly", "Weekly", ["windowMinutes": 10080]),
    window("plan_subscription", "Subscription"),
    window("subscription", "Entitlement"),
    window("plan-label-matched", "Plan subscription"),
    window("reset-packs-5h", "Five-hour reset packs", ["remaining": 0, "unit": "packs"]),
    window("reset-packs-weekly", "Weekly reset packs", ["remaining": 0, "unit": "packs"]),
  ]
  let rawKeys = common.map { $0["key"] as! String }
  let pro = try decodeAccount(accountObject("codex-pro", "codex", common, plan: "ChatGPT Pro"))
  try expect(pro.visibleWindows.map(\.key) == rawKeys.filter { !["chat_pass", "label-matched", "five_hour"].contains($0) },
    "Codex Pro presentation must hide ChatPass and an unsupplied five-hour window while keeping other data")
  try expect(pro.windows.map(\.key) == rawKeys && pro.windows[0].usedPercent == 20,
    "Presentation filtering must preserve every raw Codex window and actual percentage")
  let zeroFiveHour = window("five_hour", "5-hour", ["windowMinutes": 300, "usedPercent": 0, "remainingPercent": 100])
  for plan in ["Plus", "Pro"] {
    let actual = try decodeAccount(accountObject("codex-\(plan)", "codex", [zeroFiveHour], plan: plan))
    try expect(actual.visibleWindows.count == 1 && actual.visibleWindows[0].usedPercent == 0 &&
      actual.visibleWindows[0].remainingPercent == 100,
      "A real zero-used five-hour quota must remain visible for Plus and Pro")
  }
  let plusUnknown = try decodeAccount(accountObject("codex-plus-unknown", "codex", [common[2]], plan: "Plus"))
  try expect(plusUnknown.visibleWindows.count == 1,
    "The empty-five-hour suppression must be scoped to Pro rather than remove Plus data")
  let qwen = try decodeAccount(accountObject("qwen", "qwen", common))
  try expect(qwen.visibleWindows.map(\.key) == rawKeys.filter { !["plan_subscription", "subscription", "plan-label-matched"].contains($0) },
    "Qwen must suppress subscription metadata by actual subscription key or plan-subscription label")
  try expect(qwen.windows.map(\.key) == rawKeys,
    "Qwen presentation must preserve original subscription fields for raw data consumers")

  let packs = [
    window("reset-packs-5h", "Five-hour reset packs", ["remaining": 0, "unit": "packs"]),
    window("reset-packs-weekly", "Weekly reset packs", ["remaining": 0, "unit": "packs"]),
    window("reset-packs-empty", "Reset packs"),
    window("five-hour", "Five-hour", ["usedPercent": 0, "remainingPercent": 100]),
    window("weekly", "Weekly", ["usedPercent": 5]),
    window("reset-packs-5h-positive", "Reset packs", ["remaining": 2, "unit": "packs"]),
    window("pack-individual", "Individual reset pack", ["remaining": 0, "expiresAt": "2026-12-31T23:59:59Z", "unit": "packs"]),
  ]
  let zai = try decodeAccount(accountObject("zai", "zai", packs))
  try expect(zai.visibleWindows.map(\.key) == ["five-hour", "weekly", "reset-packs-5h-positive", "pack-individual"],
    "Z.ai must hide zero reset-pack summaries and retain real usage and individual pack details")
  try expect(zai.windows.count == packs.count && zai.windows[0].remaining == 0 &&
    zai.windows[6].expiresAt == "2026-12-31T23:59:59Z",
    "Z.ai filtering must preserve zero summary counts and exact individual expiration in raw data")
  let positiveSummary = try decodeAccount(accountObject("zai-positive", "zai", [
    window("reset-packs-5h", "Five-hour reset packs", ["remaining": 2, "unit": "packs"]),
    window("reset-packs-weekly", "Weekly reset packs", ["remaining": 1, "unit": "packs"]),
  ]))
  try expect(positiveSummary.visibleWindows.map(\.remaining) == [2, 1],
    "Positive Z.ai reset-pack summary counts must remain visible with actual values")
  let timedSummary = try decodeAccount(accountObject("zai-timed", "zai", [
    window("reset-packs-5h", "Five-hour reset packs", ["remaining": 0, "resetAt": "2026-12-31T01:02:03Z"]),
  ]))
  try expect(timedSummary.visibleWindows.count == 1 && timedSummary.visibleWindows[0].resetAt == "2026-12-31T01:02:03Z",
    "A reset-pack summary with actual timing detail must not be hidden solely because its count is zero")
  for provider in ["claude", "cursor", "muse", "antigravity", "kimi-code", "opencode-go"] {
    let untouched = try decodeAccount(accountObject(provider, provider, common, plan: "Pro"))
    try expect(untouched.visibleWindows.map(\.key) == rawKeys && untouched.windows.count == common.count,
      "Codex, Qwen, and Z.ai presentation rules must not filter other providers with similarly named windows")
  }
  let claudeModelWindows = [
    window("five_hour", "Five-hour usage", ["usedPercent": 12]),
    window("seven_day", "Weekly usage", ["usedPercent": 34]),
    window("seven_day_opus", "Weekly Opus usage", ["usedPercent": 56]),
    window("seven_day_fable", "Weekly Fable usage", ["usedPercent": 0, "resetAt": "2026-10-08T12:00:00Z"]),
  ]
  let maxClaude = try decodeAccount(accountObject("claude-max", "claude", claudeModelWindows, plan: "Max"))
  try expect(maxClaude.compactWindows.map(\.key) == ["five_hour", "seven_day", "seven_day_fable"] &&
    maxClaude.compactWindows.last?.usedPercent == 0 && maxClaude.compactWindows.last?.resetAt == "2026-10-08T12:00:00Z",
    "Claude Max must display the real Fable quota, exact zero usage/reset, without aliasing Opus")
  let maxWithoutFable = try decodeAccount(accountObject("claude-max-no-fable", "claude", Array(claudeModelWindows.prefix(3)), plan: "Max"))
  try expect(!maxWithoutFable.compactWindows.contains { $0.key == "seven_day_fable" },
    "No Claude Fable quota may be manufactured when absent")
  let proClaude = try decodeAccount(accountObject("claude-pro", "claude", claudeModelWindows, plan: "Pro"))
  try expect(proClaude.compactWindows.map(\.key) == ["five_hour", "seven_day", "seven_day_opus"],
    "Fable compact-bar preference applies only to Claude Max; all raw scoped windows stay in details")
  let goOne = accountObject("go-account-one", "opencode-go", [
    window("go-first-hourly", "Hourly", ["usedPercent": 10, "remainingPercent": 90, "resetAt": "2026-12-31T01:00:00Z"]),
  ])
  let goTwo = accountObject("go-account-two", "opencode-go", [
    window("go-second-weekly", "Weekly", ["usedPercent": 80, "remainingPercent": 20, "resetAt": "2027-01-07T02:00:00Z"]),
  ])
  var goDashboardObject = original
  goDashboardObject["accounts"] = [goOne, goTwo]
  let goDashboard = try JSONDecoder().decode(AccountDashboard.self,
    from: JSONSerialization.data(withJSONObject: goDashboardObject))
  let groups = goDashboard.providerGroups
  try expect(groups.count == 1 && groups[0].accounts.map(\.id) == ["go-account-one", "go-account-two"],
    "Provider grouping must preserve distinct OpenCode Go accounts")
  try expect(groups[0].accounts[0].visibleWindows[0].key == "go-first-hourly" &&
    groups[0].accounts[0].visibleWindows[0].usedPercent == 10 &&
    groups[0].accounts[1].visibleWindows[0].key == "go-second-weekly" &&
    groups[0].accounts[1].visibleWindows[0].usedPercent == 80 &&
    groups[0].accounts[1].visibleWindows[0].resetAt == "2027-01-07T02:00:00Z",
    "OpenCode Go accounts must retain their own windows, usage, and exact reset data after grouping")
}

// Decode a complete server DTO rather than reconstructing individual quota windows.
private func checkCodexCompactFullDashboardDTO() throws {
  let sourceRoot = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
  let packageRoot = URL(fileURLWithPath: FileManager.default.currentDirectoryPath, isDirectory: true)
  let candidates = [sourceRoot, packageRoot].map {
    $0.appendingPathComponent("Tests/Fixtures/codex-compact-full-dto.json")
  }
  guard let fixtureURL = candidates.first(where: { FileManager.default.fileExists(atPath: $0.path) }) else {
    throw CheckFailure(description: "Full Codex DTO fixture is missing from the source checkout or package root")
  }
  let dashboard = try JSONDecoder().decode(AccountDashboard.self, from: Data(contentsOf: fixtureURL))
  try expect(dashboard.schemaVersion == 1 && dashboard.accounts.count == 11 &&
    dashboard.updatedAt == "2026-10-01T10:00:00.123Z" && dashboard.settings?.validatedInterval == 120,
    "The complete offline dashboard DTO must retain its accounts, schema, update time, and refresh settings")
  try expect(!dashboard.codexAutoSwitch.enabled && dashboard.codexAutoSwitch.thresholdPercent == 5 &&
    dashboard.codexAutoSwitch.pollIntervalSeconds == 60 && dashboard.codexAutoSwitch.outcome == "disabled" &&
    !dashboard.codexAutoSwitch.activationInProgress && dashboard.codexAutoSwitch.lastCheckedAt == nil,
    "Full dashboard decoding must preserve automatic-switch settings without inventing activity")
  try expect(dashboard.accounts.allSatisfy { $0.identity.hasSuffix("@example.invalid") },
    "The full-dashboard fixture must contain only synthetic account identities")
  func account(_ id: String) throws -> DashboardAccount {
    guard let value = dashboard.accounts.first(where: { $0.id == id }) else {
      throw CheckFailure(description: "Full dashboard fixture is missing account \(id)")
    }
    return value
  }

  let plus = try account("codex-plus-core")
  let rawKeys = ["additional_requests", "seven_day", "chat_pass_additional", "five_hour",
    "weekly", "additional_credits", "label_only_additional"]
  try expect(plus.windows.map(\.key) == rawKeys && plus.isActive &&
    plus.capabilities.codexProfile == "codex-plus-core",
    "Full DTO decoding must preserve source order, active identity, capabilities, and all raw additional windows")
  try expect(plus.visibleWindows.map(\.key) == ["additional_requests", "seven_day", "five_hour", "weekly", "additional_credits"],
    "Details must retain generic and supplementary Codex windows while hiding ChatPass by key and label")
  let compact = plus.compactWindows
  try expect(compact.map(\.key) == ["five_hour", "seven_day"],
    "Codex compact bars must order exact five_hour then seven_day even when a generic 300-minute extra precedes reversed core windows")
  try expect(compact[0].usedPercent == 0 && compact[0].remainingPercent == 100 &&
    compact[0].used == 0 && compact[0].limit == 100 && compact[0].resetAt == "2026-10-01T15:00:00.789Z" &&
    compact[1].usedPercent == 24 && compact[1].resetAt == "2026-10-08T10:00:00.123Z",
    "Compact Codex bars must retain exact reported zero usage, counters, percentages, and each canonical reset")
  let rawExtra = plus.windows[0], visibleExtra = plus.visibleWindows[0]
  try expect(rawExtra.key == "additional_requests" && visibleExtra.key == rawExtra.key &&
    rawExtra.windowMinutes == 300 && rawExtra.usedPercent == 0 && rawExtra.used == 0 &&
    rawExtra.resetAt == "2026-10-01T14:31:00.125Z" && visibleExtra.usedPercent == 0 && visibleExtra.remainingPercent == 100 &&
    visibleExtra.used == 0 && visibleExtra.limit == 100 && visibleExtra.remaining == 100 &&
    visibleExtra.kind == "usage" && visibleExtra.enabled == true && visibleExtra.unit == "requests" &&
    visibleExtra.resetAt == "2026-10-01T14:31:00.125Z" && visibleExtra.status == "cached" &&
    visibleExtra.sampledAt == "2026-09-30T23:59:00.456Z",
    "The generic 300-minute extra must keep its actual values and cached provenance in raw and visible Details")
  try expect(plus.windows[2].usedPercent == 41 && plus.windows[6].usedPercent == 52 &&
    !compact.contains { $0.key == "chat_pass_additional" || $0.key == "label_only_additional" },
    "ChatPass extras must remain raw evidence without becoming visible compact bars")
  try expect(plus.visibleWindows.last?.remaining == 8.75 &&
    plus.visibleWindows.last?.expiresAt == "2026-12-31T23:59:59Z",
    "Codex credits must retain independent balance and expiration in Details")

  let unknownPro = try account("codex-pro-unknown")
  try expect(unknownPro.windows.map(\.key) == ["additional_requests", "five_hour", "seven_day", "chat_pass_additional"] &&
    unknownPro.windows[1].usedPercent == nil && unknownPro.windows[1].remainingPercent == nil &&
    unknownPro.windows[1].resetAt == "2026-10-01T15:00:00Z" &&
    unknownPro.visibleWindows.map(\.key) == ["additional_requests", "seven_day"] &&
    unknownPro.compactWindows.map(\.key) == ["seven_day"],
    "Pro must filter a genuinely unknown five_hour value even when it has timing, without borrowing a populated 300-minute extra")
  let absentPro = try account("codex-pro-absent")
  try expect(absentPro.visibleWindows.map(\.key) == ["additional_requests", "weekly", "seven_day"] &&
    absentPro.compactWindows.map(\.key) == ["seven_day"],
    "A missing Pro five_hour core must remain missing while generic and weekly-alias windows stay in Details")
  let missingWeekly = try account("codex-missing-weekly")
  try expect(missingWeekly.visibleWindows.map(\.key) == ["weekly", "additional_requests", "five_hour"] &&
    missingWeekly.compactWindows.map(\.key) == ["five_hour"],
    "A missing seven_day core must not fall back to a weekly alias or another duration")
  let aliases = try account("codex-aliases-only")
  let aliasKeys = ["additional_requests", "five-hour", "Five_Hour", "5h", "weekly", "Seven_Day",
    "duration_only_week", "label_only_core", "label_only_week", "seven-day"]
  try expect(aliases.windows.map(\.key) == aliasKeys && aliases.visibleWindows.map(\.key) == aliasKeys &&
    aliases.compactWindows.isEmpty,
    "With neither exact core key, Codex compact bars must stay empty; duration, labels, case, punctuation, 5h, and weekly aliases are Details only")
  let hiddenCore = try account("codex-hidden-core")
  try expect(hiddenCore.windows.map(\.key) == ["additional_requests", "five_hour", "seven_day"] &&
    hiddenCore.visibleWindows.map(\.key) == ["additional_requests"] && hiddenCore.compactWindows.isEmpty,
    "Canonical key matching must use visibleWindows so ChatPass-labelled core windows remain hidden with no extra-window fallback")
  let unknownPlus = try account("codex-plus-unknown")
  try expect(unknownPlus.visibleWindows.map(\.key) == ["additional_requests", "seven_day", "five_hour"] &&
    unknownPlus.compactWindows.map(\.key) == ["five_hour", "seven_day"] &&
    unknownPlus.compactWindows[0].usedPercent == nil && unknownPlus.compactWindows[0].remainingPercent == nil,
    "Plus must retain its genuinely unknown exact five_hour core without inventing usage or substituting generic values")

  let claude = try account("claude-max-fable")
  try expect(claude.windows.map(\.key) == ["five_hour", "seven_day", "seven_day_opus", "seven_day_fable"] &&
    claude.visibleWindows.count == 4 && claude.compactWindows.map(\.key) == ["five_hour", "seven_day", "seven_day_fable"] &&
    claude.compactWindows.last?.usedPercent == 0 && claude.compactWindows.last?.resetAt == "2026-10-08T12:00:00Z",
    "The Codex compact selection must preserve Claude Max's genuine zero-used Fable preference and complete model details")
  let cursor = try account("cursor-unaffected")
  try expect(cursor.visibleWindows.map(\.key) == cursor.windows.map(\.key) &&
    cursor.compactWindows.map(\.key) == ["additional_requests", "seven_day", "five_hour"] &&
    cursor.visibleWindows.contains { $0.key == "chat_pass_additional" } &&
    cursor.visibleWindows.contains { $0.key == "label_only_additional" },
    "Other-provider compact ordering and ChatPass-named Details must remain unaffected by Codex rules")
  let qwen = try account("qwen-subscription")
  try expect(qwen.windows.count == 3 && qwen.visibleWindows.map(\.key) == ["five_hour", "seven_day"] &&
    qwen.compactWindows.map(\.key) == ["five_hour", "seven_day"] && qwen.compactWindows[0].usedPercent == 0,
    "Qwen must preserve raw subscription metadata while keeping its own visibility rule and genuine zero usage")
  let zai = try account("zai-reset-packs")
  try expect(zai.windows.count == 5 && zai.visibleWindows.map(\.key) == ["five-hour", "weekly", "pack-individual"] &&
    zai.compactWindows.map(\.key) == ["five-hour", "weekly", "pack-individual"] &&
    zai.visibleWindows.last?.remaining == 0 && zai.visibleWindows.last?.expiresAt == "2026-12-31T23:59:59Z",
    "Z.ai must retain its own zero-summary filtering, alias-key compact windows, and real individual-pack expiration")
  let codexGroup = dashboard.providerGroups.first(where: { $0.id == "codex" })
  try expect(dashboard.providerGroups.flatMap(\.accounts).count == dashboard.accounts.count &&
    codexGroup?.representative.id == plus.id &&
    codexGroup?.representative.compactWindows.map(\.key) == ["five_hour", "seven_day"],
    "Provider grouping must preserve every account, the active Codex identity, and its exact account compact cores")
}

// MARK: Antigravity switching routes (commit 9cf75fbe)

private func antigravityOffer(profile: String = "agy-two", token: String = "agy_fixture_token_0123456789",
  expires: Date = Date().addingTimeInterval(120), email: String = "antigravity-2@example.invalid",
  processes: [[String: Any]] = [["pid": 4321, "role": "cli", "label": "FIXTURE_ONLY_SERVER_LABEL"]]) -> [String: Any] {
  [
    "status": "confirmation-required", "profileId": profile, "hostId": "ubuntu", "email": email,
    "confirmation": [
      "token": token, "expiresAt": ISO8601DateFormatter().string(from: expires), "profileId": profile,
      "hostId": "ubuntu", "email": email, "warning": "FIXTURE_ONLY_SERVER_WARNING", "processes": processes,
    ] as [String: Any],
  ]
}

private func checkAntigravityClient() async throws {
  let activated = try JSONSerialization.data(withJSONObject: [
    "status": "active", "profileId": "agy-two", "hostId": "ubuntu", "email": "antigravity-2@example.invalid",
  ])
  let transport = MockTransport(replies: ["POST /api/antigravity/profiles/agy-two/activate": [MockReply(200, activated)]])
  let client = AccountsClient(connection: testConnection(), transport: transport)
  let result = try await client.activateAntigravity(profile: "agy-two")
  try expect(result.status == "active" && result.profileId == "agy-two" && result.email == "antigravity-2@example.invalid",
    "A verified Antigravity activation must report the requested profile")
  let call = (await transport.recorded()).first { $0.url.path == "/api/antigravity/profiles/agy-two/activate" }
  try expect(call?.method == "POST" && call?.headers["origin"] == "http://ccs.invalid:3000"
    && call?.headers["content-type"] == "application/json",
    "Antigravity activation must be a same-origin JSON POST")
  let activateBody = try call?.jsonBody()
  try expect(activateBody.map { $0.count == 1 && $0["hostId"] as? String == "ubuntu" } == true,
    "Antigravity activation must address only the Ubuntu host and send no token")

  // Running programs: a one-use offer that only an explicit confirm may answer.
  let offerTransport = MockTransport(replies: [
    "POST /api/antigravity/profiles/agy-two/activate": [MockReply(409, try JSONSerialization.data(withJSONObject: antigravityOffer()))],
  ])
  let offerClient = AccountsClient(connection: testConnection(), transport: offerTransport)
  do {
    _ = try await offerClient.activateAntigravity(profile: "agy-two")
    throw CheckFailure(description: "A running-program response must not activate")
  } catch BarClientError.antigravityConfirmation(let offer) {
    try expect(offer.profileId == "agy-two" && offer.email == "antigravity-2@example.invalid" && offer.isValid(for: "agy-two"),
      "The Antigravity offer must be bound to the requested profile and identity")
    try expect(offer.processes.count == 1 && offer.processes[0].pid == 4321 && offer.processes[0].label == "Antigravity CLI",
      "Process labels must come from the fixed client table, never the server's text")
    try expect(!AntigravitySwitchConfirmation.warning.contains("FIXTURE_ONLY"),
      "The confirmation warning must be the fixed client copy")
  }
  let offerCalls = await offerTransport.recorded()
  try expect(offerCalls.filter { $0.url.path.hasSuffix("/activate") }.count == 1,
    "An offer must wait for consent without a second request")

  // Offers that do not describe exactly what will stop are refused with fixed guidance.
  let rejected: [[String: Any]] = [
    antigravityOffer(profile: "agy-other"),
    antigravityOffer(token: "short"),
    antigravityOffer(expires: Date().addingTimeInterval(-5)),
    antigravityOffer(processes: [["pid": 0, "role": "cli"]]),
    antigravityOffer(processes: [["pid": 12, "role": "FIXTURE_ONLY_ROLE"]]),
  ]
  for body in rejected {
    let bad = MockTransport(replies: [
      "POST /api/antigravity/profiles/agy-two/activate": [MockReply(409, try JSONSerialization.data(withJSONObject: body))],
    ])
    do {
      _ = try await AccountsClient(connection: testConnection(), transport: bad).activateAntigravity(profile: "agy-two")
      throw CheckFailure(description: "A malformed Antigravity offer unexpectedly activated")
    } catch BarClientError.status(let status, let message) {
      try expect(status == 409 && message == "Antigravity programs are running on Ubuntu. Activate again to review them.",
        "A malformed offer must become fixed guidance, never consent")
    }
  }

  // Confirm sends the token once and never retries it after an authentication failure.
  let confirmed = try JSONSerialization.data(withJSONObject: ["status": "active", "profileId": "agy-two", "hostId": "ubuntu"])
  let confirmTransport = MockTransport(replies: ["POST /api/antigravity/profiles/agy-two/confirm": [MockReply(200, confirmed)]])
  try await AccountsClient(connection: testConnection(), transport: confirmTransport)
    .confirmAntigravity(profile: "agy-two", confirmationToken: "agy_fixture_token_0123456789")
  let confirmCall = (await confirmTransport.recorded()).first { $0.url.path.hasSuffix("/confirm") }
  let confirmBody = try confirmCall?.jsonBody()
  try expect(confirmBody.map { $0.count == 2 && $0["hostId"] as? String == "ubuntu"
    && $0["confirmationToken"] as? String == "agy_fixture_token_0123456789" } == true,
    "Confirm must send exactly the host and the reviewed token")
  let expired = MockTransport(replies: ["POST /api/antigravity/profiles/agy-two/confirm": [MockReply(401)]])
  do {
    try await AccountsClient(connection: testConnection(), transport: expired)
      .confirmAntigravity(profile: "agy-two", confirmationToken: "agy_fixture_token_0123456789")
    throw CheckFailure(description: "An unauthorized confirm unexpectedly succeeded")
  } catch BarClientError.status(let status, _) {
    try expect(status == 401, "An unauthorized confirm must surface the expired session")
  }
  let expiredCalls = await expired.recorded()
  try expect(expiredCalls.filter { $0.url.path.hasSuffix("/confirm") }.count == 1,
    "A consumed confirmation token must never be replayed")
  for bad in ["", "has spaces in it 1234567", String(repeating: "a", count: 257)] {
    try await expectError(.invalidConnection, "Invalid confirmation tokens must be rejected locally") {
      try await AccountsClient(connection: testConnection(), transport: MockTransport())
        .confirmAntigravity(profile: "agy-two", confirmationToken: bad)
    }
  }
  try await expectError(.invalidConnection, "Unsafe Antigravity profile identifiers must be rejected locally") {
    _ = try await AccountsClient(connection: testConnection(), transport: MockTransport()).activateAntigravity(profile: "../x")
  }

  // Fixed public guidance chosen only by the dashboard's activation status word.
  let canary = "FIXTURE_ONLY_PRIVATE_ERROR_CANARY /fixture/private"
  let statuses: [(Int, String, String)] = [
    (409, "busy", "Antigravity is busy on Ubuntu. Activate again when it is idle."),
    (409, "deferred", "Antigravity activation is deferred on Ubuntu. Refresh its native status before trying again."),
    (409, "unsupported-runtime-probe", "The Ubuntu Antigravity runtime could not be verified. Account switching is unavailable."),
    (409, "stale-confirmation", "This Antigravity confirmation is no longer valid. Activate again to review the running programs."),
    (400, "invalid-profile", "The selected Antigravity profile has no valid saved login."),
    (500, "failed-rolled-back", "Antigravity could not switch accounts on Ubuntu. The previous state was restored."),
    (500, "recovery-required", "Antigravity activation needs recovery on Ubuntu. Account switching is unavailable."),
    (500, "FIXTURE_ONLY_UNKNOWN", "Antigravity account activation failed safely. Refresh the account list before retrying."),
  ]
  for (status, word, expected) in statuses {
    let body = try JSONSerialization.data(withJSONObject: ["status": word, "error": canary, "profileId": "agy-two", "hostId": "ubuntu"])
    let failing = MockTransport(replies: ["POST /api/antigravity/profiles/agy-two/activate": [MockReply(status, body)]])
    do {
      _ = try await AccountsClient(connection: testConnection(), transport: failing).activateAntigravity(profile: "agy-two")
      throw CheckFailure(description: "A failed Antigravity activation unexpectedly succeeded")
    } catch BarClientError.status(let actual, let message) {
      try expect(actual == status && message == expected && !(message ?? "").contains("FIXTURE_ONLY"),
        "Antigravity failures must map to fixed guidance without server text")
    }
  }
  let wrongProfile = MockTransport(replies: ["POST /api/antigravity/profiles/agy-two/activate": [MockReply(200, try JSONSerialization.data(
    withJSONObject: ["status": "active", "profileId": "agy-one", "hostId": "ubuntu"]))]])
  try await expectError(.decoding, "A success for another profile must not count as this activation") {
    _ = try await AccountsClient(connection: testConnection(), transport: wrongProfile).activateAntigravity(profile: "agy-two")
  }

  // Antigravity's own automatic switching: % used, Ubuntu only, validated locally.
  let autoTransport = MockTransport()
  let autoClient = AccountsClient(connection: testConnection(), transport: autoTransport)
  _ = try await autoClient.setAntigravityAutomaticSwitching(enabled: true)
  _ = try await autoClient.setAntigravityAutomaticSwitching(thresholdUsedPercent: 90)
  let puts = (await autoTransport.recorded()).filter { $0.url.path == "/api/antigravity/auto-switch" }
  try expect(puts.count == 2 && puts.allSatisfy { $0.method == "PUT" && $0.headers["origin"] == "http://ccs.invalid:3000" },
    "Antigravity automatic settings must be same-origin PUTs")
  let firstPut = try puts[0].jsonBody(), secondPut = try puts[1].jsonBody()
  try expect(firstPut.count == 1 && firstPut["enabled"] as? Bool == true
    && secondPut.count == 1 && secondPut["thresholdUsedPercent"] as? Int == 90,
    "Each Antigravity settings change must send only the changed field")
  for value in [0, 100] {
    try await expectError(.invalidConnection, "Out-of-range Antigravity thresholds must be rejected locally") {
      _ = try await autoClient.setAntigravityAutomaticSwitching(thresholdUsedPercent: value)
    }
  }
  try await expectError(.invalidConnection, "An empty Antigravity settings change must be rejected locally") {
    _ = try await autoClient.setAntigravityAutomaticSwitching()
  }
}

// MARK: Dashboard additions: Antigravity policy, capabilities, hidden providers

private func checkDashboardAdditions() throws {
  var object = try JSONSerialization.jsonObject(with: dashboardJSON) as! [String: Any]
  let prototype = (object["accounts"] as! [[String: Any]])[0]
  func account(_ id: String, _ provider: String, active: Bool = false, capabilities: [String: Any] = [:]) -> [String: Any] {
    var value = prototype
    value["id"] = id
    value["provider"] = provider
    value["isActive"] = active
    var caps = prototype["capabilities"] as! [String: Any]
    for (key, item) in capabilities { caps[key] = item }
    value["capabilities"] = caps
    return value
  }
  object["accounts"] = [
    account("claude-a", "claude"), account("codex-a", "codex", active: true),
    account("agy-a", "antigravity", active: true, capabilities: ["antigravityProfileId": "agy-a", "antigravityHostIds": ["ubuntu"], "antigravityCanActivate": true]),
    account("agy-b", "antigravity", capabilities: ["antigravityProfileId": "agy-b", "antigravityHostIds": ["ubuntu"], "antigravityCanActivate": true]),
    account("agy-c", "antigravity", capabilities: ["antigravityProfileId": "../c", "antigravityHostIds": ["ubuntu"], "antigravityCanActivate": true]),
    account("agy-d", "antigravity", capabilities: ["antigravityProfileId": "agy-d", "antigravityHostIds": ["mac"], "antigravityCanActivate": true]),
    account("kimi-a", "kimi-code"), account("cursor-a", "cursor"),
  ]
  object["antigravityAutoSwitch"] = [
    "enabled": false, "thresholdUsedPercent": 95, "pollIntervalSeconds": 60, "requestedPoolId": NSNull(),
    "outcome": "disabled", "message": "Fixture", "activationInProgress": false,
  ] as [String: Any]
  let dashboard = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: object))
  try expect(dashboard.antigravityAutoSwitch?.thresholdUsedPercent == 95 && dashboard.antigravityAutoSwitch?.enabled == false,
    "A reported Antigravity policy must decode with its % used threshold")
  try expect(dashboard.hiddenProviders.isEmpty && dashboard.visibleAccounts.count == 8,
    "Without hidden providers in the DTO, every provider stays visible")
  let byID = Dictionary(uniqueKeysWithValues: dashboard.accounts.map { ($0.id, $0) })
  try expect(byID["agy-a"]?.canActivateAntigravity == false && byID["agy-b"]?.canActivateAntigravity == true
    && byID["agy-c"]?.canActivateAntigravity == false && byID["agy-d"]?.canActivateAntigravity == false,
    "Antigravity Activate needs an inactive account, a safe profile id and the Ubuntu host")
  try expect(dashboard.providerGroups.map(\.id) == ["claude", "codex", "antigravity", "cursor", "kimi-code"],
    "Tray order must place Antigravity after Codex, then the other providers")
  try expect(dashboard.canActivateAntigravity(byID["agy-b"]!) && !dashboard.canActivateAntigravity(byID["agy-a"]!)
    && !dashboard.canActivateAntigravity(byID["agy-d"]!),
    "The dashboard-level Antigravity guard keeps the account-level rules")

  // Activation guards: nothing is offered while an activation is already running, and Antigravity needs
  // a second account to switch to.
  var guarded = object
  var guardedAccounts = guarded["accounts"] as! [[String: Any]]
  guardedAccounts.append(account("codex-b", "codex"))
  guarded["accounts"] = guardedAccounts
  var codexStatus = guarded["codexAutoSwitch"] as! [String: Any]
  codexStatus["activationInProgress"] = true
  guarded["codexAutoSwitch"] = codexStatus
  var agyStatus = guarded["antigravityAutoSwitch"] as! [String: Any]
  agyStatus["activationInProgress"] = true
  guarded["antigravityAutoSwitch"] = agyStatus
  let busy = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: guarded))
  let busyByID = Dictionary(uniqueKeysWithValues: busy.accounts.map { ($0.id, $0) })
  try expect(busyByID["codex-b"]!.canActivate && !busy.canActivateCodex(busyByID["codex-b"]!)
    && !busy.canActivateAntigravity(busyByID["agy-b"]!),
    "Activate must wait while a Codex or Antigravity activation is already running")
  codexStatus["activationInProgress"] = false
  guarded["codexAutoSwitch"] = codexStatus
  guarded["accounts"] = guardedAccounts.filter { !["agy-a", "agy-c", "agy-d"].contains($0["id"] as! String) }
  agyStatus["activationInProgress"] = false
  guarded["antigravityAutoSwitch"] = agyStatus
  let single = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: guarded))
  let singleByID = Dictionary(uniqueKeysWithValues: single.accounts.map { ($0.id, $0) })
  try expect(single.canActivateCodex(singleByID["codex-b"]!) && singleByID["agy-b"]!.canActivateAntigravity
    && !single.canActivateAntigravity(singleByID["agy-b"]!),
    "A lone Antigravity account has nothing to switch to")
  var hiddenAgy = guarded
  hiddenAgy["accounts"] = guardedAccounts
  hiddenAgy["providers"] = [["id": "antigravity", "visible": true, "trayVisible": false]]
  let hiddenSection = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: hiddenAgy))
  try expect(!hiddenSection.canActivateAntigravity(hiddenSection.accounts.first { $0.id == "agy-b" }!),
    "A provider hidden in the tray offers no switching in the tray")
  hiddenAgy["providers"] = [["id": "antigravity", "visible": false, "trayVisible": true]]
  let dashboardOnly = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: hiddenAgy))
  try expect(dashboardOnly.canActivateAntigravity(dashboardOnly.accounts.first { $0.id == "agy-b" }!),
    "Hiding a provider on the dashboard alone leaves it, and its switching, in the tray")
  // One of two Antigravity accounts hidden in the tray: the other keeps Activate, because the server keeps the hidden
  // one as a switch candidate and "Show in tray" never changes what the user can operate.
  var oneHiddenAgy = guarded
  oneHiddenAgy["accounts"] = guardedAccounts.filter { ["agy-a", "agy-b"].contains($0["id"] as! String) }.map { row -> [String: Any] in
    var row = row
    if row["id"] as? String == "agy-a" { row["trayHidden"] = true }
    return row
  }
  let oneHidden = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: oneHiddenAgy))
  try expect(oneHidden.visibleAccounts.filter { $0.provider == "antigravity" }.map(\.id) == ["agy-b"]
    && oneHidden.antigravityAccountCount == 2
    && oneHidden.canActivateAntigravity(oneHidden.accounts.first { $0.id == "agy-b" }!),
    "Hiding one of two Antigravity accounts in the tray keeps switching for the other")

  var malformed = object
  malformed["antigravityAutoSwitch"] = ["enabled": "yes", "thresholdUsedPercent": 400]
  let lenient = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: malformed))
  try expect(lenient.antigravityAutoSwitch == nil && lenient.accounts.count == 8,
    "A malformed Antigravity policy must be ignored without losing the dashboard")
  malformed["antigravityAutoSwitch"] = ["enabled": true, "thresholdUsedPercent": 100]
  let outOfRange = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: malformed))
  try expect(outOfRange.antigravityAutoSwitch == nil, "An out-of-range Antigravity threshold must not be shown as a policy")

  // "Show on dashboard" and "Show in tray" are independent (Jared, 2026-10-02): the tray follows only
  // providers[].trayVisible and settings.trayHiddenProviders, plus accounts[].trayHidden, never
  // providers[].visible or accounts[].hidden.
  var hidden = object
  hidden["hiddenProviders"] = ["kimi-code"]
  let topLevel = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: hidden))
  try expect(topLevel.hiddenProviders == ["kimi-code"] && topLevel.visibleAccounts.contains { $0.provider == "kimi-code" }
    && topLevel.providerGroups.contains { $0.id == "kimi-code" } && topLevel.accounts.count == 8,
    "A provider hidden only on the dashboard stays in the tray")
  var inSettings = object
  inSettings["settings"] = ["refreshIntervalSeconds": 60, "hiddenProviders": ["cursor"], "trayHiddenProviders": ["kimi-code"]]
  let fromSettings = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: inSettings))
  try expect(fromSettings.hiddenProviders == ["cursor"] && fromSettings.trayHiddenProviders == ["kimi-code"]
    && fromSettings.settings?.validatedInterval == 60
    && !fromSettings.visibleAccounts.contains { $0.provider == "kimi-code" }
    && fromSettings.visibleAccounts.contains { $0.provider == "cursor" }
    && !fromSettings.providerGroups.contains { $0.id == "kimi-code" } && fromSettings.accounts.count == 8,
    "settings.trayHiddenProviders closes the provider up in the tray; the dashboard list does not, and raw accounts stay")
  var flagged = object
  flagged["providers"] = [["id": "cursor", "label": "Cursor", "order": 3, "visible": false, "trayVisible": false],
    ["id": "kimi-code", "visible": false], ["id": 7, "trayVisible": "no"], "junk"]
  let fromProviders = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: flagged))
  try expect(fromProviders.trayHiddenProviders == ["cursor"] && fromProviders.providers.count == 2
    && !fromProviders.visibleAccounts.contains { $0.provider == "cursor" }
    && fromProviders.visibleAccounts.contains { $0.provider == "kimi-code" },
    "providers[].trayVisible false hides in the tray; a missing trayVisible is visible; malformed entries are skipped")
  // One account's own switches, in all four combinations: [hidden on the dashboard, hidden in the tray].
  let base = object["accounts"] as! [[String: Any]]
  let ids = base.prefix(4).map { $0["id"] as! String }
  let combos: [(Bool, Bool)] = [(false, false), (true, false), (false, true), (true, true)]
  var fourWays = object
  fourWays["accounts"] = base.enumerated().map { index, account -> [String: Any] in
    var row = account
    if index < combos.count { row["hidden"] = combos[index].0; row["trayHidden"] = combos[index].1 }
    return row
  }
  let perAccount = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: fourWays))
  let shown = Set(perAccount.visibleAccounts.map(\.id))
  try expect(shown.contains(ids[0]) && shown.contains(ids[1]) && !shown.contains(ids[2]) && !shown.contains(ids[3])
    && perAccount.visibleAccounts.count == 6 && perAccount.accounts.count == 8
    && perAccount.trayHiddenAccounts.map(\.id) == [ids[2], ids[3]],
    "Each account follows only its own Show in tray: shown in both and dashboard-hidden stay, tray-hidden and both-hidden go")
  // accounts[].hidden alone (an older dashboard, or "Show on dashboard" off) never hides a tray row.
  var dashboardHidden = object
  dashboardHidden["accounts"] = base.map { account -> [String: Any] in var row = account; row["hidden"] = true; return row }
  let onlyDashboard = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: dashboardHidden))
  try expect(onlyDashboard.visibleAccounts.count == 8 && onlyDashboard.trayHiddenAccounts.isEmpty,
    "accounts[].hidden (the dashboard switch) never leaves an account out of the tray")
  // A provider shown in the tray still drops its own tray-hidden account; a provider hidden in the tray drops all of
  // its accounts whatever their own switch says.
  var providerOff = fourWays
  let firstProvider = base[0]["provider"] as! String
  providerOff["providers"] = [["id": firstProvider, "visible": true, "trayVisible": false]]
  let offProvider = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: providerOff))
  try expect(!offProvider.visibleAccounts.contains { $0.provider == firstProvider }
    && offProvider.accounts.count == 8,
    "providers[].trayVisible false hides every account of that provider in the tray")
  inSettings["settings"] = ["refreshIntervalSeconds": 60, "hiddenProviders": "cursor", "trayHiddenProviders": "kimi-code"]
  let badHidden = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: inSettings))
  try expect(badHidden.hiddenProviders.isEmpty && badHidden.trayHiddenProviders.isEmpty && badHidden.visibleAccounts.count == 8,
    "A malformed hidden-provider list must be a no-op")
}

// MARK: Tray presentation rules

private func checkTrayPresentation() throws {
  func window(_ key: String, _ label: String, _ values: [String: Any] = [:]) -> [String: Any] {
    var result: [String: Any] = [
      "key": key, "label": label, "usedPercent": NSNull(), "remainingPercent": NSNull(), "resetAt": NSNull(),
      "windowMinutes": NSNull(), "used": NSNull(), "limit": NSNull(), "unit": NSNull(),
    ]
    for (name, value) in values { result[name] = value }
    return result
  }
  let original = try JSONSerialization.jsonObject(with: dashboardJSON) as! [String: Any]
  let prototype = (original["accounts"] as! [[String: Any]])[0]
  func account(_ id: String, _ provider: String, plan: String?, windows: [[String: Any]], active: Bool = false,
    email: String? = nil) -> [String: Any] {
    var value = prototype
    value["id"] = id
    value["provider"] = provider
    value["plan"] = plan.map { $0 as Any } ?? NSNull()
    value["windows"] = windows
    value["isActive"] = active
    value["email"] = email.map { $0 as Any } ?? NSNull()
    return value
  }
  func decode(_ object: [String: Any]) throws -> DashboardAccount {
    try JSONDecoder().decode(DashboardAccount.self, from: JSONSerialization.data(withJSONObject: object))
  }
  let claudeWindows = [
    window("five_hour", "Five-hour usage", ["usedPercent": 12]),
    window("seven_day", "Weekly usage", ["usedPercent": 34]),
    window("seven_day_opus", "Weekly Opus usage", ["usedPercent": 56]),
    window("seven_day_fable", "Weekly Fable usage", ["usedPercent": 0]),
  ]
  let max = try decode(account("claude-max", "claude", plan: "Max 20x", windows: claudeWindows))
  guard case .window(let fable) = max.fableCell, fable.key == "seven_day_fable", fable.meterUsedPercent == 0 else {
    throw CheckFailure(description: "Claude Max must show its real Fable reading, zero included")
  }
  try expect(max.fiveHourWindow?.key == "five_hour" && max.weeklyWindow?.key == "seven_day",
    "Claude columns must use the exact five_hour and seven_day windows, not a model-scoped one")
  guard case .notReported = try decode(account("claude-max-none", "claude", plan: "max", windows: Array(claudeWindows.prefix(3)))).fableCell
  else { throw CheckFailure(description: "A Max account without a Fable window must read Not reported yet, never zero") }
  guard case .notApplicable = try decode(account("claude-pro", "claude", plan: "Pro", windows: claudeWindows)).fableCell
  else { throw CheckFailure(description: "Pro accounts get no Fable cell in the tray") }
  let maximal = try decode(account("claude-maxim", "claude", plan: "Maximal", windows: claudeWindows))
  try expect(!maximal.isMaxPlan,
    "Only real Max plan names count as Max")

  let codex = try decode(account("codex-pro", "codex", plan: "pro", windows: [
    window("additional_requests", "5h extra", ["windowMinutes": 300, "usedPercent": 70]),
    window("seven_day", "week", ["usedPercent": 9, "windowMinutes": 10080]),
  ], active: true, email: "codex-2@example.invalid"))
  try expect(codex.fiveHourWindow == nil && codex.weeklyWindow?.meterUsedPercent == 9,
    "No Codex 5-hour cell may appear unless the exact five_hour window is reported")

  var object = original
  object["accounts"] = [
    account("codex-pro", "codex", plan: "pro", windows: [
      window("five_hour", "5h", ["usedPercent": 40.5]), window("seven_day", "week", ["usedPercent": 9]),
    ], active: true, email: "codex-2@example.invalid"),
    account("agy-a", "antigravity", plan: "Google AI Pro", windows: [
      window("gemini-weekly", "Gemini Models · Weekly", ["usedPercent": 0.0886, "windowMinutes": 10080]),
      window("gemini-5h", "Gemini Models · 5-hour", ["usedPercent": 0, "windowMinutes": 300]),
      window("3p-weekly", "Claude and GPT models · Weekly", ["remainingPercent": 82, "windowMinutes": 10080]),
    ], active: true, email: "antigravity-1@example.invalid"),
    account("cursor-a", "cursor", plan: "pro", windows: [
      window("plan-reported", "Included usage", ["usedPercent": 1.4461]),
      window("plan", "Plan spend", ["usedPercent": 33.35, "kind": "spend"]),
    ]),
  ]
  let dashboard = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: object))
  let remaining = MenuBarReading.make(dashboard: dashboard, provider: "codex", mode: .remaining)
  try expect(remaining?.text == "\(TrayFormat.number(59.5))%" && remaining?.detail == "Codex · codex-2@example.invalid · 5-hour remaining",
    "The menu bar must show the active Codex account's 5-hour window as % remaining")
  try expect(MenuBarReading.make(dashboard: dashboard, provider: "codex", mode: .used)?.value == 40.5,
    "% used must show the reading itself")
  let agy = MenuBarReading.make(dashboard: dashboard, provider: "antigravity", mode: .used)
  try expect(agy?.value == 0 && agy?.detail == "Antigravity · antigravity-1@example.invalid · 5-hour used",
    "Antigravity's menu-bar reading must use its 5-hour window, zero included")
  try expect(MenuBarReading.make(dashboard: dashboard, provider: "cursor", mode: .used) == nil,
    "A provider with no 5-hour or weekly window shows the icon alone")
  try expect(MenuBarReading.make(dashboard: dashboard, provider: MenuBarReading.nothingProvider, mode: .remaining) == nil,
    "Nothing must show no percentage")
  try expect(TrayColumns.antigravity(dashboard.accounts.filter { $0.provider == "antigravity" }).map(\.key)
    == ["gemini-5h", "gemini-weekly", "3p-weekly"], "Antigravity columns must follow the reported buckets in concept order")
  let cursor = dashboard.accounts.first { $0.provider == "cursor" }!
  try expect(cursor.glanceMeters.map(\.key) == ["plan-reported"]
    && TrayColumns.shortLabel(provider: "cursor", cursor.glanceMeters[0]) == "Included",
    "Spend and balances are amounts, not meters; Cursor captions follow the concept")
  let summary = TrayStatusSummary(dashboard: dashboard)
  try expect(summary.providers == 3 && summary.reporting == 0 && summary.allCached,
    "The status line must count only providers that actually report")

  // Numbers: at most two decimals, the percent in the number's own run, no invented zero.
  try expect(TrayFormat.decimals(9) == 0 && TrayFormat.decimals(0.0886) == 2 && TrayFormat.decimals(25.06101) == 2
    && TrayFormat.decimals(1.5) == 1, "Count-ups keep the reading's own decimal places, at most two")
  try expect(TrayFormat.number(0.0886) == 0.09.formatted(.number.precision(.fractionLength(0...2))),
    "Readings round to at most two decimals")
  let unknown = try decode(account("muse", "muse", plan: nil, windows: [window("weekly", "Weekly")]))
  try expect(unknown.visibleWindows[0].meterUsedPercent == nil && MeterSeverity.of(nil) == .unavailable,
    "A missing reading stays unavailable, never zero")
  try expect(MeterSeverity.of(79.99) == .calm && MeterSeverity.of(80) == .warn && MeterSeverity.of(95) == .crit
    && MeterSeverity.of(100) == .crit && MeterSeverity.of(100.5) == .over, "Severity thresholds are 80, 95 and over 100")
  let now = Date(timeIntervalSince1970: 1_790_000_000)
  try expect(TrayFormat.duration(2 * 86_400 + 4 * 3600 + 59) == "2d 4h" && TrayFormat.duration(3 * 3600 + 5 * 60) == "3h 5m"
    && TrayFormat.duration(59) == "59s", "Countdowns use the concept's d/h/m form")
  try expect(TrayFormat.relative(now.addingTimeInterval(-5), now: now) == "just now"
    && TrayFormat.relative(now.addingTimeInterval(-39), now: now) == "39s ago"
    && TrayFormat.relative(nil, now: now) == "time unavailable", "Sample ages read plainly")
  let iso = ISO8601DateFormatter()
  try expect(TrayFormat.shortReset(iso.string(from: now.addingTimeInterval(-60)), now: now) == "due"
    && TrayFormat.shortReset(iso.string(from: now.addingTimeInterval(6 * 86_400 + 14 * 3600 + 30)), now: now) == "6d 14h"
    && TrayFormat.shortReset(nil, now: now) == nil, "Row resets show a countdown, due, or nothing when unreported")
  try expect(TrayFormat.longReset(nil, now: now) == "No reset reported", "Details never invent a reset")

  // Value motion never overshoots: the ease-out curve stays within 0...1 and only rises.
  var previous = 0.0
  for step in 0...200 {
    let value = TrayMotion.progress(Double(step) / 200)
    try expect(value >= previous - 1e-9 && value <= 1 + 1e-9, "Value easing must be monotonic and never pass its target")
    previous = value
  }
  try expect(abs(TrayMotion.progress(1) - 1) < 1e-6 && TrayMotion.progress(0) < 1e-6, "Value easing must start at 0 and end at 1")
  try expect(TrayMotion.valueCurve.y1 <= 1 && TrayMotion.valueCurve.y2 <= 1, "Control points above 1 would overshoot")
  try expect(TrayMotion.fillFraction(120) == 1 && TrayMotion.fillFraction(-3) == 0 && TrayMotion.fillFraction(nil) == 0
    && TrayMotion.fillFraction(42) == 0.42, "Meter fills are capped at the track; text keeps the real value")
}

// MARK: Sign-in and Change verify before they save

/// What the fixture dashboard saw, across every client the session builds.
private final class SignInFixture: @unchecked Sendable {
  private let lock = NSLock()
  private var log: [String] = []
  private var slow = false
  func record(_ line: String) { lock.lock(); log.append(line); lock.unlock() }
  var requests: [String] { lock.lock(); defer { lock.unlock() }; return log }
  func slowLoginStarted() { lock.lock(); slow = true; lock.unlock() }
  func takeSlowLogin() -> Bool { lock.lock(); defer { slow = false; lock.unlock() }; return slow }
}

/// One client's transport to a fixture dashboard, with its own session: POST /api/auth/login accepts only
/// fixture/fixture-new; GET /api/accounts/settings needs that login. refused.invalid refuses the connection,
/// elsewhere.invalid answers every path with a 404 page, and "fixture-slow" logins never answer until cancelled.
private actor SignInTransport: BarHTTPTransport {
  let fixture: SignInFixture
  private var signedIn = false
  init(_ fixture: SignInFixture) { self.fixture = fixture }

  func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
    let url = request.url!
    let host = url.host ?? ""
    let method = request.httpMethod ?? "GET"
    func reply(_ status: Int, _ body: String = "{}") -> (Data, HTTPURLResponse) {
      fixture.record("\(method) \(host)\(url.path) \(status)")
      return (Data(body.utf8), HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: nil)!)
    }
    if host == "refused.invalid" { throw URLError(.cannotConnectToHost) }
    if host == "elsewhere.invalid" { return reply(404, "<html>Not found</html>") }
    if method == "POST" && url.path == "/api/auth/login" {
      let fields = (try? JSONSerialization.jsonObject(with: request.httpBody ?? Data())) as? [String: String] ?? [:]
      if fields["username"] == "fixture-slow" {
        fixture.slowLoginStarted()
        try await Task.sleep(nanoseconds: 60_000_000_000)
        return reply(200)
      }
      guard fields["username"] == "fixture", fields["password"] == "fixture-new" else { return reply(401, "{\"error\":\"Invalid credentials\"}") }
      signedIn = true
      return reply(200, "{\"success\":true}")
    }
    if method == "GET" && url.path == "/api/accounts/settings" {
      return signedIn ? reply(200, "{\"refreshIntervalSeconds\":60}") : reply(401, "{\"error\":\"Authentication required\"}")
    }
    return reply(404)
  }
}

/// A loopback port nothing listens on: bound for a moment to learn a free number, then closed.
private func closedLoopbackPort() -> Int {
  let socketFD = socket(AF_INET, SOCK_STREAM, 0)
  defer { close(socketFD) }
  var address = sockaddr_in()
  address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
  address.sin_family = sa_family_t(AF_INET)
  address.sin_port = 0
  address.sin_addr.s_addr = inet_addr("127.0.0.1")
  var length = socklen_t(MemoryLayout<sockaddr_in>.size)
  _ = withUnsafePointer(to: &address) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(socketFD, $0, length) } }
  _ = withUnsafeMutablePointer(to: &address) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(socketFD, $0, &length) } }
  return Int(UInt16(bigEndian: address.sin_port))
}

/// Sign-in and Change through the tray's own `ConnectionSession`, with the connection file in an isolated temporary
/// folder (never ~/.ccs). Every failure must leave the file's bytes and the live client exactly as they were; success
/// must replace both, through the same writer.
@MainActor private func checkConnectionChange() async throws {
  let directory = FileManager.default.temporaryDirectory.appendingPathComponent("aac-signin-check-\(UUID().uuidString)")
  defer { try? FileManager.default.removeItem(at: directory) }
  let file = directory.appendingPathComponent("bar/accounts-connection.json")
  let real = BarConnection.configURL.deletingLastPathComponent().standardizedFileURL.path
  try expect(!file.standardizedFileURL.path.hasPrefix(real) && file.standardizedFileURL.path != BarConnection.configURL.standardizedFileURL.path,
    "Sign-in checks must use an isolated connection file")
  let fixture = SignInFixture()
  let dashboard = "http://dashboard.invalid:3000"
  try ConnectionStore.save(BarConnection(baseURL: URL(string: dashboard)!, username: "fixture", password: "fixture-old"), to: file)
  let session = ConnectionSession(fileURL: file, makeTransport: { SignInTransport(fixture) })
  try session.load()
  let saved = try Data(contentsOf: file)
  guard let live = session.client else { throw CheckFailure(description: "The saved connection must load a live client") }
  func unchanged() -> Bool {
    (try? Data(contentsOf: file)) == saved && session.client === live && session.connection?.password == "fixture-old" && !session.isChecking
  }
  let kept = "The saved connection was not changed."

  let refused = await session.change(baseURL: "http://refused.invalid:3000", username: "fixture", password: "fixture-new")
  try expect(refused == "Could not reach a dashboard at that address. \(kept)" && unchanged(),
    "A wrong address must keep the saved connection and the live client")
  let elsewhere = await session.change(baseURL: "http://elsewhere.invalid:3000", username: "fixture", password: "fixture-new")
  try expect(elsewhere == "That address answered, but not as an AI Account Center dashboard. \(kept)" && unchanged(),
    "An address that is not the dashboard must keep the saved connection and the live client")
  let rejected = await session.change(baseURL: dashboard, username: "fixture", password: "wrong-password")
  try expect(rejected == "The dashboard did not accept that username and password. \(kept)" && unchanged()
    && !fixture.requests.contains { $0.contains("/api/accounts/settings") },
    "A wrong login must keep the saved connection and the live client, and read nothing")

  // Cancel while the dashboard is still answering the login: the call Cancel and Escape make.
  _ = fixture.takeSlowLogin()
  let slow = Task { @MainActor in await session.change(baseURL: dashboard, username: "fixture-slow", password: "fixture-new") }
  var waited = 0
  while !fixture.takeSlowLogin() && waited < 500 { try await Task.sleep(nanoseconds: 10_000_000); waited += 1 }
  let checking = session.isChecking
  session.cancelCheck()
  let cancelled = await slow.value
  try expect(waited < 500 && checking && cancelled == "Connection check cancelled. \(kept)" && unchanged(),
    "Cancel during the check must keep the saved connection and the live client")

  session.checkTimeout = 0.3
  let late = await session.change(baseURL: dashboard, username: "fixture-slow", password: "fixture-new")
  session.checkTimeout = 15
  try expect(late == "The dashboard took too long to answer. \(kept)" && unchanged(),
    "A timeout must keep the saved connection and the live client")

  let invalid = await session.change(baseURL: "\(dashboard)/account", username: "fixture", password: "fixture-new")
  try expect(invalid == ConnectionCheckError.invalidDetails.errorDescription && unchanged(),
    "Invalid details must never be checked or saved")

  let before = fixture.requests.count
  let ok = await session.change(baseURL: dashboard, username: "fixture", password: "fixture-new")
  let reloaded = try BarConnection.load(from: file)
  let keys = (try JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any]).map { Set($0.keys) }
  let filePermissions = (try FileManager.default.attributesOfItem(atPath: file.path)[.posixPermissions] as? NSNumber)?.intValue
  let folderPermissions = (try FileManager.default.attributesOfItem(atPath: file.deletingLastPathComponent().path)[.posixPermissions] as? NSNumber)?.intValue
  try expect(Array(fixture.requests.dropFirst(before)) == ["POST dashboard.invalid/api/auth/login 200", "GET dashboard.invalid/api/accounts/settings 200"],
    "Success must log in once, then read the settings with that session")
  let savedAfter = try Data(contentsOf: file)
  try expect(ok == nil && savedAfter != saved && reloaded.password == "fixture-new"
    && reloaded.baseURL.absoluteString == dashboard && session.client !== live && session.connection?.password == "fixture-new",
    "Success must replace the saved connection and the live client")
  try expect(keys == ["baseURL", "username", "password"] && filePermissions == 0o600 && folderPermissions == 0o700,
    "The verified connection must keep the file's JSON shape and private permissions")

  // Other members of the saved file survive a verified Change exactly; sign-in members never carry over, so a
  // password can never sit next to a key.
  let paired = directory.appendingPathComponent("paired/accounts-connection.json")
  try FileManager.default.createDirectory(at: paired.deletingLastPathComponent(), withIntermediateDirectories: true)
  try JSONSerialization.data(withJSONObject: ["baseURL": dashboard, "username": "fixture", "password": "fixture-old",
    "fixtureNote": "kept-exactly", "fixtureCount": 1] as [String: Any]).write(to: paired)
  try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: paired.path)
  let pairedSession = ConnectionSession(fileURL: paired, makeTransport: { SignInTransport(fixture) })
  try pairedSession.load()
  let pairedFailed = await pairedSession.change(baseURL: dashboard, username: "fixture", password: "wrong-password")
  let pairedOk = await pairedSession.change(baseURL: dashboard, username: "fixture", password: "fixture-new")
  let pairedObject = try JSONSerialization.jsonObject(with: Data(contentsOf: paired)) as? [String: Any]
  try expect(pairedFailed != nil && pairedOk == nil && pairedObject?["fixtureNote"] as? String == "kept-exactly"
    && (pairedObject?["fixtureCount"] as? NSNumber)?.intValue == 1 && pairedObject?["password"] as? String == "fixture-new",
    "A verified Change must keep the file's other members exactly")
  let keyed = BarConnection(baseURL: URL(string: dashboard)!, username: "fixture", deviceId: "dev_0123456789abcdef",
    deviceToken: "aacd_" + String(repeating: "A", count: 43), installId: UUID().uuidString, pairedAt: "2026-10-02T00:00:00Z")
  try ConnectionStore.write(keyed, to: paired)
  try ConnectionStore.save(BarConnection(baseURL: URL(string: dashboard)!, username: "fixture", password: "fixture-new"), to: paired)
  let overKey = try JSONSerialization.jsonObject(with: Data(contentsOf: paired)) as? [String: Any]
  try expect(overKey?["deviceToken"] == nil && overKey?["deviceId"] == nil && overKey?["version"] == nil
    && overKey?["fixtureNote"] as? String == "kept-exactly",
    "A password login saved over a paired file must drop the key")
  try ConnectionStore.write(keyed, to: paired)
  let overPassword = try JSONSerialization.jsonObject(with: Data(contentsOf: paired)) as? [String: Any]
  try expect(overPassword?["password"] == nil && overPassword?["deviceToken"] as? String == keyed.deviceToken,
    "A key saved over a password file must drop the password")

  // First run: nothing saved yet. A failure saves nothing and leaves no client; success saves and connects.
  let firstFile = directory.appendingPathComponent("first/accounts-connection.json")
  let first = ConnectionSession(fileURL: firstFile, makeTransport: { SignInTransport(fixture) })
  let firstRejected = await first.change(baseURL: dashboard, username: "fixture", password: "wrong-password")
  let firstRefused = await first.change(baseURL: "http://refused.invalid:3000", username: "fixture", password: "fixture-new")
  try expect(firstRejected == "The dashboard did not accept that username and password. Nothing was saved."
    && firstRefused == "Could not reach a dashboard at that address. Nothing was saved."
    && !FileManager.default.fileExists(atPath: firstFile.path) && first.client == nil && first.connection == nil,
    "A failed first sign-in must save nothing and leave no client")
  let firstOk = await first.change(baseURL: dashboard, username: "fixture", password: "fixture-new")
  try expect(firstOk == nil && (try? BarConnection.load(from: firstFile))?.password == "fixture-new" && first.client != nil,
    "A verified first sign-in must save the connection and connect")

  // The real URLSession transport against a loopback port nothing listens on.
  let loopback = ConnectionSession(fileURL: firstFile)
  try loopback.load()
  let loopbackSaved = try Data(contentsOf: firstFile)
  let loopbackClient = loopback.client
  let loopbackRefused = await loopback.change(baseURL: "http://127.0.0.1:\(closedLoopbackPort())", username: "fixture", password: "fixture-new")
  let loopbackAfter = try Data(contentsOf: firstFile)
  try expect(loopbackRefused == "Could not reach a dashboard at that address. \(kept)"
    && loopbackAfter == loopbackSaved && loopback.client === loopbackClient,
    "A refused loopback connection must keep the saved connection and the live client")
  try expect(ConnectionCheckError.login(status: 429) == .rateLimited && ConnectionCheckError.login(status: 302) == .notDashboard
    && ConnectionCheckError.login(status: 400) == .signInNotSetUp && ConnectionCheckError.login(status: 500) == .failed,
    "Login failures map to fixed public reasons")
}

// MARK: Claude Open progress (CONTRACT-serving-misc 4.4)

/// What one Open reported to its row, in order.
private final class OpenProgressLog: @unchecked Sendable {
  private let lock = NSLock()
  private var values: [ClaudeOpenProgress] = []
  func add(_ value: ClaudeOpenProgress) { lock.lock(); values.append(value); lock.unlock() }
  var texts: [String] { lock.lock(); defer { lock.unlock() }; return values.map(\.text) }
  var all: [ClaudeOpenProgress] { lock.lock(); defer { lock.unlock() }; return values }
}

/// A check's clock: the poll's sleep advances it, so a three-minute deadline costs no real time.
private final class OpenClock: @unchecked Sendable {
  private let lock = NSLock()
  private var current = Date(timeIntervalSince1970: 1_800_000_000)
  private var steps: [TimeInterval] = []
  func now() -> Date { lock.lock(); defer { lock.unlock() }; return current }
  func advance(_ interval: TimeInterval) {
    lock.lock(); current = current.addingTimeInterval(interval); steps.append(interval); lock.unlock()
  }
  var slept: [TimeInterval] { lock.lock(); defer { lock.unlock() }; return steps }
}

/// Holds one Open inside its poll until the check releases it, so a second Open can be attempted while it runs.
private final class OpenGate: @unchecked Sendable {
  private let lock = NSLock()
  private var arrivedFlag = false
  private var releasedFlag = false
  var arrived: Bool { lock.lock(); defer { lock.unlock() }; return arrivedFlag }
  var released: Bool { lock.lock(); defer { lock.unlock() }; return releasedFlag }
  func release() { lock.lock(); releasedFlag = true; lock.unlock() }
  /// The poll's sleep: the first call parks here until the check lets it go.
  func hold() async {
    markArrived()
    var waited = 0
    while !released && waited < 1000 { try? await Task.sleep(nanoseconds: 5_000_000); waited += 1 }
  }

  private func markArrived() { lock.lock(); arrivedFlag = true; lock.unlock() }
}

private func openOperationJSON(id: String = "op_fixture_0001", platform: String = "mac", state: String,
  confirmed: Int? = nil, total: Int? = nil, message: String? = nil) -> String {
  func number(_ value: Int?) -> String { value.map { "\($0)" } ?? "null" }
  return "{\"id\":\"\(id)\",\"platform\":\"\(platform)\",\"state\":\"\(state)\","
    + "\"confirmedCount\":\(number(confirmed)),\"totalCount\":\(number(total)),"
    + "\"message\":\(message.map { "\"\($0)\"" } ?? "null")}"
}

private func openProfileJSON(id: String = "gmail", operation: String? = nil) -> String {
  "{\"id\":\"\(id)\",\"email\":\"\(id)@example.invalid\",\"openOperation\":\(operation ?? "null")}"
}

/// One `GET /api/claude/desktop-profiles` answer. A profile without a manifest id carries no `openOperation`, exactly
/// as the server sends it.
private func openProfilesReply(_ profiles: String) -> MockReply {
  MockReply(200, Data("{\"profiles\":[\(profiles)]}".utf8))
}

private let openAcceptedReply = MockReply(202,
  Data("{\"id\":\"gmail\",\"platform\":\"mac\",\"state\":\"checking\",\"operationId\":\"op_fixture_0001\"}".utf8))
private let openOKReply = MockReply(200, Data("{\"opened\":true,\"id\":\"gmail\",\"platform\":\"mac\"}".utf8))

/// The production cadence with a check clock, so the three-minute deadline is exact and instant.
private let openPolling = ClaudeOpenPolling()

private func checkClaudeOpenProgress() async throws {
  let openPath = "/api/claude/desktop-profiles/gmail/open"
  let listPath = "/api/claude/desktop-profiles"

  // 1. Today's 200: one POST that opts into the async answer, no profile-list read, and the row ends on "Opened".
  do {
    let transport = MockTransport(replies: ["POST \(openPath)": [openOKReply]])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    let log = OpenProgressLog()
    let outcome = try await ClaudeOpenFlow.run(client: client, coordinator: ClaudeOpenCoordinator(),
      profile: "gmail", platform: "mac", polling: openPolling, sleep: { _ in }, progress: { log.add($0) })
    try expect(outcome?.opened == true && outcome?.finished == true && outcome?.text == "Opened",
      "A 200 Open must finish at once as opened")
    try expect(log.texts == ["Opening", "Opened"], "A 200 Open must show Opening then Opened on the row")
    let calls = await transport.recorded()
    let posts = calls.filter { $0.url.path == openPath }
    try expect(posts.count == 1 && posts[0].method == "POST", "A 200 Open must send exactly one POST")
    try expect(posts[0].headers["prefer"] == "respond-async",
      "The Open POST must opt into the progress answer with Prefer: respond-async")
    let body = try posts[0].jsonBody()
    try expect(Set(body.keys) == ["platform"] && body["platform"] as? String == "mac",
      "The Open POST body must select only the platform")
    try expect(!calls.contains { $0.url.path == listPath }, "A 200 Open must not read the profile list")
  }

  // 2. 202, polled to opened: the counts the server reports are the counts the row shows.
  do {
    let transport = MockTransport(replies: [
      "POST \(openPath)": [openAcceptedReply],
      "GET \(listPath)": [
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "checking"))),
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "copying", confirmed: 3, total: 18))),
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "copying", confirmed: 18, total: 18))),
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "opening"))),
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "opened"))),
      ],
    ])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    let log = OpenProgressLog()
    let clock = OpenClock()
    let outcome = try await ClaudeOpenFlow.run(client: client, coordinator: ClaudeOpenCoordinator(),
      profile: "gmail", platform: "mac", polling: openPolling, now: { clock.now() },
      sleep: { clock.advance($0) }, progress: { log.add($0) })
    try expect(log.texts == ["Opening", "Copying history", "Copying history 3 of 18",
      "Copying history 18 of 18", "Opening", "Opened"],
      "A polled Open must show the copy counts, then Opening, then Opened")
    try expect(outcome?.opened == true && outcome?.finished == true, "A polled Open must end opened")
    try expect(clock.slept == [1, 1, 1, 1, 1], "A short Open must be read about once a second")
    let calls = await transport.recorded()
    try expect(calls.filter { $0.url.path == openPath }.count == 1, "Polling must never repeat the Open POST")
    let reads = calls.filter { $0.url.path == listPath }
    try expect(reads.count == 5 && reads.allSatisfy { $0.method == "GET" && $0.url.query == nil && $0.body == nil },
      "Progress must come from a body-free GET of the profile list")
  }

  // 3. 202 to failed: the server's own fixed sentence reaches the row, and the Open did not open Claude.
  do {
    let transport = MockTransport(replies: [
      "POST \(openPath)": [openAcceptedReply],
      "GET \(listPath)": [
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "copying", confirmed: 2, total: 9))),
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "failed",
          message: "Claude desktop request timed out."))),
      ],
    ])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    let log = OpenProgressLog()
    let outcome = try await ClaudeOpenFlow.run(client: client, coordinator: ClaudeOpenCoordinator(),
      profile: "gmail", platform: "mac", polling: openPolling, sleep: { _ in }, progress: { log.add($0) })
    try expect(outcome?.text == "Claude desktop request timed out." && outcome?.finished == true
      && outcome?.opened == false, "A failed Open must show the server's message and must not read as opened")
    try expect(log.texts.last == "Claude desktop request timed out.", "A failed Open must end the row's progress")
    let posts = await transport.recorded()
    try expect(posts.filter { $0.url.path == openPath }.count == 1,
      "A failed Open must not be retried")
  }

  // 4. 202 to blocked_uncertain with no usable message: the fixed client sentence, never a server string.
  do {
    let transport = MockTransport(replies: [
      "POST \(openPath)": [openAcceptedReply],
      "GET \(listPath)": [
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "blocked_uncertain"))),
      ],
    ])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    let log = OpenProgressLog()
    let outcome = try await ClaudeOpenFlow.run(client: client, coordinator: ClaudeOpenCoordinator(),
      profile: "gmail", platform: "mac", polling: openPolling, sleep: { _ in }, progress: { log.add($0) })
    try expect(outcome?.text == ClaudeOpenFlow.historyUnconfirmed && outcome?.opened == false,
      "A blocked_uncertain Open with no message must say the history copy could not be confirmed")
    // A long or multiline "message" is not a fixed sentence, so the client's own replaces it.
    try expect(ClaudeOpenFlow.publicMessage(String(repeating: "x", count: 301)) == ClaudeOpenFlow.historyUnconfirmed
      && ClaudeOpenFlow.publicMessage("line\nFIXTURE_ONLY_PRIVATE") == ClaudeOpenFlow.historyUnconfirmed
      && ClaudeOpenFlow.publicMessage("") == ClaudeOpenFlow.historyUnconfirmed
      && ClaudeOpenFlow.publicMessage(nil) == ClaudeOpenFlow.historyUnconfirmed
      && ClaudeOpenFlow.publicMessage("Claude history copy is unconfirmed.") == "Claude history copy is unconfirmed.",
      "Only a bounded single-line server sentence may reach the row")
    try expect(log.all.last?.finished == true, "A blocked_uncertain Open must end the poll")
  }

  // 5. 409 history_unconfirmed: the fixed sentence, one POST, and no poll.
  do {
    let transport = MockTransport(replies: ["POST \(openPath)": [MockReply(409,
      Data("{\"error\":\"FIXTURE_ONLY_PRIVATE canary\",\"code\":\"history_unconfirmed\"}".utf8))]])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    let log = OpenProgressLog()
    var thrown: String?
    do {
      _ = try await ClaudeOpenFlow.run(client: client, coordinator: ClaudeOpenCoordinator(),
        profile: "gmail", platform: "mac", polling: openPolling, sleep: { _ in }, progress: { log.add($0) })
      throw CheckFailure(description: "A 409 history_unconfirmed Open must not succeed")
    } catch let error as BarClientError {
      guard case .status(409, let text) = error else {
        throw CheckFailure(description: "A 409 history_unconfirmed Open must keep its status")
      }
      thrown = text
    }
    try expect(thrown == ClaudeOpenFlow.historyUnconfirmed,
      "A 409 history_unconfirmed Open must say the history copy could not be confirmed")
    try expect(!(thrown ?? "").contains("FIXTURE_ONLY"), "A server error string must never reach the row")
    let calls = await transport.recorded()
    try expect(calls.filter { $0.url.path == openPath }.count == 1 && !calls.contains { $0.url.path == listPath },
      "A refused Open must send one POST and read no profile list")
  }

  // 5b. An expired session on the Open POST is reported, never re-sent: an Open is never repeated.
  do {
    let transport = MockTransport(replies: ["POST \(openPath)": [MockReply(401), openAcceptedReply]])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    var status = 0
    do {
      _ = try await ClaudeOpenFlow.run(client: client, coordinator: ClaudeOpenCoordinator(),
        profile: "gmail", platform: "mac", polling: openPolling, sleep: { _ in }, progress: { _ in })
      throw CheckFailure(description: "An Open refused with 401 must not succeed")
    } catch let error as BarClientError {
      guard case .status(let actual, _) = error else {
        throw CheckFailure(description: "An Open refused with 401 must keep its status")
      }
      status = actual
    }
    let calls = await transport.recorded()
    try expect(status == 401 && calls.filter { $0.url.path == openPath }.count == 1,
      "An expired session must never re-send the Open POST")
  }

  // 6. No terminal state: the poll gives up after three minutes, on the production cadence, without a second POST.
  do {
    let checking = openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "checking")))
    let transport = MockTransport(replies: [
      "POST \(openPath)": [openAcceptedReply],
      "GET \(listPath)": Array(repeating: checking, count: 140),
    ])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    let log = OpenProgressLog()
    let clock = OpenClock()
    let outcome = try await ClaudeOpenFlow.run(client: client, coordinator: ClaudeOpenCoordinator(),
      profile: "gmail", platform: "mac", polling: openPolling, now: { clock.now() },
      sleep: { clock.advance($0) }, progress: { log.add($0) })
    try expect(outcome?.text == ClaudeOpenFlow.stillWorking && outcome?.finished == true && outcome?.opened == false,
      "A poll that never ends must say the dashboard is still working on it")
    let slept = clock.slept
    try expect(slept.count == 132 && slept.prefix(120).allSatisfy { $0 == 1 } && slept.suffix(12).allSatisfy { $0 == 5 }
      && slept.reduce(0, +) == 180,
      "The poll must read every second for two minutes, then every five seconds, and stop at three minutes")
    try expect(log.texts == ["Opening", "Copying history", ClaudeOpenFlow.stillWorking],
      "A stuck Open must keep the last text it was given, then say the dashboard is still working on it")
    let posts = await transport.recorded()
    try expect(posts.filter { $0.url.path == openPath }.count == 1,
      "Giving up must never repeat the Open POST")
  }

  // 7. A second Open for the same account while one runs sends nothing at all, on either platform button.
  do {
    let transport = MockTransport(replies: [
      "POST \(openPath)": [openAcceptedReply],
      "GET \(listPath)": [
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "copying", confirmed: 1, total: 4))),
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "opened"))),
      ],
    ])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    let coordinator = ClaudeOpenCoordinator()
    let gate = OpenGate()
    let log = OpenProgressLog()
    let first = Task {
      try await ClaudeOpenFlow.run(client: client, coordinator: coordinator, profile: "gmail", platform: "mac",
        polling: openPolling, sleep: { _ in await gate.hold() }, progress: { log.add($0) })
    }
    var waited = 0
    while !gate.arrived && waited < 1000 { try await Task.sleep(nanoseconds: 5_000_000); waited += 1 }
    try expect(waited < 1000, "The first Open must reach its poll before the second is attempted")
    let refused = try await ClaudeOpenFlow.run(client: client, coordinator: coordinator, profile: "gmail",
      platform: "windows", polling: openPolling, sleep: { _ in }, progress: { _ in })
    let running = await coordinator.isRunning("gmail")
    gate.release()
    let outcome = try await first.value
    let stillRunning = await coordinator.isRunning("gmail")
    try expect(refused == nil && running, "A second Open for the same account must be refused while the first runs")
    try expect(outcome?.opened == true && !stillRunning,
      "The account must be free again once its Open ends")
    let calls = await transport.recorded()
    try expect(calls.filter { $0.url.path == openPath }.count == 1,
      "A refused second Open must send no POST at all")
    try expect(calls.filter { $0.url.path == openPath }
      .allSatisfy { ((try? $0.jsonBody()) ?? [:])["platform"] as? String == "mac" },
      "The only Open POST must be the first one's")
  }

  // 8. The poll reads only this Open: another profile's operation, another platform's, or another operation id is
  //    ignored, and an unknown state adds no text.
  do {
    let transport = MockTransport(replies: [
      "POST \(openPath)": [openAcceptedReply],
      "GET \(listPath)": [
        openProfilesReply(openProfileJSON(id: "party",
          operation: openOperationJSON(state: "opened"))),
        openProfilesReply(openProfileJSON(operation: openOperationJSON(platform: "windows", state: "opened"))),
        openProfilesReply(openProfileJSON(operation: openOperationJSON(id: "op_other", state: "opened"))),
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "fixture_unknown_state"))),
        openProfilesReply(openProfileJSON(operation: nil)),
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "opened"))),
      ],
    ])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    let log = OpenProgressLog()
    let outcome = try await ClaudeOpenFlow.run(client: client, coordinator: ClaudeOpenCoordinator(),
      profile: "gmail", platform: "mac", polling: openPolling, sleep: { _ in }, progress: { log.add($0) })
    try expect(outcome?.opened == true && log.texts == ["Opening", "Opened"],
      "Only this profile's, this platform's and this operation's state may end the poll")
    let reads = await transport.recorded()
    try expect(reads.filter { $0.url.path == listPath }.count == 6,
      "An answer that does not match must leave the poll running")
  }

  // 9. A read that fails never ends the Open: the server keeps the operation and the POST is never replayed.
  do {
    let transport = MockTransport(replies: [
      "POST \(openPath)": [openAcceptedReply],
      "GET \(listPath)": [MockReply(500), MockReply(200, Data("{\"notProfiles\":true}".utf8)),
        openProfilesReply(openProfileJSON(operation: openOperationJSON(state: "opened")))],
    ])
    let client = AccountsClient(connection: testConnection(), transport: transport)
    let log = OpenProgressLog()
    let outcome = try await ClaudeOpenFlow.run(client: client, coordinator: ClaudeOpenCoordinator(),
      profile: "gmail", platform: "mac", polling: openPolling, sleep: { _ in }, progress: { log.add($0) })
    try expect(outcome?.opened == true && log.texts == ["Opening", "Opened"],
      "A failed or unreadable profile-list read must be retried, never answered with an invented state")
  }

  // 10. The row's text forms, including a copy whose counts are not known yet.
  func operation(_ state: String, confirmed: Int? = nil, total: Int? = nil, message: String? = nil) throws -> ClaudeOpenOperation {
    try JSONDecoder().decode(ClaudeOpenOperation.self,
      from: Data(openOperationJSON(state: state, confirmed: confirmed, total: total, message: message).utf8))
  }
  let checking = try operation("checking")
  let copying = try operation("copying")
  let copyingCounted = try operation("copying", confirmed: 3, total: 18)
  let copyingZero = try operation("copying", confirmed: 0, total: 18)
  let copyingPartial = try operation("copying", confirmed: 3)
  let opening = try operation("opening")
  let opened = try operation("opened")
  let failed = try operation("failed", message: "Claude account could not be opened safely.")
  let blocked = try operation("blocked_uncertain")
  let unknown = try operation("fixture_unknown_state")
  try expect(ClaudeOpenFlow.text(for: checking) == "Copying history"
    && ClaudeOpenFlow.text(for: copying) == "Copying history"
    && ClaudeOpenFlow.text(for: copyingCounted) == "Copying history 3 of 18"
    && ClaudeOpenFlow.text(for: copyingZero) == "Copying history 0 of 18"
    && ClaudeOpenFlow.text(for: copyingPartial) == "Copying history"
    && ClaudeOpenFlow.text(for: opening) == "Opening"
    && ClaudeOpenFlow.text(for: opened) == "Opened"
    && ClaudeOpenFlow.text(for: failed) == "Claude account could not be opened safely."
    && ClaudeOpenFlow.text(for: blocked) == ClaudeOpenFlow.historyUnconfirmed
    && ClaudeOpenFlow.text(for: unknown) == nil,
    "The row's text must come only from the reported state and counts")
  try expect(opened.isTerminal && failed.isTerminal && blocked.isTerminal && !checking.isTerminal
    && !copying.isTerminal && !opening.isTerminal && !unknown.isTerminal,
    "Only opened, failed and blocked_uncertain end the poll")
  try expect(opened.isOpened && !failed.isOpened && !blocked.isOpened,
    "Only the opened state may read as an Open that opened Claude")
}

// MARK: F6: a reading from before its window's reset is not shown

private func checkResetPending() throws {
  let iso = ISO8601DateFormatter()
  let now = iso.date(from: "2026-10-02T12:00:00Z")!
  let past = "2026-10-02T11:00:00Z", future = "2026-10-02T15:00:00Z"
  let reset = iso.date(from: past)!
  func window(_ resetAt: String, sampled: String? = nil, extra: String = "") throws -> AccountQuotaWindow {
    let sample = sampled.map { ",\"sampledAt\":\"\($0)\"" } ?? ""
    let json = "{\"key\":\"five_hour\",\"label\":\"Five-hour usage\",\"usedPercent\":37,\"kind\":\"rate_limit\",\"windowMinutes\":300,\"resetAt\":\"\(resetAt)\"\(sample)\(extra)}"
    return try JSONDecoder().decode(AccountQuotaWindow.self, from: Data(json.utf8))
  }
  let older = "2026-10-02T10:00:00Z", newer = "2026-10-02T11:30:00Z"
  let passed = try window(past), ahead = try window(future)
  let ownNewer = try window(past, sampled: "2026-10-02T11:45:00Z"), ownOlder = try window(past, sampled: "2026-10-02T10:30:00Z")
  let unlimited = try window(past, extra: ",\"unlimited\":true"), disabled = try window(past, extra: ",\"enabled\":false")
  try expect(TrayReset.pending(passed, accountSampledAt: older, now: now) == reset,
    "A past reset with an older sample is pending")
  try expect(TrayReset.pending(passed, accountSampledAt: newer, now: now) == nil,
    "A past reset with a newer sample shows the reading")
  try expect(TrayReset.pending(ahead, accountSampledAt: older, now: now) == nil
    && TrayReset.pending(ahead, accountSampledAt: nil, now: now) == nil,
    "A future reset shows the reading")
  try expect(TrayReset.pending(passed, accountSampledAt: nil, now: now) == reset
    && TrayReset.pending(passed, accountSampledAt: "not-a-time", now: now) == reset,
    "A past reset with no known sample time is pending")
  try expect(TrayReset.pending(ownNewer, accountSampledAt: older, now: now) == nil
    && TrayReset.pending(ownOlder, accountSampledAt: newer, now: now) == reset,
    "A window's own sample time comes before the account's")
  let balance = try JSONDecoder().decode(AccountQuotaWindow.self, from: Data(
    "{\"key\":\"credits\",\"label\":\"Credits\",\"kind\":\"balance\",\"remaining\":4,\"resetAt\":\"\(past)\"}".utf8))
  try expect(TrayReset.pending(unlimited, accountSampledAt: older, now: now) == nil
    && TrayReset.pending(disabled, accountSampledAt: older, now: now) == nil
    && TrayReset.pending(balance, accountSampledAt: older, now: now) == nil,
    "Amounts, unlimited and disabled windows are left alone")
  let forms = TrayReset.forms(reset, now: now)
  try expect(forms[0] == "\(TrayReset.at(reset, now: now)) · new reading pending" && forms[0].hasPrefix("Reset at ")
    && forms.allSatisfy { !$0.contains("%") } && TrayReset.long(reset).hasSuffix(" · new reading pending"),
    "Pending text names the reset and shows no number")

  // The menu bar never shows a reading from before its reset; the timer's key set flips when a reset passes.
  func dashboard(sampledAt: String, resetAt: String) throws -> AccountDashboard {
    let json = """
    {"schemaVersion":1,"updatedAt":"\(older)","accounts":[{"id":"codex:a","provider":"codex","providerLabel":"Codex","label":"a",
      "email":"codex-2@example.com","plan":"pro","platform":"ubuntu","source":"Fixture","status":"cached","message":null,
      "fetchedAt":"\(sampledAt)","sampledAt":"\(sampledAt)","isActive":true,
      "windows":[{"key":"seven_day","label":"Weekly","usedPercent":9.25,"windowMinutes":10080,"resetAt":"\(resetAt)"}],
      "capabilities":{"codexProfile":"a","claudePlatforms":[]}}],
     "codexAutoSwitch":{"enabled":true,"thresholdPercent":5,"pollIntervalSeconds":60,"outcome":"healthy","message":"",
      "activationInProgress":false,"lastCheckedAt":null,"lastSwitchedAt":null}}
    """
    return try JSONDecoder().decode(AccountDashboard.self, from: Data(json.utf8))
  }
  let saved = TrayFormat.referenceNow
  defer { TrayFormat.referenceNow = saved }
  TrayFormat.referenceNow = now
  let pending = try dashboard(sampledAt: older, resetAt: past)
  let fresh = try dashboard(sampledAt: newer, resetAt: past)
  try expect(MenuBarReading.make(dashboard: pending, provider: "codex", mode: .remaining) == nil
    && MenuBarReading.make(dashboard: fresh, provider: "codex", mode: .remaining)?.text == "\(TrayFormat.number(90.75))%",
    "The menu bar hides a weekly reading from before its reset and shows a newer one")
  let soon = try dashboard(sampledAt: older, resetAt: "2026-10-02T12:00:30Z")
  try expect(soon.pendingResetKeys(now: now).isEmpty && soon.pendingResetKeys(now: now.addingTimeInterval(60)) == ["codex:a|seven_day"],
    "A window flips to pending when its reset passes while the panel is open")
}

private func checkMenuBarSelection() throws {
  func window(_ key: String, _ label: String, _ values: [String: Any] = [:]) -> [String: Any] {
    var result: [String: Any] = [
      "key": key, "label": label, "usedPercent": NSNull(), "remainingPercent": NSNull(), "resetAt": NSNull(),
      "windowMinutes": NSNull(), "used": NSNull(), "limit": NSNull(), "unit": NSNull(),
    ]
    for (name, value) in values { result[name] = value }
    return result
  }
  let original = try JSONSerialization.jsonObject(with: dashboardJSON) as! [String: Any]
  let prototype = (original["accounts"] as! [[String: Any]])[0]
  func account(_ id: String, _ provider: String, email: String, windows: [[String: Any]], active: Bool = false) -> [String: Any] {
    var value = prototype
    value["id"] = id
    value["provider"] = provider
    value["email"] = email
    value["windows"] = windows
    value["isActive"] = active
    return value
  }
  func dashboard(codexActive: String, agyActive: String) throws -> AccountDashboard {
    var object = original
    object["accounts"] = [
      account("codex-a", "codex", email: "codex-a@example.invalid", windows: [
        window("five_hour", "5h", ["usedPercent": 40]), window("seven_day", "week", ["usedPercent": 10]),
      ], active: codexActive == "codex-a"),
      account("codex-b", "codex", email: "codex-b@example.invalid", windows: [
        window("five_hour", "5h", ["usedPercent": 70]), window("seven_day", "week", ["usedPercent": 20]),
      ], active: codexActive == "codex-b"),
      account("claude-a", "claude", email: "claude-a@example.invalid", windows: [
        window("five_hour", "Five-hour usage", ["usedPercent": 12]),
        window("seven_day", "Weekly usage", ["usedPercent": 34]),
      ]),
      account("claude-b", "claude", email: "claude-b@example.invalid", windows: [
        window("seven_day", "Weekly usage", ["usedPercent": 78]),
      ]),
      account("agy-a", "antigravity", email: "agy-a@example.invalid", windows: [
        window("gemini-5h", "Gemini Models · 5-hour", ["usedPercent": 5, "windowMinutes": 300]),
      ], active: agyActive == "agy-a"),
      account("agy-b", "antigravity", email: "agy-b@example.invalid", windows: [
        window("gemini-5h", "Gemini Models · 5-hour", ["usedPercent": 60, "windowMinutes": 300]),
      ], active: agyActive == "agy-b"),
      account("cursor-a", "cursor", email: "cursor-a@example.invalid", windows: [
        window("seven_day", "Weekly", ["usedPercent": 25, "windowMinutes": 10080]),
      ]),
      account("cursor-b", "cursor", email: "cursor-b@example.invalid", windows: [
        window("seven_day", "Weekly", ["usedPercent": 50, "windowMinutes": 10080]),
      ]),
      account("qwen-a", "qwen", email: "qwen-a@example.invalid", windows: [
        window("five_hour", "Five-hour", ["usedPercent": 60, "windowMinutes": 300]),
      ]),
      account("zai-a", "zai", email: "zai-a@example.invalid", windows: [
        window("five_hour", "Five-hour", ["usedPercent": 120, "windowMinutes": 300]),
      ]),
      account("opencode-a", "opencode-go", email: "opencode-a@example.invalid", windows: [
        window("monthly", "Monthly", ["usedPercent": 27, "windowMinutes": 43200]),
      ]),
    ]
    return try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: object))
  }
  let first = try dashboard(codexActive: "codex-a", agyActive: "agy-a")
  // Codex and Antigravity follow the active account; other providers use the single or first account.
  try expect(MenuBarReading.make(dashboard: first, provider: "codex", mode: .used)?.value == 40
    && MenuBarReading.make(dashboard: first, provider: "codex", mode: .used)?.detail == "Codex · codex-a@example.invalid · 5-hour used",
    "Codex must show the active account's 5-hour window and name it in the tooltip")
  let flipped = try dashboard(codexActive: "codex-b", agyActive: "agy-b")
  try expect(MenuBarReading.make(dashboard: flipped, provider: "codex", mode: .used)?.value == 70
    && MenuBarReading.make(dashboard: flipped, provider: "antigravity", mode: .used)?.value == 60,
    "The reading must follow the newly active Codex and Antigravity accounts")
  try expect(MenuBarReading.make(dashboard: first, provider: "antigravity", mode: .used)?.value == 5,
    "Antigravity must show its active account")
  try expect(MenuBarReading.make(dashboard: first, provider: "cursor", mode: .used)?.value == 25,
    "A provider with several accounts shows the first one")
  try expect(MenuBarReading.make(dashboard: first, provider: "qwen", mode: .used)?.value == 60,
    "A provider with one account shows it")
  // The 5-hour window wins when reported, else the weekly one; Claude shows the picked account.
  try expect(MenuBarReading.make(dashboard: first, provider: "claude", mode: .used, claudeAccountID: "claude-a")?.value == 12
    && MenuBarReading.make(dashboard: first, provider: "claude", mode: .used, claudeAccountID: "claude-b")?.value == 78
    && MenuBarReading.make(dashboard: first, provider: "claude", mode: .used, claudeAccountID: "claude-b")?.detail
    == "Claude · claude-b@example.invalid · Weekly used",
    "Claude shows the picked account: its 5-hour window, else its weekly one")
  // Two accounts sharing one local-part: the tag must name the full identity, the same
  // string the Settings picker shows, so it names the account actually shown.
  var collisionObject = original
  collisionObject["accounts"] = [
    account("claude-x", "claude", email: "jared@platyr.invalid", windows: [
      window("five_hour", "Five-hour usage", ["usedPercent": 12]),
    ]),
    account("claude-y", "claude", email: "jared@party.invalid", windows: [
      window("five_hour", "Five-hour usage", ["usedPercent": 34]),
    ]),
  ]
  let collision = try JSONDecoder().decode(AccountDashboard.self,
    from: JSONSerialization.data(withJSONObject: collisionObject))
  try expect(MenuBarReading.make(dashboard: collision, provider: "claude", mode: .used, claudeAccountID: "claude-y")?.detail
    == "Claude · jared@party.invalid · 5-hour used"
    && MenuBarReading.make(dashboard: collision, provider: "claude", mode: .used, claudeAccountID: "claude-x")?.detail
    == "Claude · jared@platyr.invalid · 5-hour used",
    "When two accounts shorten to one name, the tag must name the full identity")
  // Settings > Menu bar hover tags: Show and Claude account describe their picker; Value
  // names the full identity of the account actually shown, the same string the Show preview uses.
  try expect(MenuBarReading.showHelp == "Choose which provider's usage number appears in the menu bar."
    && MenuBarReading.claudeAccountHelp == "Choose which Claude account the menu bar number comes from.",
    "The Show and Claude account hover tags must describe their picker")
  try expect(MenuBarReading.valueHelp(dashboard: first, provider: "codex", mode: .used)
    == "Whether the menu bar shows Used or Remaining for codex-a@example.invalid.",
    "The Value hover tag must name the Codex account actually shown")
  try expect(MenuBarReading.valueHelp(dashboard: collision, provider: "claude", mode: .used, claudeAccountID: "claude-y")
    == "Whether the menu bar shows Used or Remaining for jared@party.invalid.",
    "The Value hover tag must name the full identity when two accounts shorten alike")
  // T1: the Show preview names the FULL identity even when no two accounts shorten
  // alike, and the Value hover tag names exactly that same account.
  guard let codexShown = MenuBarReading.make(dashboard: first, provider: "codex", mode: .used) else {
    throw CheckFailure(description: "The Codex fixture must produce a menu-bar reading")
  }
  try expect(codexShown.accountName == "codex-a@example.invalid"
    && codexShown.detail == "Codex · codex-a@example.invalid · 5-hour used",
    "The Show preview must name the full account identity, not the short local-part")
  try expect(MenuBarReading.valueHelp(dashboard: first, provider: "codex", mode: .used)
    == "Whether the menu bar shows Used or Remaining for \(codexShown.accountName).",
    "The Value hover tag must name exactly the account the Show preview names")
  guard let claudeShown = MenuBarReading.make(dashboard: first, provider: "claude", mode: .used,
    claudeAccountID: "claude-b") else {
    throw CheckFailure(description: "The picked Claude fixture must produce a menu-bar reading")
  }
  try expect(MenuBarReading.valueHelp(dashboard: first, provider: "claude", mode: .used, claudeAccountID: "claude-b")
    == "Whether the menu bar shows Used or Remaining for \(claudeShown.accountName).",
    "The Value hover tag must name the picked Claude account exactly as the Show preview does")
  // Fix 2: the real-world Codex shape — 3 profiles in registry order with short labels
  // and full emails, the first one (gmail) active — built through the same projection
  // the server uses (label = profile name, email = profile email, isActive = live login).
  func realCodex(_ id: String, _ label: String, _ email: String, active: Bool) -> [String: Any] {
    var value = account(id, "codex", email: email, windows: [
      window("five_hour", "5h", ["usedPercent": 40]),
      window("seven_day", "week", ["usedPercent": 10]),
    ], active: active)
    value["label"] = label
    return value
  }
  func realShape(activeCodex: String) throws -> AccountDashboard {
    var object = original
    object["accounts"] = [
      realCodex("codex:gmail", "gmail", "gmail-user@example.invalid", active: activeCodex == "codex:gmail"),
      realCodex("codex:party", "party", "party-user@example.invalid", active: activeCodex == "codex:party"),
      realCodex("codex:gio", "gio", "gio-user@example.invalid", active: activeCodex == "codex:gio"),
    ]
    return try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: object))
  }
  let live = try realShape(activeCodex: "codex:gmail")
  guard let liveReading = MenuBarReading.make(dashboard: live, provider: "codex", mode: .used) else {
    throw CheckFailure(description: "The real-shaped fixture must produce a menu-bar reading")
  }
  try expect(liveReading.accountName == "gmail-user@example.invalid"
    && liveReading.detail == "Codex · gmail-user@example.invalid · 5-hour used",
    "Show must display the full email of the ACTIVE Codex account, not a short label")
  try expect(MenuBarReading.valueHelp(dashboard: live, provider: "codex", mode: .used)
    == "Whether the menu bar shows Used or Remaining for \(liveReading.accountName).",
    "Value must name exactly the active account Show displays (gmail, not party)")
  for (activeID, email) in [("codex:party", "party-user@example.invalid"), ("codex:gio", "gio-user@example.invalid")] {
    let switched = try realShape(activeCodex: activeID)
    guard let switchedReading = MenuBarReading.make(dashboard: switched, provider: "codex", mode: .used) else {
      throw CheckFailure(description: "The switched fixture must produce a menu-bar reading")
    }
    try expect(switchedReading.accountName == email
      && switchedReading.detail.contains(email)
      && MenuBarReading.valueHelp(dashboard: switched, provider: "codex", mode: .used)
      == "Whether the menu bar shows Used or Remaining for \(email).",
      "Show and Value must follow the newly active Codex account together (\(email))")
  }
  // FW4: the Show hover tag says what the picker does and names the same account Show
  // displays and Value names (the ACTIVE Codex account, never the first or another row).
  try expect(MenuBarReading.showHelp(dashboard: live, provider: "codex", mode: .used)
    == "Choose which provider's usage number appears in the menu bar. Now showing gmail-user@example.invalid."
    && MenuBarReading.showHelp(for: liveReading) == MenuBarReading.showHelp(dashboard: live, provider: "codex", mode: .used)
    && MenuBarReading.valueHelp(for: liveReading) == MenuBarReading.valueHelp(dashboard: live, provider: "codex", mode: .used),
    "The Show hover tag must name the active Codex account Show displays, in full")
  for (activeID, email) in [("codex:party", "party-user@example.invalid"), ("codex:gio", "gio-user@example.invalid")] {
    let switched = try realShape(activeCodex: activeID)
    let show = MenuBarReading.showHelp(dashboard: switched, provider: "codex", mode: .remaining)
    let value = MenuBarReading.valueHelp(dashboard: switched, provider: "codex", mode: .remaining)
    try expect(show.hasSuffix("Now showing \(email).") && value.hasSuffix("for \(email).")
      && !show.contains("gmail-user") && !value.contains("gmail-user"),
      "The Show and Value hover tags must follow the newly active Codex account together (\(email))")
  }
  try expect(MenuBarReading.showHelp(dashboard: first, provider: "claude", mode: .used, claudeAccountID: "claude-b")
    == "Choose which provider's usage number appears in the menu bar. Now showing claude-b@example.invalid.",
    "The Show hover tag must name the picked Claude account")
  try expect(MenuBarReading.showHelp(dashboard: first, provider: MenuBarReading.nothingProvider, mode: .used)
    == "Choose which provider's usage number appears in the menu bar. Now showing the logo only."
    && MenuBarReading.showHelp(for: nil) == MenuBarReading.showHelp(dashboard: nil, provider: "codex", mode: .used)
    && MenuBarReading.valueHelp(for: nil) == MenuBarReading.valueHelpHidden,
    "With no reading shown, the Show hover tag says the logo shows alone")
  try expect(MenuBarReading.valueHelp(dashboard: first, provider: MenuBarReading.nothingProvider, mode: .used)
    == MenuBarReading.valueHelpHidden
    && MenuBarReading.valueHelp(dashboard: nil, provider: "codex", mode: .used) == MenuBarReading.valueHelpHidden
    && MenuBarReading.valueHelpHidden == "Whether the menu bar would show Used or Remaining.",
    "With no reading shown, the Value hover tag stays generic")
  try expect(MenuBarReading.make(dashboard: first, provider: "claude", mode: .used)?.value == 12,
    "With no Claude account picked, the first Claude account shows")
  try expect(MenuBarReading.make(dashboard: first, provider: "claude", mode: .used, claudeAccountID: "gone")?.value == 12,
    "A picked Claude account that is no longer reported falls back to the first one")
  // Used versus remaining arithmetic, with remaining floored at zero past 100%.
  try expect(MenuBarReading.make(dashboard: first, provider: "codex", mode: .remaining)?.value == 60
    && MenuBarReading.make(dashboard: first, provider: "codex", mode: .remaining)?.text == "60%",
    "Remaining must read 100 minus used")
  try expect(MenuBarReading.make(dashboard: first, provider: "zai", mode: .used)?.value == 120
    && MenuBarReading.make(dashboard: first, provider: "zai", mode: .remaining)?.value == 0,
    "Past 100%, used shows the real value and remaining floors at zero")
  // Nothing, unknown providers, missing windows and missing dashboards show the icon alone.
  try expect(MenuBarReading.make(dashboard: first, provider: MenuBarReading.nothingProvider, mode: .used) == nil
    && MenuBarReading.make(dashboard: first, provider: "nope", mode: .used) == nil
    && MenuBarReading.make(dashboard: first, provider: "opencode-go", mode: .used) == nil
    && MenuBarReading.make(dashboard: nil, provider: "codex", mode: .used) == nil,
    "Nothing, unknown providers, no 5-hour or weekly window and no dashboard show no number")

  // A reset-pending or unavailable 5-hour window hides the number: no 0%, no weekly fallback.
  let saved = TrayFormat.referenceNow
  defer { TrayFormat.referenceNow = saved }
  TrayFormat.referenceNow = ISO8601DateFormatter().date(from: "2026-10-02T12:00:00Z")!
  var pendingObject = original
  pendingObject["accounts"] = [
    account("codex-p", "codex", email: "codex-p@example.invalid", windows: [
      window("five_hour", "5h", ["usedPercent": 40, "resetAt": "2026-10-02T11:00:00Z"]),
      window("seven_day", "week", ["usedPercent": 10, "resetAt": "2026-10-09T11:00:00Z"]),
    ], active: true),
  ]
  let pendingDashboard = try JSONDecoder().decode(AccountDashboard.self,
    from: JSONSerialization.data(withJSONObject: pendingObject))
  try expect(MenuBarReading.make(dashboard: pendingDashboard, provider: "codex", mode: .used) == nil,
    "A reset-pending 5-hour window shows the icon alone, never 0% or the weekly fallback")
  var unavailableObject = original
  unavailableObject["accounts"] = [
    account("codex-u", "codex", email: "codex-u@example.invalid", windows: [
      window("five_hour", "5h", ["resetAt": "2026-10-09T11:00:00Z"]),
    ], active: true),
  ]
  let unavailableDashboard = try JSONDecoder().decode(AccountDashboard.self,
    from: JSONSerialization.data(withJSONObject: unavailableObject))
  try expect(MenuBarReading.make(dashboard: unavailableDashboard, provider: "codex", mode: .used) == nil,
    "A 5-hour window with no reading shows the icon alone, never 0%")
}

private func checkStatusItemToggle() throws {
  let button = CGRect(x: 100, y: 900, width: 40, height: 22)
  // A left or right press on our own button is not an outside click: the button action toggles.
  try expect(!StatusItemClick.isOutsideClick(at: CGPoint(x: 120, y: 911), buttonFrame: button, buttonActs: true),
    "A press on the status-item button must not dismiss the panel")
  try expect(!StatusItemClick.isOutsideClick(at: CGPoint(x: 100.5, y: 900.5), buttonFrame: button, buttonActs: true)
    && !StatusItemClick.isOutsideClick(at: CGPoint(x: 139.5, y: 921.5), buttonFrame: button, buttonActs: true),
    "Presses just inside the button's edges must not dismiss the panel")
  // Anything off the button dismisses, as does a press the button ignores (another mouse button).
  try expect(StatusItemClick.isOutsideClick(at: CGPoint(x: 99.9, y: 911), buttonFrame: button, buttonActs: true)
    && StatusItemClick.isOutsideClick(at: CGPoint(x: 140.1, y: 911), buttonFrame: button, buttonActs: true)
    && StatusItemClick.isOutsideClick(at: CGPoint(x: 120, y: 500), buttonFrame: button, buttonActs: true)
    && StatusItemClick.isOutsideClick(at: CGPoint(x: 120, y: 911), buttonFrame: button, buttonActs: false),
    "Presses off the button, or that the button ignores, must still dismiss the panel")
}

private func printJSON(_ object: [String: Any]) throws {
  let data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
  print(String(decoding: data, as: UTF8.self))
}

private func checkLive() async {
  do {
    let connection = try BarConnection.load()
    let client = AccountsClient(connection: connection)
    let dashboard = try await client.dashboard(refresh: false)
    let passed = dashboard.schemaVersion == 1
    let statusCounts = Dictionary(grouping: dashboard.accounts, by: \.status).mapValues { $0.count }
    try printJSON([
      "passed": passed,
      "schemaVersion": dashboard.schemaVersion,
      "accountCount": dashboard.accounts.count,
      "providers": Array(Set(dashboard.accounts.map(\.provider))).sorted(),
      "statusCounts": statusCounts,
      "codexAutomaticEnabled": dashboard.codexAutoSwitch.enabled,
    ])
    if !passed { exit(1) }
  } catch {
    try? printJSON(["passed": false, "error": safeLiveError(error)])
    exit(1)
  }
}


// MARK: - Pairing, device keys and sign-out (CONTRACT-auth-devices sections 2a, 5 to 9; TSIGN-C local HTTP)

/// A fake dashboard's sign-in side: the owner switch, the password, the paired devices and the request log.
private final class FakeDashboard: @unchecked Sendable {
  struct Device { var id: String; var token: String; var installId: String?; var previous: String?; var revoked: String? }
  private let lock = NSLock()
  var username = "owner"
  var password = "fixture-pass-1"
  var accessMode = "login"
  var trustLocalNetwork = true
  var peerTrusted = true
  var peer = "192.168.50.23"
  var supportsPairing = true
  var setupCode = "K7QF2MXD"
  var failures = 0
  var rotateAfter = "2099-01-01T00:00:00Z"
  var revokedFields: [String: String] = [:]
  var meFails = false
  var refuseRotate = false
  /// The pair answer waits this long after the key was issued (Cancel while the answer is on its way).
  var pairDelay: Double = 0
  /// DELETE devices/me gets no answer.
  var deleteFails = false
  private(set) var devices: [Device] = []
  private(set) var log: [String] = []
  private(set) var authorizations: [String] = []
  private var counter = 0

  func with<T>(_ body: (FakeDashboard) -> T) -> T { lock.lock(); defer { lock.unlock() }; return body(self) }
  func record(_ line: String, auth: String?) { lock.lock(); log.append(line); authorizations.append(auth ?? ""); lock.unlock() }
  var requests: [String] { lock.lock(); defer { lock.unlock() }; return log }
  var auths: [String] { lock.lock(); defer { lock.unlock() }; return authorizations }
  var deviceList: [Device] { lock.lock(); defer { lock.unlock() }; return devices }

  func newToken() -> String {
    counter += 1
    let seed = String(format: "%043d", counter)
    return "aacd_" + seed.replacingOccurrences(of: "0", with: "A")
  }
  func pair(installId: String?) -> Device {
    lock.lock(); defer { lock.unlock() }
    for index in devices.indices where devices[index].installId != nil && devices[index].installId == installId && devices[index].revoked == nil {
      devices[index].revoked = "replaced"
    }
    counter += 1
    let device = Device(id: String(format: "dev_%016x", counter), token: "aacd_" + String(format: "%043d", counter).replacingOccurrences(of: "0", with: "B"),
      installId: installId, previous: nil, revoked: nil)
    devices.append(device)
    return device
  }
  /// The device a bearer token names, and whether it still works.
  func device(for token: String) -> (index: Int, code: String?)? {
    lock.lock(); defer { lock.unlock() }
    guard let index = devices.firstIndex(where: { $0.token == token || $0.previous == token }) else { return nil }
    let device = devices[index]
    if let reason = device.revoked { return (index, reason == "expired" ? "device_expired" : "device_revoked") }
    if device.previous == token { return (index, nil) }
    devices[index].previous = nil
    return (index, nil)
  }
  func revoke(_ index: Int, _ reason: String) { lock.lock(); devices[index].revoked = reason; lock.unlock() }
  func rotate(_ index: Int) -> String {
    lock.lock(); defer { lock.unlock() }
    counter += 1
    let token = "aacd_" + String(format: "%043d", counter).replacingOccurrences(of: "0", with: "C")
    devices[index].previous = devices[index].token
    devices[index].token = token
    return token
  }
}

private actor FakeDashboardTransport: BarHTTPTransport {
  let fake: FakeDashboard
  private var cookie = false
  init(_ fake: FakeDashboard) { self.fake = fake }

  func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
    let url = request.url!
    let host = url.host ?? ""
    let method = request.httpMethod ?? "GET"
    let auth = request.value(forHTTPHeaderField: "Authorization")
    func reply(_ status: Int, _ object: Any = [String: Any](), headers: [String: String]? = nil) -> (Data, HTTPURLResponse) {
      fake.record("\(method) \(host)\(url.path) \(status)", auth: auth)
      let data = (try? JSONSerialization.data(withJSONObject: object)) ?? Data("{}".utf8)
      return (data, HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!)
    }
    if host.hasPrefix("refused.") || host == "192.168.77.77" { throw URLError(.cannotConnectToHost) }
    if host.hasPrefix("ats.") { throw URLError(.appTransportSecurityRequiresSecureConnection) }
    if host.hasPrefix("slow.") { try await Task.sleep(nanoseconds: 30_000_000_000) }
    if host.hasPrefix("elsewhere.") {
      fake.record("\(method) \(host)\(url.path) 404", auth: auth)
      return (Data("<html>Not found</html>".utf8), HTTPURLResponse(url: url, statusCode: 404, httpVersion: "HTTP/1.1", headerFields: nil)!)
    }
    let body = (try? JSONSerialization.jsonObject(with: request.httpBody ?? Data())) as? [String: Any] ?? [:]
    let secure = fake.with { $0.trustLocalNetwork && $0.peerTrusted }
    let connection: [String: Any] = fake.with { ["peer": $0.peer, "trusted": $0.trustLocalNetwork && $0.peerTrusted] }
    switch (method, url.path) {
    case ("GET", "/api/auth/check"):
      return reply(200, fake.with { ["accessMode": $0.accessMode, "authenticated": false, "username": NSNull(),
        "setupCodeRequired": $0.accessMode == "setup", "secureTransport": false, "trustedLocalNetwork": $0.trustLocalNetwork,
        "connection": connection] as [String: Any] })
    case ("GET", "/api/auth/setup"):
      return reply(200, fake.with { ["enabled": true, "configured": $0.accessMode != "setup", "setupCodeRequired": $0.accessMode == "setup",
        "secureTransport": false, "trustedLocalNetwork": $0.trustLocalNetwork, "connection": connection] as [String: Any] })
    case ("POST", "/api/auth/setup"):
      guard fake.with({ $0.accessMode == "setup" }) else { return reply(409, ["code": "already_configured"]) }
      guard secure else { return reply(403, ["code": "secure_transport_required"]) }
      guard let code = body["setupCode"] as? String else { return reply(403, ["code": "setup_code_required"]) }
      guard code.filter({ $0.isLetter || $0.isNumber }).uppercased() == fake.with({ $0.setupCode }) else {
        return reply(403, ["code": "setup_code_invalid", "triesLeft": 4])
      }
      if let password = body["password"] as? String, password.count < 8 { return reply(400, ["code": "weak_password", "reason": "too_short"]) }
      fake.with { $0.accessMode = "login"; $0.username = body["username"] as? String ?? ""; $0.password = body["password"] as? String ?? "" }
      return reply(201, ["ok": true])
    case ("POST", "/api/auth/devices/pair"):
      guard fake.with({ $0.supportsPairing }) else { return reply(404, ["error": "Not found"]) }
      guard secure else { return reply(403, ["code": "secure_transport_required"]) }
      if fake.with({ $0.failures >= 5 }) { return reply(429, ["code": "rate_limited", "retryAfterSeconds": 600], headers: ["Retry-After": "600"]) }
      guard Set(body.keys).isSubset(of: ["username", "password", "deviceName", "platform", "installId", "appVersion"]),
        body["platform"] as? String == "mac" else { return reply(400, ["code": "invalid_body"]) }
      guard body["username"] as? String == fake.with({ $0.username }), body["password"] as? String == fake.with({ $0.password }) else {
        let left = fake.with { dashboard -> Int in dashboard.failures += 1; return 5 - dashboard.failures }
        return reply(401, ["code": "invalid_credentials", "triesLeft": left])
      }
      let device = fake.pair(installId: body["installId"] as? String)
      let delay = fake.with { $0.pairDelay }
      if delay > 0 { try await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000)) }
      return reply(201, ["deviceId": device.id, "token": device.token, "name": body["deviceName"] as? String ?? "",
        "platform": "mac", "pairedAt": "2026-10-02T15:00:00.000Z", "rotateAfter": fake.with { $0.rotateAfter }])
    case ("POST", "/api/auth/login"):
      guard body["username"] as? String == fake.with({ $0.username }), body["password"] as? String == fake.with({ $0.password })
      else { return reply(401, ["code": "invalid_credentials", "triesLeft": 4]) }
      cookie = true
      return reply(200, ["success": true])
    case ("GET", "/api/accounts/settings"):
      return cookie && auth == nil ? reply(200, ["refreshIntervalSeconds": 60]) : reply(401, ["code": "auth_required"])
    default: break
    }
    // Bearer routes.
    guard let auth, auth.hasPrefix("Bearer ") else {
      if url.path == "/api/accounts/dashboard" && cookie { return (dashboardJSON, HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: nil)!) }
      return reply(401, ["code": "auth_required"])
    }
    let token = String(auth.dropFirst(7))
    guard let found = fake.device(for: token) else { return reply(401, ["code": "invalid_token"]) }
    if let code = found.code {
      var object: [String: Any] = ["error": "fixed", "code": code]
      for (key, value) in fake.with({ $0.revokedFields }) { object[key] = value }
      return reply(401, object)
    }
    switch (method, url.path) {
    case ("GET", "/api/auth/devices/me"):
      if fake.with({ $0.meFails }) { throw URLError(.timedOut) }
      return reply(200, ["id": fake.deviceList[found.index].id, "name": "Mac", "platform": "mac", "pairedAt": "2026-10-02T15:00:00.000Z",
        "rotateAfter": fake.with { $0.rotateAfter }, "idleExpiresAt": "2026-12-31T15:00:00.000Z"])
    case ("POST", "/api/auth/devices/me/rotate"):
      if fake.with({ $0.refuseRotate }) || !secure { return reply(403, ["code": "secure_transport_required"]) }
      let next = fake.rotate(found.index)
      fake.with { $0.rotateAfter = "2099-01-01T00:00:00Z" }
      return reply(200, ["token": next, "rotateAfter": "2099-01-01T00:00:00Z"])
    case ("DELETE", "/api/auth/devices/me"):
      if fake.with({ $0.deleteFails }) { throw URLError(.timedOut) }
      fake.revoke(found.index, "self")
      fake.record("\(method) \(host)\(url.path) 204", auth: auth)
      return (Data(), HTTPURLResponse(url: url, statusCode: 204, httpVersion: "HTTP/1.1", headerFields: nil)!)
    case ("GET", "/api/accounts/dashboard"):
      fake.record("\(method) \(host)\(url.path) 200", auth: auth)
      return (dashboardJSON, HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: nil)!)
    case ("GET", "/api/claude/desktop-profiles"):
      return reply(200, ["profiles": [[String: Any]]()])
    case ("POST", _), ("PUT", _):
      return reply(200, [String: Any]())
    default:
      return reply(403, ["code": "device_scope"])
    }
  }
}

private func pairingDirectory(_ name: String) throws -> URL {
  let directory = FileManager.default.temporaryDirectory.appendingPathComponent("aac-pairing-check-\(name)-\(UUID().uuidString)")
  let real = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".ccs").standardizedFileURL.path
  try expect(!directory.standardizedFileURL.path.hasPrefix(real), "Pairing checks must never use ~/.ccs")
  return directory
}

private func permissions(_ url: URL) -> Int? {
  (try? FileManager.default.attributesOfItem(atPath: url.path)[.posixPermissions] as? NSNumber)?.intValue
}

private func jsonKeys(_ url: URL) -> Set<String> {
  ((try? JSONSerialization.jsonObject(with: Data(contentsOf: url))) as? [String: Any]).map { Set($0.keys) } ?? []
}

private let sampleToken = "aacd_" + String(repeating: "Z", count: 43)

/// Version 1, version 2 and signed-out files: exactly one shape, private, and never a password next to a key.
private func checkConnectionVersions() async throws {
  let directory = try pairingDirectory("versions")
  defer { try? FileManager.default.removeItem(at: directory) }
  let file = directory.appendingPathComponent("bar/accounts-connection.json")
  let base = URL(string: "http://192.168.50.10:3000")!
  let install = UUID().uuidString
  let v2 = BarConnection(baseURL: base, username: "owner", deviceId: "dev_00000000000000aa", deviceToken: sampleToken,
    installId: install, pairedAt: "2026-10-02T15:00:00Z")
  try ConnectionStore.write(v2, to: file)
  let loaded = try BarConnection.load(from: file)
  try expect(loaded.isPaired && !loaded.hasPassword && loaded.version == 2 && loaded.installId == install,
    "A version 2 file must load as paired, with no password")
  try expect(jsonKeys(file) == ["version", "baseURL", "username", "deviceId", "deviceToken", "installId", "pairedAt"]
    && permissions(file) == 0o600 && permissions(file.deletingLastPathComponent()) == 0o700,
    "Version 2 must hold exactly the contract's members in a 0600 file in a 0700 folder")
  let leftovers = try FileManager.default.contentsOfDirectory(atPath: file.deletingLastPathComponent().path).filter { $0.hasSuffix(".tmp") }
  try expect(leftovers.isEmpty, "The private writer must leave no temporary file")
  let out = v2.signingOut(SignedOutNote(reason: "device_revoked", at: "2026-10-02T16:00:00Z"))
  try ConnectionStore.write(out, to: file)
  let signedOut = try BarConnection.load(from: file)
  try expect(signedOut.isSignedOut && signedOut.deviceToken == nil && signedOut.deviceId == nil
    && signedOut.username == "owner" && signedOut.baseURL == base && signedOut.installId == install
    && signedOut.signedOut?.reason == "device_revoked" && !jsonKeys(file).contains("password"),
    "A signed-out file keeps the address, username and install id, and no key")
  let bad: [[String: Any]] = [
    ["version": 2, "baseURL": base.absoluteString, "username": "owner", "password": "pw", "deviceId": "dev_00000000000000aa", "deviceToken": sampleToken],
    ["version": 2, "baseURL": base.absoluteString, "username": "owner", "deviceToken": sampleToken],
    ["version": 2, "baseURL": base.absoluteString, "username": "owner", "deviceId": "dev_00000000000000aa", "deviceToken": "aacd_short"],
    ["baseURL": base.absoluteString, "username": "owner", "password": "pw", "deviceToken": sampleToken, "deviceId": "dev_00000000000000aa"],
    ["version": 2, "baseURL": base.absoluteString, "username": "owner"],
    ["version": 2, "baseURL": base.absoluteString, "username": "owner", "deviceId": "dev_00000000000000aa", "deviceToken": sampleToken,
     "installId": "not-a-uuid"],
    ["version": 3, "baseURL": base.absoluteString, "username": "owner", "deviceId": "dev_00000000000000aa", "deviceToken": sampleToken],
    ["version": 2, "baseURL": "http://192.168.50.10:3000/path", "username": "owner", "deviceId": "dev_00000000000000aa", "deviceToken": sampleToken],
  ]
  for object in bad {
    try ConnectionStore.writePrivately(JSONSerialization.data(withJSONObject: object), to: file)
    try await expectError(.invalidConnection, "Mixed or malformed connection shapes must be refused") {
      _ = try BarConnection.load(from: file)
    }
  }
  try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: file.path)
  try await expectError(.privateConfigRequired, "A version 2 file readable by others must be refused") {
    _ = try BarConnection.load(from: file)
  }
  // The state folder override is honoured (the isolated end-to-end run uses it) and defaults to ~/.ccs/bar.
  try expect(BarConnection.configURL.lastPathComponent == "accounts-connection.json"
    && BarConnection.rollbackURL().lastPathComponent == "accounts-connection.v1-rollback.json",
    "The connection and its rollback copy keep the contract's file names")
}

/// The bearer client: the key on every request, never a login, and a 401 device code as a sign-out.
private func checkBearerClient() async throws {
  let fake = FakeDashboard()
  let transport = FakeDashboardTransport(fake)
  let device = fake.pair(installId: UUID().uuidString)
  let base = URL(string: "http://192.168.50.10:3000")!
  let connection = BarConnection(baseURL: base, username: "owner", deviceId: device.id, deviceToken: device.token,
    installId: UUID().uuidString, pairedAt: "2026-10-02T15:00:00Z")
  let client = AccountsClient(connection: connection, transport: transport)
  _ = try await client.dashboard()
  try await client.activateCodex(profile: "work")
  _ = try await client.setAutomaticSwitching(enabled: true)
  try await client.openClaude(profile: "alpha", platform: "windows")
  _ = try await client.claudeDesktopProfiles()
  _ = try await client.setAntigravityAutomaticSwitching(enabled: false)
  let me = try await client.deviceSelf()
  try expect(me.id == device.id && me.rotateAfter != nil, "devices/me must read this tray's own record")
  let requests = fake.requests, auths = fake.auths
  try expect(!requests.contains { $0.contains("/api/auth/login") }, "A paired tray never logs in with a password")
  try expect(auths.count == requests.count && auths.allSatisfy { $0 == "Bearer \(device.token)" },
    "Every request of a paired tray must carry its device key as Authorization: Bearer")

  for (code, expected) in [("device_revoked", "device_revoked"), ("device_expired", "device_expired")] {
    fake.revoke(fake.deviceList.firstIndex { $0.id == device.id }!, code == "device_expired" ? "expired" : "dashboard")
    do {
      _ = try await client.dashboard()
      throw CheckFailure(description: "A revoked key must not read the dashboard")
    } catch BarClientError.signedOut(let note) {
      try expect(note.reason == expected && AccountFormatting.date(note.at) != nil && note.revokedBy == nil,
        "A 401 \(code) must become a sign-out note with the time it was noticed")
    }
  }
  fake.with { $0.revokedFields = ["revokedAt": "2026-10-02T14:41:00.000Z", "revokedReason": "revoke-all", "revokedBy": "owner"] }
  do {
    _ = try await client.dashboard()
  } catch BarClientError.signedOut(let note) {
    try expect(note.revokedBy == "owner" && note.revokedReason == "revoke-all" && note.at == "2026-10-02T14:41:00.000Z",
      "Who and when are kept when the dashboard sends them")
  }
  fake.with { $0.revokedFields = ["revokedAt": "yesterday", "revokedReason": "Revoke All\n", "revokedBy": "x\nInjected"] }
  do {
    _ = try await client.dashboard()
  } catch BarClientError.signedOut(let note) {
    try expect(note.revokedBy == nil && note.revokedReason == nil && note.at != "yesterday",
      "Malformed who and when fields from the dashboard are dropped, never shown")
  }
  let stranger = AccountsClient(connection: BarConnection(baseURL: base, username: "owner", deviceId: "dev_00000000000000ff",
    deviceToken: sampleToken, installId: UUID().uuidString, pairedAt: "2026-10-02T15:00:00Z"), transport: transport)
  do {
    _ = try await stranger.dashboard()
    throw CheckFailure(description: "An unknown key must not read the dashboard")
  } catch BarClientError.signedOut(let note) {
    try expect(note.reason == "invalid_token", "An unknown key is invalid_token")
  }
  // 503 auth_store_unavailable is not a sign-out.
  let unavailable = MockTransport(replies: ["GET /api/accounts/dashboard": [MockReply(503, Data("{\"code\":\"auth_store_unavailable\"}".utf8))]])
  let busy = AccountsClient(connection: connection, transport: unavailable)
  do {
    _ = try await busy.dashboard()
    throw CheckFailure(description: "503 must fail")
  } catch BarClientError.status(let status, let message) {
    try expect(status == 503 && message == "The dashboard cannot check paired trays right now. Try again shortly.",
      "auth_store_unavailable keeps the key and says to try again")
  }
  // A key rotated while a request was in flight is retried once with the saved new key.
  let rotating = MockTransport(replies: ["GET /api/accounts/dashboard": [
    MockReply(401, Data("{\"code\":\"invalid_token\"}".utf8), delayNanoseconds: 80_000_000), MockReply(200, dashboardJSON)]])
  let racing = AccountsClient(connection: connection, transport: rotating)
  async let read = racing.dashboard()
  try await Task.sleep(nanoseconds: 20_000_000)
  let next = "aacd_" + String(repeating: "N", count: 43)
  await racing.adopt(token: next)
  _ = try await read
  let raced = await rotating.recorded()
  try expect(raced.count == 2 && raced[0].headers["authorization"] == "Bearer \(device.token)"
    && raced[1].headers["authorization"] == "Bearer \(next)",
    "A request that crossed a rotation goes once more with the new key instead of signing out")
}

/// The tray's own local-network check (nothing is sent to an outside address over plain HTTP).
private func checkLocalNetwork() throws {
  let local = ["10.0.0.1", "10.255.255.254", "172.16.0.1", "172.31.255.254", "192.168.0.1", "192.168.50.179", "127.0.0.1",
    "::1", "fc00::1", "fd12:3456::9", "::ffff:192.168.50.20", "::ffff:c0a8:3214", "[fd00::5]", "fe80::1%en0x"]
  let outside = ["8.8.8.8", "172.15.255.255", "172.32.0.1", "192.169.0.1", "11.0.0.1", "100.64.1.2", "169.254.3.4",
    "203.0.113.5", "2001:db8::1", "fe80::1", "::", "0.0.0.0", "::ffff:8.8.8.8", "fbff::1", "fe00::1"]
  for address in local where address != "fe80::1%en0x" {
    try expect(LocalNetwork.isLocal(address: address), "\(address) is on the local network")
  }
  for address in outside {
    try expect(!LocalNetwork.isLocal(address: address), "\(address) is not on the local network")
  }
  try expect(LocalNetwork.verdict(host: "192.168.50.179", resolver: { _ in ["8.8.8.8"] }) == .local,
    "A literal address is judged as written, never by a lookup")
  try expect(LocalNetwork.verdict(host: "dashboard.local", resolver: { _ in ["192.168.50.179", "fd00::5"] }) == .local,
    "A name that resolves only to local addresses is local")
  try expect(LocalNetwork.verdict(host: "home.example.net", resolver: { _ in ["192.168.50.179", "203.0.113.9"] }) == .outside("203.0.113.9"),
    "A name with any outside address is refused")
  try expect(LocalNetwork.verdict(host: "nowhere.invalid", resolver: { _ in [] }) == .unresolved,
    "A name that does not resolve is left to the reachability check")
  for (raw, expected) in [("192.168.50.179:3000", "http://192.168.50.179:3000"), (" http://dash.local:3000/ ", "http://dash.local:3000"),
    ("https://10.0.0.5", "https://10.0.0.5"), ("[fd00::5]:3000", "http://[fd00::5]:3000")] {
    try expect(DashboardProbe.normalize(raw)?.absoluteString == expected, "\(raw) must normalize to \(expected)")
  }
  for raw in ["", "ftp://host", "http://user:pw@host", "http://host/path", "http://host?x=1", "http://host#f", "http://", "javascript:alert(1)"] {
    try expect(DashboardProbe.normalize(raw) == nil, "\(raw) is not a dashboard address")
  }
  // App Transport Security with local networking allowed: numbers, .local and one-word names only (measured on the Mac).
  for host in ["192.168.50.10", "10.6.0.9", "[fd00::5]", "fe80::1%en0", "localhost", "dashboard", "dash.local", "Dash.Local."] {
    try expect(LocalNetwork.plainHTTPReaches(host: host), "\(host) is reachable over plain HTTP from the packaged app")
  }
  for host in ["box.home.arpa", "dash.lan", "192.168.50.179.nip.io", "vpn.example.net", "dash.local.example.net"] {
    try expect(!LocalNetwork.plainHTTPReaches(host: host), "\(host) is refused over plain HTTP by the Mac")
  }
  try expect(ConnectionCheckError.reason(URLError(.appTransportSecurityRequiresSecureConnection), cancelled: false) == .insecureAddress
    && ConnectionCheckError.insecureAddress.errorDescription == SignInCopy.useNumericAddress,
    "The Mac's own HTTP refusal is its own message, never 'could not reach'")
}

/// States 1, 2, 4, 5 and 8 from the address check, with nothing saved.
@MainActor private func checkAddressStates() async throws {
  let directory = try pairingDirectory("address")
  defer { try? FileManager.default.removeItem(at: directory) }
  let fake = FakeDashboard()
  let session = ConnectionSession(fileURL: directory.appendingPathComponent("accounts-connection.json"),
    makeTransport: { FakeDashboardTransport(fake) })
  session.resolver = { host in
    host == "home.example.net" ? ["203.0.113.9"] : ["dash.local", "box.home.arpa"].contains(host) ? ["192.168.50.179"] : []
  }
  let before = fake.requests.count
  for outside in ["home.example.net:3000", "203.0.113.5:3000", "100.64.1.2:3000", "169.254.3.4:3000", "[2001:db8::1]:3000"] {
    let result = await session.checkAddress(outside)
    guard case .notLocal(_, let seen) = result, seen == nil else { throw CheckFailure(description: "\(outside) must be refused by the tray itself (state 4)") }
  }
  try expect(fake.requests.count == before, "Nothing at all is sent to an outside address")
  let answer1 = await session.checkAddress("dash.local:3000")
  try expect(answer1 == .ready(URL(string: "http://dash.local:3000")!),
    "A local name with the switch on goes to the password step")
  fake.with { $0.trustLocalNetwork = false; $0.peerTrusted = false }
  let answer2 = await session.checkAddress("192.168.50.10:3000")
  try expect(answer2 == .pairingOff(URL(string: "http://192.168.50.10:3000")!),
    "Trust this local network off gives state 5, not 'not local'")
  fake.with { $0.trustLocalNetwork = true; $0.peerTrusted = false; $0.peer = "100.70.1.4" }
  let answer3 = await session.checkAddress("192.168.50.10:3000")
  try expect(answer3 == .notLocal(URL(string: "http://192.168.50.10:3000")!, seenAs: "100.70.1.4"),
    "The dashboard refusing this connection gives state 4 with the address it saw")
  fake.with { $0.peerTrusted = true; $0.accessMode = "setup" }
  let answer4 = await session.checkAddress("192.168.50.10:3000")
  try expect(answer4 == .setup(URL(string: "http://192.168.50.10:3000")!, codeRequired: true),
    "A dashboard with no sign-in yet gives state 2 with the setup code")
  fake.with { $0.accessMode = "open" }
  let answer5 = await session.checkAddress("192.168.50.10:3000")
  try expect(answer5 == .signInOff(URL(string: "http://192.168.50.10:3000")!),
    "A dashboard with sign-in off has nothing to pair with")
  fake.with { $0.accessMode = "login" }
  let answer6 = await session.checkAddress("refused.local:3000")
  try expect(answer6 == .unreachable(URL(string: "http://refused.local:3000")!),
    "No answer gives state 8 unreachable")
  let answer7 = await session.checkAddress("elsewhere.local:8080")
  try expect(answer7 == .notDashboard(URL(string: "http://elsewhere.local:8080")!),
    "An answer that is not the dashboard gives state 8 wrong address")
  // A local name the Mac will not reach over plain HTTP: refused with its own message before anything is sent.
  let sentBeforeName = fake.requests.count
  let named = await session.checkAddress("box.home.arpa:3000")
  try expect(named == .insecureName(URL(string: "http://box.home.arpa:3000")!) && fake.requests.count == sentBeforeName,
    "A dotted name other than .local is refused before anything is sent: use the numeric address")
  let single = await session.checkAddress("dashboard:3000")
  try expect(single == .ready(URL(string: "http://dashboard:3000")!), "A one-word name goes to the password step")
  let blocked = await session.checkAddress("ats.local:3000")
  try expect(blocked == .insecureName(URL(string: "http://ats.local:3000")!),
    "The Mac's own refusal (App Transport Security) gets the numeric-address message, not 'unreachable'")
  let answer8 = await session.checkAddress("http://x/y")
  try expect(answer8 == .invalid, "An address with a path is refused before anything is sent")
  session.addressTimeout = 0.3
  let started = Date()
  let slow = await session.checkAddress("slow.local:3000")
  try expect(slow == .unreachable(URL(string: "http://slow.local:3000")!) && Date().timeIntervalSince(started) < 3,
    "A dashboard that never answers ends at the time limit")
  // An older dashboard without the new fields: let pairing answer.
  let older = DashboardProbe(baseURL: URL(string: "http://192.168.50.10:3000")!)
  let olderCheck = try JSONDecoder().decode(AuthCheck.self, from: Data("{\"accessMode\":\"login\",\"authenticated\":false}".utf8))
  try expect(older.classify(olderCheck, setup: nil) == .ready(URL(string: "http://192.168.50.10:3000")!),
    "An older dashboard without the trust fields goes to the password step")
  try expect(!FileManager.default.fileExists(atPath: session.fileURL.path) && session.client == nil,
    "The address step saves nothing")
}

/// States 3, 6, 7 and 11: pair, prove the key, then save; every refusal keeps what was saved.
@MainActor private func checkPairingFlow() async throws {
  let directory = try pairingDirectory("pair")
  defer { try? FileManager.default.removeItem(at: directory) }
  let fake = FakeDashboard()
  let file = directory.appendingPathComponent("bar/accounts-connection.json")
  let session = ConnectionSession(fileURL: file, makeTransport: { FakeDashboardTransport(fake) })
  session.deviceName = "Fixture Mac"
  session.appVersion = "2.0.0"
  let url = URL(string: "http://192.168.50.10:3000")!

  guard case .wrongPassword(let tries) = await session.pair(url: url, username: "owner", password: "wrong-one") else {
    throw CheckFailure(description: "A wrong password must be state 6")
  }
  try expect(tries == 4 && !FileManager.default.fileExists(atPath: file.path), "Tries left come from the dashboard; nothing is saved")

  fake.with { $0.trustLocalNetwork = false; $0.peerTrusted = false }
  guard case .refused(.pairingOff) = await session.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "A 403 with the switch off must lead to state 5")
  }
  fake.with { $0.trustLocalNetwork = true; $0.peerTrusted = false }
  guard case .refused(.notLocal(_, "192.168.50.23")) = await session.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "A 403 with the switch on must lead to state 4 with the peer")
  }
  fake.with { $0.peerTrusted = true }

  // Verify before saving: the dashboard issues a key but never confirms it.
  fake.with { $0.meFails = true }
  guard case .failed(let notConfirmed, _) = await session.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "A key that is never confirmed must not be saved")
  }
  fake.with { $0.meFails = false }
  try expect(notConfirmed == SignInCopy.notConfirmed && !FileManager.default.fileExists(atPath: file.path) && session.client == nil,
    "Nothing is saved until devices/me answers 200")

  let mark = fake.requests.count
  guard case .paired(let connection, let me) = await session.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "A right password must pair")
  }
  let log = Array(fake.requests.dropFirst(mark)), auths = Array(fake.auths.dropFirst(mark))
  try expect(log == ["POST 192.168.50.10/api/auth/devices/pair 201", "GET 192.168.50.10/api/auth/devices/me 200"]
    && auths[0].isEmpty && auths[1] == "Bearer \(connection.deviceToken ?? "")",
    "Pairing sends the password once without a key, then proves the key with devices/me")
  let saved = try BarConnection.load(from: file)
  let text = try String(contentsOf: file, encoding: .utf8)
  try expect(saved.isPaired && saved.deviceToken == connection.deviceToken && me.id == connection.deviceId
    && !text.contains("fixture-pass-1") && !jsonKeys(file).contains("password") && permissions(file) == 0o600,
    "The saved key is version 2 at 0600, and the password is nowhere in it")
  let liveUsesKey = await session.client?.usesDeviceKey == true
  try expect(liveUsesKey && session.device?.id == connection.deviceId, "The live client now uses the key")

  func active() -> [String] { fake.deviceList.filter { $0.revoked == nil }.map(\.token) }
  func state(_ token: String?) -> String? { fake.deviceList.first { $0.token == token }?.revoked }
  try expect(fake.deviceList.filter { $0.installId == connection.installId }.count == 2 && active() == [connection.deviceToken ?? ""],
    "The unconfirmed attempt and the saved one share one install id, and the unconfirmed key was revoked")

  // Re-pair pairs under a new install id, so the working key stays valid until the new one is saved; then the old
  // key is revoked with itself (DELETE devices/me) and only the new key stays active.
  let oldToken = connection.deviceToken
  guard case .paired(let again, _) = await session.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "Re-pair must pair")
  }
  await session.finishRetiring()
  try expect(again.installId != connection.installId && again.deviceToken != oldToken && state(oldToken) == "self"
    && !session.isRetiringKey && active() == [again.deviceToken ?? ""],
    "Re-pair uses a new install id, saves the new key, then revokes the old key with itself")

  // A failed Change keeps the working key.
  let before = try Data(contentsOf: file)
  _ = await session.pair(url: URL(string: "http://refused.invalid:3000")!, username: "owner", password: "fixture-pass-1")
  _ = await session.pair(url: url, username: "owner", password: "wrong-again")
  try expect((try? Data(contentsOf: file)) == before && session.connection?.deviceToken == again.deviceToken,
    "A failed re-pair or change keeps the saved key and the live client")

  // Re-pair whose new key never gets a devices/me answer after the 201: the working key is still valid and still
  // saved, and the unconfirmed key is revoked rather than left active.
  fake.with { $0.meFails = true }
  guard case .failed(let unconfirmed, _) = await session.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "An unconfirmed Re-pair must fail")
  }
  fake.with { $0.meFails = false }
  let unconfirmedKey = fake.deviceList.last
  let stillReads = try await session.client?.deviceSelf()
  try expect(unconfirmed == SignInCopy.notConfirmed && (try? Data(contentsOf: file)) == before
    && session.connection?.deviceToken == again.deviceToken && unconfirmedKey?.token != again.deviceToken
    && unconfirmedKey?.revoked == "self" && active() == [again.deviceToken ?? ""] && stillReads?.id == again.deviceId,
    "A Re-pair with no devices/me answer keeps the working key valid and revokes the unconfirmed one")

  // Its key cannot even be revoked (no answer either): the next attempt reuses the install id, so the dashboard
  // replaces the stray record instead of keeping it toward the 20-tray limit.
  fake.with { $0.meFails = true; $0.deleteFails = true }
  _ = await session.pair(url: url, username: "owner", password: "fixture-pass-1")
  fake.with { $0.meFails = false; $0.deleteFails = false }
  let stray = fake.deviceList.last
  try expect(stray?.revoked == nil && stray?.installId == unconfirmedKey?.installId && session.connection?.deviceToken == again.deviceToken,
    "A key that could not be revoked stays active for now, under the same install id")
  guard case .paired(let fourth, _) = await session.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "Re-pair must pair after a stray key")
  }
  await session.finishRetiring()
  try expect(fourth.installId == stray?.installId && state(stray?.token) == "replaced" && state(again.deviceToken) == "self"
    && active() == [fourth.deviceToken ?? ""],
    "One install id per sign-in session: the retry replaces the stray key and only the saved key stays active")

  // Re-pair whose new key cannot be saved: no file changes, the working key stays valid, the new key is revoked.
  let folder = file.deletingLastPathComponent()
  let beforeSave = try Data(contentsOf: file)
  try FileManager.default.setAttributes([.immutable: true], ofItemAtPath: folder.path)
  let unsavedOutcome = await session.pair(url: url, username: "owner", password: "fixture-pass-1")
  try FileManager.default.setAttributes([.immutable: false], ofItemAtPath: folder.path)
  guard case .failed(let unsavedText, _) = unsavedOutcome else { throw CheckFailure(description: "An unsaved Re-pair must fail") }
  let unsaved = fake.deviceList.last
  try expect(unsavedText == SignInCopy.saveFailed && (try? Data(contentsOf: file)) == beforeSave
    && session.connection?.deviceToken == fourth.deviceToken && unsaved?.token != fourth.deviceToken
    && unsaved?.revoked == "self" && active() == [fourth.deviceToken ?? ""],
    "A Re-pair that cannot save keeps the file and the working key, and revokes the new key")

  // Cancel while the pair call is on its way back: nothing is saved, the issued key is revoked, the working key stays,
  // and the next attempt uses a new install id so the late answer can never replace it.
  fake.with { $0.pairDelay = 0.4 }
  let issuedBefore = fake.deviceList.count
  let running = Task { @MainActor in await session.pair(url: url, username: "owner", password: "fixture-pass-1") }
  var spins = 0
  while fake.deviceList.count == issuedBefore && spins < 400 { try await Task.sleep(nanoseconds: 5_000_000); spins += 1 }
  let inFlight = session.isPairing
  session.cancelCheck()
  let cancelled = await running.value
  fake.with { $0.pairDelay = 0 }
  let dropped = fake.deviceList.last
  guard case .cancelled = cancelled else { throw CheckFailure(description: "Cancel during the pair call must cancel it") }
  try expect(inFlight && (try? Data(contentsOf: file)) == beforeSave && session.connection?.deviceToken == fourth.deviceToken
    && dropped?.token != fourth.deviceToken && dropped?.revoked == "self" && active() == [fourth.deviceToken ?? ""],
    "Cancel during the pair call saves nothing, revokes the issued key and keeps the working key")
  guard case .paired(let fifth, _) = await session.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "Re-pair must pair after a cancel")
  }
  await session.finishRetiring()
  try expect(fifth.installId != dropped?.installId && active() == [fifth.deviceToken ?? ""],
    "After a cancel the next attempt uses a new install id")

  // The fifth failure pauses pairing (state 7) with the dashboard's own wait.
  fake.with { $0.failures = 5 }
  guard case .rateLimited(let until) = await session.pair(url: url, username: "owner", password: "x") else {
    throw CheckFailure(description: "429 must be state 7")
  }
  try expect(until.timeIntervalSinceNow > 590 && until.timeIntervalSinceNow <= 601, "The countdown follows retryAfterSeconds")
  fake.with { $0.failures = 0 }

  // A dashboard without pairing yet (404) keeps today's verified password login.
  let olderFile = directory.appendingPathComponent("older/accounts-connection.json")
  let older = ConnectionSession(fileURL: olderFile, makeTransport: { FakeDashboardTransport(fake) })
  fake.with { $0.supportsPairing = false }
  guard case .unsupported = await older.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "A dashboard without pairing must fall back to the password login")
  }
  fake.with { $0.supportsPairing = true }
  try expect((try? BarConnection.load(from: olderFile))?.hasPassword == true, "The fallback saves a verified version 1 login")
}

/// State 2: create the sign-in with the setup code, then pair.
@MainActor private func checkSetupFlow() async throws {
  let directory = try pairingDirectory("setup")
  defer { try? FileManager.default.removeItem(at: directory) }
  let fake = FakeDashboard()
  fake.with { $0.accessMode = "setup"; $0.username = ""; $0.password = "" }
  let file = directory.appendingPathComponent("accounts-connection.json")
  let session = ConnectionSession(fileURL: file, makeTransport: { FakeDashboardTransport(fake) })
  let url = URL(string: "http://192.168.50.10:3000")!
  guard case .setupCode(let tries) = await session.setupAndPair(url: url, username: "owner", password: "summit-ledger-42",
    setupCode: "WRON-GCOD") else { throw CheckFailure(description: "A wrong setup code must be refused") }
  try expect(tries == 4, "A wrong setup code says the tries left")
  guard case .failed(let weak, let field) = await session.setupAndPair(url: url, username: "owner", password: "short",
    setupCode: "K7QF-2MXD") else { throw CheckFailure(description: "A weak password must be refused") }
  try expect(weak == SignInCopy.passwordShort && field == "pass", "weak_password names the password field")
  guard case .paired(let connection, _) = await session.setupAndPair(url: url, username: "owner", password: "summit-ledger-42",
    setupCode: "k7qf-2mxd") else { throw CheckFailure(description: "A right setup code must create the sign-in and pair") }
  try expect(connection.isPaired && (try? BarConnection.load(from: file))?.isPaired == true, "Setup then pairs and saves the key")
  guard case .alreadyConfigured = await session.setupAndPair(url: url, username: "owner", password: "summit-ledger-42",
    setupCode: "K7QF-2MXD") else { throw CheckFailure(description: "A configured dashboard refuses setup") }
  // The form's own checks and the strength meter.
  try expect(PasswordStrength.setupProblem(username: "1x", password: "summit-ledger-42", confirm: "summit-ledger-42", code: "K7QF-2MXD", codeRequired: true)?.field == "user"
    && PasswordStrength.setupProblem(username: "owner", password: "short", confirm: "short", code: "K7QF-2MXD", codeRequired: true)?.field == "pass"
    && PasswordStrength.setupProblem(username: "owner", password: String(repeating: "é", count: 40), confirm: String(repeating: "é", count: 40), code: "K7QF-2MXD", codeRequired: true)?.message == SignInCopy.passwordLong
    && PasswordStrength.setupProblem(username: "owner", password: "summit-ledger-42", confirm: "summit-ledger-43", code: "K7QF-2MXD", codeRequired: true)?.field == "confirm"
    && PasswordStrength.setupProblem(username: "owner", password: "summit-ledger-42", confirm: "summit-ledger-42", code: "", codeRequired: true)?.field == "code"
    && PasswordStrength.setupProblem(username: "owner", password: "summit-ledger-42", confirm: "summit-ledger-42", code: "K7QF", codeRequired: true)?.field == "code"
    && PasswordStrength.setupProblem(username: "owner", password: "summit-ledger-42", confirm: "summit-ledger-42", code: "", codeRequired: false) == nil,
    "The setup form checks username, length, bytes, confirmation and the code before sending")
  try expect(PasswordStrength.evaluate("").level == 0 && PasswordStrength.evaluate("abc").word == "Too short"
    && PasswordStrength.evaluate("password123").word == "Weak" && PasswordStrength.evaluate("summit-ledger-42").level >= 3
    && PasswordStrength.evaluate("a-long-and-mixed-Passphrase-2026").word == "Strong",
    "The strength meter follows the concept's levels")
}

/// State 9 and section 8: the stored password is traded for a key once; the rollback copy lives until the first 200.
@MainActor private func checkMigrationFlow() async throws {
  let directory = try pairingDirectory("migrate")
  defer { try? FileManager.default.removeItem(at: directory) }
  let fake = FakeDashboard()
  let file = directory.appendingPathComponent("bar/accounts-connection.json")
  let url = URL(string: "http://192.168.50.10:3000")!
  func seedV1(_ password: String = "fixture-pass-1") throws -> Data {
    try? FileManager.default.removeItem(at: directory)
    try ConnectionStore.save(BarConnection(baseURL: url, username: "owner", password: password), to: file)
    return try Data(contentsOf: file)
  }

  // 200 on the first key check: the password and the rollback copy are gone.
  let v1 = try seedV1()
  var session = ConnectionSession(fileURL: file, makeTransport: { FakeDashboardTransport(fake) })
  try session.load()
  let outcome = await session.migrate()
  let text = try String(contentsOf: file, encoding: .utf8)
  try expect(outcome == .secured && !session.hasPendingRollback && (try? BarConnection.load(from: file))?.isPaired == true
    && !text.contains("fixture-pass-1") && session.connection?.password == nil,
    "A migration that pairs and checks the key deletes the password and the rollback copy")
  let leftovers = try FileManager.default.contentsOfDirectory(atPath: file.deletingLastPathComponent().path)
  try expect(leftovers == ["accounts-connection.json"], "Only the version 2 file remains")

  // No answer to the first key check: both files stay, and the next poll finishes it.
  _ = try seedV1()
  session = ConnectionSession(fileURL: file, makeTransport: { FakeDashboardTransport(fake) })
  try session.load()
  fake.with { $0.meFails = true }
  let pending = await session.migrate()
  let rollback = BarConnection.rollbackURL(for: file)
  try expect(pending == .pending && session.hasPendingRollback && (try? Data(contentsOf: rollback)) == v1
    && permissions(rollback) == 0o600 && (try? BarConnection.load(from: file))?.isPaired == true,
    "Without an answer the rollback copy (the version 1 bytes, 0600) and the new key both stay")
  fake.with { $0.meFails = false }
  try await session.maintain()
  try expect(!session.hasPendingRollback, "The next poll's 200 deletes the rollback copy")

  // A 401 device code on the first check puts version 1 back.
  _ = try seedV1()
  session = ConnectionSession(fileURL: file, makeTransport: { FakeDashboardTransport(fake) })
  try session.load()
  fake.with { $0.meFails = true }
  _ = await session.migrate()
  fake.with { $0.meFails = false }
  if let index = fake.deviceList.firstIndex(where: { $0.token == session.connection?.deviceToken }) { fake.revoke(index, "dashboard") }
  let restored = await session.confirmMigration()
  try expect(restored == .keptPassword && (try? Data(contentsOf: file)) == v1 && !session.hasPendingRollback
    && session.connection?.hasPassword == true,
    "A 401 device code on the first check restores version 1 exactly and keeps the password login")

  // The rollback copy never lives past 24 hours.
  _ = try seedV1()
  session = ConnectionSession(fileURL: file, makeTransport: { FakeDashboardTransport(fake) })
  try session.load()
  fake.with { $0.meFails = true }
  _ = await session.migrate()
  try FileManager.default.setAttributes([.modificationDate: Date().addingTimeInterval(-25 * 3600)], ofItemAtPath: rollback.path)
  _ = await session.confirmMigration()
  fake.with { $0.meFails = false }
  try expect(!session.hasPendingRollback && (try? BarConnection.load(from: file))?.isPaired == true,
    "A rollback copy older than 24 hours is deleted")

  // No pairing yet (404): today's login stays, and pairing waits an hour before trying again.
  _ = try seedV1()
  session = ConnectionSession(fileURL: file, makeTransport: { FakeDashboardTransport(fake) })
  try session.load()
  fake.with { $0.supportsPairing = false }
  let kept = await session.migrate()
  let attempts = fake.requests.filter { $0.contains("/devices/pair") }.count
  _ = await session.migrate()
  try expect(kept == .keptPassword && (try? Data(contentsOf: file)) == v1 && !session.migrationDue
    && fake.requests.filter { $0.contains("/devices/pair") }.count == attempts,
    "Without pairing the version 1 login stays, and pairing is not retried within the hour")
  fake.with { $0.supportsPairing = true }

  // Trust turned on after launch: the refused migration (403) is due again after an hour, and then pairs.
  _ = try seedV1()
  session = ConnectionSession(fileURL: file, makeTransport: { FakeDashboardTransport(fake) })
  try session.load()
  var clock = Date()
  session.now = { clock }
  fake.with { $0.trustLocalNetwork = false; $0.peerTrusted = false }
  let refused = await session.migrate()
  let blockedNow = !session.migrationDue
  fake.with { $0.trustLocalNetwork = true; $0.peerTrusted = true }
  clock = clock.addingTimeInterval(30 * 60)
  let stillBlocked = await session.migrate()
  clock = clock.addingTimeInterval(31 * 60)
  let dueLater = session.migrationDue
  let later = await session.migrate()
  try expect(refused == .keptPassword && blockedNow && stillBlocked == .keptPassword && dueLater && later == .secured
    && (try? BarConnection.load(from: file))?.isPaired == true && !session.hasPendingRollback,
    "A migration refused with trust off is retried after an hour and pairs once trust is on, without a restart")

  // The stored password is wrong: the pairing screen, nothing changed.
  _ = try seedV1("stale-password")
  session = ConnectionSession(fileURL: file, makeTransport: { FakeDashboardTransport(fake) })
  try session.load()
  guard case .needsPassword = await session.migrate() else { throw CheckFailure(description: "A stale stored password needs the pairing screen") }
  try expect(!session.hasPendingRollback && (try? BarConnection.load(from: file))?.hasPassword == true, "A refused migration changes nothing")
  fake.with { $0.failures = 0 }
}

/// Section 9 and Disconnect: the key goes, the address and username stay; nothing retries on its own.
@MainActor private func checkSignOutAndDisconnect() async throws {
  let directory = try pairingDirectory("signout")
  defer { try? FileManager.default.removeItem(at: directory) }
  let fake = FakeDashboard()
  let file = directory.appendingPathComponent("accounts-connection.json")
  let url = URL(string: "http://192.168.50.10:3000")!
  let session = ConnectionSession(fileURL: file, makeTransport: { FakeDashboardTransport(fake) })
  guard case .paired(let connection, _) = await session.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "Pairing must work for the sign-out checks")
  }
  session.signOut(SignedOutNote(reason: "device_revoked", at: "2026-10-02T16:00:00Z", revokedReason: "dashboard", revokedBy: "owner"))
  let out = try BarConnection.load(from: file)
  try expect(out.isSignedOut && out.username == "owner" && out.baseURL == url && out.installId == connection.installId
    && out.signedOut?.revokedBy == "owner" && session.client == nil,
    "A sign-out deletes the key, keeps the address, username and install id, and stops the client")
  try session.load()
  try expect(session.client == nil && session.connection?.isSignedOut == true, "After a restart the signed-out file feeds only the sign-in screen")

  guard case .paired = await session.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "Pair again must work after a sign-out")
  }
  let mark = fake.requests.count
  let told = await session.disconnect()
  let gone = try BarConnection.load(from: file)
  try expect(told && Array(fake.requests.dropFirst(mark)) == ["DELETE 192.168.50.10/api/auth/devices/me 204"]
    && gone.isSignedOut && gone.signedOut?.reason == "disconnected" && gone.baseURL == url && session.client == nil
    && fake.deviceList.last?.revoked == "self",
    "Disconnect revokes this key on the dashboard, then forgets it and keeps the address")

  guard case .paired = await session.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "Pairing must work again")
  }
  // The dashboard cannot be told: the key is still forgotten here, and the caller says so.
  let offline = ConnectionSession(fileURL: file, makeTransport: { MockTransportThrowing() })
  try offline.load()
  let notTold = await offline.disconnect()
  try expect(!notTold && (try? BarConnection.load(from: file))?.isSignedOut == true, "An unreachable dashboard still loses the key here")
}

private actor MockTransportThrowing: BarHTTPTransport {
  func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) { throw URLError(.cannotConnectToHost) }
}

/// Section 7: devices/me now and then, and rotation once rotateAfter has passed; the new key is saved before its first use.
@MainActor private func checkRotation() async throws {
  let directory = try pairingDirectory("rotate")
  defer { try? FileManager.default.removeItem(at: directory) }
  let fake = FakeDashboard()
  let file = directory.appendingPathComponent("accounts-connection.json")
  let url = URL(string: "http://192.168.50.10:3000")!
  let session = ConnectionSession(fileURL: file, makeTransport: { FakeDashboardTransport(fake) })
  guard case .paired(let first, _) = await session.pair(url: url, username: "owner", password: "fixture-pass-1") else {
    throw CheckFailure(description: "Pairing must work for the rotation checks")
  }
  var clock = Date()
  session.now = { clock }
  // Not due: devices/me was just read, nothing is sent.
  let mark = fake.requests.count
  try await session.maintain()
  try expect(fake.requests.count == mark, "Within six hours of the last devices/me, maintenance sends nothing")
  // Due: devices/me says rotateAfter has passed; the tray rotates, saves, then uses the new key.
  fake.with { $0.rotateAfter = "2020-01-01T00:00:00Z" }
  clock = clock.addingTimeInterval(7 * 3600)
  try await session.maintain()
  let saved = try BarConnection.load(from: file)
  try expect(saved.deviceToken != first.deviceToken && saved.deviceId == first.deviceId && saved.installId == first.installId
    && session.connection?.deviceToken == saved.deviceToken,
    "Rotation saves the new key over the old one in the same pairing")
  _ = try await session.client?.dashboard()
  try expect(fake.auths.last == "Bearer \(saved.deviceToken ?? "")", "The next request uses the saved new key")
  // Rotation refused over this transport (trust turned off): keep the key, try again only after an hour.
  fake.with { $0.rotateAfter = "2020-01-01T00:00:00Z"; $0.refuseRotate = true }
  clock = clock.addingTimeInterval(7 * 3600)
  try await session.maintain()
  let refusedAt = fake.requests.filter { $0.contains("/rotate") }.count
  clock = clock.addingTimeInterval(10 * 60)
  try await session.maintain()
  try expect(fake.requests.filter { $0.contains("/rotate") }.count == refusedAt
    && (try? BarConnection.load(from: file))?.deviceToken == saved.deviceToken,
    "A refused rotation keeps the current key and waits an hour before trying again")
  fake.with { $0.refuseRotate = false }
  // A revoked key found by maintenance is a sign-out.
  if let index = fake.deviceList.firstIndex(where: { $0.token == saved.deviceToken }) { fake.revoke(index, "dashboard") }
  clock = clock.addingTimeInterval(7 * 3600)
  do {
    try await session.maintain()
    throw CheckFailure(description: "A revoked key must surface from maintenance")
  } catch BarClientError.signedOut(let note) {
    try expect(note.reason == "device_revoked", "Maintenance reports the dashboard's sign-out")
  }
}

/// F6 from the dashboard, OR'd with the tray's own rule.
private func checkResetPassedField() throws {
  func window(_ extra: [String: Any]) throws -> AccountQuotaWindow {
    var object: [String: Any] = ["key": "five_hour", "label": "Five-hour", "usedPercent": 87.5, "remainingPercent": 12.5,
      "resetAt": "2026-10-02T11:00:00.000Z", "windowMinutes": 300, "used": NSNull(), "limit": NSNull(), "unit": NSNull()]
    for (key, value) in extra { object[key] = value }
    return try JSONDecoder().decode(AccountQuotaWindow.self, from: JSONSerialization.data(withJSONObject: object))
  }
  let before = AccountFormatting.date("2026-10-02T10:00:00.000Z")!
  let marked = try window(["resetPassed": true])
  try expect(TrayReset.pending(marked, accountSampledAt: "2026-10-02T10:30:00.000Z", now: before) != nil,
    "resetPassed from the dashboard shows new reading pending even when this Mac's clock is behind")
  let plain = try window([:])
  try expect(TrayReset.pending(plain, accountSampledAt: "2026-10-02T10:30:00.000Z", now: before) == nil,
    "Without the mark, a reset still ahead stands")
  let after = AccountFormatting.date("2026-10-02T12:00:00.000Z")!
  try expect(TrayReset.pending(plain, accountSampledAt: "2026-10-02T10:30:00.000Z", now: after) != nil,
    "The tray's own rule still applies without the mark")
  let amount = try window(["resetPassed": true, "kind": "balance"])
  try expect(TrayReset.pending(amount, accountSampledAt: nil, now: after) == nil, "Amounts are left alone")
  try expect(marked.meterUsedPercent == 87.5, "The old reading stays as history; nothing is zeroed")
}

/// "Opened · copied 3 of 18" when a bounded copy opened Claude before every record was across.
private func checkClaudeOpenPartialCopy() throws {
  func operation(_ state: String, _ confirmed: Int?, _ total: Int?) throws -> ClaudeOpenOperation {
    var object: [String: Any] = ["id": "op", "platform": "mac", "state": state, "message": NSNull()]
    object["confirmedCount"] = confirmed ?? NSNull()
    object["totalCount"] = total ?? NSNull()
    return try JSONDecoder().decode(ClaudeOpenOperation.self, from: JSONSerialization.data(withJSONObject: object))
  }
  let partial = try operation("opened", 3, 18), whole = try operation("opened", 18, 18), uncounted = try operation("opened", nil, nil)
  try expect(ClaudeOpenFlow.text(for: partial) == "Opened · copied 3 of 18"
    && ClaudeOpenFlow.text(for: whole) == "Opened"
    && ClaudeOpenFlow.text(for: uncounted) == "Opened",
    "An Open that ended with part of the history copied says how much")
}

if CommandLine.arguments.contains("--check-live") {
  // Opt-in deployment check is read-only. It never refreshes usage, launches a
  // desktop, activates a profile, or changes automatic switching settings.
  await checkLive()
} else {
do {
  try await checkSessionAndQueries()
  print("PASS session sharing and Mac dashboard queries")
  try await checkWrites()
  print("PASS Origin, JSON, activation, auto-switch, and Claude launch contract")
  try await checkRejectedProfileIdentifiers()
  print("PASS rejected profile identifiers and Claude allowlist")
  try await checkExpiredSession()
  print("PASS bounded expired-session retry")
  try await checkConfirmedCodexSwitch()
  print("PASS confirmed busy Codex switch, explicit consent, expiry, and rejected confirmation")
  try await checkLoginBackoffAndBadPayload()
  print("PASS failed-login backoff and malformed payload handling")
  try await checkSafeHTTPFailures()
  print("PASS 52 private-error canary cases, local action isolation, settings guidance, and structured consent")
  try checkUnknownQuotaAndExactReset()
  print("PASS unknown quota and exact reset timestamps")
  try checkUsageMetadata()
  print("PASS optional usage metadata, balances, and independent expiration")
  try checkCachedWindowProvenance()
  print("PASS retained optional-window Cached labels and original sample timestamps")
  try checkProviderGrouping()
  print("PASS provider grouping preserves accounts, actual active quota, and all detail windows")
  try checkCodexAutoStatusText()
  print("PASS stuck Codex switch line names the vetted candidate and hides when healthy")
  try checkVisibleUsageWindows()
  print("PASS provider-scoped visible windows, real zero quota, reset packs, and distinct Go accounts")
  try await checkPrivateConnectionFile()
  print("PASS private connection file and origin validation")
  try checkCodexCompactFullDashboardDTO()
  print("PASS complete Codex dashboard DTO, exact compact cores, no aliases, raw Details, and provider isolation")
  try await checkAntigravityClient()
  print("PASS Antigravity activation, one-use confirmation, fixed guidance, and % used automatic settings")
  try checkDashboardAdditions()
  print("PASS Antigravity policy and capabilities, activation guards, tray order, and tray-hidden providers and accounts")
  try checkTrayPresentation()
  print("PASS Fable on Max only, exact Codex cells, menu-bar reading, two-decimal numbers, and no-overshoot motion")
  try checkMenuBarSelection()
  print("PASS menu-bar provider, account, window and value selection with pending hiding the number")
  try await checkConnectionChange()
  print("PASS sign-in and Change verify before saving: wrong address, wrong login, cancel, timeout, success, first run")
  try checkResetPending()
  print("PASS readings from before their reset show as new reading pending, never 0%")
  try checkStatusItemToggle()
  print("PASS status-item presses toggle instead of dismissing; presses elsewhere still dismiss")
  try await checkClaudeOpenProgress()
  print("PASS Claude Open progress: 200, 202 polling with counts, failed, blocked_uncertain, 409, deadline, one POST")
  try checkClaudeOpenPartialCopy()
  print("PASS Claude Open that ends with part of the history copied says how much")
  try await checkConnectionVersions()
  print("PASS connection file versions 1 and 2 and signed out: one shape, 0600 in 0700, never a password next to a key")
  try await checkBearerClient()
  print("PASS device key as Bearer on every tray route, no login, 401 device codes as sign-outs with who and when, 503 kept")
  try checkLocalNetwork()
  print("PASS local-network check: private, loopback and fc00::/7 local; public, CGNAT, link-local and unknown refused")
  try await checkAddressStates()
  print("PASS address check states: not local (tray and dashboard), pairing off, setup code, sign-in off, unreachable, wrong address, numeric address for plain HTTP")
  try await checkPairingFlow()
  print("PASS pairing: wrong password with tries, refusals, verify before saving, Re-pair under a new install id (unconfirmed, unsaved, cancelled), one install id per session, rate limit, password fallback")
  try await checkSetupFlow()
  print("PASS first-run setup code, form checks and strength meter, then pairing")
  try await checkMigrationFlow()
  print("PASS migration from a stored password: rollback copy until the first 200, restore on a device code, 24-hour rollback limit, hourly retry after a refusal")
  try await checkSignOutAndDisconnect()
  print("PASS sign-out keeps the address and username without the key; Disconnect revokes on the dashboard, then forgets it")
  try await checkRotation()
  print("PASS rotation after rotateAfter: saved before first use, refusals keep the key and wait, revocation surfaces")
  try checkResetPassedField()
  print("PASS dashboard resetPassed mark honoured with the tray's own F6 rule")
  print("AI Account Center core checks passed (offline; no real credentials or network)")
} catch {
  fputs("AI Account Center core check failed: \(error)\n", stderr)
  exit(1)
}
}
