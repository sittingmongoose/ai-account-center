import SwiftUI
import AppKit
import CCSBarCore

enum AccountsLayout {
  static var screen: NSScreen? {
    NSScreen.screens.first(where: { $0.frame.contains(NSEvent.mouseLocation) }) ?? NSScreen.main
  }
  static var panelWidth: CGFloat { min(760, max(400, (screen?.visibleFrame.width ?? 808) - 48)) }
  static var maximumContentHeight: CGFloat { max(220, (screen?.visibleFrame.height ?? 880) - 175) }
}

enum AccountsPalette {
  static let plate = Color(red: 0.043, green: 0.090, blue: 0.157)
  static let card = Color(red: 0.063, green: 0.125, blue: 0.200)
  static let border = Color(red: 0.125, green: 0.212, blue: 0.306)
  static let track = Color(red: 0.125, green: 0.227, blue: 0.333)
  static let accent = Color(red: 0.204, green: 0.596, blue: 1.0)
  static let indigo = accent
  static let text = Color(red: 0.902, green: 0.941, blue: 1.0)
  static let muted = Color(red: 0.643, green: 0.757, blue: 0.894)
  static let green = Color(red: 0.17, green: 0.81, blue: 0.52)
  static let amber = Color(red: 0.86, green: 0.67, blue: 0.31)
  static let coral = Color(red: 0.91, green: 0.46, blue: 0.36)
  static let red = Color(red: 0.85, green: 0.34, blue: 0.31)
  static func quota(_ used: Double) -> Color { accent }
}

struct AccountsMenuView: View {
  @ObservedObject var model: AccountsViewModel
  @Environment(\.openWindow) private var openWindow
  @State private var showAutoSettings = false
  @State private var showQwenPacks = false
  @State private var detailedProviders: Set<String> = []
  @State private var measuredContentHeight: CGFloat = 0

  private var panelWidth: CGFloat {
    AccountsLayout.panelWidth
  }
  private var maximumContentHeight: CGFloat {
    AccountsLayout.maximumContentHeight
  }
  private var contentHeight: CGFloat {
    model.dashboard == nil ? 170 : min(maximumContentHeight, measuredContentHeight > 0 ? measuredContentHeight : maximumContentHeight)
  }

