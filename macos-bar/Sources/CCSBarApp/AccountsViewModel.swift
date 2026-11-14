import SwiftUI
import AppKit
import Combine
import CCSBarCore

@MainActor
final class AccountsViewModel: ObservableObject {
  /// The menu-bar label's own state, synced on every delivery (see `syncMenuBar`).
  let menuBar = MenuBarState()
  /// False while the real panel is closed: refreshes then store their results silently and update only
  /// the menu-bar label, instead of re-laying the whole invisible panel on every publish. True everywhere
  /// else (checks, previews, an open panel), which behave exactly as before.
  var panelOpen = true
  private func publish() {
    if panelOpen { objectWillChange.send() }
  }
  private var _dashboard: AccountDashboard?
  private(set) var dashboard: AccountDashboard? {
    get { _dashboard }
    set { publish(); _dashboard = newValue }
  }
  private var _isRefreshing = false
  private(set) var isRefreshing: Bool {
    get { _isRefreshing }
    set { publish(); _isRefreshing = newValue }
  }
  @Published private(set) var busyAction: String?
  private var _message: String?
  private(set) var message: String? {
    get { _message }
    set { publish(); _message = newValue }
  }
  private var _connection: BarConnection?
  private(set) var connection: BarConnection? {
    get { _connection }
    set { publish(); _connection = newValue }
  }
  private var _connected = false
  private(set) var connected: Bool {
    get { _connected }
    set { publish(); _connected = newValue }
  }
  private var _lastSyncedAt: Date?
  private(set) var lastSyncedAt: Date? {
    get { _lastSyncedAt }
    set { publish(); _lastSyncedAt = newValue }
  }
  /// Meter readings as they were when the panel last closed: a later open animates only what changed.
  var lastShown: [String: Double] = [:]
  /// The first open of a session sweeps every meter from zero.
  var hasOpenedThisSession = false
  let isPreview: Bool
  /// The saved connection and its client. Pairing, Re-pair and the address check replace them only after the
  /// dashboard accepts the new details and the new key has worked once (`ConnectionSession.pair`).
  let session: ConnectionSession
  private var client: AccountsClient? { session.client }
  /// The sign-in screen (first run, pairing, the local-network refusals, a remote sign-out, Re-pair, Disconnect).
  let signIn = SignInModel()
  /// This connection as the dashboard sees it (`GET /api/auth/check`), read when Settings opens.
  @Published private(set) var connectionCheck: AuthCheck?
  @Published private(set) var checkingConnectionInfo = false
  /// A short status line after pairing ("Paired · signed in with a device key"), shown in the header for a moment.
  private var _statusFlash: String?
  private(set) var statusFlash: String? {
    get { _statusFlash }
    set { publish(); _statusFlash = newValue }
  }
  /// After a hand-off the list loads in with the first-open stagger and every meter sweeps from 0.
  @Published private(set) var listEntrance: OpenContext?
  @Published private(set) var listGeneration = 0
  private var signInWatch: [AnyCancellable] = []
  /// Bumped when a verified Change replaces the client, so a sample still in flight from the old one is dropped.
  private var connectionGeneration = 0
  /// A stored-password migration started from the refresh path is running.
  private var retryingMigration = false
  /// The windows now drawn as "new reading pending" (F6), so the timer redraws only when one flips.
  private var pendingResets = Set<String>()
  private var timer: Timer?
  /// The Claude Open now running for an account, keyed by account id, as its row's calm secondary text. The entry is
  /// removed once the Open ends, so the row returns to its plan, platform and sample time.
  @Published private(set) var openProgress: [String: ClaudeOpenProgress] = [:]
  /// One Open per Claude account: a second click while one runs sends nothing at all.
  private let openCoordinator = ClaudeOpenCoordinator()
  /// How often a running Open is read: 1 s, then every 5 s after two minutes, giving up after three.
  private let openPolling = ClaudeOpenPolling()

