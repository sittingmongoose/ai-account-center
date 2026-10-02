import Foundation
import SystemConfiguration

// Pairing this tray with the dashboard (CONTRACT-auth-devices sections 2a, 4, 5, 6, 7 and 8, as amended for the
// trusted local network on 2026-10-02). The dashboard stays on plain HTTP at its LAN address; its owner switch
// "Trust this local network" lets pairing work from a private address, at home or over the home VPN.

/// `GET /api/auth/devices/me`.
public struct DeviceSelf: Decodable, Sendable, Equatable {
  public let id: String
  public let name: String?
  public let platform: String?
  public let pairedAt: String?
  public let rotateAfter: String?
  public let idleExpiresAt: String?
}

/// `POST /api/auth/devices/me/rotate`.
public struct RotatedKey: Decodable, Sendable {
  public let token: String
  public let rotateAfter: String?
}

/// `POST /api/auth/devices/pair` 201: the only time the key is ever sent.
public struct PairedDevice: Decodable, Sendable {
  public let deviceId: String
  public let token: String
  public let name: String?
  public let platform: String?
  public let pairedAt: String?
  public let rotateAfter: String?
}

/// The public `GET /api/auth/check` (and `GET /api/auth/setup`), only the fields the tray reads. Every field is
/// optional: an older dashboard sends fewer of them.
public struct AuthCheck: Decodable, Sendable, Equatable {
  public struct Connection: Decodable, Sendable, Equatable {
    public let peer: String?
    public let trusted: Bool?
  }
  public let accessMode: String?
  public let configured: Bool?
  public let setupCodeRequired: Bool?
  public let secureTransport: Bool?
  public let trustedLocalNetwork: Bool?
  public let connection: Connection?

  /// A plain, short peer address to show; anything else is dropped.
  public var peer: String? {
    guard let value = connection?.peer, value != "unknown", value.count <= 64,
      value.range(of: "^[0-9A-Fa-f:.]+\\z", options: .regularExpression) != nil else { return nil }
    return value
  }

  /// Pairing would be accepted from here: a secure transport, or the trusted local network.
  public var pairingAllowed: Bool { secureTransport == true || connection?.trusted == true }
}

/// The tray's own check of a dashboard address before any password goes to it over plain HTTP: private IPv4
/// (10/8, 172.16/12, 192.168/16), IPv6 fc00::/7 and loopback are local; anything else (public, CGNAT, link-local,
/// unknown) is not. A name is judged by every address it resolves to.
public enum LocalNetwork {
  public enum Verdict: Sendable, Equatable {
    case local
    /// The first address outside the local network.
    case outside(String)
    /// The name did not resolve: nothing can be sent anyway, and the reachability check says so.
    case unresolved
  }

  public static func isLocal(address raw: String) -> Bool {
    var address = raw.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
    if let zone = address.firstIndex(of: "%") { address = String(address[..<zone]) }
    if let v4 = ipv4(address) { return isLocalV4(v4) }
    guard let v6 = ipv6(address) else { return false }
    // IPv4-mapped (::ffff:a.b.c.d) is judged as the IPv4 address.
    if v6[0..<10].allSatisfy({ $0 == 0 }) && v6[10] == 0xFF && v6[11] == 0xFF {
      return isLocalV4(Array(v6[12..<16]))
    }
    if v6[0..<15].allSatisfy({ $0 == 0 }) && v6[15] == 1 { return true }  // ::1
    return v6[0] & 0xFE == 0xFC  // fc00::/7
  }

  private static func isLocalV4(_ bytes: [UInt8]) -> Bool {
    let a = bytes[0], b = bytes[1]
    return a == 10 || a == 127 || (a == 172 && (16...31).contains(b)) || (a == 192 && b == 168)
  }

  static func ipv4(_ text: String) -> [UInt8]? {
    var value = in_addr()
    guard inet_pton(AF_INET, text, &value) == 1 else { return nil }
    return withUnsafeBytes(of: value.s_addr) { Array($0) }
  }

  static func ipv6(_ text: String) -> [UInt8]? {
    var value = in6_addr()
    guard inet_pton(AF_INET6, text, &value) == 1 else { return nil }
    return withUnsafeBytes(of: value) { Array($0) }
  }

