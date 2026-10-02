import SwiftUI
import AppKit
import CCSBarCore

/// Details for one account, or every account of a one-row provider. A system popover: no custom
/// background view, so it gets the system glass.
struct AccountDetailsPopover: View {
  @ObservedObject var model: AccountsViewModel
  let accounts: [DashboardAccount]
  var maxHeight: CGFloat = 520

  var body: some View {
    PanelScroll {
      VStack(alignment: .leading, spacing: 18) {
        ForEach(accounts) { account in
          AccountDetailsBody(model: model, account: account)
          if account.id != accounts.last?.id { Divider() }
        }
      }
      .padding(16)
      .frame(width: 470, alignment: .leading)
    }
    .frame(width: 470)
    .frame(maxHeight: maxHeight)
    .fixedSize(horizontal: false, vertical: accounts.count == 1)
    .accessibilityIdentifier("account-details-\(accounts.first?.provider ?? "")")
  }
}

struct AccountDetailsBody: View {
  @ObservedObject var model: AccountsViewModel
  let account: DashboardAccount

  var body: some View {
    withPalette { palette in
      let meters = account.visibleWindows.filter(\.isMeter)
      let amounts = account.visibleWindows.filter { !$0.isMeter }
      VStack(alignment: .leading, spacing: 12) {
        HStack(spacing: 10) {
          ProviderMark(provider: account.provider, size: 24)
          VStack(alignment: .leading, spacing: 1) {
            Text(account.identity).font(.system(size: 14, weight: .semibold)).foregroundStyle(palette.label)
              .lineLimit(1).truncationMode(.middle)
            Text(details).font(.system(size: 11.5)).foregroundStyle(palette.label2).lineLimit(2)
          }
        }
        if let message = account.message {
          Text(message).font(.system(size: 12)).foregroundStyle(palette.label2).fixedSize(horizontal: false, vertical: true)
        }
        if meters.isEmpty && amounts.isEmpty {
          Text("Usage unavailable").font(.system(size: 12)).foregroundStyle(palette.label3)
        }
        if !meters.isEmpty {
          LazyVGrid(columns: [GridItem(.flexible(), spacing: 18, alignment: .top), GridItem(.flexible(), spacing: 18, alignment: .top)],
            alignment: .leading, spacing: 12) {
            ForEach(meters) { window in
              VStack(alignment: .leading, spacing: 3) {
                MeterView(key: "\(account.id)|\(window.key)", window: window,
                  labelText: TrayColumns.fullLabel(provider: account.provider, window),
                  motion: MeterMotion(animate: false), detail: true)
                if let used = window.used, let limit = window.limit {
                  Text("\(TrayFormat.number(used)) of \(TrayFormat.number(limit))\(window.unit.map { " \($0)" } ?? "")")
                    .font(.system(size: 11.5)).monospacedDigit().foregroundStyle(palette.label2).lineLimit(1)
                }
                if let cached = AccountFormatting.cachedSample(status: window.status, sampledAt: window.sampledAt) {
                  Text(cached).font(.system(size: 11)).foregroundStyle(palette.warnText).lineLimit(1)
                }
              }
            }
          }
        }
        if !amounts.isEmpty {
          VStack(alignment: .leading, spacing: 0) {
            ForEach(amounts) { window in AmountRow(window: window) }
          }
        }
        HStack(spacing: 14) {
          if account.provider == "codex" || account.provider == "antigravity" {
            if account.isActive {
              ActiveLabel(platform: account.platform)
            } else if account.provider == "codex" && account.canActivate {
              ActivateButton(title: "Activate", busy: model.busyAction == account.id,
                enabled: model.busyAction == nil && !model.isRefreshing && !model.hasPendingConfirmation,
                help: "Make \(account.identity) the active Codex account on Ubuntu", id: "details-activate-\(account.id)") {
                model.activate(account)
              }
            } else if account.provider == "antigravity" && account.canActivateAntigravity {
              ActivateButton(title: "Activate", busy: model.busyAction == account.id,
                enabled: model.busyAction == nil && !model.isRefreshing && !model.hasPendingConfirmation,
                help: "Make \(account.identity) the active Antigravity account on Ubuntu", id: "details-activate-\(account.id)") {
                model.activateAntigravity(account)
              }
            }
          }
          Button {
            model.openDashboard()
          } label: {
            Label("Open in dashboard", systemImage: "arrow.up.right.square").font(.system(size: 12.5, weight: .medium))
          }
          .buttonStyle(.plain).foregroundStyle(palette.label2)
          .disabled(model.connection == nil)
        }
        .padding(.top, 2)
      }
    }
  }

