import SwiftUI
import AppKit
import CCSBarCore

/// Shared per-open context: the values meters start from and the stagger of the open.
struct OpenContext {
  var from: [String: Double] = [:]
  var firstOpen = false
  var animate = true
  /// The live login per provider (Codex, Antigravity) when the panel opened, so the check draws in only
  /// when the dashboard reports a different live login while the panel is open.
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
  var slot: CGFloat { provider == "claude" ? TrayMetrics.claudeSlot : TrayMetrics.actionSlot }
  /// Codex and Antigravity report which saved account is the live login (the dashboard's Active mark); Claude does not.
  var marksLiveLogin: Bool { provider != "claude" }

  static func make(_ provider: String, accounts: [DashboardAccount]) -> SectionLayout {
    switch provider {
    case "claude":
      return SectionLayout(provider: provider, columns: [("5h", "5-hour"), ("week", "Weekly")]
        + (accounts.contains(where: \.isMaxPlan) ? [("fable", "Fable")] : []))
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
  let open: OpenContext
  let block: Int
  var maxDetailHeight: CGFloat = 520
  @State private var hovered = false
  @State private var showDetails = false
  @Environment(\.trayPreviewHover) private var previewHover

  private var isActive: Bool { layout.marksLiveLogin && account.isActive }
  /// Hover from the pointer, or the row a still render is asked to show hovered.
  private var rowHovered: Bool { hovered || previewHover == account.id }

  var body: some View {
    withPalette { palette in
      HStack(spacing: layout.gap) {
        ProviderMark(provider: account.provider, size: 20).frame(width: TrayMetrics.markColumn)
        identity(palette).frame(width: layout.identity, alignment: .leading)
        if account.provider == "antigravity" && account.status == "needs_sign_in" {
          Text("No readings until the supervised CLI login finishes")
            .font(.system(size: 12)).foregroundStyle(palette.label3)
            .frame(maxWidth: .infinity, alignment: .leading)
        } else if account.antigravityNoQuota {
          // A plan with no Antigravity entitlement (quotaPolicy "none") and nothing reported reads as
          // one honest line instead of meters, like the needs-sign-in line.
          Text("No Antigravity quota on this plan")
            .font(.system(size: 12)).foregroundStyle(palette.label3)
            .frame(maxWidth: .infinity, alignment: .leading)
        } else {
          ForEach(layout.columns, id: \.key) { column in
            cell(column.key).frame(maxWidth: .infinity, alignment: .leading)
          }
        }
        // The slot keeps its width when it shows nothing (an inactive Codex row), so the meter columns never shift.
        ZStack(alignment: layout.provider == "claude" ? .trailing : .leading) {
          Color.clear.frame(width: layout.slot, height: 1)
          actions(palette)
        }
        .frame(width: layout.slot, alignment: layout.provider == "claude" ? .trailing : .leading)
        // The disclosure chevron reserves a trailing column inside the row (the pattern the provider
        // rows already used), so it sits inside the hover highlight and the active row's platter
        // instead of overhanging the row's trailing edge, and the meters never move when it appears.
        Image(systemName: "chevron.right").font(.system(size: 11, weight: .semibold)).foregroundStyle(palette.label3)
          .frame(width: TrayMetrics.chevronColumn)
          .opacity(rowHovered || showDetails ? 1 : 0).offset(x: rowHovered || showDetails ? 0 : -3)
          .alignmentProbe("chevron|\(account.id)")
      }
      .padding(.leading, TrayMetrics.rowLeading).padding(.trailing, TrayMetrics.rowTrailing).padding(.vertical, 6)
      .frame(minHeight: 45)
      .background {
        ConcentricRectangle().fill(rowHovered && !isActive ? palette.rowHover : .clear)
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
    if let signIn = account.signInNeededText {
      // Claude: which computer needs a sign-in before Open; the plan and last reading follow.
      let needed = Text(verbatim: signIn).foregroundColor(palette.warnText).fontWeight(.semibold)
      var rest = [TrayFormat.planLabel(account.plan, provider: account.provider)].filter { !$0.isEmpty }
      if let sampled = AccountFormatting.date(account.sampledAt ?? account.fetchedAt) { rest.append(TrayFormat.relative(sampled)) }
      return rest.isEmpty ? needed : Text("\(needed) · \(rest.joined(separator: " · "))")
    }
    if account.status == "needs_sign_in" {
      let needed = Text(verbatim: "Sign-in needed").foregroundColor(palette.warnText).fontWeight(.semibold)
      return Text("\(needed) · \(TrayFormat.platformName(account.provenance))")
    }
    var parts = [TrayFormat.planLabel(account.plan, provider: account.provider), TrayFormat.platformName(account.provenance)].filter { !$0.isEmpty }
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
            hovered: rowHovered)
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
        meter(window)
      } else {
        Color.clear.frame(height: 1)
      }
    default:
      if let window = account.visibleWindows.first(where: { $0.key == column }) {
        meter(window)
      } else {
        missingCell(column)
      }
    }
  }

  /// An Antigravity column this account does not report: an honest muted cell from the plan facts
  /// ("Weekly only" on a weekly-only plan's 5-hour cell, "Not on plan" when the plan has no Claude/GPT
  /// models), or the existing unavailable treatment when the plan does not explain the gap. Never a
  /// fake 0%.
  private func missingCell(_ column: String) -> some View {
    let missing = account.antigravityMissingCell(column)
    return MeterView(key: "\(account.id)|\(column)", window: nil, unavailableText: missing.text,
      unavailableHelp: missing.help, hovered: rowHovered)
  }

  private func meter(_ window: AccountQuotaWindow) -> some View {
    let key = "\(account.id)|\(window.key)"
    return MeterView(key: key, window: window, sampledAt: account.sampledAt, pendingReset: account.pendingReset(window), hovered: rowHovered,
      motion: open.motion(key, block: block))
  }

  @ViewBuilder private func actions(_ palette: TrayPalette) -> some View {
    switch account.provider {
    case "claude":
      ClaudeOpenPair(model: model, account: account)
    case "codex":
      // The live ~/.codex login on Ubuntu: a fact the dashboard marks Active too.
      if account.isActive {
        ActiveLabel(platform: account.provenance, draw: open.activeAtOpen["codex"] != nil && open.activeAtOpen["codex"] != account.id,
          probe: "slot|active-\(account.id)")
      }
    default:
      antigravityAction(palette)
    }
  }

  /// The dashboard's own slot: the Active mark on the live login, or Finish setup for a login still signing in.
  /// Nothing else: with switching gone (2026-11-14) no row asks for, or reports the lack of, an active account.
  @ViewBuilder private func antigravityAction(_ palette: TrayPalette) -> some View {
    if account.isActive {
      ActiveLabel(platform: account.provenance, draw: open.activeAtOpen["antigravity"] != nil && open.activeAtOpen["antigravity"] != account.id,
        probe: "slot|active-\(account.id)")
    } else if account.status == "needs_sign_in" {
      SlotButton(title: "Finish setup", help: "Finish the supervised login in the dashboard's Accounts and Settings",
        id: "finish-setup-\(account.id)") { model.openDashboard() }
    }
  }
}

/// Open on Mac / Open on Windows: two visually separate glass buttons with a clear gap. Behaviour,
/// tooltips and the in-button progress spinner are unchanged; only the joined capsule is gone.
struct ClaudeOpenPair: View {
  @ObservedObject var model: AccountsViewModel
  let account: DashboardAccount
  @Environment(\.trayStaticRender) private var staticRender
  @Environment(\.colorScheme) private var scheme
  @Environment(\.colorSchemeContrast) private var contrast