  /// Every address a host name resolves to (numeric hosts resolve to themselves).
  public static func resolve(_ host: String) -> [String] {
    var hints = addrinfo()
    hints.ai_family = AF_UNSPEC
    hints.ai_socktype = SOCK_STREAM
    var result: UnsafeMutablePointer<addrinfo>?
    guard getaddrinfo(host, nil, &hints, &result) == 0, let first = result else { return [] }
    defer { freeaddrinfo(first) }
    var addresses: [String] = []
    var cursor: UnsafeMutablePointer<addrinfo>? = first
    while let info = cursor {
      var buffer = [CChar](repeating: 0, count: Int(NI_MAXHOST))
      if getnameinfo(info.pointee.ai_addr, info.pointee.ai_addrlen, &buffer, socklen_t(buffer.count), nil, 0,
        NI_NUMERICHOST) == 0 {
        let text = String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
        if !addresses.contains(text) { addresses.append(text) }
      }
      cursor = info.pointee.ai_next
    }
    return addresses
  }

  /// The verdict for a host: a literal address is judged as it is; a name by every address it resolves to.
  public static func verdict(host: String, resolver: (String) -> [String] = resolve) -> Verdict {
    let bare = host.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
    if ipv4(bare) != nil || ipv6(bare.split(separator: "%").first.map(String.init) ?? bare) != nil {
      return isLocal(address: bare) ? .local : .outside(bare)
    }
    let addresses = resolver(host)
    guard !addresses.isEmpty else { return .unresolved }
    if let outside = addresses.first(where: { !isLocal(address: $0) }) { return .outside(outside) }
    return .local
  }
}

/// What checking a dashboard address found (sign-in states 1, 2, 4, 5 and 8).
public enum AddressCheck: Sendable, Equatable {
  /// Not a usable address (no host, a path, a query, ...).
  case invalid
  /// State 4. `seenAs` is the address the dashboard saw this Mac at, when the refusal came from the dashboard.
  case notLocal(URL, seenAs: String?)
  /// State 5: the dashboard has not turned on "Trust this local network".
  case pairingOff(URL)
  /// State 8: no answer within the time limit.
  case unreachable(URL)
  /// State 8: it answered, but not as AI Account Center.
  case notDashboard(URL)
  /// Sign-in is turned off on that dashboard, so there is nothing to pair with.
  case signInOff(URL)
  /// State 2: a dashboard with no password yet. `codeRequired` is true away from the dashboard machine.
  case setup(URL, codeRequired: Bool)
  /// State 1, password step: pairing can go ahead.
  case ready(URL)

  public var url: URL? {
    switch self {
    case .invalid: return nil
    case .notLocal(let url, _), .pairingOff(let url), .unreachable(let url), .notDashboard(let url),
      .signInOff(let url), .setup(let url, _), .ready(let url): return url
    }
  }
}

/// What a pair (or setup and pair) attempt found.
public enum PairOutcome: Sendable {
  /// 201, then `devices/me` 200: the version 2 connection to save.
  case paired(BarConnection, DeviceSelf)
  /// 401 `invalid_credentials` (state 6).
  case wrongPassword(triesLeft: Int?)
  /// 429 (state 7): when pairing opens again.
  case rateLimited(until: Date)
  /// 403 `secure_transport_required`: the address check's answer now (state 4 or 5).
  case refused(AddressCheck)
  /// 404 or 405: a dashboard without pairing yet. The tray keeps today's password login (section 8).
  case unsupported
  /// Setup only: 403 `setup_code_required` or `setup_code_invalid`.
  case setupCode(triesLeft: Int?)
  /// Setup only: 409 `already_configured`: the dashboard has a password now; go to the password step.
  case alreadyConfigured
  /// A fixed sentence for anything else, and which field it is about.
  case failed(String, field: String?)
}

/// The dashboard's public sign-in routes: check, setup and pair. No session and no key are sent.
public struct DashboardProbe: Sendable {
  public let baseURL: URL
  private let transport: BarHTTPTransport

  public init(baseURL: URL, transport: BarHTTPTransport = BarSessionTransport()) {
    self.baseURL = baseURL
    self.transport = transport
  }

