import SwiftUI
import CCSBarCore

/// How a meter enters when the panel opens: from zero on the session's first open, from the value
/// last shown on later opens (so only changed readings move), or settled for previews.
struct MeterMotion {
  var from: Double? = nil
  var delay: Double = 0
  var animate = true
}

/// A number that counts to its value one frame at a time. Its animatable value follows the meter's
/// ease-out curve, so it never passes the reading.
struct CountingPercent: View, Animatable {
  var value: Double
  let decimals: Int
  var size: CGFloat = 15
  var weight: Font.Weight = .semibold
  var animatableData: Double {
    get { value }
    set { value = newValue }
  }
  var body: some View {
    // The percent sign is part of the same run: same font, size and weight, on the same baseline.
    Text("\(TrayFormat.number(value, decimals: decimals))%")
      .font(.system(size: size, weight: weight))
      .monospacedDigit()
      .lineLimit(1)
      .fixedSize()
  }
}

/// The 6 pt track with quarter ticks, the severity fill, an optional auto-switch notch and an
/// overage end cap that grows into the meter's reserved gutter. Every horizontal measure derives from
/// the track's actual laid-out width, so the meter stays accurate at any column width.
struct MeterTrack: View {
  let key: String
  let shown: Double?
  let severity: MeterSeverity
  var notch: Double? = nil
  var notchOpacity: Double = 1
  var hovered = false
  var dashed = false
  @Environment(\.colorScheme) private var scheme
  @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
  @Environment(\.colorSchemeContrast) private var contrast

  var body: some View {
    let palette = TrayPalette(scheme, reduceTransparency: reduceTransparency, increasedContrast: contrast == .increased)
    GeometryReader { geometry in
      let width = geometry.size.width
      ZStack(alignment: .leading) {
        if dashed {
          Capsule().strokeBorder(palette.label4, style: StrokeStyle(lineWidth: 1, dash: [3, 2]))
        } else {
          Capsule().fill(hovered ? palette.trackHover : palette.track)
          // Ticks and the notch sit on leading padding, not offset: offset is invisible to the
          // alignment probes, while the pixels are identical either way.
          ForEach([0.25, 0.5, 0.75], id: \.self) { mark in
            Rectangle().fill(palette.tick).frame(width: 1, height: 4)
              .alignmentProbe("meter|\(key)|tick\(Int(mark * 100))")
              .padding(.leading, width * mark)
          }
          let colors = palette.fill(severity)
          Capsule()
            .fill(LinearGradient(colors: [colors.start, colors.end], startPoint: .leading, endPoint: .trailing))
            .frame(width: max(0, width * TrayMotion.fillFraction(shown)))
            .opacity((shown ?? 0) > 0 ? 1 : 0)
            .alignmentProbe("meter|\(key)|fill")
          if let shown, shown > 100 {
            Capsule().fill(palette.over)
              .frame(width: min(8, width * (shown - 100) / 100) + 3)
              .offset(x: width - 3)
          }
        }
        if let notch {
          RoundedRectangle(cornerRadius: 1).fill(palette.label)
            .frame(width: 2, height: 12)
            .alignmentProbe("meter|\(key)|notch")
            .opacity(notchOpacity)
            .padding(.leading, width * min(100, max(0, notch)) / 100 - 1)
        }
      }
      .alignmentProbe("meter|\(key)|track")
    }
    .frame(height: 6)
  }
}

/// One usage meter. `labelText` gives the labelled form used by provider rows; without it the value
/// and reset share the top line, as in the Claude, Codex and Antigravity columns.
struct MeterView: View {
  let key: String
  let window: AccountQuotaWindow?
  /// The account's sample time, for the tooltip when the window has none of its own.
  var sampledAt: String? = nil
  /// F6: the reset time when this window's reading was taken before its reset (or at an unknown time), from
  /// `DashboardAccount.pendingReset`. The parent works it out, so a timer tick that flips it redraws this meter. The
  /// meter then shows no number, no fill and no notch, never 0%: "Reset at 10:15 AM · new reading pending".
  var pendingReset: Date? = nil
  var labelText: String? = nil
  var notch: Double? = nil
  var notchOpacity: Double = 1
  var showAmount = false
  var unavailableText = "Unavailable"
  var unavailableHelp = "Unavailable: no reading was reported, which is not the same as zero."
  var hovered = false
  var motion = MeterMotion()
  /// Details form: the foot carries the full reset date, time and countdown.
  var detail = false
  @State private var shown: Double?
  @Environment(\.trayStaticRender) private var staticRender
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  private var target: Double? { pendingReset == nil ? window?.meterUsedPercent : nil }

  var body: some View {
    withPalette { palette in
      let severity = MeterSeverity.of(target)
      VStack(alignment: .leading, spacing: 0) {
        top(palette, severity)
        MeterTrack(key: key, shown: target == nil ? nil : (shown ?? target), severity: severity, notch: target == nil ? nil : notch,
          notchOpacity: notchOpacity, hovered: hovered, dashed: target == nil)
          .padding(.top, labelText == nil ? 6 : 5)
        if labelText != nil { foot(palette).padding(.top, 4) }
      }
      .padding(.trailing, 8)
      .trayHelp(helpText)
    }
    .onAppear(perform: enter)
    .onChange(of: target) { _, next in
      guard let next else { shown = nil; return }
      if staticRender || reduceMotion { shown = next; return }
      withAnimation(.trayValue()) { shown = next }
    }
  }

