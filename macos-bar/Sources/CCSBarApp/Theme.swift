import SwiftUI
import AppKit
import CCSBarCore

/// Panel geometry from the approved tray concept (trays/tray.css, LIQUID-GLASS-NOTES.md D6).
enum TrayMetrics {
  static let panelRadius: CGFloat = 20
  static let groupRadius: CGFloat = 12
  static let rowRadius: CGFloat = 8
  static let groupInset: CGFloat = 4
  /// The strip of panel glass between two provider platters: the divider between providers.
  static let sectionGap: CGFloat = 12
  /// Extra room above a section header, inside its platter.
  static let sectionHeaderTop: CGFloat = 6
  static let markColumn: CGFloat = 22
  static let identityColumn: CGFloat = 168
  /// Antigravity's identity column and gap give its four quota columns room at the panel's 760 pt:
  /// the "Claude/GPT 5-hour" caption fits a cell at the captions' 0.85 minimum scale.
  static let antigravityIdentityColumn: CGFloat = 164
  static let columnGap: CGFloat = 13
  static let antigravityColumnGap: CGFloat = 10
  /// Codex and Antigravity share one fixed action slot, trailing-anchored: the Activate capsule
  /// (97.5 pt) plus a little room, so every slot ends on the same line in every section.
  static let switchSlot: CGFloat = 100
  /// Claude's Open pair: two 28 pt buttons with an 8 pt gap, trailing-anchored like the switch slot.
  static let claudeSlot: CGFloat = 64
  static let openButton: CGFloat = 28
  static let openPairGap: CGFloat = 8
  /// The Activate capsule's label inset: the 18 pt check plus its 6 pt gap, so "Active" lines up.
  static let check: CGFloat = 18
  static let checkGap: CGFloat = 6
  static var activateInset: CGFloat { check + checkGap }
  static let rowLeading: CGFloat = 8
  static let rowTrailing: CGFloat = 6
  /// The disclosure chevron's reserved trailing column inside every account and provider row, so the
  /// chevron sits inside the row's hover highlight (and the active row's platter) instead of
  /// overhanging the row's edge, and the meters never move when it appears.
  static let chevronColumn: CGFloat = 14
  static let footerControl: CGFloat = 34
}

/// Daylight Atlas colours on Liquid Glass. The panel itself is system glass; these are content colours.
struct TrayPalette {
  let scheme: ColorScheme
  let reduceTransparency: Bool
  let increasedContrast: Bool

  init(_ scheme: ColorScheme, reduceTransparency: Bool = false, increasedContrast: Bool = false) {
    self.scheme = scheme
    self.reduceTransparency = reduceTransparency
    self.increasedContrast = increasedContrast
  }

  private var dark: Bool { scheme == .dark }
  private static func hex(_ value: UInt32, _ alpha: Double = 1) -> Color {
    Color(.sRGB, red: Double((value >> 16) & 0xFF) / 255, green: Double((value >> 8) & 0xFF) / 255,
      blue: Double(value & 0xFF) / 255, opacity: alpha)
  }

  var accent: Color { dark ? Self.hex(0x86ABFF) : Self.hex(0x2552CC) }
  var accentText: Color { dark ? Self.hex(0x9BBAFF) : Self.hex(0x2149B8) }
  /// The check inside the filled check-circle: white on the light accent, near-black on the dark one.
  var accentInk: Color { dark ? Self.hex(0x0B1424) : .white }
  var goodText: Color { dark ? Self.hex(0x5AD3BF) : Self.hex(0x1C7D6E) }

  // Vibrant-style label tiers (D1/D6).
  var label: Color { dark ? Color.white.opacity(0.94) : Color.black.opacity(0.86) }
  var label2: Color {
    if increasedContrast { return label }
    return dark ? Self.hex(0xEBEBF5, 0.84) : Self.hex(0x1C1C24, 0.74)
  }
  var label3: Color {
    if increasedContrast { return label2 }
    return dark ? Self.hex(0xEBEBF5, 0.74) : Self.hex(0x1C1C24, 0.62)
  }
  var label4: Color { dark ? Self.hex(0xEBEBF5, 0.34) : Self.hex(0x1C1C24, 0.38) }
  var separator: Color {
    if reduceTransparency || increasedContrast { return Color(nsColor: .separatorColor) }
    return dark ? Color.white.opacity(0.09) : Color.black.opacity(0.09)
  }