  /// The first-run sign-in screen with no connection file and no network, for offline renders and the self-test.
  init(previewWithoutConnection: Bool) {
    isPreview = true
    session = ConnectionSession(fileURL: FileManager.default.temporaryDirectory
      .appendingPathComponent("aac-preview-\(UUID().uuidString)/accounts-connection.json"))
    watchSignIn()
    signIn.holdsState = true
    signIn.showFirstRun()
  }

  init(preview: AccountDashboard? = nil, session: ConnectionSession? = nil) {
    self.session = session ?? ConnectionSession()
    if let preview {
      isPreview = true
      dashboard = preview
      connected = true
      lastSyncedAt = AccountFormatting.date(preview.updatedAt)
      watchSignIn()
      syncMenuBar()
      return
    }
    isPreview = false
    watchSignIn()
    configure()
    configureRefreshTimer(60)
  }

  /// The panel follows the sign-in screen's visibility, state and Re-pair flag.
  private func watchSignIn() {
    signIn.owner = self
    signInWatch = [
      signIn.$active.removeDuplicates().sink { [weak self] _ in self?.signInChanged() },
      signIn.$state.removeDuplicates().sink { [weak self] _ in self?.signInChanged() },
      signIn.$repair.removeDuplicates().sink { [weak self] _ in self?.signInChanged() },
    ]
  }

  /// A sign-in transition: the panel follows it when open, and the menu-bar label always does (a 401
  /// during a background refresh takes the number down to the logo alone at once).
  private func signInChanged() {
    syncMenuBar()
    publish()
  }

  /// The menu-bar label's copy of what it shows. Every delivery path calls this: refresh, a reset flip,
  /// disconnect, pairing hand-offs (through `signInChanged`), and the preview inits below.
  func syncMenuBar() {
    menuBar.snapshot = MenuBarSnapshot(dashboard: _dashboard, refreshFailed: !connected && _dashboard != nil, signInActive: signIn.active,
      signInRepair: signIn.repair, signInHelp: signIn.menuBarHelp)
  }

  var deviceName: String { session.deviceName }

  private func configureRefreshTimer(_ interval: TimeInterval) {
    if timer?.timeInterval == interval { return }
    timer?.invalidate()
    timer = Timer.scheduledTimer(withTimeInterval: interval, repeats: true) { [weak self] _ in
      Task { @MainActor in await self?.tick() }
    }
    // A tenth of the interval of leeway lets macOS coalesce this timer with other system work
    // instead of waking the CPU on the exact minute (N4). The cadence shifts by a few seconds at
    // most, which nothing the person sees depends on.
    timer?.tolerance = max(0.5, interval * 0.1)
  }

  /// Signed out or not paired, the menu bar shows the template logo with no percentage (section 9).
  func menuBarReading(_ prefs: TrayPreferences) -> MenuBarReading? {
    guard !signIn.active || signIn.repair else { return nil }
    return MenuBarReading.make(dashboard: dashboard, provider: prefs.menuBarProvider, mode: prefs.menuBarMode,
      claudeAccountID: prefs.menuBarClaudeAccountID)
  }

  /// The sign-in screen shows in place of the list.
  var needsConnection: Bool { signIn.active }

  func configure() {
    // A reload never resumes an Open: its progress lived only in memory.
    openProgress = [:]
    connected = false
    do {
      try session.load()
      connection = session.connection
      message = nil
      guard let saved = session.connection else { signIn.showFirstRun(); return }
      if saved.isPaired {
        Task { await refresh() }
      } else if saved.hasPassword {
        // Section 8: a stored password is traded for a device key by itself, once (state 9).
        signIn.startSecuring()
      } else if let note = saved.signedOut {
        if note.reason == "disconnected" {
          signIn.showDisconnected(at: AccountFormatting.date(note.at) ?? Date(), told: note.disconnectTold == true, address: saved.baseURL.absoluteString, credentialKind: note.credentialKind)
        } else {
          signIn.showSignedOut(note, connection: saved)
        }
      }
    } catch {
      session.clear()
      connection = nil
      let exists = FileManager.default.fileExists(atPath: session.fileURL.path)
      signIn.showFirstRun()
      message = exists ? error.localizedDescription : nil
    }
  }