  var body: some View {
    VStack(spacing: 0) {
      header
      ScrollView {
        VStack(alignment: .leading, spacing: 8) {
          if let error = model.message { errorBanner(error) }
          if let dashboard = model.dashboard {
            ForEach(dashboard.providerGroups) { group in
              if group.id == "claude" || group.id == "codex" { accountSection(group) }
              else { providerCard(group) }
            }
            if dashboard.accounts.isEmpty {
              Text("No accounts are available yet.").font(.caption).foregroundStyle(AccountsPalette.muted).padding(20)
            }
          } else if model.isRefreshing {
            HStack { ProgressView().controlSize(.small); Text("Loading accounts…").font(.caption) }
              .frame(maxWidth: .infinity).padding(.vertical, 28)
          } else {
            Button("Connect to AI Account Center") { settings() }.buttonStyle(.borderedProminent).tint(AccountsPalette.accent)
              .frame(maxWidth: .infinity).padding(.vertical, 20)
          }
        }.padding(.horizontal, 18).padding(.bottom, 10)
          .fixedSize(horizontal: false, vertical: true)
          .background(GeometryReader { geometry in
            Color.clear.preference(key: AccountsContentHeight.self, value: geometry.size.height)
          })
      }
      .onPreferenceChange(AccountsContentHeight.self) { measuredContentHeight = $0 }
      .scrollIndicators(.hidden)
      .scrollDisabled(measuredContentHeight > 0 && measuredContentHeight <= maximumContentHeight)
      .frame(height: contentHeight)
      footer
    }
    .frame(width: panelWidth)
    .background(AccountsPalette.plate)
    .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(AccountsPalette.border, lineWidth: 1))
    .clipShape(RoundedRectangle(cornerRadius: 14))
    .foregroundStyle(AccountsPalette.text)
    .environment(\.colorScheme, .dark)
    .preferredColorScheme(.dark)
    .onAppear { Task { await model.refresh(force: true) } }
    .alert(item: $model.pendingCodexSwitch) { offer in
      Alert(title: Text("Switch Codex account?"), message: Text(offer.warningText),
        primaryButton: .destructive(Text("Yes — Stop, Switch, Restart")) { model.confirmCodexSwitch(offer) },
        secondaryButton: .cancel { model.cancelCodexSwitch() })
    }
  }

  private var header: some View {
    HStack(spacing: 12) {
      CCSStackMark().frame(width: 28, height: 28)
      Text("AI Account Center").font(.system(size: 20, weight: .semibold))
      Spacer()
      if model.isRefreshing { ProgressView().controlSize(.mini) }
      Circle().fill(model.connected ? AccountsPalette.green : AccountsPalette.amber)
        .frame(width: 7, height: 7)
      Text(model.connected ? "Connected" : "Connecting")
        .font(.system(size: 11)).foregroundStyle(AccountsPalette.muted)
      NativeTooltipButton(symbol: "ellipsis", tooltip: "AI Account Center menu", identifier: "header-menu", menuActions: [
        NativeMenuAction(title: "About AI Account Center") { openWindow(id: "about"); NSApplication.shared.activate(ignoringOtherApps: true) },
        NativeMenuAction(title: "Quit AI Account Center") { NSApplication.shared.terminate(nil) },
      ]) {}.frame(width: 24, height: 26)
    }.padding(.horizontal, 18).padding(.vertical, 10)
  }

  private func errorBanner(_ error: String) -> some View {
    HStack(alignment: .top, spacing: 8) {
      Image(systemName: "exclamationmark.circle").foregroundStyle(AccountsPalette.amber)
      Text(error).font(.caption).fixedSize(horizontal: false, vertical: true)
    }.padding(10).frame(maxWidth: .infinity, alignment: .leading)
      .background(AccountsPalette.amber.opacity(0.10), in: RoundedRectangle(cornerRadius: 9))
  }

  private func automaticControls(_ status: CodexAutoSwitch) -> some View {
    HStack(spacing: 6) {
      Text("Auto-switch").font(.system(size: 10)).foregroundStyle(AccountsPalette.muted)
      Toggle("Codex automatic switching", isOn: Binding(
        get: { status.enabled }, set: { model.toggleAutomaticSwitching($0) }
      )).labelsHidden().toggleStyle(.switch).controlSize(.small).tint(AccountsPalette.accent)
        .accessibilityIdentifier("codex-auto-switch")
        .disabled(model.busyAction != nil || model.isRefreshing)
      Picker("Used quota threshold", selection: Binding(
        get: { 100 - Int(status.thresholdPercent) },
        set: { model.setAutomaticThreshold(usedPercent: $0) }
      )) {
        ForEach(thresholdOptions(status), id: \.self) { value in Text("at \(value)%").tag(value) }
      }.labelsHidden().pickerStyle(.menu).frame(width: 82)
        .accessibilityIdentifier("codex-auto-threshold")
        .disabled(model.busyAction != nil || model.isRefreshing)
      NativeTooltipButton(symbol: "info.circle", tooltip: "Codex automatic switching settings", identifier: "codex-auto-info") {
        showAutoSettings = true
      }.frame(width: 24, height: 26)
        .popover(isPresented: $showAutoSettings, arrowEdge: .top) {
          VStack(alignment: .leading, spacing: 9) {
            Text("Codex automatic switching").font(.system(size: 12, weight: .semibold))
            Text(status.message).font(.system(size: 11))
            Text("Switch at \(Int(status.thresholdPercent))% remaining (\(100 - Int(status.thresholdPercent))% used). Check every \(status.pollIntervalSeconds) seconds. Switching waits until Codex is idle. Claude accounts stay manual.")
              .font(.system(size: 10)).foregroundStyle(AccountsPalette.muted)
          }.padding(16).frame(width: 350).foregroundStyle(AccountsPalette.text)
            .background(AccountsPalette.plate).environment(\.colorScheme, .dark)
        }
    }
  }

  private func thresholdOptions(_ status: CodexAutoSwitch) -> [Int] {
    Array(Set([85, 90, 95, 98, 100 - Int(status.thresholdPercent)])).sorted()
  }

  private func accountSection(_ group: ProviderGroup) -> some View {
    VStack(alignment: .leading, spacing: 5) {
      HStack(spacing: 7) {
        ProviderMark(provider: group.id).frame(width: 21, height: 21)
        Text(group.label).font(.system(size: 12, weight: .semibold))
        Text("(\(group.accounts.count))").font(.system(size: 10)).foregroundStyle(AccountsPalette.muted)
        Spacer()
      }.padding(.horizontal, 4).padding(.top, 3)
      VStack(spacing: 0) {
        ForEach(Array(group.accounts.enumerated()), id: \.element.id) { index, account in
          if index > 0 { Rectangle().fill(AccountsPalette.border.opacity(0.55)).frame(height: 1).padding(.horizontal, 12) }
          AccountDetailsView(model: model, account: account, panelWidth: panelWidth, detailHeight: min(420, maximumContentHeight))
        }
      }.background(AccountsPalette.card, in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(AccountsPalette.border.opacity(0.65), lineWidth: 1))
    }
  }

  private func providerCard(_ group: ProviderGroup) -> some View {
    HStack(spacing: 12) {
      HStack(spacing: 11) {
        ProviderMark(provider: group.id).frame(width: group.id == "cursor" ? 34 : 30, height: group.id == "cursor" ? 34 : 30)
        VStack(alignment: .leading, spacing: 3) {
          Text(group.label).font(.system(size: 12, weight: .semibold)).lineLimit(1)
          HStack(spacing: 5) {
            Circle().fill(group.representative.status == "ok" ? AccountsPalette.green : AccountsPalette.amber)
              .frame(width: 5, height: 5)
            Text(group.statusLabel).font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
          }
        }
      }.frame(width: panelWidth > 640 ? 211 : 160, alignment: .leading)
      if group.primaryWindows.isEmpty {
        Text(group.representative.message ?? "Usage unavailable")
          .font(.system(size: 10)).foregroundStyle(AccountsPalette.muted).lineLimit(2)
          .frame(maxWidth: .infinity, alignment: .leading)
      } else {
        HStack(spacing: 14) {
          ForEach(group.primaryWindows) { quota in
            PrimaryQuotaView(quota: quota, percentageOnly: group.id == "muse", showReset: true, showFractionPercentage: group.id == "qwen")
          }
          if group.id == "qwen", !qwenPacks(group).isEmpty {
            qwenPackChip(group)
          } else if group.primaryWindows.count < 3,
            let extra = group.supplementaryWindows.first(where: { item in !group.primaryWindows.contains(where: { $0.key == item.key }) }) {
            SupplementaryQuotaChip(quota: extra)
          }
        }.frame(maxWidth: .infinity)
      }
    }.padding(.horizontal, 13).padding(.vertical, 9)
      .background(AccountsPalette.card, in: RoundedRectangle(cornerRadius: 10))
      .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(AccountsPalette.border.opacity(0.65), lineWidth: 1))
      .overlay {
        NativeTooltipButton(symbol: "", tooltip: "\(group.label) usage details: windows, balances and reset times", identifier: "provider-row-\(group.id)", isDetailsRow: true) { detailedProviders.insert(group.id) }
          .frame(maxWidth: .infinity, maxHeight: .infinity)
          .popover(isPresented: Binding(
            get: { detailedProviders.contains(group.id) },
            set: { if !$0 { detailedProviders.remove(group.id) } }
          ), arrowEdge: .trailing) {
            ScrollView {
              VStack(alignment: .leading, spacing: 18) {
                ForEach(group.accounts) { account in AccountInformationView(account: account) }
              }.padding(16)
            }.frame(width: min(640, panelWidth - 40), height: min(480, maximumContentHeight))
              .background(AccountsPalette.plate).environment(\.colorScheme, .dark)
              .accessibilityIdentifier("provider-details-\(group.id)")
          }
      }
  }

  private func qwenPacks(_ group: ProviderGroup) -> [QwenPackEntry] {
    group.accounts.flatMap { account in
      account.visibleWindows.filter { $0.key.hasPrefix("addon-pack-") }
        .map { QwenPackEntry(account: account, quota: $0) }
    }
  }

  private func qwenPackChip(_ group: ProviderGroup) -> some View {
    let packs = qwenPacks(group)
    return NativeTooltipButton(symbol: "gift", title: "\(packs.count) packs", tooltip: "View every Qwen credit pack", identifier: "qwen-packs") {
      showQwenPacks = true
    }.frame(width: 88, height: 27)
      .background(AccountsPalette.track.opacity(0.4), in: RoundedRectangle(cornerRadius: 7))
      .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(AccountsPalette.border, lineWidth: 1))
      .popover(isPresented: $showQwenPacks, arrowEdge: .bottom) {
        QwenPacksPopover(packs: packs, showAccount: group.accounts.count > 1,
          maximumHeight: min(420, maximumContentHeight), width: min(430, panelWidth - 40))
      }
  }

  private var footer: some View {
    HStack(spacing: 8) {
      if let status = model.dashboard?.codexAutoSwitch { automaticControls(status) }
      Spacer(minLength: 8)
      NativeTooltipButton(symbol: "", title: "Dashboard", tooltip: "Open account dashboard", enabled: model.connection != nil, identifier: "footer-dashboard") { model.openDashboard() }
        .frame(width: 70, height: 28)
      NativeTooltipButton(symbol: "arrow.clockwise", tooltip: "Refresh account usage", enabled: !model.isRefreshing && model.busyAction == nil, identifier: "footer-refresh") { Task { await model.refresh(force: true) } }
        .frame(width: 28, height: 28)
      NativeTooltipButton(symbol: "gearshape", tooltip: "Connection and startup settings", identifier: "footer-settings", dismissWindowOnPress: true) { settings() }
        .frame(width: 28, height: 28)
    }.padding(.horizontal, 12).padding(.vertical, 6)
      .background(AccountsPalette.card, in: RoundedRectangle(cornerRadius: 10))
      .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(AccountsPalette.border, lineWidth: 1))
      .padding(.horizontal, 18).padding(.top, 6).padding(.bottom, 10)
  }

  private func settings() {
    SettingsWindowController.shared.show(model: model)
  }
}