  /// Section group platters: a translucent lift on the glass (.fill.quinary in spirit). Under Reduce
  /// Transparency they turn solid and stay a lift: the concept's --lg-solid-group (white in light; in dark
  /// a grey above the opaque panel, where controlBackgroundColor would sink below it as a well).
  var group: Color {
    if reduceTransparency { return solidGroup }
    return dark ? Color.white.opacity(0.055) : Color.white.opacity(0.34)
  }
  var solidGroup: Color { dark ? Self.hex(0x2D2E33) : Self.hex(0xFFFFFF) }

  /// Provider section platters: each provider is its own block of content on the glass, so the fill is a
  /// firmer lift than `group`, and a hairline edge marks where it ends over any wallpaper. Under Reduce
  /// Transparency the fill is the same solid lift as `group`; Increase Contrast draws the edge at 1 pt in a
  /// stronger tone.
  var section: Color {
    if reduceTransparency { return solidGroup }
    return dark ? Color.white.opacity(0.085) : Color.white.opacity(0.6)
  }
  var sectionEdge: Color {
    if increasedContrast { return label4 }
    if reduceTransparency { return dark ? Color.white.opacity(0.12) : Color.black.opacity(0.12) }
    return dark ? Color.white.opacity(0.10) : Color.black.opacity(0.07)
  }
  var rowHover: Color { dark ? Color.white.opacity(0.07) : Self.hex(0x767680, 0.11) }
  var controlInner: Color { dark ? Color.white.opacity(0.09) : Self.hex(0x767680, 0.14) }

  // Meters.
  var track: Color {
    if reduceTransparency { return dark ? Self.hex(0x45464C) : Self.hex(0xDADDE2) }  // --lg-solid-track
    if increasedContrast { return dark ? Self.hex(0x8C8C96, 0.42) : Self.hex(0x767680, 0.32) }
    return dark ? Self.hex(0x8C8C96, 0.30) : Self.hex(0x767680, 0.20)
  }
  var trackHover: Color { dark ? Self.hex(0x8C8C96, 0.40) : Self.hex(0x767680, 0.28) }
  var tick: Color { dark ? Color.black.opacity(0.35) : Color.white.opacity(0.75) }

  func fill(_ severity: MeterSeverity) -> (start: Color, end: Color) {
    switch severity {
    case .warn: return dark ? (Self.hex(0x946311), Self.hex(0xF3B743)) : (Self.hex(0xF2CD78), Self.hex(0xC98410))
    case .crit, .over: return dark ? (Self.hex(0xA1331F), Self.hex(0xFF6D50)) : (Self.hex(0xF3A28A), Self.hex(0xCF4127))
    default: return dark ? (Self.hex(0x1E7468), Self.hex(0x46C7B2)) : (Self.hex(0x8ED3C6), Self.hex(0x23907F))
    }
  }
  var over: Color { dark ? Self.hex(0xFF5C93) : Self.hex(0x8C1D48) }