  private var details: String {
    var parts = [TrayFormat.planLabel(account.plan), TrayFormat.platformName(account.platform), account.source].filter { !$0.isEmpty }
    if let sampled = AccountFormatting.date(account.sampledAt ?? account.fetchedAt) {
      parts.append("sampled \(TrayFormat.relative(sampled))")
    }
    if account.status != "ok" { parts.append(TrayFormat.statusWord(account.status)) }
    return parts.joined(separator: " · ")
  }
}

/// A balance, extra-usage or spend line: what is left or spent and when it expires, exactly as reported.
struct AmountRow: View {
  let window: AccountQuotaWindow
  var body: some View {
    withPalette { palette in
      VStack(alignment: .leading, spacing: 2) {
        HStack(alignment: .firstTextBaseline) {
          Text(window.label).font(.system(size: 12.5)).foregroundStyle(palette.label)
          Spacer(minLength: 8)
          Text(value).font(.system(size: 12.5, weight: .medium)).monospacedDigit().foregroundStyle(palette.label)
        }
        if let sub { Text(sub).font(.system(size: 11)).foregroundStyle(palette.label2) }
      }
      .padding(.vertical, 6)
      .overlay(alignment: .top) { Rectangle().fill(palette.separator).frame(height: 0.5) }
    }
  }

  private func quantity(_ value: Double) -> String {
    if window.unit == "USD" { return value.formatted(.currency(code: "USD").precision(.fractionLength(0...2))) }
    return TrayFormat.number(value) + (window.unit.map { " \($0)" } ?? "")
  }

  private var value: String {
    if window.enabled == false { return window.kind == "extra_usage" ? "Off" : "Disabled" }
    if window.unlimited == true { return "Unlimited" }
    if let remaining = window.remaining { return "\(quantity(remaining)) left" }
    if let used = window.used, let limit = window.limit { return "\(quantity(used)) of \(quantity(limit))" }
    if let used = window.used { return "\(quantity(used)) used" }
    if let limit = window.limit { return "Limit \(quantity(limit))" }
    return "Not reported"
  }

  private var sub: String? {
    var parts: [String] = []
    if window.remaining != nil, let used = window.used, let limit = window.limit { parts.append("\(quantity(used)) of \(quantity(limit)) used") }
    if window.resetAt != nil { parts.append(TrayFormat.longReset(window.resetAt)) }
    if window.expiresAt != nil || window.kind == "balance" { parts.append(AccountFormatting.expiration(window.expiresAt)) }
    if window.enabled == true && window.kind == "extra_usage" { parts.append("Extra usage enabled") }
    return parts.isEmpty ? nil : parts.joined(separator: " · ")
  }
}

/// One row per other provider (today's representative rule), with up to three labelled meters.
struct ProviderRow: View {
  @ObservedObject var model: AccountsViewModel
  let group: ProviderGroup
  let open: OpenContext
  let block: Int
  var maxDetailHeight: CGFloat = 520
  @State private var hovered = false
  @State private var showDetails = false

