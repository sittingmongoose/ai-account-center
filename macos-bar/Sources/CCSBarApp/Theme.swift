import SwiftUI
import AppKit
import CCSBarCore

/// Panel geometry from the approved tray concept (trays/tray.css, LIQUID-GLASS-NOTES.md D6).
enum TrayMetrics {
  static let panelRadius: CGFloat = 20
  static let groupRadius: CGFloat = 12
  static let rowRadius: CGFloat = 8
  static let groupInset: CGFloat = 4
  static let markColumn: CGFloat = 22
  static let identityColumn: CGFloat = 168
  static let antigravityIdentityColumn: CGFloat = 180
  static let columnGap: CGFloat = 13
  static let antigravityColumnGap: CGFloat = 11
  /// Codex and Antigravity share one fixed action slot; Claude's Open pair ends on the same line.
  static let switchSlot: CGFloat = 108
  static let claudeSlot: CGFloat = 62
  static let tail: CGFloat = 14
  /// The Activate capsule's label inset: the 18 pt check plus its 6 pt gap, so "Active" lines up.
  static let check: CGFloat = 18
  static let checkGap: CGFloat = 6
  static var activateInset: CGFloat { check + checkGap }
  static let rowLeading: CGFloat = 8
  static let rowTrailing: CGFloat = 6
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

  /// Section group platters: a translucent lift on the glass (.fill.quinary in spirit), opaque under
  /// Reduce Transparency.
  var group: Color {
    if reduceTransparency { return Color(nsColor: .controlBackgroundColor) }
    return dark ? Color.white.opacity(0.055) : Color.white.opacity(0.34)
  }
  var rowHover: Color { dark ? Color.white.opacity(0.07) : Self.hex(0x767680, 0.11) }
  var controlInner: Color { dark ? Color.white.opacity(0.09) : Self.hex(0x767680, 0.14) }

  // Meters.
  var track: Color {
    if reduceTransparency { return Color(nsColor: .separatorColor) }
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

/// Interactive glass for a control: capsule or circle, with the system hover and press response.
struct GlassControl: ViewModifier {
  var circle = false
  var tint: Color? = nil
  func body(content: Content) -> some View {
    if circle {
      content.glassEffect(Glass.regular.tint(tint).interactive(), in: Circle())
    } else {
      content.glassEffect(Glass.regular.tint(tint).interactive(), in: Capsule())
    }
  }
}

extension View {
  func glassControl(circle: Bool = false, tint: Color? = nil) -> some View { modifier(GlassControl(circle: circle, tint: tint)) }
}

/// Value animation: ease-out only, so no meter, number or platter passes its reading.
extension Animation {
  static func trayValue(duration: Double = TrayMotion.meterDuration) -> Animation {
    let c = TrayMotion.valueCurve
    return .timingCurve(c.x1, c.y1, c.x2, c.y2, duration: duration)
  }
}

/// Lets the preview renderer and Reduce Motion show the settled state without animation.
private struct StaticRenderKey: EnvironmentKey { static let defaultValue = false }
extension EnvironmentValues {
  var trayStaticRender: Bool {
    get { self[StaticRenderKey.self] }
    set { self[StaticRenderKey.self] = newValue }
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