  /// Only the one status number is coloured; calm values stay in the label colour.
  func valueText(_ severity: MeterSeverity) -> Color {
    switch severity {
    case .warn: return dark ? Self.hex(0xF5C35D) : Self.hex(0x9A6100)
    case .crit: return dark ? Self.hex(0xFF8469) : Self.hex(0xBC3219)
    case .over: return dark ? Self.hex(0xFF7AA6) : Self.hex(0x8C1D48)
    case .unavailable: return label3
    case .calm: return label
    }
  }
  var critText: Color { dark ? Self.hex(0xFF8469) : Self.hex(0xBC3219) }
  /// The error edge of a field, and its halo.
  var crit: Color { dark ? Self.hex(0xFF6D50) : Self.hex(0xCF4127) }
  /// The calm green of a finished pairing (the primary button's done layer, the step checks).
  var calm: Color { dark ? Self.hex(0x46C7B2) : Self.hex(0x23907F) }
  /// A field on the glass: the glass control body and its hairline edge (--lg-ctl, --lg-ctl-edge).
  var control: Color { dark ? Color.white.opacity(0.11) : Color.white.opacity(0.52) }
  var controlEdge: Color { dark ? Color.black.opacity(0.40) : Color.black.opacity(0.10) }
  /// Reduce Transparency: a solid field (--lg-solid-ctl).
  var solidControl: Color { dark ? Self.hex(0x3A3B40) : Self.hex(0xFFFFFF) }
  var warnText: Color { dark ? Self.hex(0xF5C35D) : Self.hex(0x9A6100) }
  var warn: Color { dark ? Self.hex(0xF3B743) : Self.hex(0xC98410) }
}

private struct PaletteReader<Content: View>: View {
  @Environment(\.colorScheme) private var scheme
  @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
  @Environment(\.colorSchemeContrast) private var contrast
  let content: (TrayPalette) -> Content
  var body: some View {
    content(TrayPalette(scheme, reduceTransparency: reduceTransparency, increasedContrast: contrast == .increased))
  }
}

/// Reads the palette for the current appearance and accessibility settings.
func withPalette<Content: View>(@ViewBuilder _ content: @escaping (TrayPalette) -> Content) -> some View {
  PaletteReader(content: content)
}

/// A translucent group platter (content, not glass) at the concentric 12 pt radius.
struct GroupPlatter: ViewModifier {
  @Environment(\.colorScheme) private var scheme
  @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
  @Environment(\.colorSchemeContrast) private var contrast
  var padding: CGFloat = TrayMetrics.groupInset

  func body(content: Content) -> some View {
    let palette = TrayPalette(scheme, reduceTransparency: reduceTransparency, increasedContrast: contrast == .increased)
    let shape = RoundedRectangle(cornerRadius: TrayMetrics.groupRadius, style: .continuous)
    return content
      .padding(padding)
      .background(palette.group, in: shape)
      .overlay {
        if reduceTransparency || contrast == .increased { shape.strokeBorder(palette.separator, lineWidth: 0.5) }
      }
      .containerShape(shape)
  }
}

extension View {
  func groupPlatter(padding: CGFloat = TrayMetrics.groupInset) -> some View { modifier(GroupPlatter(padding: padding)) }
}

/// One provider's platter (its header and rows, or one other provider's row): content, not glass, at the
/// concentric 12 pt radius. Platters are stacked `TrayMetrics.sectionGap` apart, so the panel's own glass
/// shows between them as the divider. Rows inside keep their 4 pt inset, so the hover and Selected-row
/// platters stay concentric and inside it.
struct SectionPlatter: ViewModifier {
  @Environment(\.colorScheme) private var scheme
  @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
  @Environment(\.colorSchemeContrast) private var contrast

  func body(content: Content) -> some View {
    let palette = TrayPalette(scheme, reduceTransparency: reduceTransparency, increasedContrast: contrast == .increased)
    let shape = RoundedRectangle(cornerRadius: TrayMetrics.groupRadius, style: .continuous)
    return content
      .padding(TrayMetrics.groupInset)
      .background(palette.section, in: shape)
      .overlay(shape.strokeBorder(palette.sectionEdge, lineWidth: contrast == .increased ? 1 : 0.5))
      .containerShape(shape)
  }
}

extension View {
  func sectionPlatter() -> some View { modifier(SectionPlatter()) }
}

enum GlassShape { case capsule, circle, rounded(CGFloat) }

