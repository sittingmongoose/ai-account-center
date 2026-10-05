import SwiftUI
import AppKit
import CCSBarCore

/// Shared per-open context: the values meters start from and the stagger of the open.
struct OpenContext {
  var from: [String: Double] = [:]
  var firstOpen = false
  var animate = true
  /// The active account per switchable provider when the panel opened, so the check draws in only
  /// after a switch made while it is open.
  var activeAtOpen: [String: String] = [:]

  func motion(_ key: String, block: Int) -> MeterMotion {
    let stagger = firstOpen ? TrayMotion.firstOpenStagger : TrayMotion.laterOpenStagger
    let delay = min(0.15, Double(block) * stagger) + (firstOpen ? 0.08 : 0)
    return MeterMotion(from: firstOpen ? 0 : from[key], delay: delay, animate: animate)
  }
}

/// Column layout for one account section; the header captions use the same grid.
struct SectionLayout {
  let provider: String
  let columns: [(key: String, label: String)]
  var identity: CGFloat { provider == "antigravity" ? TrayMetrics.antigravityIdentityColumn : TrayMetrics.identityColumn }
  var gap: CGFloat { provider == "antigravity" ? TrayMetrics.antigravityColumnGap : TrayMetrics.columnGap }
  var slot: CGFloat { provider == "claude" ? TrayMetrics.claudeSlot : TrayMetrics.switchSlot }
  var switchable: Bool { provider != "claude" }

  static func make(_ provider: String, accounts: [DashboardAccount]) -> SectionLayout {
    switch provider {
    case "claude":
      return SectionLayout(provider: provider, columns: [("5h", "5-hour"), ("week", "Weekly"), ("fable", "Fable")])
    case "codex":
      return SectionLayout(provider: provider, columns: [("5h", "5-hour"), ("week", "Weekly")])
    default:
      return SectionLayout(provider: provider, columns: TrayColumns.antigravity(accounts))
    }
  }
}

/// One Claude, Codex or Antigravity account row. The whole row opens Details; nested controls act alone.
struct AccountRow: View {
  @ObservedObject var model: AccountsViewModel
  let account: DashboardAccount
  let layout: SectionLayout
  let sectionAccounts: [DashboardAccount]
  let open: OpenContext
  let block: Int
  var maxDetailHeight: CGFloat = 520
  @State private var hovered = false
  @State private var showDetails = false

  private var isActive: Bool { layout.switchable && account.isActive }

  var body: some View {
    withPalette { palette in
      HStack(spacing: layout.gap) {
        ProviderMark(provider: account.provider, size: 20).frame(width: TrayMetrics.markColumn)
        identity(palette).frame(width: layout.identity, alignment: .leading)
        if account.provider == "antigravity" && account.status == "needs_sign_in" {
          Text("No readings until the supervised CLI login finishes")
            .font(.system(size: 12)).foregroundStyle(palette.label3)
            .frame(maxWidth: .infinity, alignment: .leading)
        } else {
          ForEach(layout.columns, id: \.key) { column in
            cell(column.key).frame(maxWidth: .infinity, alignment: .leading)
          }
        }
        actions(palette).frame(width: layout.slot, alignment: layout.provider == "claude" ? .trailing : .leading)
      }
      .padding(.leading, TrayMetrics.rowLeading).padding(.trailing, TrayMetrics.rowTrailing).padding(.vertical, 6)
      // The disclosure chevron floats over the row's trailing padding instead of reserving a column:
      // it overhangs the row by 6 pt into the platter and list padding, so its glyph stays inside the
      // platter while the slot content ends 8.5 pt before the row's edge.
      .overlay(alignment: .trailing) {
        Image(systemName: "chevron.right").font(.system(size: 11, weight: .semibold)).foregroundStyle(palette.label3)
          .frame(width: 14)
          .opacity(hovered || showDetails ? 1 : 0).offset(x: (hovered || showDetails ? 0 : -3) + 6)
      }
      .frame(minHeight: 45)
      .background {
        ConcentricRectangle().fill(hovered && !isActive ? palette.rowHover : .clear)
      }
      .contentShape(Rectangle())
      .overlay {
        DetailsRowTarget(tooltip: "Usage details for \(account.identity): windows, balances and reset times",
          identifier: "account-row-\(account.id)", onHover: { value in withAnimation(.easeOut(duration: 0.14)) { hovered = value } }) {
          showDetails = true
        }
        .dismissedByPanel($showDetails)
        .popover(isPresented: $showDetails, arrowEdge: .trailing) {
          AccountDetailsPopover(model: model, accounts: [account], maxHeight: maxDetailHeight)
        }
      }
      .animation(.easeOut(duration: 0.14), value: hovered)
    }
  }