  /// One refresh-timer tick. While the sign-in screen shows (not a Re-pair), nothing polls and nothing the person is
  /// typing is reset; only a connection file deployed privately in the meantime is picked up.
  func tick() async {
    reevaluateResets()
    if signIn.active && !signIn.repair { reloadIfDeployed() }
    else if client == nil { configure() }
    else { await refresh() }
  }

  /// A paired or password connection written to the private file while the sign-in screen waited (for example a
  /// deployed file) replaces the screen; a signed-out file, or a sign-in step in progress, is left alone.
  private func reloadIfDeployed() {
    guard !signIn.busy, ![.pairing, .securing, .success].contains(signIn.state),
      let saved = try? BarConnection.load(from: session.fileURL), saved.isPaired || saved.hasPassword,
      saved.deviceToken != connection?.deviceToken || saved.password != connection?.password || connection == nil
    else { return }
    signIn.dismiss()
    configure()
  }

  // MARK: Pairing hand-offs (called by the sign-in screen)

  /// A new key works and is saved (or, on a dashboard without pairing, a verified password): the list loads in.
  func didPair(flash: String) {
    connectionGeneration += 1
    openProgress = [:]
    connection = session.connection
    connected = false
    message = nil
    var context = OpenContext()
    context.firstOpen = true
    context.animate = !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
    listEntrance = context
    listGeneration += 1
    flashStatus(flash)
    Task { await refresh() }
  }

  /// The stored password stays (no pairing on this dashboard yet, or no answer): today's login keeps working.
  func didKeepPassword() {
    connection = session.connection
    Task { await refresh() }
  }

  func cancelRepair() {
    connection = session.connection
  }

  func beginRepair() {
    guard let connection = session.connection else { signIn.showFirstRun(); return }
    signIn.lastSyncedAt = lastSyncedAt
    signIn.beginRepair(connection: connection)
  }

  /// Settings › Pair, for a tray still on its stored password.
  func pairNow() {
    guard session.connection?.hasPassword == true else { beginRepair(); return }
    signIn.startSecuring()
  }

  /// Settings › Disconnect: the dashboard revokes this key, the tray forgets it and shows the first-run screen with
  /// the last address filled in.
  func disconnect() {
    guard !isPreview else { return }
    let address = session.connection?.baseURL.absoluteString ?? ""
    Task { @MainActor in
      let told = await session.disconnect()
      connectionGeneration += 1
      connection = session.connection
      dashboard = nil
      connected = false
      openProgress = [:]
      message = nil
      recordStatus(connected: false)
      syncMenuBar()
      signIn.showDisconnected(at: Date(), told: told, address: address, credentialKind: session.connection?.signedOut?.credentialKind)
    }
  }

  /// Section 9: a 401 device code. The key is deleted, polling stops, and the signed-out screen shows who and when if
  /// the dashboard said so. During a migration's first check the stored password comes back instead.
  func handleSignedOut(_ note: SignedOutNote) {
    session.signOut(note)
    connectionGeneration += 1
    connection = session.connection
    openProgress = [:]
    message = nil
    if let restored = session.connection, restored.hasPassword {
      Task { await refresh() }
      return
    }
    dashboard = nil
    connected = false
    recordStatus(connected: false)
    syncMenuBar()
    signIn.lastSyncedAt = lastSyncedAt
    if let saved = session.connection { signIn.showSignedOut(note, connection: saved) } else { signIn.showFirstRun() }
  }

  /// One action's failure: a sign-out goes to its screen; anything else is the panel's message.
  private func fail(_ error: Error) {
    if case BarClientError.signedOut(let note) = error { handleSignedOut(note) } else { message = error.localizedDescription }
  }

  private func flashStatus(_ text: String) {
    statusFlash = text
    Task { @MainActor in
      try? await Task.sleep(nanoseconds: 3_000_000_000)
      if statusFlash == text { statusFlash = nil }
    }
  }

