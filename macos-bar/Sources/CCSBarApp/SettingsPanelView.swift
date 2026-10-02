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
  @State private var editingConnection = false

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
              row(title: "Menu bar shows", sub: menuBarPreview) {
                HStack(spacing: 8) {
                  Picker("Menu bar account", selection: $prefs.menuBarSource) {
                    Text("Active Codex account").tag(MenuBarSource.codex)
                    Text("Active Antigravity account").tag(MenuBarSource.antigravity)
                    Text("Logo only").tag(MenuBarSource.none)
                  }
                  .pickerStyle(.menu).labelsHidden().fixedSize()
                  Picker("Menu bar value", selection: $prefs.menuBarMode) {
                    Text("% left").tag(MenuBarMode.left)
                    Text("% used").tag(MenuBarMode.used)
                  }
                  .pickerStyle(.segmented).labelsHidden().fixedSize()
                  .disabled(prefs.menuBarSource == .none)
                }
              }
            }
            card {
              row(title: "Open shortcut", sub: "Option-Command-A opens or closes this panel from any app. Opening AI Account Center again from Spotlight, Launchpad or Finder also opens it.") {
                HStack(spacing: 8) {
                  ShortcutKeys(keys: ["⌥", "⌘", "A"]).opacity(prefs.openShortcutEnabled ? 1 : 0.45)
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
        .onAppear { if state.scrollToAbout { proxy.scrollTo("about", anchor: .bottom); state.scrollToAbout = false } }
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

  private var menuBarPreview: String {
    if prefs.menuBarSource == .none { return "The Apex glyph only." }
    if let reading = model.menuBarReading(prefs) { return "Now \(reading.detail)" }
    return "No active \(prefs.menuBarSource == .codex ? "Codex" : "Antigravity") account reported · logo only"
  }

  @ViewBuilder private func connectionCard(_ palette: TrayPalette) -> some View {
    card {
      Text("Connection").font(.system(size: 13, weight: .semibold)).foregroundStyle(palette.label).padding(.bottom, 8)
      HStack(spacing: 12) {
        Image(systemName: "laptopcomputer").font(.system(size: 15)).foregroundStyle(palette.label2)
          .frame(width: 34, height: 34)
          .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(palette.label4, lineWidth: 0.5))
        VStack(alignment: .leading, spacing: 2) {
          if let connection = model.connection {
            (Text("Signed in as ") + Text(connection.username).fontWeight(.semibold))
              .font(.system(size: 12.5)).foregroundStyle(palette.label)
            (Text(model.connected ? "Connected" : "Not connected").foregroundColor(model.connected ? palette.goodText : palette.warnText).fontWeight(.semibold)
              + Text(" · \(connection.baseURL.absoluteString)")
              + Text(model.lastSyncedAt.map { " · last synced \(TrayFormat.relative($0))" } ?? ""))
              .font(.system(size: 12)).foregroundStyle(palette.label2).lineLimit(1).truncationMode(.middle)
          } else {
            Text("Not connected").font(.system(size: 12.5, weight: .semibold)).foregroundStyle(palette.label)
            Text("Sign in with your dashboard address, username and password.").font(.system(size: 12)).foregroundStyle(palette.label2)
          }
        }
        Spacer(minLength: 8)
        Button(editingConnection ? "Cancel" : (model.connection == nil ? "Connect" : "Change")) {
          withAnimation(.trayValue(duration: 0.3)) { editingConnection.toggle() }
        }
        .buttonStyle(.glass).controlSize(.regular)
      }
      if editingConnection {
        ConnectionForm(model: model, compact: true) { withAnimation(.trayValue(duration: 0.3)) { editingConnection = false } }
          .padding(.top, 10)
          .transition(.opacity.combined(with: .move(edge: .top)))
      }
      HStack(alignment: .top, spacing: 6) {
        Image(systemName: "lock.shield").font(.system(size: 11.5)).foregroundStyle(palette.label2)
        Text("The login is stored only in ~/.ccs/bar, readable by you alone. Device pairing, which keeps the tray signed in through a password change, arrives with the dashboard update.")
          .font(.system(size: 11.5)).foregroundStyle(palette.label2).fixedSize(horizontal: false, vertical: true)
      }
      .padding(.top, 10)
    }
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
        Text("Hidden on the dashboard").font(.system(size: 12.5)).foregroundStyle(palette.label2).frame(width: 170, alignment: .leading)
        Text(hiddenLine(dashboard)).font(.system(size: 12.5)).foregroundStyle(palette.label)
        Button("Change in dashboard") { model.openDashboard() }.buttonStyle(.link).font(.system(size: 12.5))
          .disabled(model.connection == nil)
        Spacer(minLength: 0)
      }
      .padding(.vertical, 6)
      .overlay(alignment: .top) { Rectangle().fill(palette.separator).frame(height: 0.5) }
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
    let count = dashboard?.visibleAccounts.filter { $0.provider == "antigravity" }.count ?? 0
    guard count >= 2 else { return count == 0 ? "No Antigravity accounts" : "Starts with a second account" }
    guard let status = dashboard?.antigravityAutoSwitch else { return "Status unavailable" }
    var line = "\(status.enabled ? "On" : "Off") · switches at \(status.thresholdUsedPercent)% used"
    if let poll = status.pollIntervalSeconds { line += " · checks every \(poll) s" }
    if status.requestedPoolId == nil { line += " · no quota pool chosen" }
    return line
  }

  private func hiddenLine(_ dashboard: AccountDashboard?) -> String {
    let hidden = (dashboard?.hiddenProviders ?? []).sorted().map(ProviderMark.name)
    return hidden.isEmpty ? "None" : hidden.joined(separator: ", ")
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

/// Key caps for the open shortcut.
struct ShortcutKeys: View {
  let keys: [String]
  var body: some View {
    withPalette { palette in
      HStack(spacing: 3) {
        ForEach(keys, id: \.self) { key in
          Text(key).font(.system(size: 12, weight: .medium)).foregroundStyle(palette.label)
            .frame(minWidth: 20, minHeight: 20)
            .background(palette.controlInner, in: RoundedRectangle(cornerRadius: 5, style: .continuous))
        }
      }
      .accessibilityElement(children: .ignore)
      .accessibilityLabel("Option Command A")
    }
  }
}

/// The dashboard address and login, saved privately. Used for first-run connect and for Change.
struct ConnectionForm: View {
  @ObservedObject var model: AccountsViewModel
  var compact = false
  var onDone: () -> Void = {}
  @State private var baseURL = ""
  @State private var username = ""
  @State private var password = ""
  @State private var showPassword = false
  @State private var error: String?

  var body: some View {
    withPalette { palette in
      VStack(alignment: .leading, spacing: 10) {
        field("Dashboard address", palette) { TextField("http://host:3000", text: $baseURL).textFieldStyle(.roundedBorder) }
        field("Username", palette) { TextField("Dashboard username", text: $username).textFieldStyle(.roundedBorder) }
        field("Password", palette) {
          HStack(spacing: 6) {
            Group {
              if showPassword { TextField("Dashboard password", text: $password) } else { SecureField("Dashboard password", text: $password) }
            }.textFieldStyle(.roundedBorder)
            Button { showPassword.toggle() } label: {
              Image(systemName: showPassword ? "eye.slash" : "eye").frame(width: 26, height: 22)
            }
            .buttonStyle(.plain).foregroundStyle(palette.label2)
            .help(showPassword ? "Hide password" : "Show password")
          }
        }
        if URL(string: baseURL.trimmingCharacters(in: .whitespacesAndNewlines))?.scheme?.lowercased() == "http" {
          HStack(alignment: .top, spacing: 6) {
            Image(systemName: "info.circle").font(.system(size: 11.5))
            Text("This address uses plain HTTP, so the password crosses your network unencrypted. Use HTTPS or an SSH tunnel where you can.")
              .fixedSize(horizontal: false, vertical: true)
          }.font(.system(size: 11.5)).foregroundStyle(palette.label2)
        }
        if let error { Text(error).font(.system(size: 12)).foregroundStyle(palette.critText) }
        HStack {
          Spacer()
          Button(action: save) {
            Text(compact ? "Save" : "Connect").font(.system(size: 13, weight: .semibold)).frame(minWidth: compact ? 60 : 120)
          }
          .buttonStyle(.glassProminent).tint(palette.accent).controlSize(.large)
          .keyboardShortcut(.defaultAction)
        }
      }
      .onAppear {
        baseURL = model.connection?.baseURL.absoluteString ?? "http://192.168.50.179:3000"
        username = model.connection?.username ?? ""
        password = ""
      }
    }
  }

  private func field<Content: View>(_ title: String, _ palette: TrayPalette, @ViewBuilder _ content: () -> Content) -> some View {
    VStack(alignment: .leading, spacing: 4) {
      Text(title).font(.system(size: 12, weight: .medium)).foregroundStyle(palette.label2)
      content()
    }
  }

  private func save() {
    do {
      try model.saveConnection(baseURL: baseURL, username: username, password: password)
      password = ""
      error = nil
      onDone()
    } catch {
      self.error = "Enter an http or https dashboard address without a path, plus the dashboard username and password."
    }
  }
}

/// First run: the panel shows the connect form in place of the list.
struct ConnectView: View {
  @ObservedObject var model: AccountsViewModel
  var body: some View {
    withPalette { palette in
      PanelScroll {
        VStack(alignment: .center, spacing: 14) {
          AppIconImage(size: 60)
          Text("Connect this Mac to AI Account Center").font(.system(size: 17, weight: .semibold)).foregroundStyle(palette.label)
          Text("Sign in with your dashboard address, username and password. The login stays on this Mac in a file only you can read; provider credentials never leave the server.")
            .font(.system(size: 12.5)).foregroundStyle(palette.label2).multilineTextAlignment(.center)
            .frame(maxWidth: 440).fixedSize(horizontal: false, vertical: true)
          if let message = model.message {
            Text(message).font(.system(size: 12)).foregroundStyle(palette.critText).multilineTextAlignment(.center)
          }
          ConnectionForm(model: model).frame(maxWidth: 440)
            .padding(14).groupPlatter(padding: 0)
        }
        .padding(.horizontal, 16).padding(.vertical, 18)
        .frame(maxWidth: .infinity)
        .background(GeometryReader { geometry in
          Color.clear.preference(key: OverlayHeightKey.self, value: geometry.size.height)
        })
      }
    }
  }
}