  @ViewBuilder private func identity(_ palette: TrayPalette) -> some View {
    VStack(alignment: .leading, spacing: 0) {
      Text(account.identity).font(.system(size: 13, weight: isActive ? .semibold : .medium))
        .foregroundStyle(palette.label).lineLimit(1).truncationMode(.tail)
      meta(palette).font(.system(size: 11.5)).foregroundStyle(palette.label2).lineLimit(1).truncationMode(.tail)
    }
  }

  private func meta(_ palette: TrayPalette) -> Text {
    // A running Claude Open takes the row's secondary line, in the row's own secondary-text style, until it ends.
    if account.provider == "claude", let progress = model.openProgress[account.id] {
      return Text(verbatim: progress.text)
    }
    if account.status == "needs_sign_in" {
      let needed = Text(verbatim: "Sign-in needed").foregroundColor(palette.warnText).fontWeight(.semibold)
      return Text("\(needed) · \(TrayFormat.platformName(account.platform))")
    }
    var parts = [TrayFormat.planLabel(account.plan), TrayFormat.platformName(account.platform)].filter { !$0.isEmpty }
    if let sampled = AccountFormatting.date(account.sampledAt ?? account.fetchedAt) { parts.append(TrayFormat.relative(sampled)) }
    if account.status == "cached" && AccountFormatting.date(account.sampledAt ?? account.fetchedAt) == nil { parts.append("Cached") }
    if account.status == "error" { parts.append("Refresh failed") }
    if account.status == "unavailable" { parts.append("Usage unavailable") }
    return Text(parts.joined(separator: " · "))
  }

  @ViewBuilder private func cell(_ column: String) -> some View {
    switch account.provider {
    case "claude":
      if column == "fable" {
        switch account.fableCell {
        case .notApplicable: Color.clear.frame(height: 1)
        case .notReported:
          MeterView(key: "\(account.id)|seven_day_fable", window: nil, unavailableText: "Not reported yet",
            unavailableHelp: "Fable usage is not reported yet. It appears here as its own weekly window once the dashboard sends one.",
            hovered: hovered)
        case .window(let window):
          meter(window)
        }
      } else if let window = column == "5h" ? account.fiveHourWindow : account.weeklyWindow {
        meter(window)
      } else {
        Color.clear.frame(height: 1)
      }
    case "codex":
      if let window = column == "5h" ? account.fiveHourWindow : account.weeklyWindow {
        let used = Double(100) - (model.dashboard?.codexAutoSwitch.thresholdPercent ?? 5)
        meter(window, notch: used, notchOpacity: notchOpacity(enabled: model.dashboard?.codexAutoSwitch.enabled == true))
      } else {
        Color.clear.frame(height: 1)
      }
    default:
      if let window = account.visibleWindows.first(where: { $0.key == column }) {
        if let status = model.dashboard?.antigravityAutoSwitch, sectionAccounts.count >= 2,
          let pool = status.requestedPoolId, window.poolId == pool {
          meter(window, notch: Double(status.thresholdUsedPercent), notchOpacity: notchOpacity(enabled: status.enabled))
        } else {
          meter(window)
        }
      } else {
        Color.clear.frame(height: 1)
      }
    }
  }

  private func notchOpacity(enabled: Bool) -> Double {
    if !enabled { return 0.18 }
    return account.isActive ? 1 : 0.4
  }

