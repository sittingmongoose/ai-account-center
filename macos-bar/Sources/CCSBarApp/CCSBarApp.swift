import SwiftUI
import AppKit

/// AI Account Center's menu-bar app. An AppKit entry point owns the status item and the Liquid Glass
/// panel directly, so the panel can also be opened by the global shortcut and by launching the app again.
@main
enum CCSBarMain {
  @MainActor static func main() {
    let arguments = CommandLine.arguments
    if arguments.count == 3, arguments[1] == "--check-native-tooltips" {
      PreviewRenderer.checkNativeTooltips(input: arguments[2])
    }
    if arguments.count == 3, arguments[1] == "--check-hover-occlusion" {
      HoverOcclusionCheck.run(input: arguments[2])
    }
    if arguments.count >= 4, arguments[1] == "--render-preview" {
      PreviewRenderer.render(input: arguments[2], output: arguments[3], options: Array(arguments.dropFirst(4)))
    }
    if arguments.count == 4, arguments[1] == "--check-native-packs" {
      PreviewRenderer.checkNativePacks(input: arguments[2], output: arguments[3])
    }
    if arguments.count == 3, arguments[1] == "--check-meter-geometry" {
      PreviewRenderer.checkMeterGeometry(input: arguments[2])
    }
    if arguments.count == 2, arguments[1] == "--check-menu-bar-prefs" {
      PreviewRenderer.checkMenuBarPrefs()
    }
    if arguments.count == 3, arguments[1] == "--self-test" {
      PanelSelfTest.run(input: arguments[2])
    }
    if arguments.count == 2, arguments[1] == "--check-signin" {
      SignInCheck.run()
    }
    if arguments.count >= 3, arguments[1] == "--e2e" {
      TrayE2E.run(arguments: Array(arguments.dropFirst(2)))
    }
    if arguments.count == 3, arguments[1] == "--toggle-test" {
      PanelToggleTest.run(input: arguments[2])
    }
    if arguments.count == 4, arguments[1] == "--click-at",
      let x = Double(arguments[2]), let y = Double(arguments[3]) {
      PanelToggleTest.clickOnce(at: CGPoint(x: x, y: y))
      exit(0)
    }

    let app = NSApplication.shared
    let delegate = AppDelegate()
    app.delegate = delegate
    app.setActivationPolicy(.accessory)
    withExtendedLifetime(delegate) { app.run() }
  }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
  var controller: PanelController?

  func applicationDidFinishLaunching(_ notification: Notification) {
    let prefs = TrayPreferences.shared
    prefs.apply()
    let controller = PanelController(model: AccountsViewModel(), prefs: prefs)
    self.controller = controller
    // A launch from Spotlight, Launchpad, Finder or the Dock opens the panel; the login item does not.
    if !launchedByLoginAgent { DispatchQueue.main.async { controller.open() } }
  }

  /// Launching the app again while it runs (Spotlight, Launchpad, Finder, the Dock or `open -a`) opens the panel.
  func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
    controller?.open()
    return false
  }

  /// launchd starts the login item with the agent's own label as its XPC service name; LaunchServices
  /// launches carry an "application." name instead.
  private var launchedByLoginAgent: Bool {
    ProcessInfo.processInfo.environment["XPC_SERVICE_NAME"] == "party.sittingmongoose.ccs.accounts-bar"
  }
}
