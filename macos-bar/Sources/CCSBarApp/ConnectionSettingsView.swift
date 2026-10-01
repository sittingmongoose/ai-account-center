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

struct ConnectionSettingsView: View {
  @ObservedObject var model: AccountsViewModel
  var onClose: (() -> Void)? = nil
  @Environment(\.dismiss) private var dismiss
  @State private var baseURL = ""
  @State private var username = ""
  @State private var password = ""
  @State private var launchAtLogin = LaunchAtLogin.enabled
  @State private var error: String?

  var body: some View {
    VStack(alignment: .leading, spacing: 14) {
      Text("Connect AI Account Center").font(.title3.weight(.semibold))
      Text("Use your AI Account Center dashboard address and login. Account credentials remain on the server."
        + (URL(string: baseURL.trimmingCharacters(in: .whitespacesAndNewlines))?.scheme?.lowercased() == "http"
          ? "\nHTTP does not encrypt this connection. Use HTTPS or an encrypted SSH tunnel." : ""))
        .font(.caption).foregroundStyle(.secondary)
      Form {
        TextField("Dashboard address", text: $baseURL)
        TextField("Username", text: $username)
        SecureField("Password", text: $password)
        Toggle("Open AI Account Center when I sign in", isOn: $launchAtLogin)
      }
      if let error { Text(error).font(.caption).foregroundStyle(AccountsPalette.coral) }
      HStack {
        Spacer()
        Button("Cancel") { password = ""; closeWindow() }
        Button("Save") { save() }.buttonStyle(.borderedProminent).tint(AccountsPalette.accent)
          .keyboardShortcut(.defaultAction)
      }
    }
    .padding(22).frame(width: 430)
    .background(AccountsPalette.plate).environment(\.colorScheme, .dark).preferredColorScheme(.dark)
    .background(SettingsWindowRegistration(model: model).frame(width: 0, height: 0).allowsHitTesting(false))
    .onAppear {
      baseURL = model.connection?.baseURL.absoluteString ?? "http://192.168.50.179:3000"
      username = model.connection?.username ?? ""
      password = model.connection?.password ?? ""
      launchAtLogin = LaunchAtLogin.enabled
    }
  }

  private func save() {
    do {
      guard let url = URL(string: baseURL.trimmingCharacters(in: .whitespacesAndNewlines)),
        ["http", "https"].contains(url.scheme ?? ""), url.host != nil,
        url.user == nil, url.password == nil, url.query == nil, url.fragment == nil,
        url.path.isEmpty || url.path == "/", !username.isEmpty, !password.isEmpty
      else { throw BarClientError.invalidConnection }
      let config = BarConnection(baseURL: url, username: username, password: password)
      let directory = BarConnection.configURL.deletingLastPathComponent()
      try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
        attributes: [.posixPermissions: 0o700])
      try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
      try JSONEncoder().encode(config).write(to: BarConnection.configURL, options: .atomic)
      try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: BarConnection.configURL.path)
      try LaunchAtLogin.setEnabled(launchAtLogin)
      password = ""
      model.configure()
      closeWindow()
    } catch { self.error = error.localizedDescription }
  }

  private func closeWindow() {
    if let onClose { onClose() }
    else { dismiss() }
  }
}

private struct SettingsWindowRegistration: NSViewRepresentable {
  let model: AccountsViewModel

  func makeNSView(context: Context) -> SettingsWindowRegistrationView {
    let view = SettingsWindowRegistrationView()
    view.model = model
    return view
  }

  func updateNSView(_ view: SettingsWindowRegistrationView, context: Context) {
    guard view.model !== model else { return }
    view.model = model
    view.registerWindow()
  }
}

private final class SettingsWindowRegistrationView: NSView {
  weak var model: AccountsViewModel?

  override func viewDidMoveToWindow() {
    super.viewDidMoveToWindow()
    registerWindow()
  }

  func registerWindow() {
    guard let window, let model else { return }
    SettingsWindowController.shared.register(window: window, model: model)
  }
}