  var body: some View {
    withPalette { palette in
      let rep = group.representative
      let meters = rep.glanceMeters
      HStack(spacing: 12) {
        ProviderMark(provider: group.id, size: 24).frame(width: 24)
        VStack(alignment: .leading, spacing: 1) {
          Text(group.label).font(.system(size: 13, weight: .semibold)).foregroundStyle(palette.label).lineLimit(1)
          Text(meta).font(.system(size: 11.5)).foregroundStyle(palette.label2).lineLimit(1)
        }
        .frame(width: 158, alignment: .leading)
        if meters.isEmpty {
          Text(rep.message ?? "Usage unavailable").font(.system(size: 12)).foregroundStyle(palette.label3)
            .lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
        } else if group.id == "qwen" {
          meter(meters[0], label: TrayColumns.shortLabel(provider: group.id, meters[0]), amount: true)
            .frame(maxWidth: .infinity, alignment: .leading)
          let packs = group.accounts.flatMap { account in account.creditPacks.map { (account, $0) } }
          if !packs.isEmpty { QwenPacksButton(packs: packs, showAccount: group.accounts.count > 1, maxHeight: maxDetailHeight) }
        } else {
          ForEach(0..<3, id: \.self) { index in
            Group {
              if index < meters.count {
                meter(meters[index], label: TrayColumns.shortLabel(provider: group.id, meters[index]), amount: meters.count == 1)
              } else {
                Color.clear.frame(height: 1)
              }
            }.frame(maxWidth: .infinity, alignment: .leading)
          }
        }
        Image(systemName: "chevron.right").font(.system(size: 11, weight: .semibold)).foregroundStyle(palette.label3)
          .opacity(hovered || showDetails ? 1 : 0).offset(x: hovered || showDetails ? 0 : -3)
          .frame(width: 14)
      }
      .padding(.leading, TrayMetrics.rowLeading).padding(.trailing, TrayMetrics.rowTrailing).padding(.vertical, 6)
      .frame(minHeight: 50)
      .background { ConcentricRectangle().fill(hovered ? palette.rowHover : .clear) }
      .contentShape(Rectangle())
      .overlay {
        DetailsRowTarget(tooltip: "\(group.label) usage details: windows, balances and reset times",
          identifier: "provider-row-\(group.id)", onHover: { value in withAnimation(.easeOut(duration: 0.14)) { hovered = value } }) {
          showDetails = true
        }
        .popover(isPresented: $showDetails, arrowEdge: .trailing) {
          AccountDetailsPopover(model: model, accounts: group.accounts, maxHeight: maxDetailHeight)
        }
      }
    }
  }

  private var meta: String {
    let rep = group.representative
    if group.accounts.count > 1 {
      let short = rep.identity.split(separator: "@").first.map(String.init) ?? rep.identity
      return "\(short) · 1 of \(group.accounts.count)"
    }
    return "\(TrayFormat.statusWord(rep.status)) \(TrayFormat.relative(AccountFormatting.date(rep.sampledAt ?? rep.fetchedAt)))"
  }

  private func meter(_ window: AccountQuotaWindow, label: String, amount: Bool) -> some View {
    let key = "\(group.representative.id)|\(window.key)"
    return MeterView(key: key, window: window, labelText: label, showAmount: amount, hovered: hovered,
      motion: open.motion(key, block: block))
  }
}

/// Qwen add-on packs: how many, how many still hold credit, and the next expiry among those; never a sum.
struct QwenPacksButton: View {
  let packs: [(DashboardAccount, AccountQuotaWindow)]
  let showAccount: Bool
  var maxHeight: CGFloat = 420
  @State private var showPacks = false