  /// The Mac's own name (System Settings › General › Sharing), 1 to 64 printable characters. Read from the
  /// system configuration store, which answers at once (`Host.current()` can wait on name lookups).
  public static func deviceName(_ candidate: String? = nil) -> String {
    let computer = SCDynamicStoreCopyComputerName(nil, nil) as String?
    let raw = candidate ?? computer ?? ProcessInfo.processInfo.hostName
    let printable = raw.unicodeScalars.filter { $0.value >= 0x20 && $0.value != 0x7F }
    let trimmed = String(String.UnicodeScalarView(printable)).trimmingCharacters(in: .whitespaces)
    return trimmed.isEmpty ? "Mac" : String(trimmed.prefix(64))
  }

  /// An entered address as a dashboard origin: `http://` is added when the scheme is missing; a path, query, user or
  /// fragment makes it unusable.
  public static func normalize(_ raw: String) -> URL? {
    var text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !text.isEmpty, text.count <= 255 else { return nil }
    if text.range(of: "^[A-Za-z][A-Za-z0-9+.-]*://", options: .regularExpression) == nil { text = "http://" + text }
    while text.hasSuffix("/") { text.removeLast() }
    guard let url = URL(string: text), let scheme = url.scheme?.lowercased(), ["http", "https"].contains(scheme),
      BarConnection.isDashboardURL(url), let host = url.host, !host.isEmpty else { return nil }
    return url
  }

  private func request(_ path: String, method: String = "GET", body: Data? = nil) -> URLRequest {
    var request = URLRequest(url: baseURL.appendingPathComponent(path))
    request.httpMethod = method
    request.httpBody = body
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    if method != "GET" {
      request.setValue("application/json", forHTTPHeaderField: "Content-Type")
      request.setValue(baseURL.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/")), forHTTPHeaderField: "Origin")
    }
    return request
  }

  /// `GET /api/auth/check`: nil when it answered but not as the dashboard.
  public func check() async throws -> AuthCheck? {
    let (data, response) = try await transport.send(request("api/auth/check"))
    guard response.statusCode == 200, let value = try? JSONDecoder().decode(AuthCheck.self, from: data),
      let mode = value.accessMode, ["open", "login", "setup"].contains(mode) else { return nil }
    return value
  }

  /// `GET /api/auth/setup`.
  public func setupInfo() async throws -> AuthCheck? {
    let (data, response) = try await transport.send(request("api/auth/setup"))
    guard response.statusCode == 200 else { return nil }
    return try? JSONDecoder().decode(AuthCheck.self, from: data)
  }

  /// States 1, 2, 4, 5 and 8: the tray's own local-network check (nothing is sent to an outside address), then what
  /// the dashboard says about this connection.
  public func checkAddress(resolver: @Sendable (String) -> [String] = { LocalNetwork.resolve($0) }) async -> AddressCheck {
    guard let host = baseURL.host else { return .invalid }
    if baseURL.scheme?.lowercased() == "http", case .outside = LocalNetwork.verdict(host: host, resolver: resolver) {
      return .notLocal(baseURL, seenAs: nil)
    }
    let answer: AuthCheck?
    do { answer = try await check() }
    catch {
      if case BarClientError.nonHTTPResponse = error { return .notDashboard(baseURL) }
      return .unreachable(baseURL)
    }
    guard let answer else { return .notDashboard(baseURL) }
    return classify(answer, setup: answer.accessMode == "setup" ? (try? await setupInfo()) ?? nil : nil)
  }

  /// The address check's answer from `/api/auth/check` (and, for a fresh dashboard, `/api/auth/setup`).
  public func classify(_ answer: AuthCheck, setup: AuthCheck?) -> AddressCheck {
    if answer.accessMode == "open" { return .signInOff(baseURL) }
    // An older dashboard reports none of these fields: try pairing and let it answer.
    let reports = answer.secureTransport != nil || answer.trustedLocalNetwork != nil || answer.connection != nil
    if reports && !answer.pairingAllowed {
      if answer.trustedLocalNetwork == true { return .notLocal(baseURL, seenAs: answer.peer) }
      return .pairingOff(baseURL)
    }
    if answer.accessMode == "setup" {
      return .setup(baseURL, codeRequired: setup?.setupCodeRequired ?? answer.setupCodeRequired ?? true)
    }
    return .ready(baseURL)
  }

  private func post(_ path: String, _ body: [String: Any]) async throws -> (Data, HTTPURLResponse) {
    try await transport.send(request(path, method: "POST", body: JSONSerialization.data(withJSONObject: body)))
  }

  private static func refusal(_ data: Data, _ response: HTTPURLResponse) -> PairRefusal {
    let object = ((try? JSONSerialization.jsonObject(with: data)) as? [String: Any]) ?? [:]
    let retry = (object["retryAfterSeconds"] as? NSNumber)?.doubleValue
      ?? response.value(forHTTPHeaderField: "Retry-After").flatMap { TimeInterval($0) }
    return PairRefusal(status: response.statusCode, code: object["code"] as? String,
      triesLeft: (object["triesLeft"] as? NSNumber)?.intValue, retryAfter: retry, reason: object["reason"] as? String)
  }

  /// `POST /api/auth/devices/pair`. Only the 201's key is returned; every refusal is a fixed outcome.
  public func pair(username: String, password: String, installId: String, deviceName: String,
    appVersion: String?) async throws -> Result<PairedDevice, PairRefusal> {
    var body: [String: Any] = ["username": username, "password": password, "deviceName": deviceName,
      "platform": "mac", "installId": installId]
    if let appVersion, !appVersion.isEmpty, appVersion.count <= 32, appVersion.allSatisfy(\.isASCII) {
      body["appVersion"] = appVersion
    }
    let (reply, response) = try await post("api/auth/devices/pair", body)
    if response.statusCode == 201, let device = try? JSONDecoder().decode(PairedDevice.self, from: reply),
      DeviceToken.isWellFormed(device.token), DeviceToken.isDeviceId(device.deviceId) {
      return .success(device)
    }
    return .failure(Self.refusal(reply, response))
  }

  /// `POST /api/auth/setup`: first run. The session cookie it sets is never used; pairing follows.
  public func setup(username: String, password: String, setupCode: String?) async throws -> PairRefusal? {
    var body: [String: Any] = ["username": username, "password": password]
    if let setupCode, !setupCode.isEmpty { body["setupCode"] = setupCode }
    let (reply, response) = try await post("api/auth/setup", body)
    return response.statusCode == 201 ? nil : Self.refusal(reply, response)
  }
}

/// A refused pair or setup, as the dashboard answered it (status and public code only; never its text).
public struct PairRefusal: Error, Sendable, Equatable {
  public let status: Int
  public let code: String?
  public let triesLeft: Int?
  public let retryAfter: TimeInterval?
  public let reason: String?

