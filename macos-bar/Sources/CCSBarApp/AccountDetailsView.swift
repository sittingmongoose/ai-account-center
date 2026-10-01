import SwiftUI
import CCSBarCore

struct AccountDetailsView: View {
  @ObservedObject var model: AccountsViewModel
  let account: DashboardAccount
  var panelWidth: CGFloat = 760
  var detailHeight: CGFloat = 420
  @State private var showInformation = false

  private var primaryWindows: [AccountQuotaWindow] {
    account.compactWindows
  }

  var body: some View {
    HStack(spacing: 12) {
      VStack(alignment: .leading, spacing: 4) {
        HStack(spacing: 5) {
          if account.provider == "codex" && account.isActive {
            Image(systemName: "checkmark.circle.fill").foregroundStyle(AccountsPalette.green)
          }
          Text(account.identity).font(.system(size: 11, weight: account.isActive ? .semibold : .medium)).lineLimit(1)
        }.help(account.identity)
        HStack(spacing: 5) {
          Circle().fill(account.status == "ok" ? AccountsPalette.green : AccountsPalette.amber).frame(width: 5, height: 5)
          if let plan = account.plan { Text(plan) }
          Text(account.platform.capitalized)
          if account.status == "cached" { Text("Cached") }
          if account.status == "needs_sign_in" { Text("Sign-in needed") }
        }.font(.system(size: 8)).foregroundStyle(AccountsPalette.muted).lineLimit(1)
      }.frame(width: panelWidth > 640 ? 211 : 160, alignment: .leading)
      if primaryWindows.isEmpty {
        Text(account.message ?? "Usage unavailable").font(.system(size: 10)).foregroundStyle(AccountsPalette.muted)
          .lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
      } else {
        HStack(spacing: 14) {
          ForEach(primaryWindows) { quota in
            PrimaryQuotaView(quota: quota, percentageOnly: account.provider == "codex", showReset: true)
          }
        }.frame(maxWidth: .infinity)
      }
      HStack(spacing: 12) {
        if account.provider == "codex" {
          if account.isActive {
            Label("Active", systemImage: "checkmark.circle.fill").font(.system(size: 9))
              .foregroundStyle(AccountsPalette.green)
          } else if account.canActivate {
            Button("Activate") { model.activate(account) }
              .buttonStyle(.bordered).controlSize(.mini).font(.system(size: 9)).tint(AccountsPalette.accent)
              .disabled(model.busyAction != nil || model.isRefreshing)
              .background(RowActionExclusion().allowsHitTesting(false))
          }
        }
        if account.provider == "claude", account.capabilities.claudeProfileId != nil {
          if account.capabilities.claudePlatforms.contains("mac") {
            NativeTooltipButton(symbol: "apple.logo", tooltip: "Open \(account.identity) on Mac", enabled: model.busyAction == nil && !model.isRefreshing, identifier: "claude-mac-\(account.id)") {
              model.openClaude(account, platform: "mac")
            }.frame(width: 18, height: 26)
          }
          if account.capabilities.claudePlatforms.contains("windows") {
            NativeTooltipButton(symbol: "", customImage: WindowsMark.nativeImage, tooltip: "Open \(account.identity) on Windows", enabled: model.busyAction == nil && !model.isRefreshing, identifier: "claude-windows-\(account.id)") {
              model.openClaude(account, platform: "windows")
            }.frame(width: 18, height: 26)
          }
        }
        if model.busyAction == account.id { ProgressView().controlSize(.mini) }
      }.frame(width: account.provider == "codex" ? 65 : 48)
    }.padding(.horizontal, 13).padding(.vertical, 9)
      .background(account.provider == "codex" && account.isActive ? AccountsPalette.green.opacity(0.09) : .clear)
      .overlay(alignment: .leading) {
        if account.provider == "codex" && account.isActive {
          RoundedRectangle(cornerRadius: 2).fill(AccountsPalette.green).frame(width: 3).padding(.vertical, 7)
        }
      }
      .foregroundStyle(AccountsPalette.text)
      .overlay {
        NativeTooltipButton(symbol: "", tooltip: "Usage details for \(account.identity): windows, balances and reset times", identifier: "account-row-\(account.id)", isDetailsRow: true) { showInformation = true }
          .frame(maxWidth: .infinity, maxHeight: .infinity)
          .popover(isPresented: $showInformation, arrowEdge: .trailing) {
            ScrollView { AccountInformationView(account: account).padding(16) }
              .frame(width: min(640, panelWidth - 40), height: detailHeight)
              .background(AccountsPalette.plate).environment(\.colorScheme, .dark)
              .accessibilityIdentifier("account-details-\(account.id)")
          }
      }
  }
}

