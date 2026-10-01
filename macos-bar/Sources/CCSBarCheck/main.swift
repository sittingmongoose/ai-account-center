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
    return "The CCS connection or account request could not be completed."
  }
  switch clientError {
  case .status(let status, _): return "CCS returned HTTP \(status)."
  case .codexConfirmation: return "The Codex switch requires user confirmation."
  default: return clientError.errorDescription ?? "The CCS account request could not be completed."
  }
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
  try checkUnknownQuotaAndExactReset()
  print("PASS unknown quota and exact reset timestamps")
  try checkUsageMetadata()
  print("PASS optional usage metadata, balances, and independent expiration")
  try checkProviderGrouping()
  print("PASS provider grouping preserves accounts, actual active quota, and all detail windows")
  try await checkPrivateConnectionFile()
  print("PASS private connection file and origin validation")
  print("CCS Bar core checks passed (offline; no real credentials or network)")
} catch {
  fputs("CCS Bar core check failed: \(error)\n", stderr)
  exit(1)
}
}
