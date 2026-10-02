import SwiftUI
import AppKit
import CCSBarCore

/// The sign-in screen's states, numbered as the concept numbers them (trays/, TSIGN-C revision for local HTTP).
enum SignInState: String, CaseIterable, Identifiable {
  case firstRun = "first-run", password, setupCode = "setup-code", pairing, notLocal = "not-local"
  case pairingOff = "pairing-off", wrongPassword = "wrong-password", rateLimited = "rate-limited"
  case unreachable, wrongAddress = "wrong-address", securing, signedOut = "signed-out"
  case signedOutAll = "signed-out-all", expired, success
  var id: String { rawValue }
}

enum SignInField: String, Hashable { case addr, user, pass, confirm, code }

/// pair: a password pairs this tray; setup: a fresh dashboard's sign-in is created first; secure: the stored version 1
/// password is traded for a key by itself (state 9).
enum SignInFlow: Equatable { case pair, setup, secure }

struct SignInMessage: Equatable {
  var bold: String?
  var sub: String?
  var text: String?
  var info = false
}

/// The sign-in screen's state and its real flows. Every network step goes through the tray's `ConnectionSession`
/// (check, pair, setup, migrate); nothing here invents an answer. A newer run makes an older one's late answer stale.
@MainActor
final class SignInModel: ObservableObject {
  /// The screen is showing in place of the account list.
  @Published private(set) var active = false
  @Published private(set) var state: SignInState = .firstRun
  @Published var address = ""
  @Published var username = ""
  @Published var password = ""
  @Published var confirm = ""
  @Published var setupCode = ""
  @Published var revealed: Set<SignInField> = []
  @Published private(set) var busy = false
  @Published private(set) var step = 0
  @Published private(set) var flow: SignInFlow = .pair
  @Published private(set) var triesLeft: Int?
  @Published private(set) var limitUntil: Date?
  @Published private(set) var repair = false
  @Published private(set) var disconnectedAt: Date?
  @Published private(set) var disconnectTold = true
  @Published private(set) var note: SignedOutNote?
  /// The dashboard that answered (the password step's "at" line).
  @Published private(set) var verified: URL?
  /// The address a refusal was about.
  @Published private(set) var tried: URL?
  /// State 4 from the dashboard's side: the address it saw this Mac at.
  @Published private(set) var seenAs: String?
  @Published private(set) var codeRequired = true
  @Published private(set) var message: SignInMessage?
  @Published private(set) var badField: SignInField?
  /// Bumped for the error shake; the view runs one shake per bump.
  @Published private(set) var shake = 0
  /// The field to focus after a state change.
  @Published var focus: SignInField?
  /// Bumped when a screen mounts, so the entrance replays.
  @Published private(set) var entrance = 0
  /// When the last usage sample was read, for "Usage stopped updating at ...".
  var lastSyncedAt: Date?

  weak var owner: AccountsViewModel?
  private var run = 0
  /// The preview renderer and the checks hold a state still; the live tray never sets this.
  var holdsState = false
  /// Step timing: the four steps tick through as in the concept. Checks shorten them.
  var stepInterval: TimeInterval = 0.44
  var successHold: TimeInterval = 1.0

  init() {}

  // MARK: Derived

  var deviceKind: String { "Mac tray" }
  var deviceHost: String { owner?.deviceName ?? DashboardProbe.deviceName() }
  var keyFilePath: String {
    let path = BarConnection.configURL.path
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    return path.hasPrefix(home + "/") ? "~" + path.dropFirst(home.count) : path
  }
  var rollbackName: String { BarConnection.rollbackURL().lastPathComponent }
  var host: String { Self.host(verified) }
  static func host(_ url: URL?) -> String {
    guard let url, let host = url.host else { return "" }
    let shown = host.contains(":") ? "[\(host)]" : host
    return url.port.map { "\(shown):\($0)" } ?? shown
  }

  /// The local address this tray last paired with, for state 4's "Use this".
  var lastLocalAddress: URL? {
    guard let saved = owner?.connection?.baseURL, let host = saved.host, saved.scheme == "http" else { return nil }
    if case .local = LocalNetwork.verdict(host: host, resolver: { _ in [] }) { return saved }
    return nil
  }

  /// The header's status text while the screen shows.
  var statusText: String {
    if repair { return "Re-pairing · the current key still works" }
    switch state {
    case .pairing: return "Pairing"
    case .securing: return "Securing this tray"
    case .signedOut, .signedOutAll, .expired: return "Signed out"
    case .success: return "Paired"
    default: return "Not paired"
    }
  }

