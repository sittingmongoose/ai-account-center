import SwiftUI
import AppKit
import CCSBarCore

enum LaunchAtLogin {
  static var file: URL {
    FileManager.default.homeDirectoryForCurrentUser
      .appendingPathComponent("Library/LaunchAgents/party.sittingmongoose.ccs.accounts-bar.plist")
  }
  static var enabled: Bool {
    guard let data = try? Data(contentsOf: file),
      let document = try? PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any]
    else { return false }
    return document["RunAtLoad"] as? Bool ?? true
  }
  static func setEnabled(_ enabled: Bool) throws {
    if !enabled {
      if self.enabled { try FileManager.default.removeItem(at: file) }
      return
    }
    let executable = Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/CCSBar").path
    guard FileManager.default.isExecutableFile(atPath: executable) else { throw BarClientError.invalidConnection }
    let value: [String: Any] = [
      "Label": "party.sittingmongoose.ccs.accounts-bar",
      "ProgramArguments": [executable], "RunAtLoad": true,
      "ProcessType": "Interactive", "LimitLoadToSessionType": "Aqua",
    ]
    try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
    try PropertyListSerialization.data(fromPropertyList: value, format: .xml, options: 0).write(to: file, options: .atomic)
  }
}

/// Settings, inside the panel: it slides over the account list, has its own X, and the footer gear and
/// Escape close it too.
struct SettingsPanelView: View {
  @ObservedObject var model: AccountsViewModel
  @ObservedObject var prefs: TrayPreferences
  @ObservedObject var state: PanelState
  let glass: Namespace.ID
  @State private var launchAtLogin = LaunchAtLogin.enabled
  @State private var launchError: String?
  @State private var confirmingDisconnect = false

  var body: some View {
    withPalette { palette in
      ScrollViewReader { proxy in
        PanelScroll {
          VStack(alignment: .leading, spacing: 10) {
            header(palette)
            card {
              row(title: "Appearance", sub: "Auto follows macOS.") {
                Picker("Appearance", selection: $prefs.appearance) {
                  ForEach(TrayAppearance.allCases) { option in
                    Label(option.title, systemImage: option.symbol).tag(option)
                  }
                }
                .pickerStyle(.segmented).labelsHidden().fixedSize()
                .background(RowActionExclusion())
              }
            }
            connectionCard(palette)
            card {
              Text("Menu bar").font(.system(size: 13, weight: .semibold)).foregroundStyle(palette.label).padding(.bottom, 8)
              row(title: "Show", sub: menuBarPreview) {
                Picker("Menu bar provider", selection: $prefs.menuBarProvider) {
                  ForEach(menuBarProviders, id: \.self) { provider in
                    Text(MenuBarReading.providerName(provider)).tag(provider)
                  }
                  Text("Nothing").tag(MenuBarReading.nothingProvider)
                }
                .pickerStyle(.menu).labelsHidden().fixedSize()
              }
              if prefs.menuBarProvider == "claude", !claudeAccounts.isEmpty {
                row(title: "Claude account", sub: "Claude has no active account, so pick the one to show.") {
                  Picker("Claude account", selection: Binding(
                    get: { prefs.menuBarClaudeAccountID ?? claudeAccounts.first?.id ?? "" },
                    set: { prefs.menuBarClaudeAccountID = $0 }
                  )) {
                    ForEach(claudeAccounts) { account in
                      Text(account.identity).tag(account.id)
                    }
                  }
                  .pickerStyle(.menu).labelsHidden().fixedSize()
                }
              }
              row(title: "Value", sub: "The account's 5-hour window, or its weekly window when no 5-hour window is reported.") {
                Picker("Menu bar value", selection: $prefs.menuBarMode) {
                  Text("Used").tag(MenuBarMode.used)
                  Text("Remaining").tag(MenuBarMode.remaining)
                }
                .pickerStyle(.segmented).labelsHidden().fixedSize()
                .disabled(prefs.menuBarProvider == MenuBarReading.nothingProvider)
              }
            }
            card {
              row(title: "Open shortcut", sub: "Option-Command-A opens or closes this panel from any app. Opening AI Account Center again from Spotlight, Launchpad or Finder also opens it.") {
                HStack(spacing: 8) {
                  ShortcutKeys(keys: [.symbol("option"), .symbol("command"), .letter("A")])
                    .opacity(prefs.openShortcutEnabled ? 1 : 0.45)
                  Toggle("Open shortcut", isOn: $prefs.openShortcutEnabled).labelsHidden().toggleStyle(.switch)
                    .tint(palette.accent).controlSize(.small)
                }
              }
              if let problem = state.shortcutProblem, prefs.openShortcutEnabled {
                Text(problem).font(.system(size: 11.5)).foregroundStyle(palette.warnText).padding(.top, 4)
              }
            }
            card {
              row(title: "Launch at login", sub: "Opens in the menu bar when you sign in.") {
                Toggle("Launch at login", isOn: Binding(get: { launchAtLogin }, set: { value in
                  do { try LaunchAtLogin.setEnabled(value); launchAtLogin = value; launchError = nil }
                  catch { launchError = "Launch at login can be changed only from the installed app."; launchAtLogin = LaunchAtLogin.enabled }
                })).labelsHidden().toggleStyle(.switch).tint(palette.accent).controlSize(.small)
              }
              if let launchError {
                Text(launchError).font(.system(size: 11.5)).foregroundStyle(palette.warnText).padding(.top, 4)
              }
            }
            dashboardCard(palette)
            aboutCard(palette).id("about")
          }
          .padding(.horizontal, 8).padding(.top, 2).padding(.bottom, 10)
          .background(GeometryReader { geometry in
            Color.clear.preference(key: OverlayHeightKey.self, value: geometry.size.height)
          })
        }
        .onAppear {
          if state.scrollToAbout { proxy.scrollTo("about", anchor: .bottom); state.scrollToAbout = false }
          model.readConnectionInfo()
        }
      }
    }
  }

