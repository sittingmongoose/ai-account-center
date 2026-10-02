import Foundation

/// The Open progress of one Claude profile, exactly as `GET /api/claude/desktop-profiles` reports it
/// (CONTRACT-serving-misc 4.4). Counts and states only: no UUIDs, titles, transcript text, ssh details or paths.
public struct ClaudeOpenOperation: Decodable, Sendable, Equatable {
  public let id: String
  public let platform: String
  public let state: String
  public let confirmedCount: Int?
  public let totalCount: Int?
  /// The server's fixed sentence for `blocked_uncertain` and `failed`; null otherwise.
  public let message: String?

  /// `opened`, `failed` and `blocked_uncertain` end the poll; anything else keeps it running.
  public var isTerminal: Bool { Self.terminalStates.contains(state) }
  public var isOpened: Bool { state == "opened" }

  public static let terminalStates: Set<String> = ["opened", "failed", "blocked_uncertain"]
}

/// One row of `GET /api/claude/desktop-profiles`. A profile without a manifest id carries no `openOperation`.
public struct ClaudeDesktopProfile: Decodable, Sendable {
  public let id: String?
  public let openOperation: ClaudeOpenOperation?
}

public struct ClaudeDesktopProfileList: Decodable, Sendable {
  public let profiles: [ClaudeDesktopProfile]
}

/// What the Open POST answered: today's 200, or the opt-in 202 whose operation the tray then read-polls.
public enum ClaudeOpenStart: Sendable, Equatable {
  case opened
  case accepted(operationId: String, state: String)
}

/// One Open's calm inline progress, as the account row's secondary line shows it.
public struct ClaudeOpenProgress: Sendable, Equatable {
  /// The platform button that started it, so only that button shows the spinner.
  public let platform: String
  public let text: String
  /// True once the Open ended: Claude opened, it failed, or the poll gave up.
  public let finished: Bool
  /// True only when Claude really opened.
  public let opened: Bool

  public var running: Bool { !finished }

  public init(platform: String, text: String, finished: Bool, opened: Bool) {
    self.platform = platform
    self.text = text
    self.finished = finished
    self.opened = opened
  }
}

/// How often the profile list is read while an Open runs: 1 s for the first two minutes, then every 5 s, with a hard
/// stop at three minutes (CONTRACT-serving-misc 4.4). The checks shorten every value; nothing is replayed.
public struct ClaudeOpenPolling: Sendable {
  public var fastInterval: TimeInterval
  public var slowInterval: TimeInterval
  public var slowAfter: TimeInterval
  public var timeout: TimeInterval

  public init(fastInterval: TimeInterval = 1, slowInterval: TimeInterval = 5,
    slowAfter: TimeInterval = 120, timeout: TimeInterval = 180) {
    self.fastInterval = fastInterval
    self.slowInterval = slowInterval
    self.slowAfter = slowAfter
    self.timeout = timeout
  }
}

/// One account's Open at a time. A second start for the same Claude profile is refused while the first still runs, so
/// the POST is never repeated and the row's actions stay disabled during the poll.
public actor ClaudeOpenCoordinator {
  private var running: Set<String> = []

  public init() {}

  fileprivate func start(_ profile: String) -> Bool { running.insert(profile).inserted }
  fileprivate func finish(_ profile: String) { running.remove(profile) }

  public func isRunning(_ profile: String) -> Bool { running.contains(profile) }
}

/// Claude "Open on Mac" and "Open on Windows": the POST, then (on 202) a read-poll of the profile list. The POST is
/// sent once and is never replayed, and a poll is never resumed after a restart: this flow lives in memory only.
public enum ClaudeOpenFlow {
  /// The 409 `history_unconfirmed` refusal, and the fallback for a terminal operation with no usable server message.
  public static let historyUnconfirmed = "History copy could not be confirmed. Claude was not opened."
  /// Said when the three-minute poll ends without a terminal state.
  public static let stillWorking = "Still working on the dashboard. Check again shortly."
  /// The row's text while the POST itself is in flight.
  public static let starting = "Opening"

