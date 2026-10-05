import SwiftUI
import AppKit
import CCSBarCore

/// Shared state between the panel window and its SwiftUI content.
@MainActor
final class PanelState: ObservableObject {
  @Published var settingsOpen = false {
    didSet {
      coveredHover.set(settingsOpen)
      settingsHover.set(!settingsOpen)
    }
  }
  /// Hover-tag gates: the whole panel (shut while it closes), the content Settings covers (the account list or
  /// the sign-in screen), and Settings itself (shut from the moment it starts to close).
  let panelHover = HoverGate()
  let coveredHover = HoverGate()
  let settingsHover = HoverGate(suppressed: true)
  /// The account list and the sign-in screen replace each other: the one leaving is shut from the moment the
  /// swap starts, so a row tag cannot present over the incoming sign-in screen during the fade (or the reverse).
  let listHover = HoverGate()
  let signInHover = HoverGate(suppressed: true)

  func setNeedsConnection(_ needs: Bool) {
    listHover.set(needs)
    signInHover.set(!needs)
  }
  /// Bumped on every open, so the content is rebuilt and replays its open motion.
  @Published var openGeneration = 0
  @Published var desiredHeight: CGFloat = 0
  @Published var shortcutProblem: String?
  /// Bumped to close every popover the panel shows (Escape closes Details before anything else).
  @Published var popoverDismissal = 0
  /// False between the panel's close and its next open: `PanelRootView` then holds no content, so a
  /// closed tray evaluates and lays out none of the panel's view tree on the refresh ticks (N4).
  /// True by default so direct renders (previews, checks) show their content immediately.
  @Published var contentInstalled = true
  var scrollToAbout = false
  var open = OpenContext()
  var panelWidth: CGFloat = 760
  var maxHeight: CGFloat = 900
  var staticRender = false
  /// Offline renders only: simulate Reduce Transparency and Increase Contrast without touching the Mac's settings.
  var previewReduceTransparency = false
  var previewIncreaseContrast = false
  var reduceMotion: Bool { staticRender || NSWorkspace.shared.accessibilityDisplayShouldReduceMotion }

  func setSettings(_ open: Bool) {
    guard settingsOpen != open else { return }
    // A Details, packs or info popover anchored in the list closes when Settings covers its anchor.
    if open { popoverDismissal += 1 }
    if reduceMotion {
      withAnimation(.easeInOut(duration: 0.15)) { settingsOpen = open }
    } else {
      withAnimation(.smooth(duration: TrayMotion.settingsDuration)) { settingsOpen = open }
    }
  }
}

struct HeaderHeightKey: PreferenceKey {
  static let defaultValue: CGFloat = 0
  static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = max(value, nextValue()) }
}
struct ListHeightKey: PreferenceKey {
  static let defaultValue: CGFloat = 0
  static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = max(value, nextValue()) }
}
struct OverlayHeightKey: PreferenceKey {
  static let defaultValue: CGFloat = 0
  static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = max(value, nextValue()) }
}
struct FooterHeightKey: PreferenceKey {
  static let defaultValue: CGFloat = 0
  static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = max(value, nextValue()) }
}

/// Rebuilds the panel content on every open (state resets to the list, motion replays).
struct PanelRootView: View {
  @ObservedObject var model: AccountsViewModel
  @ObservedObject var prefs: TrayPreferences
  @ObservedObject var state: PanelState
  var body: some View {
    Group {
      // A closed panel holds no content: the tree is torn down after the close and built again on
      // the open, which already rebuilt it through `.id(openGeneration)`, so the open looks exactly
      // as before and a closed tray stays out of the view graph (N4).
      if state.contentInstalled {
        AccountsMenuView(model: model, prefs: prefs, state: state)
          .id(state.openGeneration)
          .background { if state.staticRender { PreviewGlass() } }
      }
    }
    // The hover-occlusion gate stays on the Group, outside the `if`, so hidden rows keep their
    // hover tags suppressed while Settings is open (hover fix).
    .trayHoverLayer(state.panelHover)
    .environment(\.trayStaticRender, state.staticRender)
    .environment(\.trayPopoverDismissal, state.popoverDismissal)
    .modifier(PreviewActiveControls(enabled: state.staticRender))
    .modifier(PreviewAccessibility(reduceTransparency: state.previewReduceTransparency,
      increaseContrast: state.previewIncreaseContrast))
  }
}