  private func header(_ palette: TrayPalette) -> some View {
    HStack {
      Text("Settings").font(.system(size: 15, weight: .semibold)).foregroundStyle(palette.label)
      Spacer()
      GlassEffectContainer(spacing: 8) {
        let close = { state.setSettings(false) }
        Button(action: close) {
          Image(systemName: "xmark").font(.system(size: 12, weight: .semibold)).foregroundStyle(palette.label)
            .frame(width: 30, height: 30).contentShape(Circle())
        }
        .buttonStyle(.plain)
        .glassControl(circle: true)
        .glassEffectID("settings-x", in: glass)
        .glassEffectTransition(state.reduceMotion ? .identity : .materialize)
        .hoverHelp("Close settings (Esc)", id: "settings-close", action: close)
      }
    }
    .padding(.leading, 10).padding(.trailing, 4).padding(.top, 2)
  }

  private func card<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
    VStack(alignment: .leading, spacing: 0) { content() }
      .padding(.horizontal, 10).padding(.vertical, 10)
      .frame(maxWidth: .infinity, alignment: .leading)
      .groupPlatter()
  }

  private func row<Trailing: View>(title: String, sub: String, @ViewBuilder trailing: () -> Trailing) -> some View {
    let control = trailing()
    return withPalette { palette in
      HStack(alignment: .center, spacing: 12) {
        VStack(alignment: .leading, spacing: 2) {
          Text(title).font(.system(size: 13, weight: .semibold)).foregroundStyle(palette.label)
          Text(sub).font(.system(size: 12)).foregroundStyle(palette.label2).fixedSize(horizontal: false, vertical: true)
        }
        Spacer(minLength: 12)
        control
      }
    }
  }

  /// Every provider present in the data, in tray order. A stored choice that is no longer reported
  /// stays listed until it is re-picked, so the picker never shows a blank selection.
  private var menuBarProviders: [String] {
    var providers = (model.dashboard?.providerGroups ?? []).map(\.id)
    if !providers.contains(prefs.menuBarProvider) && prefs.menuBarProvider != MenuBarReading.nothingProvider {
      providers.append(prefs.menuBarProvider)
    }
    return providers
  }

  private var claudeAccounts: [DashboardAccount] {
    model.dashboard?.visibleAccounts.filter { $0.provider == "claude" } ?? []
  }

  private var menuBarPreview: String {
    if prefs.menuBarProvider == MenuBarReading.nothingProvider { return "The Apex glyph only." }
    if let reading = model.menuBarReading(prefs) { return "Now \(reading.detail)." }
    switch prefs.menuBarProvider {
    case "codex", "antigravity":
      return "No active \(MenuBarReading.providerName(prefs.menuBarProvider)) account reported · logo only"
    default:
      return "No 5-hour or weekly reading reported · logo only"
    }
  }

  /// Settings › Connection: "Paired as Mac tray, last synced ...", the computer and the saved address, and this
  /// connection as the dashboard sees it (`GET /api/auth/check`), with Re-pair and Disconnect.
  @ViewBuilder private func connectionCard(_ palette: TrayPalette) -> some View {
    card {
      Text("Connection").font(.system(size: 13, weight: .semibold)).foregroundStyle(palette.label).padding(.bottom, 8)
      HStack(alignment: .center, spacing: 12) {
        Image(systemName: connectionIcon).font(.system(size: 15)).foregroundStyle(palette.label2)
          .frame(width: 34, height: 34)
          .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(palette.label4, lineWidth: 0.5))
        VStack(alignment: .leading, spacing: 2) { connectionLines(palette) }
        Spacer(minLength: 8)
        connectionButtons(palette)
      }
      if confirmingDisconnect {
        disconnectConfirm(palette)
          .padding(.top, 10)
          .transition(.opacity.combined(with: .move(edge: .top)))
      }
      HStack(alignment: .top, spacing: 6) {
        Image(systemName: "lock.shield").font(.system(size: 11.5)).foregroundStyle(palette.label2)
        Text("This tray signs in with its own device key, not your password, so changing the dashboard password keeps it signed in. Revoking it in the dashboard signs it out.")
          .font(.system(size: 11.5)).foregroundStyle(palette.label2).fixedSize(horizontal: false, vertical: true)
      }
      .padding(.top, 10)
    }
    .animation(.trayValue(duration: 0.3), value: confirmingDisconnect)
  }

  private var connectionIcon: String {
    guard let connection = model.connection, !model.signIn.active || model.signIn.repair else { return "cable.connector.slash" }
    return connection.isPaired ? "laptopcomputer" : "key"
  }

  @ViewBuilder private func connectionLines(_ palette: TrayPalette) -> some View {
    let paired = model.connection?.isPaired == true && (!model.signIn.active || model.signIn.repair)
    if paired, let connection = model.connection {
      let kind = Text("Mac tray").fontWeight(.semibold)
      let synced = model.lastSyncedAt.map { "last synced \(TrayFormat.relative($0))" } ?? "not synced yet"
      Text("Paired as \(kind), \(synced)").font(.system(size: 12.5)).foregroundStyle(palette.label)
        .accessibilityIdentifier("connection-paired")
      Text("On \(model.deviceName) · \(connection.baseURL.absoluteString)")
        .font(.system(size: 12)).foregroundStyle(palette.label2).lineLimit(1).truncationMode(.middle)
      Text(thisConnection).font(.system(size: 12)).foregroundStyle(palette.label2).lineLimit(1)
        .accessibilityIdentifier("connection-this")
    } else if let connection = model.connection, connection.hasPassword, !model.signIn.active {
      let user = Text(verbatim: connection.username).fontWeight(.semibold)
      Text("Signed in as \(user) with a saved password").font(.system(size: 12.5)).foregroundStyle(palette.label)
      Text("On \(model.deviceName) · \(connection.baseURL.absoluteString)")
        .font(.system(size: 12)).foregroundStyle(palette.label2).lineLimit(1).truncationMode(.middle)
      Text("Pairing replaces the saved password with a device key.").font(.system(size: 12)).foregroundStyle(palette.label2)
    } else {
      Text(model.signIn.statusText).font(.system(size: 12.5, weight: .semibold)).foregroundStyle(palette.label)
      Text("Pair with your dashboard username and password to see usage.").font(.system(size: 12)).foregroundStyle(palette.label2)
        .fixedSize(horizontal: false, vertical: true)
    }
  }

  /// "This connection: 192.168.50.23, trusted local network", read when Settings opens.
  private var thisConnection: String {
    guard let check = model.connectionCheck else {
      return model.checkingConnectionInfo ? "This connection: checking" : "This connection: unavailable"
    }
    let peer = check.peer ?? "unknown address"
    if check.connection?.trusted == true { return "This connection: \(peer), trusted local network" }
    if check.secureTransport == true { return "This connection: \(peer), secure" }
    return "This connection: \(peer), not trusted"
  }

  @ViewBuilder private func connectionButtons(_ palette: TrayPalette) -> some View {
    let paired = model.connection?.isPaired == true && !model.signIn.active
    let password = model.connection?.hasPassword == true && !model.signIn.active
    HStack(spacing: 8) {
      if paired {
        Button("Re-pair") {
          confirmingDisconnect = false
          state.setSettings(false)
          model.beginRepair()
        }
        .trayGlassButton()
        .accessibilityIdentifier("connection-repair")
      } else if password {
        Button("Pair") { state.setSettings(false); model.pairNow() }
          .trayGlassButton()
      } else {
        Button("Pair") { state.setSettings(false) }
          .trayGlassButton(prominent: true, tint: palette.accent)
      }
      if paired || password {
        Button { confirmingDisconnect = true } label: {
          Text("Disconnect").foregroundStyle(palette.critText)
        }
        .trayGlassButton()
        .disabled(confirmingDisconnect)
        .accessibilityIdentifier("connection-disconnect")
      }
    }
  }

  /// Disconnect asks inline: the dashboard revokes the key, the tray forgets it, usage stops.
  @ViewBuilder private func disconnectConfirm(_ palette: TrayPalette) -> some View {
    HStack(alignment: .center, spacing: 12) {
      VStack(alignment: .leading, spacing: 2) {
        Text("Disconnect this tray?").font(.system(size: 12.5, weight: .semibold)).foregroundStyle(palette.label)
        Text("The dashboard revokes its device key, and usage stops here until you pair again.")
          .font(.system(size: 12)).foregroundStyle(palette.label2).fixedSize(horizontal: false, vertical: true)
      }
      Spacer(minLength: 8)
      Button("Cancel") { confirmingDisconnect = false }.trayGlassButton()
      Button {
        confirmingDisconnect = false
        state.setSettings(false)
        model.disconnect()
      } label: { Text("Disconnect").fontWeight(.semibold).foregroundStyle(palette.critText) }
        .trayGlassButton()
        .accessibilityIdentifier("connection-disconnect-confirm")
    }
    .padding(10)
    .background(palette.controlInner, in: RoundedRectangle(cornerRadius: 9, style: .continuous))
  }

  @ViewBuilder private func dashboardCard(_ palette: TrayPalette) -> some View {
    card {
      HStack(alignment: .firstTextBaseline, spacing: 6) {
        Text("From the dashboard").font(.system(size: 13, weight: .semibold)).foregroundStyle(palette.label)
        Text("read only").font(.system(size: 12)).foregroundStyle(palette.label2)
      }.padding(.bottom, 4)
      let dashboard = model.dashboard
      kv("Usage refresh", dashboard?.settings.map { seconds($0.refreshIntervalSeconds) } ?? "Unavailable", palette)
      kv("Codex auto-switch", dashboard.map { d in
        "\(d.codexAutoSwitch.enabled ? "On" : "Off") · switches at \(TrayFormat.number(100 - d.codexAutoSwitch.thresholdPercent))% used · checks every \(d.codexAutoSwitch.pollIntervalSeconds) s"
      } ?? "Unavailable", palette)
      kv("Antigravity auto-switch", antigravityLine(dashboard), palette)
      HStack(alignment: .firstTextBaseline, spacing: 12) {
        Text("Hidden in the tray").font(.system(size: 12.5)).foregroundStyle(palette.label2).frame(width: 170, alignment: .leading)
        Text(trayHiddenLine(dashboard)).font(.system(size: 12.5)).foregroundStyle(palette.label).lineLimit(2)
        Button("Change in dashboard") { model.openDashboard() }.buttonStyle(.link).font(.system(size: 12.5))
          .disabled(model.connection == nil)
        Spacer(minLength: 0)
      }
      .padding(.vertical, 6)
      .overlay(alignment: .top) { Rectangle().fill(palette.separator).frame(height: 0.5) }
      kv("Hidden on the dashboard", hiddenLine(dashboard), palette)
      kv("Dashboard address", model.connection?.baseURL.absoluteString ?? "Not connected", palette)
    }
  }

  private func kv(_ key: String, _ value: String, _ palette: TrayPalette) -> some View {
    HStack(alignment: .firstTextBaseline, spacing: 12) {
      Text(key).font(.system(size: 12.5)).foregroundStyle(palette.label2).frame(width: 170, alignment: .leading)
      Text(value).font(.system(size: 12.5)).foregroundStyle(palette.label).lineLimit(2)
      Spacer(minLength: 0)
    }
    .padding(.vertical, 6)
    .overlay(alignment: .top) { Rectangle().fill(palette.separator).frame(height: 0.5) }
  }

  private func seconds(_ value: Int) -> String { value < 120 ? "Every \(value) s" : "Every \(value / 60) min" }

  private func antigravityLine(_ dashboard: AccountDashboard?) -> String {
    let count = dashboard?.antigravityAccountCount ?? 0
    guard count >= 2 else { return count == 0 ? "No Antigravity accounts" : "Starts with a second account" }
    guard let status = dashboard?.antigravityAutoSwitch else { return "Status unavailable" }
    var line = "\(status.enabled ? "On" : "Off") · switches at \(status.thresholdUsedPercent)% used"
    if let poll = status.pollIntervalSeconds { line += " · checks every \(poll) s" }
    if status.requestedPoolId == nil { line += " · no quota pool chosen" }
    return line
  }

  private func hiddenLine(_ dashboard: AccountDashboard?) -> String {
    guard let dashboard else { return "Unavailable" }
    let hidden = dashboard.hiddenProviders.sorted().map(ProviderMark.name)
    return hidden.isEmpty ? "None" : hidden.joined(separator: ", ")
  }

  /// Providers switched off with "Show in tray", plus accounts hidden one by one.
  private func trayHiddenLine(_ dashboard: AccountDashboard?) -> String {
    guard let dashboard else { return "Unavailable" }
    var parts = dashboard.trayHiddenProviders.sorted().map(ProviderMark.name)
    let accounts = dashboard.trayHiddenAccounts.filter { !dashboard.trayHiddenProviders.contains($0.provider) }.count
    if accounts > 0 { parts.append("\(accounts) account\(accounts == 1 ? "" : "s")") }
    return parts.isEmpty ? "None" : parts.joined(separator: ", ")
  }

  private func aboutCard(_ palette: TrayPalette) -> some View {
    card {
      HStack(spacing: 12) {
        AppIconImage(size: 44)
        VStack(alignment: .leading, spacing: 2) {
          Text("AI Account Center").font(.system(size: 13, weight: .semibold)).foregroundStyle(palette.label)
          Text("Version \(Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "development") · Mac tray · provider marks belong to their owners")
            .font(.system(size: 12)).foregroundStyle(palette.label2).fixedSize(horizontal: false, vertical: true)
          Text("Based on CCS and CCS Bar (kaitranntt/ccs), MIT license.").font(.system(size: 11.5)).foregroundStyle(palette.label3)
        }
        Spacer(minLength: 8)
        Button {
          if let url = Bundle.main.url(forResource: "THIRD-PARTY-NOTICES", withExtension: "txt") { NSWorkspace.shared.open(url) }
        } label: {
          Label("Third-party notices", systemImage: "doc.text").font(.system(size: 12.5, weight: .medium))
        }.buttonStyle(.plain).foregroundStyle(palette.label2)
        Button {
          NSApplication.shared.terminate(nil)
        } label: {
          Label("Quit", systemImage: "power").font(.system(size: 12.5, weight: .medium))
        }.buttonStyle(.plain).foregroundStyle(palette.label2)
      }
    }
  }
}

/// Key caps for the open shortcut. Modifier keys are SF Symbols (option, command), never text glyphs.
struct ShortcutKeys: View {
  enum Key: Hashable { case symbol(String), letter(String) }
  let keys: [Key]
  var body: some View {
    withPalette { palette in
      HStack(spacing: 3) {
        ForEach(keys, id: \.self) { key in
          Group {
            switch key {
            case .symbol(let name): Image(systemName: name).font(.system(size: 11, weight: .medium))
            case .letter(let text): Text(verbatim: text).font(.system(size: 12, weight: .medium))
            }
          }
          .foregroundStyle(palette.label)
          .frame(minWidth: 20, minHeight: 20)
          .background(palette.controlInner, in: RoundedRectangle(cornerRadius: 5, style: .continuous))
        }
      }
      .accessibilityElement(children: .ignore)
      .accessibilityLabel("Option Command A")
    }
  }
}