  private func meter(_ window: AccountQuotaWindow, notch: Double? = nil, notchOpacity: Double = 1) -> some View {
    let key = "\(account.id)|\(window.key)"
    return MeterView(key: key, window: window, sampledAt: account.sampledAt, pendingReset: account.pendingReset(window), notch: notch, notchOpacity: notchOpacity, hovered: hovered,
      motion: open.motion(key, block: block))
  }

  @ViewBuilder private func actions(_ palette: TrayPalette) -> some View {
    switch account.provider {
    case "claude":
      ClaudeOpenPair(model: model, account: account)
    case "codex":
      if account.isActive {
        ActiveLabel(platform: account.platform, draw: open.activeAtOpen["codex"] != nil && open.activeAtOpen["codex"] != account.id,
          probe: "slot|active-\(account.id)")
      } else if account.canActivate {
        let switching = model.dashboard?.codexAutoSwitch.activationInProgress == true
        ActivateButton(title: "Activate", busy: model.busyAction == account.id,
          enabled: model.busyAction == nil && !model.isRefreshing && !model.hasPendingConfirmation && !switching,
          help: switching ? "Codex is switching accounts automatically. Activate again when it finishes."
            : "Make \(account.identity) the active Codex account on Ubuntu", id: "activate-\(account.id)") {
          model.activate(account)
        }
      }
    default:
      antigravityAction(palette)
    }
  }

  @ViewBuilder private func antigravityAction(_ palette: TrayPalette) -> some View {
    let anyActive = sectionAccounts.contains(where: \.isActive)
    if account.isActive {
      ActiveLabel(platform: account.platform, draw: open.activeAtOpen["antigravity"] != nil && open.activeAtOpen["antigravity"] != account.id,
        probe: "slot|active-\(account.id)")
    } else if account.status == "needs_sign_in" {
      ActivateButton(title: "Finish setup", compact: true, help: "Finish the supervised login in the dashboard's Accounts and Settings",
        id: "finish-setup-\(account.id)") { model.openDashboard() }
    } else if sectionAccounts.count < 2 {
      if !anyActive { NotReportedLabel() }
    } else if account.canActivateAntigravity {
      let switching = model.dashboard?.antigravityAutoSwitch?.activationInProgress == true
      ActivateButton(title: "Activate", busy: model.busyAction == account.id,
        enabled: model.busyAction == nil && !model.isRefreshing && !model.hasPendingConfirmation
          && model.dashboard?.canActivateAntigravity(account) == true,
        help: switching ? "Antigravity is already switching accounts on Ubuntu. Activate again when it finishes."
          : "Make \(account.identity) the active Antigravity account on Ubuntu", id: "activate-\(account.id)") {
        model.activateAntigravity(account)
      }
    } else {
      ActivateButton(title: "Activate", enabled: false,
        help: "Antigravity switching is not available for this account on Ubuntu yet. The dashboard enables it once the runtime is verified.",
        id: "activate-\(account.id)") {}
    }
  }
}

/// Open on Mac / Open on Windows: two visually separate glass buttons with a clear gap. Behaviour,
/// tooltips and the in-button progress spinner are unchanged; only the joined capsule is gone.
struct ClaudeOpenPair: View {
  @ObservedObject var model: AccountsViewModel
  let account: DashboardAccount
  @Environment(\.trayStaticRender) private var staticRender

  var body: some View {
    let platforms = account.capabilities.claudeProfileId == nil ? [] : account.capabilities.claudePlatforms.filter { ["mac", "windows"].contains($0) }
    if !platforms.isEmpty && staticRender {
      HStack(spacing: TrayMetrics.openPairGap) {
        ForEach(platforms, id: \.self) { platform in
          PlatformGlyph(platform: platform, size: 14).foregroundStyle(.primary)
            .frame(width: TrayMetrics.openButton, height: TrayMetrics.openButton)
            .glassControl(circle: true)
            .hoverHelp("Open \(account.identity) in Claude on \(platform == "mac" ? "Mac" : "Windows")", id: "claude-\(platform)-\(account.id)")
        }
      }
    } else if !platforms.isEmpty {
      HStack(spacing: TrayMetrics.openPairGap) {
        ForEach(platforms, id: \.self) { platform in
          let name = platform == "mac" ? "Mac" : "Windows"
          let running = model.openProgress[account.id]
          let enabled = model.busyAction == nil && !model.isRefreshing && running?.running != true
          let action = { model.openClaude(account, platform: platform) }
          Button(action: action) {
            ZStack {
              if model.busyAction == "\(account.id)|\(platform)" || running?.running == true && running?.platform == platform {
                ProgressView().controlSize(.mini)
              }
              else { PlatformGlyph(platform: platform, size: 14).foregroundStyle(.primary) }
            }
            .frame(width: TrayMetrics.openButton, height: TrayMetrics.openButton)
            .contentShape(Circle())
          }
          .buttonStyle(.plain)
          .disabled(!enabled)
          .glassEffect(.regular.interactive(), in: Circle())
          .hoverHelp("Open \(account.identity) in Claude on \(name)", id: "claude-\(platform)-\(account.id)",
            action: enabled ? action : nil)
        }
      }
    }
  }
}