  @ViewBuilder private func top(_ palette: TrayPalette, _ severity: MeterSeverity) -> some View {
    HStack(alignment: .firstTextBaseline, spacing: 5) {
      if let labelText {
        Text(labelText).font(.system(size: 11.5, weight: .medium)).foregroundStyle(palette.label2)
          .lineLimit(1).truncationMode(.tail)
        Spacer(minLength: 4)
        value(palette, severity, size: 14)
      } else if let reset = pendingReset {
        PendingResetText(reset: reset)
        Spacer(minLength: 0)
      } else {
        value(palette, severity, size: 15)
        Spacer(minLength: 4)
        if let window { ResetLabel(iso: window.resetAt) }
      }
    }.frame(height: 17)
  }

  @ViewBuilder private func value(_ palette: TrayPalette, _ severity: MeterSeverity, size: CGFloat) -> some View {
    if let target {
      CountingPercent(value: shown ?? target, decimals: TrayFormat.decimals(target), size: size)
        .foregroundStyle(palette.valueText(severity))
    } else {
      Text(pendingReset == nil ? unavailableText : "Pending").font(.system(size: 12, weight: .medium)).foregroundStyle(palette.label3).lineLimit(1)
    }
  }

  @ViewBuilder private func foot(_ palette: TrayPalette) -> some View {
    HStack(alignment: .firstTextBaseline, spacing: 8) {
      if let reset = pendingReset {
        if detail {
          // Details keep the whole sentence, on a second line when the column is narrow.
          Text(TrayReset.long(reset)).font(.system(size: 11.5)).foregroundStyle(palette.label2).lineLimit(2)
            .fixedSize(horizontal: false, vertical: true)
        } else {
          HStack(spacing: 4) {
            Image(systemName: "clock").font(.system(size: 10.5, weight: .medium))
            Text(TrayReset.at(reset)).font(.system(size: 11.5)).monospacedDigit().lineLimit(1)
          }
          .foregroundStyle(palette.label2)
        }
      } else if detail {
        Text(TrayFormat.longReset(window?.resetAt)).font(.system(size: 11.5)).foregroundStyle(palette.label2).lineLimit(1)
          .minimumScaleFactor(0.85)
      } else if let window {
        ResetLabel(iso: window.resetAt)
      }
      Spacer(minLength: 0)
      if showAmount, pendingReset == nil, let window, let used = window.used, let limit = window.limit, limit > 0 {
        Text("\(TrayFormat.number(used)) of \(limit >= 1_000_000 ? limit.formatted(.number.notation(.compactName)) : TrayFormat.number(limit))\(window.unit.map { " \($0)" } ?? "")")
          .font(.system(size: 11.5)).monospacedDigit().foregroundStyle(palette.label2).lineLimit(1)
      }
    }.frame(height: pendingReset != nil && detail ? nil : 14)
  }

  private var helpText: String {
    guard let window else { return unavailableHelp }
    if let reset = pendingReset {
      let sampled = AccountFormatting.date(window.sampledAt ?? sampledAt).map { "Sampled \(TrayFormat.relative($0))" }
      return [window.label, TrayReset.long(reset), "The last reading was taken before this reset, so it is not shown",
        sampled ?? "Sample time unavailable"].joined(separator: " · ")
    }
    var parts = [window.label]
    if let target {
      var used = "\(TrayFormat.number(target))% used"
      if let amount = window.used, let limit = window.limit { used += " (\(TrayFormat.number(amount)) of \(TrayFormat.number(limit))\(window.unit.map { " \($0)" } ?? ""))" }
      if target > 100 { used += " · +\(TrayFormat.number(target - 100))% over" }
      parts.append(used)
    } else {
      parts.append(unavailableHelp)
    }
    parts.append(window.resetAt == nil ? "No reset reported" : TrayFormat.longReset(window.resetAt))
    if let notch { parts.append("Notch: auto-switch at \(TrayFormat.number(notch))% used") }
    return parts.joined(separator: " · ")
  }

  private func enter() {
    guard let target else { shown = nil; return }
    let start = motion.from ?? 0
    if staticRender || reduceMotion || !motion.animate || start == target {
      shown = target
      return
    }
    shown = start
    withAnimation(.trayValue().delay(motion.delay)) { shown = target }
  }
}

/// A compact cell whose reading was taken before its reset (F6): the longest of "Reset at 10:15 AM · new reading
/// pending" and its shorter forms that fits, in the unavailable style. The meter's tooltip keeps the whole sentence.
struct PendingResetText: View {
  let reset: Date
  var body: some View {
    withPalette { palette in
      let forms = TrayReset.forms(reset)
      ViewThatFits(in: .horizontal) {
        line(forms[0]); line(forms[1]); line(forms[2]); line(forms[3]); line(forms[4]); line(forms[5])
      }
      .foregroundStyle(palette.label3)
    }
  }

  private func line(_ text: String) -> some View {
    Text(text).font(.system(size: 12, weight: .medium)).lineLimit(1).fixedSize()
  }
}

/// "6d 20h" or "8:15 PM" with a clock glyph; quiet text, emphasised when under two hours. In a tight
/// column the glyph gives way before the text is shortened.
struct ResetLabel: View {
  let iso: String?
  var body: some View {
    withPalette { palette in
      if let text = TrayFormat.shortReset(iso) {
        let soon = TrayFormat.isSoon(iso)
        let label = Text(text).font(.system(size: 11.5, weight: soon ? .medium : .regular)).monospacedDigit().lineLimit(1)
        let clock = Image(systemName: "clock").font(.system(size: 10.5, weight: .medium))
        ViewThatFits(in: .horizontal) {
          HStack(spacing: 4) { clock; label }.fixedSize()
          label.fixedSize()
          HStack(spacing: 4) { clock; label.minimumScaleFactor(0.8) }
        }
        .foregroundStyle(soon ? palette.label : palette.label2)
        .trayHelp(TrayFormat.longReset(iso))
      }
    }
  }
}