/// Interactive glass for a control: capsule, circle or rounded rectangle, with the system hover and
/// press response. Offline renders cannot capture system glass, so they draw the concept's glass-control
/// tokens instead (a translucent body, a hairline edge and a specular top line).
struct GlassControl: ViewModifier {
  var shape: GlassShape = .capsule
  var tint: Color? = nil
  @Environment(\.trayStaticRender) private var staticRender
  @Environment(\.colorScheme) private var scheme
  @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

  func body(content: Content) -> some View {
    switch shape {
    case .capsule: apply(content, Capsule())
    case .circle: apply(content, Circle())
    case .rounded(let radius): apply(content, RoundedRectangle(cornerRadius: radius, style: .continuous))
    }
  }

  @ViewBuilder private func apply<S: InsettableShape>(_ content: Content, _ shape: S) -> some View {
    if staticRender && reduceTransparency {
      // What system glass shows under Reduce Transparency: a solid control (--lg-solid-ctl) with an edge.
      let dark = scheme == .dark
      content
        .background(shape.fill(tint ?? .clear))
        .background(shape.fill(dark ? Color(.sRGB, red: 0x3A / 255, green: 0x3B / 255, blue: 0x40 / 255) : .white))
        .overlay(shape.strokeBorder(dark ? Color(.sRGB, red: 0xEB / 255, green: 0xEB / 255, blue: 0xF5 / 255, opacity: 0.34)
          : Color(.sRGB, red: 0x1C / 255, green: 0x1C / 255, blue: 0x24 / 255, opacity: 0.38), lineWidth: 0.5))
    } else if staticRender {
      let dark = scheme == .dark
      content
        .background(shape.fill(Color.white.opacity(dark ? 0.11 : 0.52)))
        .background(shape.fill(tint ?? .clear))
        .overlay(shape.strokeBorder(Color.black.opacity(dark ? 0.40 : 0.10), lineWidth: 0.5))
        .overlay(shape.strokeBorder(LinearGradient(colors: [Color.white.opacity(dark ? 0.35 : 0.9), .clear],
          startPoint: .top, endPoint: .center), lineWidth: 0.75))
        .shadow(color: .black.opacity(dark ? 0.25 : 0.08), radius: 3, y: 1)
    } else {
      content.glassEffect(Glass.regular.tint(tint).interactive(), in: shape)
    }
  }
}

extension View {
  func glassControl(circle: Bool = false, tint: Color? = nil) -> some View {
    modifier(GlassControl(shape: circle ? .circle : .capsule, tint: tint))
  }
  func glassControl(_ shape: GlassShape, tint: Color? = nil) -> some View { modifier(GlassControl(shape: shape, tint: tint)) }
}

/// Value animation: ease-out only, so no meter, number or platter passes its reading.
extension Animation {
  static func trayValue(duration: Double = TrayMotion.meterDuration) -> Animation {
    let c = TrayMotion.valueCurve
    return .timingCurve(c.x1, c.y1, c.x2, c.y2, duration: duration)
  }
}

/// The system .glass / .glassProminent button styles. Offline renders cannot capture system glass, so
/// there the capsule is drawn from the concept's tokens; the live panel always uses the system style.
struct TrayGlassButton: ViewModifier {
  var prominent = false
  var tint: Color? = nil
  var large = false
  @Environment(\.trayStaticRender) private var staticRender

  func body(content: Content) -> some View {
    if staticRender {
      content.buttonStyle(PreviewGlassButtonStyle(prominent: prominent, tint: tint, large: large))
    } else if prominent {
      content.buttonStyle(.glassProminent).tint(tint).controlSize(large ? .large : .regular)
    } else {
      content.buttonStyle(.glass).controlSize(large ? .large : .regular)
    }
  }
}

private struct PreviewGlassButtonStyle: ButtonStyle {
  let prominent: Bool
  let tint: Color?
  let large: Bool
  @Environment(\.colorScheme) private var scheme