/// The section header: mark, name, count and (except Codex, whose selected row already shows it)
/// the active account over the identity column, and the column captions over their meters.
struct SectionHeader: View {
  @ObservedObject var model: AccountsViewModel
  let layout: SectionLayout
  let accounts: [DashboardAccount]

  var body: some View {
    withPalette { palette in
      let multiAntigravity = layout.provider == "antigravity" && accounts.count > 1
      VStack(alignment: .leading, spacing: 4) {
        HStack(spacing: layout.gap) {
          HStack(spacing: 8) {
            ProviderMark(provider: layout.provider, size: 16).frame(width: TrayMetrics.markColumn)
            title(palette)
          }
          .frame(width: multiAntigravity ? nil : TrayMetrics.markColumn + layout.gap + layout.identity, alignment: .leading)
          if multiAntigravity {
            Spacer(minLength: 8)
            AntigravityAutoControls(model: model)
          } else {
            captions(palette)
          }
        }
        if multiAntigravity {
          HStack(spacing: layout.gap) {
            Color.clear.frame(width: TrayMetrics.markColumn + layout.gap + layout.identity, height: 1)
            captions(palette)
          }
        }
      }
      // Inside the section platter, which adds the 4 pt group inset: the captions stay over their meters.
      .padding(.leading, TrayMetrics.rowLeading)
      .padding(.trailing, TrayMetrics.rowTrailing)
      .padding(.top, TrayMetrics.sectionHeaderTop)
      .frame(minHeight: 22)
    }
  }

  @ViewBuilder private func captions(_ palette: TrayPalette) -> some View {
    ForEach(layout.columns, id: \.key) { column in
      Text(column.label).font(.system(size: 11, weight: .medium)).foregroundStyle(palette.label2)
        .lineLimit(1).minimumScaleFactor(0.85).frame(maxWidth: .infinity, alignment: .leading)
    }
    Color.clear.frame(width: layout.slot, height: 1)
  }

  private func title(_ palette: TrayPalette) -> some View {
    HStack(alignment: .firstTextBaseline, spacing: 6) {
      Text(ProviderMark.name(layout.provider)).font(.system(size: 14, weight: .semibold)).foregroundStyle(palette.label)
      Text("\(accounts.count)").font(.system(size: 12)).monospacedDigit().foregroundStyle(palette.label2)
      if layout.switchable { meta(palette) }
    }.lineLimit(1)
  }

  @ViewBuilder private func meta(_ palette: TrayPalette) -> some View {
    // Codex shows no header meta: it overlapped the 5-hour column and repeats the selected row.
    if layout.provider == "codex" {
      EmptyView()
    } else if let active = accounts.first(where: \.isActive) {
      let name = Text(verbatim: String(active.identity.split(separator: "@").first ?? Substring(active.identity)))
        .fontWeight(.semibold).foregroundColor(palette.label)
      Text("\(name) active")
        .font(.system(size: 12)).foregroundStyle(palette.label2)
        .transition(.opacity)
        .id(active.id)
    } else if accounts.count > 1 {
      Text("Active account not reported yet").font(.system(size: 12)).foregroundStyle(palette.label2)
    }
  }
}

