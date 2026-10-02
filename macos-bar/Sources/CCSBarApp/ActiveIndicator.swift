import SwiftUI
import CCSBarCore

/// The filled check-circle whose check can draw in.
struct CheckCircle: View {
  var draw = false
  var size: CGFloat = TrayMetrics.check
  @State private var progress: CGFloat = 1
  @Environment(\.trayStaticRender) private var staticRender
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    withPalette { palette in
      ZStack {
        Circle().fill(palette.accent).padding(size * 0.04)
        CheckShape().trim(from: 0, to: progress)
          .stroke(palette.accentInk, style: StrokeStyle(lineWidth: size * 0.105, lineCap: .round, lineJoin: .round))
      }
    }
    .frame(width: size, height: size)
    .onAppear {
      guard draw, !staticRender, !reduceMotion else { return }
      progress = 0
      withAnimation(.trayValue(duration: TrayMotion.checkDuration).delay(0.12)) { progress = 1 }
    }
  }
}

private struct CheckShape: Shape {
  func path(in rect: CGRect) -> Path {
    var path = Path()
    let s = rect.width / 20
    path.move(to: CGPoint(x: 6.4 * s, y: 10.3 * s))
    path.addLine(to: CGPoint(x: 8.9 * s, y: 12.8 * s))
    path.addLine(to: CGPoint(x: 13.7 * s, y: 7.8 * s))
    return path
  }
}

/// The active row's action slot: the check, "Active" and "on Ubuntu", with no button chrome. It sits at
/// the slot's leading edge, so the check lands on the Activate capsule's left edge and "Active" on the
/// "Activate" label's x (the capsule's 24 pt label inset is the check plus its 6 pt gap).
struct ActiveLabel: View {
  let platform: String
  var draw = false
  var probe = ""
  var body: some View {
    withPalette { palette in
      HStack(spacing: TrayMetrics.checkGap) {
        CheckCircle(draw: draw).alignmentProbe("\(probe)|icon")
        VStack(alignment: .leading, spacing: 0) {
          Text("Active").font(.system(size: 12.5, weight: .semibold)).foregroundStyle(palette.label)
            .alignmentProbe("\(probe)|label")
          Text("on \(TrayFormat.platformName(platform))").font(.system(size: 11)).foregroundStyle(palette.label2)
            .alignmentProbe("\(probe)|sub")
        }
        .lineLimit(1)
      }
      .fixedSize()
      .accessibilityElement(children: .combine)
      .accessibilityLabel("Active on \(TrayFormat.platformName(platform))")
    }
  }
}

/// No account in the section is reported active and this one cannot be switched to.
struct NotReportedLabel: View {
  var body: some View {
    withPalette { palette in
      HStack(spacing: TrayMetrics.checkGap) {
        Circle().strokeBorder(palette.label3, style: StrokeStyle(lineWidth: 1.2, dash: [2.2, 2.2]))
          .frame(width: TrayMetrics.check, height: TrayMetrics.check)
        VStack(alignment: .leading, spacing: 0) {
          Text("Not reported").font(.system(size: 12.5, weight: .medium)).foregroundStyle(palette.label2)
          Text("as active").font(.system(size: 11)).foregroundStyle(palette.label3)
        }.lineLimit(1)
      }
      .fixedSize()
      .hoverHelp("The dashboard has not reported which Antigravity account is active", id: "antigravity-not-reported")
    }
  }
}

/// A text-only glass capsule whose label sits 24 pt from each edge.
struct ActivateButton: View {
  let title: String
  var busy = false
  var enabled = true
  let help: String
  let id: String
  let action: () -> Void

  var body: some View {
    withPalette { palette in
      Button(action: action) {
        HStack(spacing: 6) {
          if busy { ProgressView().controlSize(.mini) }
          Text(title).font(.system(size: 12.5, weight: .medium)).foregroundStyle(enabled ? palette.label : palette.label3)
            .alignmentProbe("slot|\(id)|label")
        }
        .padding(.horizontal, busy ? 12 : TrayMetrics.activateInset)
        .frame(height: 26)
        .contentShape(Capsule())
      }
      .buttonStyle(.plain)
      .glassControl()
      .alignmentProbe("slot|\(id)|icon")
      .disabled(!enabled || busy)
      .hoverHelp(help, id: id, action: enabled ? action : nil)
    }
  }
}

/// The inline switch confirmation under a row: the target, the programs that will stop, the warning,
/// Cancel and "Stop, switch, restart". Nothing is sent until the explicit confirm.
struct SwitchConfirmView: View {
  let product: String
  let identity: String
  let processes: [CodexSwitchProcess]
  let warning: String
  let expiresAt: String
  let onCancel: () -> Void
  let onConfirm: () -> Void

  var body: some View {
    withPalette { palette in
      VStack(alignment: .leading, spacing: 8) {
        HStack(spacing: 7) {
          Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(palette.warn).font(.system(size: 13))
          Text("Switch \(product) to \(identity)?").font(.system(size: 13, weight: .semibold)).foregroundStyle(palette.label)
        }
        if !processes.isEmpty {
          VStack(alignment: .leading, spacing: 3) {
            ForEach(Array(processes.enumerated()), id: \.offset) { _, process in
              HStack(spacing: 10) {
                Text(process.label).font(.system(size: 12)).foregroundStyle(palette.label)
                Spacer(minLength: 8)
                Text("PID \(process.pid)").font(.system(size: 11.5)).monospacedDigit().foregroundStyle(palette.label2)
                Text(process.role).font(.system(size: 11.5)).foregroundStyle(palette.label2).frame(width: 96, alignment: .leading)
              }
            }
          }
          .padding(.horizontal, 10).padding(.vertical, 7)
          .background(palette.group, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        }
        Text(warning).font(.system(size: 12)).foregroundStyle(palette.label2).fixedSize(horizontal: false, vertical: true)
        if let expiry = AccountFormatting.date(expiresAt) {
          Text("Review valid until \(expiry.formatted(date: .omitted, time: .standard))")
            .font(.system(size: 11)).foregroundStyle(palette.label3)
        }
        HStack(spacing: 8) {
          Spacer()
          Button("Cancel", action: onCancel).buttonStyle(.glass).controlSize(.regular)
            .keyboardShortcut(.cancelAction)
          Button(action: onConfirm) {
            Text("Stop, switch, restart").font(.system(size: 13, weight: .semibold))
          }
          .buttonStyle(.glassProminent).tint(palette.critText).controlSize(.regular)
        }
      }
      .padding(12)
      .background(palette.warn.opacity(0.14), in: RoundedRectangle(cornerRadius: TrayMetrics.rowRadius, style: .continuous))
      .overlay(RoundedRectangle(cornerRadius: TrayMetrics.rowRadius, style: .continuous).strokeBorder(palette.warn.opacity(0.45), lineWidth: 0.5))
      .padding(.horizontal, 4).padding(.vertical, 4)
    }
  }
}