  /// Offline renders only: show Settings › Connection as a paired tray (an example connection, never a saved one).
  func previewPaired(_ example: BarConnection, check: AuthCheck?) {
    guard isPreview else { return }
    connection = example
    connectionCheck = check
  }

  /// The panel opened: later opens use the panel's own entrance again.
  func panelOpened() { listEntrance = nil }

  /// Settings › Connection's "This connection" line, from the public `GET /api/auth/check`.
  func readConnectionInfo() {
    guard !isPreview, let base = session.connection?.baseURL, !checkingConnectionInfo else { return }
    checkingConnectionInfo = true
    Task { @MainActor in
      let probe = DashboardProbe(baseURL: base)
      let answer = await bounded(10) { try? await probe.check() } ?? nil
      connectionCheck = answer
      checkingConnectionInfo = false
    }
  }

  /// Offline checks only: a preview tray takes a new dashboard the way a refresh delivers one (a forced refresh
  /// at open or the refresh timer). Does nothing in the app.
  func previewReplace(_ value: AccountDashboard) {
    guard isPreview else { return }
    dashboard = value
    syncMenuBar()
  }

  func refresh(force: Bool = false) async {
    guard !isPreview, !isRefreshing, busyAction == nil, let client else { return }
    let generation = connectionGeneration
    isRefreshing = true
    var replaced = false
    var signedOut: SignedOutNote?
    do {
      let value = try await client.dashboard(refresh: force)
      guard value.schemaVersion == 1 else { throw BarClientError.decoding }
      if generation == connectionGeneration {
        dashboard = value
        pendingResets = value.pendingResetKeys()
        configureRefreshTimer(value.settings?.validatedInterval ?? 60)
        connected = true
        lastSyncedAt = Date()
        message = nil
        recordStatus(connected: true)
        // The pending migration check, devices/me and rotation (sections 7 and 8).
        do { try await session.maintain() } catch BarClientError.signedOut(let note) { signedOut = note } catch {}
        connection = session.connection
      } else { replaced = true }
    } catch BarClientError.signedOut(let note) {
      if generation == connectionGeneration { signedOut = note } else { replaced = true }
    } catch {
      if generation == connectionGeneration {
        connected = false
        message = ConnectionCheckError.isInsecureAddressBlock(error) ? SignInCopy.useNumericAddress : error.localizedDescription
        recordStatus(connected: false)
      } else { replaced = true }
    }
    isRefreshing = false
    // The menu-bar label always follows a delivery, even while the panel's own publishes stay silent.
    syncMenuBar()
    if let signedOut { handleSignedOut(signedOut); return }
    // A new connection landed while this sample was in flight: read it now.
    if replaced { await refresh(); return }
    await retryMigrationIfDue()
  }

  /// Section 8 while the tray runs: a stored password whose pairing was refused (trust off, no pairing yet) or got no
  /// answer at launch is traded for a key once `migrationDue`, so turning on "Trust this local network" later needs no
  /// restart. It runs quietly behind the list; only a rejected password opens the pairing screen, as at launch.
  private func retryMigrationIfDue() async {
    guard !isPreview, !retryingMigration, !signIn.active, session.connection?.hasPassword == true, session.migrationDue
    else { return }
    retryingMigration = true
    defer { retryingMigration = false }
    switch await session.migrate() {
    case .secured, .pending:
      connectionGeneration += 1
      connection = session.connection
      flashStatus("Secured · this tray now signs in with a device key")
    case .needsPassword(let tries):
      if let saved = session.connection, saved.hasPassword {
        signIn.lastSyncedAt = lastSyncedAt
        signIn.showPasswordNeeded(connection: saved, triesLeft: tries)
      }
    case .keptPassword, .rateLimited:
      break
    }
  }