  var body: some View {
    let platforms = account.capabilities.claudeProfileId == nil ? [] : account.capabilities.claudePlatforms.filter { ["mac", "windows"].contains($0) }
    if !platforms.isEmpty && staticRender {
      HStack(spacing: TrayMetrics.openPairGap) {
        ForEach(platforms, id: \.self) { platform in
          PlatformGlyph(platform: platform, size: 14)
            .foregroundStyle(account.signInNeededPlatforms.contains(platform) ? AnyShapeStyle(TrayPalette(scheme, increasedContrast: contrast == .increased).warnText) : AnyShapeStyle(.primary))
            .frame(width: TrayMetrics.openButton, height: TrayMetrics.openButton)
            .glassControl(circle: true)
            .hoverHelp(account.claudeOpenHelp(platform), id: "claude-\(platform)-\(account.id)")
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
              else {
                PlatformGlyph(platform: platform, size: 14)
                  .foregroundStyle(account.signInNeededPlatforms.contains(platform) ? AnyShapeStyle(TrayPalette(scheme, increasedContrast: contrast == .increased).warnText) : AnyShapeStyle(.primary))
              }
            }
            .frame(width: TrayMetrics.openButton, height: TrayMetrics.openButton)
            .contentShape(Circle())
          }
          .buttonStyle(.plain)
          .disabled(!enabled)
          .glassEffect(.regular.interactive(), in: Circle())
          .accessibilityLabel(account.signInNeededPlatforms.contains(platform) ? "Open on \(name), sign-in needed" : "Open on \(name)")
          .hoverHelp(account.claudeOpenHelp(platform), id: "claude-\(platform)-\(account.id)",
            action: enabled ? action : nil)
        }
      }
    }
  }
}

/// The section header: mark, name, count (Antigravity adds whose quota its columns are: T3's), and the column
/// captions over their meters.
struct SectionHeader: View {
  @ObservedObject var model: AccountsViewModel
  let layout: SectionLayout
  let accounts: [DashboardAccount]

  var body: some View {
    withPalette { palette in
      // Several Antigravity accounts: "In T3 (Antigravity CLI)" takes the whole first line and the captions a second;
      // one account keeps one line with the short "In T3 (CLI)".
      let twoLine = layout.provider == "antigravity" && accounts.count > 1
      VStack(alignment: .leading, spacing: 4) {
        HStack(spacing: layout.gap) {
          HStack(spacing: 8) {
            ProviderMark(provider: layout.provider, size: 16).frame(width: TrayMetrics.markColumn)
            title(palette, long: twoLine)
          }
          .frame(width: twoLine ? nil : TrayMetrics.markColumn + layout.gap + layout.identity, alignment: .leading)
          if twoLine { Spacer(minLength: 8) } else { captions(palette) }
        }
        if twoLine {
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
    // The slot, then the rows' reserved chevron column, so the captions stay exactly over their meters.
    Color.clear.frame(width: layout.slot, height: 1)
    Color.clear.frame(width: TrayMetrics.chevronColumn, height: 1)
  }

  private func title(_ palette: TrayPalette, long: Bool) -> some View {
    HStack(alignment: .firstTextBaseline, spacing: 6) {
      Text(ProviderMark.name(layout.provider)).font(.system(size: 14, weight: .semibold)).foregroundStyle(palette.label)
      Text("\(accounts.count)").font(.system(size: 12)).monospacedDigit().foregroundStyle(palette.label2)
      if layout.provider == "antigravity" {
        // The tray shows the quota the Antigravity CLI in T3 spends; the app's is in the dashboard's Details.
        Text(long ? "In T3 (Antigravity CLI)" : "In T3 (CLI)").font(.system(size: 12)).foregroundStyle(palette.label2)
      }
    }.lineLimit(1)
  }
}