  public init(status: Int, code: String?, triesLeft: Int? = nil, retryAfter: TimeInterval? = nil, reason: String? = nil) {
    self.status = status
    self.code = code
    self.triesLeft = triesLeft
    self.retryAfter = retryAfter
    self.reason = reason
  }
}

/// The fixed sentences of the sign-in screen's messages (the concept's copy).
public enum SignInCopy {
  public static let enterAddress = "Enter the dashboard address."
  public static let invalidAddress = "Enter an http address with a host and port, such as http://192.168.1.20:3000."
  public static let enterLogin = "Enter your dashboard username and password."
  public static let signInOff = "Sign-in is turned off on this dashboard, so there is nothing to pair with. Turn it on in the dashboard first."
  public static let tooManyDevices = "The dashboard already has 20 paired trays. Revoke one under Settings › Dashboard sign-in, then pair again."
  public static let storeUnavailable = "The dashboard cannot pair trays right now. Try again shortly."
  public static let notConfirmed = "The dashboard issued a key, but did not confirm it. Try again."
  public static let saveFailed = "The device key could not be saved privately on this Mac. Nothing was changed."
  public static let failed = "Pairing did not finish. Try again."
  public static let usernameRule = "Start the username with a letter; use 3 or more letters, numbers, - or _."
  public static let passwordShort = "Use at least 8 characters for the password."
  public static let passwordLong = "Keep the password within 72 bytes."
  public static let confirmMismatch = "The confirmation doesn't match the password."
  public static let enterCode = "Enter the setup code from the server's terminal."
  public static let wrongCode = "That setup code isn't right. It has 8 letters and digits."
  public static let alreadyConfigured = "This dashboard already has a sign-in. Pair with its username and password."
  public static let managedByEnv = "The dashboard's sign-in is set on the server by an environment variable."
}