  var footNote: String {
    if repair { return "Cancel keeps the current device key and returns to usage" }
    switch state {
    case .securing: return "This runs once, by itself"
    case .success: return "Opening your accounts"
    default: return "Usage appears after this tray is paired"
    }
  }

  /// The menu-bar help tag while signed out: the logo shows with no percentage.
  var menuBarHelp: String {
    switch state {
    case .signedOut, .signedOutAll, .expired: return "Signed out"
    default: return "Not paired"
    }
  }

  var fieldsDisabled: Bool { busy || state == .pairing || state == .securing || state == .success || state == .rateLimited }

  // MARK: Entry points

  private func begin(_ next: SignInState, keepFields: Bool = false) {
    run += 1
    busy = false
    step = 0
    message = nil
    badField = nil
    if !keepFields { password = ""; confirm = ""; setupCode = "" }
    revealed = []
    state = next
    active = true
    entrance += 1
    focus = Self.focusField(next, username: username)
  }

  /// First run: no saved connection.
  func showFirstRun(address prefill: String = "") {
    repair = false
    note = nil
    disconnectedAt = nil
    address = prefill
    username = owner?.connection?.username ?? ""
    verified = nil
    begin(.firstRun)
  }

  /// Disconnect landed: the first-run screen with "Disconnected at ..." and the last address filled in.
  func showDisconnected(at date: Date, told: Bool, address prefill: String) {
    showFirstRun(address: prefill)
    disconnectedAt = date
    disconnectTold = told
  }

  /// Section 9: a 401 device code. The username stays; the password field waits.
  func showSignedOut(_ signedOut: SignedOutNote, connection: BarConnection) {
    repair = false
    disconnectedAt = nil
    note = signedOut
    verified = connection.baseURL
    address = connection.baseURL.absoluteString
    username = connection.username
    let next: SignInState
    switch signedOut.reason {
    case "device_expired": next = .expired
    default:
      next = ["revoke-all", "revoke_all", "revoked_all"].contains(signedOut.revokedReason ?? "") ? .signedOutAll : .signedOut
    }
    begin(next)
  }

  /// Settings › Re-pair: the password step with Cancel. The new key is issued under a new install id, so the current
  /// key keeps working until the new one is confirmed and saved; only then is the old key revoked.
  func beginRepair(connection: BarConnection) {
    repair = true
    note = nil
    disconnectedAt = nil
    verified = connection.baseURL
    address = connection.baseURL.absoluteString
    username = connection.username
    begin(.password)
  }

  /// The stored password failed when pairing by itself: the pairing screen with the username filled in.
  func showPasswordNeeded(connection: BarConnection, triesLeft: Int?) {
    repair = false
    note = nil
    verified = connection.baseURL
    address = connection.baseURL.absoluteString
    username = connection.username
    begin(.password)
    self.triesLeft = triesLeft
    message = SignInMessage(bold: "The saved password was not accepted.", sub: "Enter your dashboard password to pair this tray.")
    badField = .pass
  }

  /// State 9, by itself on launch: trade the stored version 1 password for a key.
  func startSecuring() {
    guard let owner else { return }
    repair = false
    flow = .secure
    begin(.securing)
    busy = true
    let token = run
    Task { @MainActor in
      let outcome = await owner.session.migrate()
      guard token == run else { return }
      switch outcome {
      case .secured, .pending:
        await tick(token)
        guard token == run else { return }
        owner.didPair(flash: "Secured · this tray now signs in with a device key")
        finish(token, handOff: true)
      case .keptPassword, .rateLimited:
        // Today's password login keeps working; pairing is tried again later.
        busy = false
        active = false
        owner.didKeepPassword()
      case .needsPassword(let tries):
        if let connection = owner.connection { showPasswordNeeded(connection: connection, triesLeft: tries) }
      }
    }
  }

  /// The screen leaves without a hand-off (a connection appeared from elsewhere).
  func dismiss() {
    run += 1
    busy = false
    repair = false
    active = false
  }

  /// Cancel: a running check or pair is stopped in the session too, so nothing is saved and an issued key is revoked.
  func cancel() {
    run += 1
    busy = false
    owner?.session.cancelCheck()
    guard repair else { return }
    repair = false
    active = false
    owner?.cancelRepair()
  }

  /// Escape while a check or a pair runs: the run is dropped and the session stops it, so nothing is saved and a key
  /// the dashboard already issued is revoked rather than kept.
  func cancelRunning() -> Bool {
    guard busy, state != .securing, state != .pairing, state != .success else { return false }
    run += 1
    busy = false
    owner?.session.cancelCheck()
    return true
  }