private struct QwenPackEntry: Identifiable {
  let account: DashboardAccount
  let quota: AccountQuotaWindow
  var id: String { "\(account.id):\(quota.key)" }
}

private struct QwenPacksPopover: View {
  let packs: [QwenPackEntry]
  let showAccount: Bool
  let maximumHeight: CGFloat
  let width: CGFloat

  var body: some View {
    VStack(alignment: .leading, spacing: 12) {
      HStack {
        Text("Qwen credit packs").font(.system(size: 13, weight: .semibold))
        Spacer()
        Text("\(packs.count) listed").font(.system(size: 10)).foregroundStyle(AccountsPalette.muted)
      }
      ScrollView {
        VStack(alignment: .leading, spacing: 8) {
          ForEach(packs) { pack in
            VStack(alignment: .leading, spacing: 5) {
              Text(pack.quota.label).font(.system(size: 11, weight: .medium))
              if showAccount {
                Text(pack.account.identity).font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
              }
              HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text(amount(pack.quota)).font(.system(size: 10)).monospacedDigit()
                Spacer(minLength: 8)
                Text(status(pack.quota)).font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
              }
              if pack.quota.remaining != nil, let used = pack.quota.used, let limit = pack.quota.limit {
                Text("\(quantity(used)) / \(quantity(limit))\(unit(pack.quota)) used")
                  .font(.system(size: 9)).monospacedDigit().foregroundStyle(AccountsPalette.muted)
              }
              Text(AccountFormatting.expiration(pack.quota.expiresAt))
                .font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
            }.frame(maxWidth: .infinity, alignment: .leading).padding(10)
              .background(AccountsPalette.card, in: RoundedRectangle(cornerRadius: 8))
              .accessibilityElement(children: .contain)
              .accessibilityIdentifier("qwen-pack-\(pack.quota.key)")
          }
        }
      }.scrollIndicators(.visible).accessibilityIdentifier("qwen-packs-list")
    }.padding(16).frame(width: width, height: min(maximumHeight, CGFloat(packs.count) * (showAccount ? 113 : 99) + 66))
      .foregroundStyle(AccountsPalette.text).background(AccountsPalette.plate)
      .environment(\.colorScheme, .dark)
      .accessibilityElement(children: .contain)
      .accessibilityIdentifier("qwen-packs-popup")
  }

  private func amount(_ quota: AccountQuotaWindow) -> String {
    if let remaining = quota.remaining { return "\(quantity(remaining))\(unit(quota)) remaining" }
    if let used = quota.used, let limit = quota.limit { return "\(quantity(used)) / \(quantity(limit))\(unit(quota)) used" }
    if let limit = quota.limit { return "\(quantity(limit))\(unit(quota)) total" }
    if let used = quota.used { return "\(quantity(used))\(unit(quota)) used" }
    return "Amount not supplied"
  }

  private func status(_ quota: AccountQuotaWindow) -> String {
    if quota.enabled == false { return "Disabled" }
    if let expiry = AccountFormatting.date(quota.expiresAt), expiry <= Date() { return "Expired" }
    if quota.remaining == 0 { return "Depleted" }
    if quota.enabled == true { return "Enabled" }
    return "Status not supplied"
  }

  private func unit(_ quota: AccountQuotaWindow) -> String {
    guard let unit = quota.unit, !unit.isEmpty else { return "" }
    return " \(unit)"
  }

  private func quantity(_ value: Double) -> String {
    value.formatted(.number.precision(.fractionLength(0...2)))
  }
}