/// Offline renders only: the display accommodations as the system would report them.
struct PreviewAccessibility: ViewModifier {
  let reduceTransparency: Bool
  let increaseContrast: Bool
  func body(content: Content) -> some View {
    if reduceTransparency || increaseContrast {
      content
        .environment(\._accessibilityReduceTransparency, reduceTransparency)
        .environment(\._colorSchemeContrast, increaseContrast ? .increased : .standard)
    } else {
      content
    }
  }
}

/// Offline renders draw controls as they look in the key panel, not in an inactive window.
struct PreviewActiveControls: ViewModifier {
  let enabled: Bool
  func body(content: Content) -> some View {
    if enabled { content.environment(\.controlActiveState, .key) } else { content }
  }
}

/// Offline renders only: a baked stand-in for the panel's regular glass over a colourful wallpaper
/// (the medium tint with the wallpaper's hue showing through), since system glass is composited by the
/// window server and never reaches an offscreen render.
struct PreviewGlass: View {
  @Environment(\.colorScheme) private var scheme
  @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
  var body: some View {
    if reduceTransparency {
      // System glass turns opaque under Reduce Transparency: the concept's solid panel (--lg-solid).
      RoundedRectangle(cornerRadius: TrayMetrics.panelRadius, style: .continuous)
        .fill(scheme == .dark ? Color(.sRGB, red: 0x23 / 255, green: 0x24 / 255, blue: 0x28 / 255)
          : Color(.sRGB, red: 0xEC / 255, green: 0xED / 255, blue: 0xF0 / 255))
    } else {
      glass
    }
  }

  @ViewBuilder private var glass: some View {
    let colors: [Color] = scheme == .dark
      ? [Color(.sRGB, red: 0.20, green: 0.15, blue: 0.27), Color(.sRGB, red: 0.19, green: 0.14, blue: 0.22),
         Color(.sRGB, red: 0.23, green: 0.15, blue: 0.19)]
      : [Color(.sRGB, red: 0.985, green: 0.95, blue: 0.925), Color(.sRGB, red: 0.975, green: 0.935, blue: 0.94),
         Color(.sRGB, red: 0.955, green: 0.935, blue: 0.985)]
    RoundedRectangle(cornerRadius: TrayMetrics.panelRadius, style: .continuous)
      .fill(LinearGradient(colors: colors, startPoint: .topLeading, endPoint: .bottomTrailing))
      .overlay(RoundedRectangle(cornerRadius: TrayMetrics.panelRadius, style: .continuous)
        .strokeBorder(Color.white.opacity(scheme == .dark ? 0.16 : 0.7), lineWidth: 1))
  }
}

/// The tray panel: today's layout and order on one Liquid Glass panel. Header; Claude, Codex and
/// Antigravity sections; the other providers; the footer floating over the list.
struct AccountsMenuView: View {
  @ObservedObject var model: AccountsViewModel
  @ObservedObject var prefs: TrayPreferences
  @ObservedObject var state: PanelState
  @Namespace private var glass
  @Namespace private var platters
  @State private var heights: (header: CGFloat, list: CGFloat, overlay: CGFloat, footer: CGFloat) = (0, 0, 0, 0)

  /// After a sign-in hand-off the list uses the first-open entrance (every meter sweeps from 0); otherwise the
  /// panel's own open.
  private var open: OpenContext { model.listEntrance ?? state.open }
  private var detailHeight: CGFloat { min(560, max(260, state.maxHeight - 80)) }