  /// The row's calm secondary text for one reported operation, or nil for a state this tray does not know (the poll
  /// then keeps the text it already shows and keeps running).
  public static func text(for operation: ClaudeOpenOperation) -> String? {
    switch operation.state {
    case "checking": return "Copying history"
    case "copying":
      guard let confirmed = operation.confirmedCount, let total = operation.totalCount else { return "Copying history" }
      return "Copying history \(confirmed) of \(total)"
    case "opening": return "Opening"
    case "opened":
      // A bounded copy (45 s or 50 records) may open Claude before every record is across; the next Open copies
      // the rest (CLIENT API SHEET 4.7).
      if let confirmed = operation.confirmedCount, let total = operation.totalCount, confirmed < total {
        return "Opened · copied \(confirmed) of \(total)"
      }
      return "Opened"
    case "failed", "blocked_uncertain": return publicMessage(operation.message)
    default: return nil
    }
  }

  /// The server's own fixed sentence, or the client's fallback. The contract promises a fixed sentence, so a long or
  /// multiline value is not shown: server strings never reach the screen unbounded.
  public static func publicMessage(_ message: String?) -> String {
    guard let message, !message.isEmpty, message.count <= 300,
      !message.contains("\n"), !message.contains("\r")
    else { return historyUnconfirmed }
    return message
  }

  /// Runs one Open. `progress` reports the row's text at every change, including the end. Returns nil when another
  /// Open for the same account is still running (nothing at all is sent), otherwise the final progress. Errors from
  /// the POST are thrown unchanged, so the caller's existing error handling still applies.
  @discardableResult
  public static func run(client: AccountsClient, coordinator: ClaudeOpenCoordinator, profile: String, platform: String,
    polling: ClaudeOpenPolling = ClaudeOpenPolling(), now: @escaping @Sendable () -> Date = { Date() },
    sleep: @escaping @Sendable (TimeInterval) async throws -> Void = {
      try await Task.sleep(nanoseconds: UInt64($0 * 1_000_000_000))
    },
    progress: @escaping @Sendable (ClaudeOpenProgress) async -> Void) async throws -> ClaudeOpenProgress? {
    guard await coordinator.start(profile) else { return nil }
    do {
      let result = try await open(client: client, profile: profile, platform: platform, polling: polling,
        now: now, sleep: sleep, progress: progress)
      await coordinator.finish(profile)
      return result
    } catch {
      await coordinator.finish(profile)
      throw error
    }
  }

  private static func open(client: AccountsClient, profile: String, platform: String, polling: ClaudeOpenPolling,
    now: @escaping @Sendable () -> Date, sleep: @escaping @Sendable (TimeInterval) async throws -> Void,
    progress: @escaping @Sendable (ClaudeOpenProgress) async -> Void) async throws -> ClaudeOpenProgress {
    func value(_ text: String, finished: Bool, opened: Bool) -> ClaudeOpenProgress {
      ClaudeOpenProgress(platform: platform, text: text, finished: finished, opened: opened)
    }
    await progress(value(starting, finished: false, opened: false))
    // A POST that fails is thrown unchanged: the caller rests the row and shows the error the way it already does.
    let answer = try await client.openClaude(profile: profile, platform: platform)
    guard case .accepted(let operationId, _) = answer else {
      let done = value("Opened", finished: true, opened: true)
      await progress(done)
      return done
    }
    let began = now()
    var shown: String?
    while true {
      let elapsed = now().timeIntervalSince(began)
      if elapsed >= polling.timeout {
        let gaveUp = value(stillWorking, finished: true, opened: false)
        await progress(gaveUp)
        return gaveUp
      }
      try await sleep(elapsed < polling.slowAfter ? polling.fastInterval : polling.slowInterval)
      // A read that fails never ends the Open: the server keeps the operation and the POST is never replayed, so the
      // poll simply tries again until the deadline.
      guard let profiles = try? await client.claudeDesktopProfiles(),
        let operation = profiles.lazy.compactMap({ $0.id == profile ? $0.openOperation : nil })
          .first(where: { $0.platform == platform && (operationId.isEmpty || $0.id == operationId) })
      else { continue }
      if operation.isTerminal {
        let ended = value(text(for: operation) ?? historyUnconfirmed, finished: true, opened: operation.isOpened)
        await progress(ended)
        return ended
      }
      if let text = text(for: operation), shown != text {
        shown = text
        await progress(value(text, finished: false, opened: false))
      }
    }
  }
}