  // MARK: Actions

  func submit() {
    guard !busy else { return }
    switch state {
    case .pairing, .securing, .success, .rateLimited: return
    case .firstRun, .notLocal, .unreachable, .wrongAddress: checkAddress(address)
    case .pairingOff: recheck()
    case .setupCode: setup()
    default: pair()
    }
  }

  func changeAddress() {
    address = verified?.absoluteString ?? address
    begin(.firstRun, keepFields: true)
    password = ""
  }

  func useLocalAddress() {
    guard let local = lastLocalAddress else { return }
    address = local.absoluteString
    checkAddress(address)
  }

  func toggleReveal(_ field: SignInField) {
    if revealed.contains(field) { revealed.remove(field) } else { revealed.insert(field) }
  }

  func clearError(_ field: SignInField) {
    if badField == field { badField = nil }
    if message != nil, state != .unreachable, state != .wrongAddress { message = nil }
  }

  private func fail(_ field: SignInField?, _ text: String) {
    badField = field
    message = SignInMessage(text: text)
    shake += 1
    focus = field
  }

  /// The same refusal twice in a row shakes, so Try again always answers.
  private func go(_ next: SignInState) {
    let again = next == state && next != .password
    run += 1
    busy = false
    message = nil
    badField = nil
    state = next
    focus = Self.focusField(next, username: username)
    if again { shake += 1 }
    if [.notLocal, .wrongAddress].contains(next) { badField = .addr }
    if next == .unreachable {
      message = SignInMessage(bold: "No answer after 10 seconds.", sub: "Check the address, and that the dashboard is running.")
    }
    if next == .wrongAddress {
      message = SignInMessage(bold: "Check the host name and the port.", sub: "The dashboard shows its address under Settings › Dashboard sign-in.")
    }
  }

  static func focusField(_ state: SignInState, username: String) -> SignInField? {
    switch state {
    case .firstRun, .notLocal, .unreachable, .wrongAddress: return .addr
    case .password: return username.isEmpty ? .user : .pass
    case .setupCode: return .user
    case .wrongPassword, .signedOut, .signedOutAll, .expired: return .pass
    default: return nil
    }
  }

  private func apply(_ check: AddressCheck) {
    switch check {
    case .invalid: fail(.addr, SignInCopy.invalidAddress)
    case .notLocal(let url, let peer):
      tried = url
      seenAs = peer
      go(.notLocal)
    case .pairingOff(let url):
      verified = url
      go(.pairingOff)
    case .unreachable(let url):
      tried = url
      go(.unreachable)
    case .notDashboard(let url):
      tried = url
      go(.wrongAddress)
    case .signInOff(let url):
      tried = url
      if state != .firstRun { go(.firstRun) }
      fail(.addr, SignInCopy.signInOff)
    case .insecureName(let url):
      tried = url
      if state != .firstRun { go(.firstRun) }
      fail(.addr, SignInCopy.useNumericAddress)
    case .setup(let url, let required):
      verified = url
      codeRequired = required
      go(.setupCode)
    case .ready(let url):
      verified = url
      go(.password)
    }
  }

  func checkAddress(_ raw: String) {
    guard let owner else { return }
    if raw.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { fail(.addr, SignInCopy.enterAddress); return }
    run += 1
    let token = run
    busy = true
    message = nil
    badField = nil
    Task { @MainActor in
      let result = await owner.session.checkAddress(raw)
      guard token == run else { return }
      busy = false
      if let url = result.url { address = url.absoluteString }
      apply(result)
    }
  }

  private func recheck() {
    guard let url = verified else { changeAddress(); return }
    checkAddress(url.absoluteString)
  }

  func pair() {
    guard let owner, let url = verified else { changeAddress(); return }
    let user = username.trimmingCharacters(in: .whitespaces)
    if user.isEmpty || password.isEmpty { fail(user.isEmpty ? .user : .pass, SignInCopy.enterLogin); return }
    run += 1
    let token = run
    busy = true
    message = nil
    badField = nil
    let secret = password
    Task { @MainActor in
      let outcome = await owner.session.pair(url: url, username: user, password: secret)
      guard token == run else { return }
      busy = false
      handle(outcome, flow: .pair, token: token)
    }
  }