  /// The refresh timer's first step (F6): a window whose reset passes while the panel is open turns into "Reset at
  /// ... · new reading pending" on this tick, even when the refresh after it is skipped or fails. Nothing redraws
  /// unless a window changed state.
  func reevaluateResets() {
    guard let dashboard else { return }
    let now = dashboard.pendingResetKeys()
    guard now != pendingResets else { return }
    pendingResets = now
    // A flip can take the menu-bar number down to the logo alone, so the label follows it too.
    syncMenuBar()
    publish()
  }

  private func recordStatus(connected: Bool) {
    // Next to the connection file: ~/.ccs/bar for the installed tray, the isolated folder for checks.
    let file = session.fileURL.deletingLastPathComponent().appendingPathComponent("native-status.json")
    let value: [String: Any] = [
      "connected": connected,
      "checkedAt": AccountFormatting.iso8601(Date()),
      "accountCount": dashboard?.accounts.count ?? 0,
      "providers": Array(Set(dashboard?.accounts.map(\.provider) ?? [])).sorted(),
    ]
    do {
      try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]).write(to: file, options: .atomic)
      try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
    } catch { /* Diagnostic persistence never affects account controls or prints private data. */ }
  }

  /// Every meter reading now on screen, keyed by account and window.
  var currentReadings: [String: Double] {
    var values: [String: Double] = [:]
    for account in dashboard?.visibleAccounts ?? [] {
      for window in account.visibleWindows where account.pendingReset(window) == nil {
        if let used = window.meterUsedPercent { values["\(account.id)|\(window.key)"] = used }
      }
    }
    return values
  }

  // MARK: Claude

  /// Claude "Open on Mac" and "Open on Windows". The POST goes out once with `Prefer: respond-async`; a 202 turns
  /// into a read-poll of the profile list whose progress the row shows, and a 200 is today's finished Open. The row's
  /// actions stay disabled for the whole poll, and no second Open starts for the same account.
  func openClaude(_ account: DashboardAccount, platform: String = "mac") {
    guard account.provider == "claude", let profile = account.capabilities.claudeProfileId,
      account.capabilities.claudePlatforms.contains(platform), let client else { return }
    guard openProgress[account.id]?.running != true, busyAction == nil else { return }
    let key = account.id
    let generation = connectionGeneration
    openProgress[key] = ClaudeOpenProgress(platform: platform, text: ClaudeOpenFlow.starting, finished: false, opened: false)
    Task { @MainActor in
      var outcome: ClaudeOpenProgress?
      var failure: String?
      do {
        outcome = try await ClaudeOpenFlow.run(client: client, coordinator: openCoordinator, profile: profile,
          platform: platform, polling: openPolling, sleep: { interval in
            try await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
            // A verified Change replaced the connection: stop reading it, and never resume after it.
            let current = await MainActor.run { self.connectionGeneration }
            guard generation == current else { throw CancellationError() }
          }, progress: { value in await self.report(key: key, generation: generation, value) })
      } catch BarClientError.signedOut(let note) {
        openProgress[key] = nil
        if generation == connectionGeneration { handleSignedOut(note) }
        return
      } catch { failure = error.localizedDescription }
      guard generation == connectionGeneration else { openProgress[key] = nil; return }
      // Nothing came back: the POST failed, or another Open for this Claude profile refused it. Either way this
      // account's row rests again; an Open that is really running belongs to its own account's entry.
      guard let outcome else {
        openProgress[key] = nil
        if let failure { message = failure }
        return
      }
      if outcome.opened {
        // "Opened" stays on the row while the new sample is read, then the row returns to its own secondary text.
        await refresh()
        if generation == connectionGeneration { openProgress[key] = nil }
      } else {
        openProgress[key] = nil
        message = outcome.text
      }
    }
  }

  /// One Open's progress on its row, dropped when a verified Change replaced the connection while it ran.
  private func report(key: String, generation: Int, _ value: ClaudeOpenProgress) {
    guard generation == connectionGeneration else { return }
    openProgress[key] = value
  }

  func openDashboard() {
    guard let base = connection?.baseURL else { return }
    NSWorkspace.shared.open(base)
  }
}
