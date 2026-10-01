import SwiftUI

@main
struct CCSBarApp: App {
  @StateObject private var model: AccountsViewModel

  init() {
    if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--check-native-tooltips" {
      PreviewRenderer.checkNativeTooltips(input: CommandLine.arguments[2])
    }
    if CommandLine.arguments.count == 4, CommandLine.arguments[1] == "--render-preview" {
      PreviewRenderer.render(input: CommandLine.arguments[2], output: CommandLine.arguments[3])
    }
    if CommandLine.arguments.count == 4, CommandLine.arguments[1] == "--check-native-packs" {
      PreviewRenderer.checkNativePacks(input: CommandLine.arguments[2], output: CommandLine.arguments[3])
    }
    _model = StateObject(wrappedValue: AccountsViewModel())
  }

  var body: some Scene {
    MenuBarExtra {
      AccountsMenuView(model: model)
    } label: {
      Image(nsImage: MenuBarIcon.statusImage(.color))
      Text(model.statusTitle)
    }.menuBarExtraStyle(.window)

    Window("AI Account Center settings", id: "connection") {
      ConnectionSettingsView(model: model)
    }.windowResizability(.contentSize)
      .defaultPosition(.center)

    Window("About AI Account Center", id: "about") {
      AboutView()
    }.windowResizability(.contentSize)
      .defaultPosition(.center)
  }
}