  func setup() {
    guard let owner, let url = verified else { changeAddress(); return }
    let user = username.trimmingCharacters(in: .whitespaces)
    if let problem = PasswordStrength.setupProblem(username: user, password: password, confirm: confirm,
      code: setupCode, codeRequired: codeRequired) {
      fail(SignInField(rawValue: problem.field), problem.message)
      return
    }
    run += 1
    let token = run
    busy = true
    message = nil
    badField = nil
    let secret = password, code = setupCode.uppercased()
    Task { @MainActor in
      let outcome = await owner.session.setupAndPair(url: url, username: user, password: secret,
        setupCode: codeRequired ? code : nil)
      guard token == run else { return }
      busy = false
      handle(outcome, flow: .setup, token: token)
    }
  }

  private func handle(_ outcome: PairOutcome, flow: SignInFlow, token: Int) {
    switch outcome {
    case .paired:
      self.flow = flow
      password = ""
      confirm = ""
      setupCode = ""
      revealed = []
      state = .pairing
      step = 0
      Task { @MainActor in
        await tick(token)
        guard token == run else { return }
        owner?.didPair(flash: "Paired · signed in with a device key")
        finish(token, handOff: true)
      }
    case .unsupported:
      password = ""
      owner?.didPair(flash: "Signed in with your password · this dashboard has no pairing yet")
      active = false
    case .wrongPassword(let tries):
      triesLeft = tries
      if tries == 0 {
        limitUntil = Date().addingTimeInterval(15 * 60)
        password = ""
        go(.rateLimited)
      } else {
        go(.wrongPassword)
        badField = .pass
        message = SignInMessage(bold: "Username or password isn't right.",
          sub: tries.map { "\($0) \($0 == 1 ? "try" : "tries") left before pairing pauses for 15 minutes." }
            ?? "Check them and try again.")
        shake += 1
      }
    case .rateLimited(let until):
      limitUntil = until
      password = ""
      go(.rateLimited)
    case .refused(let check): apply(check)
    case .setupCode(let tries):
      fail(.code, tries.map { "\(SignInCopy.wrongCode) \($0) \($0 == 1 ? "try" : "tries") left." } ?? SignInCopy.wrongCode)
    case .alreadyConfigured:
      go(.password)
      message = SignInMessage(text: SignInCopy.alreadyConfigured, info: true)
    case .failed(let text, let field):
      fail(field.flatMap(SignInField.init(rawValue:)), text)
    case .cancelled:
      break
    }
  }

  /// The four steps tick through, then success holds briefly before the hand-off.
  private func tick(_ token: Int) async {
    for index in 0..<4 {
      try? await Task.sleep(nanoseconds: UInt64(stepInterval * 1_000_000_000))
      guard token == run else { return }
      withAnimation(.smooth(duration: 0.26)) { step = index + 1 }
    }
    try? await Task.sleep(nanoseconds: UInt64(0.24 * 1_000_000_000))
    guard token == run else { return }
    withAnimation(.smooth(duration: 0.26)) { state = .success }
    try? await Task.sleep(nanoseconds: UInt64(successHold * 1_000_000_000))
  }

  private func finish(_ token: Int, handOff: Bool) {
    guard token == run else { return }
    busy = false
    repair = false
    note = nil
    disconnectedAt = nil
    if holdsState { return }
    withAnimation(.easeInOut(duration: 0.52)) { active = false }
  }

  /// The rate limit ran out: back to the password step.
  func limitEnded() {
    guard state == .rateLimited else { return }
    triesLeft = nil
    limitUntil = nil
    go(.password)
  }

  // MARK: Previews and checks

  /// Holds one state with example-free values from the given connection (renders and the self-test).
  func preview(_ next: SignInState, verified url: URL?, username user: String = "", note signedOut: SignedOutNote? = nil) {
    holdsState = true
    verified = url
    tried = url
    username = user
    note = signedOut
    address = url?.absoluteString ?? ""
    begin(next)
    switch next {
    case .wrongPassword:
      triesLeft = 4
      badField = .pass
      message = SignInMessage(bold: "Username or password isn't right.", sub: "4 tries left before pairing pauses for 15 minutes.")
    case .rateLimited: limitUntil = Date().addingTimeInterval(15 * 60 - 23)
    case .unreachable:
      message = SignInMessage(bold: "No answer after 10 seconds.", sub: "Check the address, and that the dashboard is running.")
    case .wrongAddress:
      badField = .addr
      message = SignInMessage(bold: "Check the host name and the port.", sub: "The dashboard shows its address under Settings › Dashboard sign-in.")
    case .notLocal: badField = .addr
    case .pairing: step = 2
    case .securing: flow = .secure; step = 1
    case .success: step = 4
    default: break
    }
    if [.pairing, .securing].contains(next) { busy = true }
  }
}
