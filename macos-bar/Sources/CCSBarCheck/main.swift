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
  for profile in ["platyr", "gmail", "party", "me"] { try await client.openClaude(profile: profile) }
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
  for (offset, profile) in ["platyr", "gmail", "party", "me"].enumerated() {
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
  for profile in ["", "unknown", "Platyr", "GMAIL", "../gmail", "gmail/open", "gmail\n", "gmail?platform=windows"] {
    try await expectError(.invalidConnection, "Claude profile must be one of exact four configured IDs") {
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
    // Authentication retry is intentionally bounded to one extra request.
    let replies = Array(repeating: MockReply(status, data), count: status == 401 ? 2 : 1)
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
  hiddenAgy["hiddenProviders"] = ["antigravity"]
  let hiddenSection = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: hiddenAgy))
  try expect(!hiddenSection.canActivateAntigravity(hiddenSection.accounts.first { $0.id == "agy-b" }!),
    "A provider hidden on the dashboard offers no switching in the tray")

  var malformed = object
  malformed["antigravityAutoSwitch"] = ["enabled": "yes", "thresholdUsedPercent": 400]
  let lenient = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: malformed))
  try expect(lenient.antigravityAutoSwitch == nil && lenient.accounts.count == 8,
    "A malformed Antigravity policy must be ignored without losing the dashboard")
  malformed["antigravityAutoSwitch"] = ["enabled": true, "thresholdUsedPercent": 100]
  let outOfRange = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: malformed))
  try expect(outOfRange.antigravityAutoSwitch == nil, "An out-of-range Antigravity threshold must not be shown as a policy")

  var hidden = object
  hidden["hiddenProviders"] = ["kimi-code"]
  let topLevel = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: hidden))
  try expect(topLevel.hiddenProviders == ["kimi-code"] && !topLevel.visibleAccounts.contains { $0.provider == "kimi-code" }
    && !topLevel.providerGroups.contains { $0.id == "kimi-code" } && topLevel.accounts.count == 8,
    "Providers hidden on the dashboard must close up in the tray while raw accounts stay intact")
  var inSettings = object
  inSettings["settings"] = ["refreshIntervalSeconds": 60, "hiddenProviders": ["cursor"]]
  let fromSettings = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: inSettings))
  try expect(fromSettings.hiddenProviders == ["cursor"] && fromSettings.settings?.validatedInterval == 60,
    "Hidden providers inside settings must be honoured without changing the refresh interval")
  inSettings["settings"] = ["refreshIntervalSeconds": 60, "hiddenProviders": "cursor"]
  let badHidden = try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: inSettings))
  try expect(badHidden.hiddenProviders.isEmpty, "A malformed hidden-provider list must be a no-op")
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
  let left = MenuBarReading.make(dashboard: dashboard, source: .codex, mode: .left)
  try expect(left?.text == "\(TrayFormat.number(59.5))%" && left?.detail.hasPrefix("codex-2 5-hour:") == true,
    "The menu bar must show the active Codex account's tightest canonical window as % left")
  try expect(MenuBarReading.make(dashboard: dashboard, source: .codex, mode: .used)?.value == 40.5,
    "% used must show the reading itself")
  let agy = MenuBarReading.make(dashboard: dashboard, source: .antigravity, mode: .used)
  try expect(agy?.value == 18 && agy?.detail.contains("Claude and GPT weekly") == true,
    "Antigravity's menu-bar reading must use its reported columns, including a remaining-only window")
  try expect(MenuBarReading.make(dashboard: dashboard, source: .none, mode: .left) == nil,
    "Logo only must show no percentage")
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

  // Other members of the saved file (a device token from a later pairing) survive a verified Change exactly.
  let paired = directory.appendingPathComponent("paired/accounts-connection.json")
  try FileManager.default.createDirectory(at: paired.deletingLastPathComponent(), withIntermediateDirectories: true)
  try JSONSerialization.data(withJSONObject: ["baseURL": dashboard, "username": "fixture", "password": "fixture-old",
    "deviceToken": "fixture-device-token", "pairedAt": 1] as [String: Any]).write(to: paired)
  try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: paired.path)
  let pairedSession = ConnectionSession(fileURL: paired, makeTransport: { SignInTransport(fixture) })
  try pairedSession.load()
  let pairedFailed = await pairedSession.change(baseURL: dashboard, username: "fixture", password: "wrong-password")
  let pairedOk = await pairedSession.change(baseURL: dashboard, username: "fixture", password: "fixture-new")
  let pairedObject = try JSONSerialization.jsonObject(with: Data(contentsOf: paired)) as? [String: Any]
  try expect(pairedFailed != nil && pairedOk == nil && pairedObject?["deviceToken"] as? String == "fixture-device-token"
    && (pairedObject?["pairedAt"] as? NSNumber)?.intValue == 1 && pairedObject?["password"] as? String == "fixture-new",
    "A verified Change must keep the file's other members exactly")

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
  try expect(MenuBarReading.make(dashboard: pending, source: .codex, mode: .left) == nil
    && MenuBarReading.make(dashboard: fresh, source: .codex, mode: .left)?.text == "\(TrayFormat.number(90.75))%",
    "The menu bar hides a weekly reading from before its reset and shows a newer one")
  let soon = try dashboard(sampledAt: older, resetAt: "2026-10-02T12:00:30Z")
  try expect(soon.pendingResetKeys(now: now).isEmpty && soon.pendingResetKeys(now: now.addingTimeInterval(60)) == ["codex:a|seven_day"],
    "A window flips to pending when its reset passes while the panel is open")
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
  try checkVisibleUsageWindows()
  print("PASS provider-scoped visible windows, real zero quota, reset packs, and distinct Go accounts")
  try await checkPrivateConnectionFile()
  print("PASS private connection file and origin validation")
  try checkCodexCompactFullDashboardDTO()
  print("PASS complete Codex dashboard DTO, exact compact cores, no aliases, raw Details, and provider isolation")
  try await checkAntigravityClient()
  print("PASS Antigravity activation, one-use confirmation, fixed guidance, and % used automatic settings")
  try checkDashboardAdditions()
  print("PASS Antigravity policy and capabilities, activation guards, tray order, and dashboard-hidden providers")
  try checkTrayPresentation()
  print("PASS Fable on Max only, exact Codex cells, menu-bar reading, two-decimal numbers, and no-overshoot motion")
  try await checkConnectionChange()
  print("PASS sign-in and Change verify before saving: wrong address, wrong login, cancel, timeout, success, first run")
  try checkResetPending()
  print("PASS readings from before their reset show as new reading pending, never 0%")
  print("AI Account Center core checks passed (offline; no real credentials or network)")
} catch {
  fputs("AI Account Center core check failed: \(error)\n", stderr)
  exit(1)
}
}