  var body: some View {
    withPalette { palette in
      VStack(spacing: 0) {
        header(palette)
          .modifier(Entrance(index: 0, context: open))
          .background(GeometryReader { Color.clear.preference(key: HeaderHeightKey.self, value: $0.size.height) })
        ZStack(alignment: .top) {
          if model.needsConnection {
            // The sign-in screen replaces the list; the header and the footer stay. It leaves with a fade and an
            // 8 pt lift while the list loads in underneath.
            SignInView(model: model.signIn)
              .trayHoverLayer(state.coveredHover)
              .trayHoverLayer(state.signInHover)
              .offset(x: state.settingsOpen ? -28 : 0)
              .opacity(state.settingsOpen ? 0 : 1)
              .allowsHitTesting(!state.settingsOpen)
              .transition(state.reduceMotion ? .opacity
                : .asymmetric(insertion: .opacity, removal: .opacity.combined(with: .offset(y: -8))))
              .zIndex(1)
          } else {
            PanelScroll {
              list(palette)
                .id(model.listGeneration)
                .background(GeometryReader { Color.clear.preference(key: ListHeightKey.self, value: $0.size.height) })
            }
            .scrollEdgeEffectStyle(.soft, for: .bottom)
            .trayHoverLayer(state.coveredHover)
            .trayHoverLayer(state.listHover)
            .offset(x: state.settingsOpen ? -24 : 0)
            .opacity(state.settingsOpen ? 0 : 1)
            .allowsHitTesting(!state.settingsOpen)
          }
          if state.settingsOpen {
            SettingsPanelView(model: model, prefs: prefs, state: state, glass: glass)
              .trayHoverLayer(state.settingsHover)
              .transition(state.reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
              .zIndex(1)
          }
        }
        .frame(maxHeight: .infinity, alignment: .top)
        .safeAreaBar(edge: .bottom) {
          footer(palette)
            .modifier(Entrance(index: 5, context: open))
            .background(GeometryReader { Color.clear.preference(key: FooterHeightKey.self, value: $0.size.height) })
        }
      }
      .frame(width: state.panelWidth)
      .foregroundStyle(palette.label)
      .containerShape(RoundedRectangle(cornerRadius: TrayMetrics.panelRadius, style: .continuous))
    }
    .onChange(of: model.needsConnection, initial: true) { _, needs in state.setNeedsConnection(needs) }
    .onPreferenceChange(HeaderHeightKey.self) { heights.header = $0; report() }
    .onPreferenceChange(ListHeightKey.self) { heights.list = $0; report() }
    .onPreferenceChange(OverlayHeightKey.self) { heights.overlay = $0; report() }
    .onPreferenceChange(FooterHeightKey.self) { heights.footer = $0; report() }
    .onChange(of: state.settingsOpen) { _, open in if !open { heights.overlay = 0; report() } }
  }

  private func report() {
    // The close teardown resets the measured heights to zero: ignore it and keep the last height for
    // the next open, so the panel opens at its real size instead of jumping (N4).
    guard state.contentInstalled else { return }
    let body = max(heights.list, heights.overlay, model.dashboard == nil ? 170 : 0)
    let total = (heights.header + body + heights.footer).rounded(.up)
    if abs(total - state.desiredHeight) > 0.5 { state.desiredHeight = total }
  }

  // MARK: Header

  private func header(_ palette: TrayPalette) -> some View {
    HStack(spacing: 10) {
      ApexMark(size: 22)
      Text("AI Account Center").font(.system(size: 15, weight: .semibold)).foregroundStyle(palette.label)
        .fixedSize()
      Spacer(minLength: 12)
      status(palette)
      Menu {
        Button("Settings") { state.setSettings(true) }
        Button("About AI Account Center") { state.scrollToAbout = true; state.setSettings(true) }
        Divider()
        Button("Quit AI Account Center") { NSApplication.shared.terminate(nil) }
      } label: {
        Image(systemName: "ellipsis").font(.system(size: 13, weight: .semibold)).foregroundStyle(palette.label2)
          .frame(width: 26, height: 26).contentShape(Circle())
      }
      .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
      .glassControl(circle: true)
      .hoverHelp("About, Settings and Quit", id: "header-menu")
    }
    .padding(.top, 13).padding(.bottom, 9).padding(.leading, 18).padding(.trailing, 14)
  }

  @ViewBuilder private func status(_ palette: TrayPalette) -> some View {
    Group {
      if let flash = model.statusFlash {
        Text(flash)
      } else if model.needsConnection {
        Text(model.signIn.statusText)
      } else if model.isRefreshing && model.dashboard == nil {
        Text("Loading accounts")
      } else if model.isRefreshing {
        Text("Refreshing usage")
      } else if let dashboard = model.dashboard {
        let summary = TrayStatusSummary(dashboard: dashboard)
        let updated = AccountFormatting.date(dashboard.updatedAt)?.formatted(date: .omitted, time: .shortened) ?? "time unavailable"
        let count = Text(verbatim: "\(summary.reporting) of \(summary.providers)").fontWeight(.semibold).foregroundColor(palette.label)
        Text("\(count) reporting · \(summary.allCached ? "cached" : "live") · updated \(updated)")
      } else if !model.connected {
        Text("Connecting")
      }
    }
    .font(.system(size: 12)).foregroundStyle(palette.label2).lineLimit(1).monospacedDigit()
    .contentTransition(.opacity)
    .animation(.easeOut(duration: 0.2), value: model.isRefreshing)
    .trayHelp(statusHelp)
  }

  private var statusHelp: String {
    if model.needsConnection {
      return model.signIn.repair ? "Re-pairing: the current device key keeps working until the new one is saved"
        : "\(model.signIn.statusText): usage appears after this tray is paired"
    }
    guard let dashboard = model.dashboard else { return "" }
    var text = "Updated \(TrayFormat.relative(AccountFormatting.date(dashboard.updatedAt))). Every reading is the dashboard's sample."
    var hidden = dashboard.trayHiddenProviders.sorted().map(ProviderMark.name)
    let oneByOne = dashboard.trayHiddenAccounts.filter { !dashboard.trayHiddenProviders.contains($0.provider) }.count
    if oneByOne > 0 { hidden.append("\(oneByOne) account\(oneByOne == 1 ? "" : "s")") }
    if !hidden.isEmpty { text += " Hidden in the tray: \(hidden.joined(separator: ", "))." }
    return text
  }

  // MARK: List

  /// Every provider sits on its own platter, `sectionGap` apart, so the panel's glass divides them.
  private func list(_ palette: TrayPalette) -> some View {
    VStack(alignment: .leading, spacing: TrayMetrics.sectionGap) {
      if let message = model.message {
        HStack(alignment: .top, spacing: 8) {
          Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(palette.warn)
          Text(message).font(.system(size: 12.5)).foregroundStyle(palette.label).fixedSize(horizontal: false, vertical: true)
          Spacer(minLength: 0)
        }
        .padding(10)
        .background(Color.orange.opacity(0.14), in: RoundedRectangle(cornerRadius: TrayMetrics.groupRadius, style: .continuous))
        .transition(.opacity)
      }
      if let dashboard = model.dashboard {
        let groups = dashboard.providerGroups
        let sectionIDs = ["claude", "codex", "antigravity"]
        ForEach(Array(sectionIDs.enumerated()), id: \.element) { index, provider in
          if let group = groups.first(where: { $0.id == provider }) {
            accountSection(group.accounts, provider: provider)
              .modifier(Entrance(index: index + 1, context: open))
          }
        }
        let others = groups.filter { !sectionIDs.contains($0.id) }
        if !others.isEmpty {
          VStack(spacing: TrayMetrics.sectionGap) {
            ForEach(Array(others.enumerated()), id: \.element.id) { index, group in
              ProviderRow(model: model, group: group, open: open, block: 4 + index, maxDetailHeight: detailHeight)
                .sectionPlatter()
            }
          }
          .modifier(Entrance(index: 4, context: open))
        }
        if dashboard.visibleAccounts.isEmpty {
          Text(dashboard.accounts.isEmpty ? "No accounts are available yet."
            : "Every account is hidden in the tray. Change it in the dashboard.")
            .font(.system(size: 12.5)).foregroundStyle(palette.label2).padding(20)
        }
      } else {
        HStack(spacing: 8) {
          ProgressView().controlSize(.small)
          Text(model.connected ? "Loading accounts" : "Connecting to AI Account Center").font(.system(size: 12.5))
            .foregroundStyle(palette.label2)
        }
        .frame(maxWidth: .infinity).padding(.vertical, 40)
      }
    }
    .padding(.horizontal, 8).padding(.top, 2).padding(.bottom, 6)
  }

  private func accountSection(_ accounts: [DashboardAccount], provider: String) -> some View {
    let layout = SectionLayout.make(provider, accounts: accounts)
    let activeID = accounts.first(where: \.isActive)?.id
    // The header and its rows share one platter. The value animations sit outside the platter, as they did
    // when it held only the rows, so it still grows with a switch confirmation; the header keeps its own
    // unanimated update when the active account changes, as when it sat above the platter.
    return VStack(alignment: .leading, spacing: 2) {
      SectionHeader(model: model, layout: layout, accounts: accounts)
        .animation(nil, value: activeID)
      VStack(spacing: 0) {
        ForEach(Array(accounts.enumerated()), id: \.element.id) { index, account in
          if index > 0 {
            let besideActive = layout.switchable && (account.isActive || accounts[index - 1].isActive)
            withPalette { palette in
              Rectangle().fill(palette.separator).frame(height: 0.5).padding(.leading, 42).padding(.trailing, 8)
                .opacity(besideActive ? 0 : 1)
            }
          }
          AccountRow(model: model, account: account, layout: layout, sectionAccounts: accounts, open: open,
            block: 1 + index, maxDetailHeight: detailHeight)
            .background {
              if layout.switchable && account.isActive {
                ActivePlatter().matchedGeometryEffect(id: "active-\(provider)", in: platters)
              }
            }
            .alignmentProbe("row|\(account.id)")
          if let offer = model.pendingCodexSwitch, provider == "codex", offer.accountID == account.id {
            SwitchConfirmView(product: "Codex", identity: offer.identity, processes: offer.processes, warning: offer.warning,
              expiresAt: offer.confirmation.expiresAt, onCancel: model.cancelCodexSwitch,
              onConfirm: { model.confirmCodexSwitch(offer) })
              .transition(.opacity.combined(with: .move(edge: .top)))
          }
          if let offer = model.pendingAntigravitySwitch, provider == "antigravity", offer.accountID == account.id {
            SwitchConfirmView(product: "Antigravity", identity: offer.identity, processes: offer.processes, warning: offer.warning,
              expiresAt: offer.confirmation.expiresAt, onCancel: model.cancelAntigravitySwitch,
              onConfirm: { model.confirmAntigravitySwitch(offer) })
              .transition(.opacity.combined(with: .move(edge: .top)))
          }
        }
      }
    }
    .alignmentProbe("section|\(provider)")
    .sectionPlatter()
    .animation(state.reduceMotion ? nil : .trayValue(duration: TrayMotion.platterDuration), value: activeID)
    .animation(.trayValue(duration: 0.3), value: model.pendingCodexSwitch?.id)
    .animation(.trayValue(duration: 0.3), value: model.pendingAntigravitySwitch?.id)
  }

  // MARK: Footer

  @ViewBuilder private func footer(_ palette: TrayPalette) -> some View {
    GlassEffectContainer(spacing: 8) {
      HStack(spacing: 8) {
        if model.needsConnection {
          Text(model.signIn.footNote).font(.system(size: 12)).foregroundStyle(palette.label2)
            .contentTransition(.opacity)
        }
        // The Codex auto-switch moved into the Codex section header; the footer's left side stays empty.
        Spacer(minLength: 8)
        if !model.needsConnection {
          let openDashboard = { model.openDashboard() }
          Button(action: openDashboard) {
            HStack(spacing: 6) {
              Text("Dashboard").font(.system(size: 13, weight: .medium))
              Image(systemName: "arrow.up.right.square").font(.system(size: 12, weight: .medium))
            }
            .foregroundStyle(palette.label)
            .padding(.horizontal, 15).frame(height: TrayMetrics.footerControl)
            .contentShape(Capsule())
          }
          .buttonStyle(.plain).glassControl()
          .hoverHelp("Open the dashboard in your browser", id: "footer-dashboard", action: openDashboard)
          let refresh = { Task { await model.refresh(force: true) } }
          Button { _ = refresh() } label: {
            Image(systemName: "arrow.clockwise").font(.system(size: 14, weight: .medium)).foregroundStyle(palette.label)
              .symbolEffect(.rotate, options: .speed(1.4), isActive: model.isRefreshing && !state.reduceMotion)
              .frame(width: TrayMetrics.footerControl, height: TrayMetrics.footerControl).contentShape(Circle())
          }
          .buttonStyle(.plain).glassControl(circle: true)
          .disabled(model.isRefreshing || model.busyAction != nil)
          .hoverHelp("Refresh usage", id: "footer-refresh", action: { _ = refresh() })
        }
        let toggle = { state.setSettings(!state.settingsOpen) }
        Button(action: toggle) {
          Image(systemName: "gearshape").font(.system(size: 15, weight: .medium))
            .foregroundStyle(state.settingsOpen ? palette.accentText : palette.label)
            .rotationEffect(.degrees(state.settingsOpen ? 60 : 0))
            .animation(state.reduceMotion ? nil : .spring(response: 0.38, dampingFraction: 0.62), value: state.settingsOpen)
            .frame(width: TrayMetrics.footerControl, height: TrayMetrics.footerControl).contentShape(Circle())
        }
        .buttonStyle(.plain)
        .glassControl(circle: true, tint: state.settingsOpen ? palette.accent.opacity(0.28) : nil)
        .accessibilityAddTraits(state.settingsOpen ? .isSelected : [])
        .hoverHelp("Settings", id: "footer-settings", action: toggle)
        if model.needsConnection {
          Button { NSApplication.shared.terminate(nil) } label: {
            Image(systemName: "power").font(.system(size: 14, weight: .medium))
              .frame(width: TrayMetrics.footerControl, height: TrayMetrics.footerControl).contentShape(Circle())
          }
          .buttonStyle(.plain).glassControl(circle: true)
          .hoverHelp("Quit AI Account Center", id: "footer-quit")
        }
      }
      .alignmentProbe("footer")
    }
    .padding(.horizontal, 12).padding(.top, 8).padding(.bottom, 12)
  }

}

/// The selected-row platter: a soft accent fill over a faint lift, a hairline accent outline and a 27-style
/// specular top line. Content, not glass. One per switchable section; it glides between rows.
struct ActivePlatter: View {
  @Environment(\.colorScheme) private var scheme
  @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
  @Environment(\.colorSchemeContrast) private var contrast