/// Why Codex automatic switching is stuck, in plain words, above the Codex accounts.
/// Visible only for blocked outcomes; healthy switching adds no line and changes no layout.
struct CodexAutoStatusLine: View {
  @ObservedObject var model: AccountsViewModel

  private static let blocked: Set<String> = ["waiting_idle", "no_quota", "no_candidate", "error"]

  var body: some View {
    withPalette { palette in
      if let status = model.dashboard?.codexAutoSwitch, status.enabled,
        Self.blocked.contains(status.outcome)
      {
        Text(verbatim: CodexAutoStatusLine.text(status: status, accounts: model.dashboard?.accounts ?? []))
          .font(.system(size: 12)).foregroundStyle(palette.label2)
          .fixedSize(horizontal: false, vertical: true)
          .padding(.leading, TrayMetrics.rowLeading)
          .padding(.trailing, TrayMetrics.rowTrailing)
          .padding(.top, 2)
          .accessibilityIdentifier("codex-auto-status")
      }
    }
  }

  static func text(status: CodexAutoSwitch, accounts: [DashboardAccount]) -> String {
    guard status.outcome == "waiting_idle", let candidate = status.candidate else {
      return status.message
    }
    let identity =
      accounts.first(where: { $0.provider == "codex" && $0.capabilities.codexProfile == candidate })?
      .identity ?? candidate
    return "\(status.message) Activate \(identity) to switch now."
  }
}

/// Antigravity's own auto-switch (thresholdUsedPercent, % used), shown once two accounts exist.
struct AntigravityAutoControls: View {
  @ObservedObject var model: AccountsViewModel

  var body: some View {
    withPalette { palette in
      let status = model.dashboard?.antigravityAutoSwitch
      let canToggle = status != nil && (status?.enabled == true || model.antigravityAutoCanEnable)
        && model.busyAction == nil && !model.isRefreshing
      HStack(spacing: 6) {
        Toggle("Auto-switch", isOn: Binding(
          get: { status?.enabled == true },
          set: { model.toggleAntigravityAutomatic($0) }
        ))
        .toggleStyle(.switch).controlSize(.mini).tint(palette.accent)
        .font(.system(size: 12.5))
        .disabled(!canToggle)
        .background(RowActionExclusion())
        .hoverHelp(status == nil ? "Antigravity automatic switching status is unavailable"
          : model.antigravityAutoCanEnable || status?.enabled == true
            ? "Switch Antigravity accounts on Ubuntu automatically when the active one reaches the threshold"
            : "Choose the shared quota pool in the dashboard first; then automatic switching can start here",
          id: "antigravity-auto-switch")
        ThresholdMenu(value: status?.thresholdUsedPercent ?? 95,
          enabled: status != nil && model.busyAction == nil && !model.isRefreshing,
          id: "antigravity-threshold") { model.setAntigravityThreshold(usedPercent: $0) }
      }
      .padding(.leading, 10).padding(.trailing, 4).frame(height: 30)
      .glassControl()
    }
  }
}

/// "at 95%" with a chevron: the switch threshold, always shown as % used.
struct ThresholdMenu: View {
  let value: Int
  var enabled = true
  let id: String
  let onSelect: (Int) -> Void

  var body: some View {
    withPalette { palette in
      Menu {
        ForEach(Array(Set([85, 90, 95, 98, value])).sorted(), id: \.self) { option in
          Button {
            onSelect(option)
          } label: {
            if option == value { Label("at \(option)% used", systemImage: "checkmark") } else { Text("at \(option)% used") }
          }
        }
      } label: {
        HStack(spacing: 3) {
          Text("at").foregroundStyle(palette.label2)
          Text("\(value)%").foregroundStyle(palette.label).monospacedDigit()
          Image(systemName: "chevron.down").font(.system(size: 9, weight: .semibold)).foregroundStyle(palette.label2)
        }
        .font(.system(size: 12.5, weight: .medium))
        .padding(.horizontal, 8).frame(height: 24)
        .contentShape(Capsule())
      }
      .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
      .disabled(!enabled)
      .background(RowActionExclusion())
      .accessibilityIdentifier(id)
    }
  }
}