struct AccountInformationView: View {
  let account: DashboardAccount
  var body: some View {
    VStack(alignment: .leading, spacing: 9) {
      Text(account.identity).font(.system(size: 12, weight: .semibold))
      Text(account.source).font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
      if let date = AccountFormatting.date(account.sampledAt ?? account.fetchedAt) {
        Text("Sampled \(date.formatted(date: .abbreviated, time: .shortened))")
          .font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
      }
      if let message = account.message { Text(message).font(.system(size: 10)).foregroundStyle(AccountsPalette.muted) }
      if account.visibleWindows.isEmpty {
        Text("Usage unavailable").font(.system(size: 10)).foregroundStyle(AccountsPalette.muted)
      } else {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 205), spacing: 10)], alignment: .leading, spacing: 9) {
          ForEach(account.visibleWindows) { quota in AccountQuotaView(quota: quota) }
        }
      }
    }.foregroundStyle(AccountsPalette.text)
  }
}

struct AccountQuotaView: View {
  let quota: AccountQuotaWindow
  var body: some View {
    VStack(alignment: .leading, spacing: 4) {
      if let cached = AccountFormatting.cachedSample(status: quota.status, sampledAt: quota.sampledAt) {
        Text(cached).font(.system(size: 9)).foregroundStyle(AccountsPalette.amber)
      }
      HStack {
        Text(quota.label).font(.system(size: 10))
        Spacer()
        if quota.enabled == false {
          Text("Disabled").font(.system(size: 10)).foregroundStyle(AccountsPalette.muted)
        } else if quota.unlimited == true {
          Text("Unlimited").font(.system(size: 10)).foregroundStyle(AccountsPalette.green)
        } else if let used = quota.clampedUsedPercent {
          Text("\(used.formatted(.number.precision(.fractionLength(0...2))))% used").monospacedDigit().font(.system(size: 10, weight: .medium))
            .foregroundStyle(AccountsPalette.quota(used))
        } else if let remaining = quota.clampedRemainingPercent {
          Text("\(remaining.formatted(.number.precision(.fractionLength(0...2))))% remaining").monospacedDigit().font(.system(size: 10, weight: .medium))
            .foregroundStyle(AccountsPalette.muted)
        } else if let remaining = quota.remaining {
          Text("\(number(remaining)) \(quota.unit ?? "") remaining")
            .font(.system(size: 10)).foregroundStyle(AccountsPalette.muted)
        } else if let used = quota.used, let limit = quota.limit {
          Text("\(number(used)) / \(number(limit)) \(quota.unit ?? "")")
            .font(.system(size: 10)).foregroundStyle(AccountsPalette.muted)
        } else if let used = quota.used {
          Text("\(number(used)) \(quota.unit ?? "") used").font(.system(size: 10)).foregroundStyle(AccountsPalette.muted)
        } else if let limit = quota.limit {
          Text("Limit \(number(limit)) \(quota.unit ?? "")").font(.system(size: 10)).foregroundStyle(AccountsPalette.muted)
        } else { Text("Unavailable").font(.system(size: 10)).foregroundStyle(AccountsPalette.muted) }
      }
      if let used = quota.clampedUsedPercent {
        GeometryReader { geometry in
          ZStack(alignment: .leading) {
            Capsule().fill(.white.opacity(0.10))
            Capsule().fill(AccountsPalette.quota(used)).frame(width: geometry.size.width * min(used, 100) / 100)
          }
        }.frame(height: 5)
      }
      if quota.clampedUsedPercent != nil || quota.clampedRemainingPercent != nil || quota.remaining != nil || quota.unlimited == true || quota.enabled == false {
        if let used = quota.used, let limit = quota.limit {
          Text("\(number(used)) / \(number(limit)) \(quota.unit ?? "")")
            .font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
        } else if let used = quota.used {
          Text("\(number(used)) \(quota.unit ?? "") used").font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
        } else if let limit = quota.limit {
          Text("Limit \(number(limit)) \(quota.unit ?? "")").font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
        }
      }
      if quota.clampedUsedPercent != nil || quota.clampedRemainingPercent != nil || quota.unlimited == true || quota.enabled == false {
        if let remaining = quota.remaining {
          Text("\(number(remaining)) \(quota.unit ?? "") remaining")
            .font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
        }
      }
      if quota.unlimited == true && quota.enabled == false {
        Text("Unlimited allowance").font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
      }
      if quota.enabled == true && quota.kind == "extra_usage" {
        Text("Extra usage enabled").font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
      }
      if quota.resetAt != nil || quota.kind != "balance" {
        Text(AccountFormatting.reset(quota.resetAt)).font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
      }
      if quota.expiresAt != nil || quota.kind == "balance" {
        Text(AccountFormatting.expiration(quota.expiresAt)).font(.system(size: 9)).foregroundStyle(AccountsPalette.muted)
      }
    }.padding(10).background(AccountsPalette.plate.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
  }

  private func number(_ value: Double) -> String {
    value.formatted(.number.precision(.fractionLength(0...2)))
  }
}