  var body: some View {
    let palette = TrayPalette(scheme, reduceTransparency: reduceTransparency, increasedContrast: contrast == .increased)
    let dark = scheme == .dark
    let shape = ConcentricRectangle()
    // Strokes are clipped to the shape, so half of each line shows: 0.5 pt normally, 1 pt under Reduce
    // Transparency and Increase Contrast, as in the concept.
    ZStack {
      if reduceTransparency {
        shape.fill(palette.solidGroup)
        shape.fill(palette.accent.opacity(dark ? 0.24 : 0.18))
        shape.stroke(palette.accent.opacity(contrast == .increased ? 1 : 0.6), lineWidth: 2)
      } else {
        shape.fill(Color.white.opacity(dark ? 0.03 : 0.5))
        shape.fill(palette.accent.opacity(dark ? 0.24 : 0.18))
        shape.stroke(palette.accent.opacity(contrast == .increased ? 1 : 0.55), lineWidth: contrast == .increased ? 2 : 1)
        if contrast != .increased {
          Rectangle().fill(Color.white.opacity(0.22)).frame(height: 0.5)
            .padding(.horizontal, TrayMetrics.rowRadius)
            .frame(maxHeight: .infinity, alignment: .top)
        }
      }
    }
    .clipShape(shape)
  }
}

/// Panel open: blocks rise 6 pt and fade in, staggered; later opens use a short stagger.
struct Entrance: ViewModifier {
  let index: Int
  let context: OpenContext
  @State private var shown: Bool

  init(index: Int, context: OpenContext) {
    self.index = index
    self.context = context
    _shown = State(initialValue: !context.animate)
  }

  func body(content: Content) -> some View {
    content
      .opacity(shown ? 1 : 0)
      .offset(y: shown ? 0 : 6)
      .onAppear {
        guard !shown else { return }
        let stagger = context.firstOpen ? TrayMotion.firstOpenStagger : TrayMotion.laterOpenStagger
        withAnimation(.trayValue(duration: context.firstOpen ? 0.42 : 0.24).delay(min(0.15, Double(index) * stagger))) { shown = true }
      }
  }
}