struct PrimaryQuotaView: View {
  let quota: AccountQuotaWindow
  var percentageOnly = false
  var showReset = false
  var showFractionPercentage = false
  var body: some View {
    VStack(alignment: .leading, spacing: 5) {
      HStack(spacing: 4) {
        Text(shortLabel).font(.system(size: 9)).lineLimit(2).fixedSize(horizontal: false, vertical: true)
        Spacer(minLength: 2)
        Text(value).font(.system(size: 9)).monospacedDigit().foregroundStyle(AccountsPalette.muted).lineLimit(1)
      }
      if let used = quota.clampedUsedPercent {
        GeometryReader { geometry in
          ZStack(alignment: .leading) {
            Capsule().fill(AccountsPalette.track)
            Capsule().fill(AccountsPalette.accent).frame(width: geometry.size.width * min(used, 100) / 100)
          }
        }.frame(height: 6)
      } else { Color.clear.frame(height: 6) }
      if showReset {
        Text(AccountFormatting.reset(quota.resetAt)).font(.system(size: 8)).foregroundStyle(AccountsPalette.muted)
          .lineLimit(1).minimumScaleFactor(0.8)
      }
    }.frame(maxWidth: .infinity).help("\(quota.label): \(value). \(AccountFormatting.reset(quota.resetAt))")
  }
  private var shortLabel: String {
    let label = quota.label.trimmingCharacters(in: .whitespacesAndNewlines)
    // Scoped feature/model labels describe different quotas even at the same cadence.
    switch label.lowercased().replacingOccurrences(of: "_", with: "-") {
    case "5h", "5-hour", "5 hour", "five-hour", "five hour", "five hours": return "5H"
    case "week", "weekly", "seven-day", "seven day": return "Weekly"
    default:
      if !label.isEmpty { return quota.label }
      switch quota.key.lowercased().replacingOccurrences(of: "_", with: "-") {
      case "five-hour", "5h": return "5H"
      case "seven-day", "weekly", "week": return "Weekly"
      default: return quota.label
      }
    }
  }
  private var value: String {
    if quota.enabled == false { return "Disabled" }
    if quota.unlimited == true { return "Unlimited" }
    if !percentageOnly, let used = quota.used, let limit = quota.limit {
      let fraction = "\(quantity(used)) / \(quantity(limit))"
      if showFractionPercentage, let percent = quota.clampedUsedPercent { return "\(fraction) · \(quantity(percent))%" }
      return fraction
    }
    if let used = quota.clampedUsedPercent { return "\(used.formatted(.number.precision(.fractionLength(0...2))))%" }
    if let remaining = quota.clampedRemainingPercent { return "\(remaining.formatted(.number.precision(.fractionLength(0...2))))% left" }
    if let remaining = quota.remaining { return "\(quantity(remaining)) \(quota.unit ?? "")" }
    return "Unavailable"
  }
  private func quantity(_ value: Double) -> String { value.formatted(.number.precision(.fractionLength(0...2))) }
}

struct SupplementaryQuotaChip: View {
  let quota: AccountQuotaWindow
  var body: some View {
    HStack(spacing: 6) {
      Image(systemName: "gift").foregroundStyle(AccountsPalette.accent)
      Text(quota.remaining.map { "\($0.formatted(.number.precision(.fractionLength(0...2)))) \(quota.unit ?? "") left" }
        ?? (quota.enabled == false ? "Extra usage off" : quota.label))
        .font(.system(size: 9)).lineLimit(1)
    }.padding(.horizontal, 8).padding(.vertical, 7)
      .background(AccountsPalette.track.opacity(0.4), in: RoundedRectangle(cornerRadius: 7))
      .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(AccountsPalette.border, lineWidth: 1))
      .help(quota.label)
  }
}

private struct AccountsContentHeight: PreferenceKey {
  static let defaultValue: CGFloat = 0
  static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = nextValue() }
}