  var body: some View {
    withPalette { palette in
      let withCredit = packs.filter { ($0.1.remaining ?? 0) > 0 }
      let next = withCredit.compactMap { AccountFormatting.date($0.1.expiresAt) }.filter { $0 > Date() }.min()
      let action = { showPacks = true }
      Button(action: action) {
        HStack(spacing: 8) {
          Image(systemName: "shippingbox").font(.system(size: 13)).foregroundStyle(palette.label2)
          VStack(alignment: .leading, spacing: 0) {
            Text("\(packs.count) packs · \(withCredit.count) with credit").font(.system(size: 12, weight: .semibold))
              .foregroundStyle(palette.label)
            Text(next.map { "next expires \($0.formatted(.dateTime.month(.abbreviated).day()))" } ?? "none with credit")
              .font(.system(size: 11)).foregroundStyle(palette.label2)
          }
          Image(systemName: "chevron.down").font(.system(size: 9, weight: .semibold)).foregroundStyle(palette.label2)
        }
        .padding(.horizontal, 12).frame(height: 40)
        .contentShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
      }
      .buttonStyle(.plain)
      .glassControl(.rounded(14))
      .hoverHelp("Every Qwen credit pack, with what is left and when it expires", id: "qwen-packs", action: action)
      .popover(isPresented: $showPacks, arrowEdge: .bottom) {
        QwenPacksPopover(packs: packs, showAccount: showAccount, maxHeight: maxHeight)
      }
    }
  }
}

private struct QwenPacksPopover: View {
  let packs: [(DashboardAccount, AccountQuotaWindow)]
  let showAccount: Bool
  let maxHeight: CGFloat

  var body: some View {
    withPalette { palette in
      VStack(alignment: .leading, spacing: 10) {
        HStack {
          Text("Qwen credit packs").font(.system(size: 13, weight: .semibold))
          Spacer()
          Text("\(packs.count) listed").font(.system(size: 11.5)).foregroundStyle(palette.label2)
        }
        ScrollView {
          VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(packs.enumerated()), id: \.offset) { _, entry in
              let (account, pack) = entry
              VStack(alignment: .leading, spacing: 3) {
                Text(pack.label).font(.system(size: 12.5, weight: .medium))
                if showAccount { Text(account.identity).font(.system(size: 11)).foregroundStyle(palette.label2) }
                HStack(alignment: .firstTextBaseline) {
                  Text(amount(pack)).font(.system(size: 12)).monospacedDigit()
                  Spacer(minLength: 8)
                  Text(status(pack)).font(.system(size: 11)).foregroundStyle(palette.label2)
                }
                if pack.remaining != nil, let used = pack.used, let limit = pack.limit {
                  Text("\(TrayFormat.number(used)) / \(TrayFormat.number(limit))\(unit(pack)) used")
                    .font(.system(size: 11)).monospacedDigit().foregroundStyle(palette.label2)
                }
                Text(AccountFormatting.expiration(pack.expiresAt)).font(.system(size: 11)).foregroundStyle(palette.label2)
              }
              .frame(maxWidth: .infinity, alignment: .leading).padding(10)
              .background(palette.group, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
              .accessibilityElement(children: .contain)
              .accessibilityIdentifier("qwen-pack-\(pack.key)")
            }
          }
        }.scrollIndicators(.visible).accessibilityIdentifier("qwen-packs-list")
      }
      .padding(16).frame(width: 400)
      .frame(maxHeight: min(maxHeight, CGFloat(packs.count) * (showAccount ? 104 : 90) + 60))
      .accessibilityElement(children: .contain)
      .accessibilityIdentifier("qwen-packs-popup")
    }
  }

  private func unit(_ pack: AccountQuotaWindow) -> String { pack.unit.map { " \($0)" } ?? "" }
  private func amount(_ pack: AccountQuotaWindow) -> String {
    if let remaining = pack.remaining { return "\(TrayFormat.number(remaining))\(unit(pack)) remaining" }
    if let used = pack.used, let limit = pack.limit { return "\(TrayFormat.number(used)) / \(TrayFormat.number(limit))\(unit(pack)) used" }
    if let limit = pack.limit { return "\(TrayFormat.number(limit))\(unit(pack)) total" }
    if let used = pack.used { return "\(TrayFormat.number(used))\(unit(pack)) used" }
    return "Amount not supplied"
  }
  private func status(_ pack: AccountQuotaWindow) -> String {
    if pack.enabled == false { return "Disabled" }
    if let expiry = AccountFormatting.date(pack.expiresAt), expiry <= Date() { return "Expired" }
    if pack.remaining == 0 { return "Depleted" }
    if pack.enabled == true { return "Enabled" }
    return "Status not supplied"
  }
}