  func makeBody(configuration: Configuration) -> some View {
    let dark = scheme == .dark
    configuration.label
      .font(.system(size: 13, weight: prominent ? .semibold : .regular))
      .foregroundStyle(prominent ? (dark ? Color(.sRGB, red: 0x0B / 255, green: 0x14 / 255, blue: 0x24 / 255) : .white) : .primary)
      .padding(.horizontal, large ? 18 : 14)
      .frame(height: large ? 34 : 28)
      .background(Capsule().fill(prominent ? (tint ?? .accentColor) : Color.white.opacity(dark ? 0.11 : 0.52)))
      .overlay(Capsule().strokeBorder(Color.black.opacity(dark ? 0.40 : 0.10), lineWidth: 0.5))
  }
}

extension View {
  func trayGlassButton(prominent: Bool = false, tint: Color? = nil, large: Bool = false) -> some View {
    modifier(TrayGlassButton(prominent: prominent, tint: tint, large: large))
  }
}

/// The panel's request to close its popovers: Details, the Qwen packs and the auto-switch info.
private struct PopoverDismissalKey: EnvironmentKey { static let defaultValue = 0 }
extension EnvironmentValues {
  var trayPopoverDismissal: Int {
    get { self[PopoverDismissalKey.self] }
    set { self[PopoverDismissalKey.self] = newValue }
  }
}

private struct DismissedByPanel: ViewModifier {
  @Binding var isPresented: Bool
  @Environment(\.trayPopoverDismissal) private var dismissal
  func body(content: Content) -> some View {
    content.onChange(of: dismissal) { _, _ in if isPresented { isPresented = false } }
  }
}

extension View {
  /// Closes this view's popover when the panel asks (Escape with a popover showing).
  func dismissedByPanel(_ isPresented: Binding<Bool>) -> some View { modifier(DismissedByPanel(isPresented: isPresented)) }
}

/// Lets the preview renderer and Reduce Motion show the settled state without animation.
private struct StaticRenderKey: EnvironmentKey { static let defaultValue = false }
extension EnvironmentValues {
  var trayStaticRender: Bool {
    get { self[StaticRenderKey.self] }
    set { self[StaticRenderKey.self] = newValue }
  }
}

/// Offline renders only (`--hover=`): the account or provider id whose row renders hovered, so the
/// hover highlight and the disclosure chevron are visible in a still render. Nil in the app.
private struct PreviewHoverKey: EnvironmentKey { static let defaultValue: String? = nil }
extension EnvironmentValues {
  var trayPreviewHover: String? {
    get { self[PreviewHoverKey.self] }
    set { self[PreviewHoverKey.self] = newValue }
  }
}

/// A vertical scroll view. Offline renders lay the content out flat instead: SwiftUI scroll content
/// is drawn by the window server and does not appear in an offscreen layer render.
struct PanelScroll<Content: View>: View {
  @Environment(\.trayStaticRender) private var staticRender
  @ViewBuilder let content: () -> Content
  var body: some View {
    if staticRender {
      VStack(spacing: 0) { content() }.frame(maxHeight: .infinity, alignment: .top)
    } else {
      ScrollView { content() }.scrollIndicators(.hidden)
    }
  }
}

/// Records laid-out frames during offline checks, so alignment can be measured rather than eyeballed.
@MainActor
enum AlignmentProbe {
  static var frames: [String: CGRect] = [:]
  /// Offline checks of the live (scrolling, animated) panel record frames too.
  static var live = false
}

private struct AlignmentProbeModifier: ViewModifier {
  let id: String
  @Environment(\.trayStaticRender) private var staticRender
  func body(content: Content) -> some View {
    if staticRender || AlignmentProbe.live {
      content.background(GeometryReader { geometry in
        Color.clear
          .onAppear { AlignmentProbe.frames[id] = geometry.frame(in: .global) }
          .onChange(of: geometry.frame(in: .global)) { _, frame in AlignmentProbe.frames[id] = frame }
      })
    } else {
      content
    }
  }
}

extension View {
  func alignmentProbe(_ id: String) -> some View { modifier(AlignmentProbeModifier(id: id)) }
}
