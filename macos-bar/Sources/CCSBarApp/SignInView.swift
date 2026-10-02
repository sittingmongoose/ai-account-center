import SwiftUI
import AppKit
import CCSBarCore

/// The sign-in screen (TSIGN-C, revision for local HTTP): the dashboard's sign-in page at the panel's real size. It
/// replaces the account list inside the same panel; the header (the Apex Soft lockup) and the footer stay. A form card
/// sits on the left as content on the glass (no glass on glass: only its buttons are glass), and the atlas motif sits
/// behind it on the right. The body keeps one height in every state.
struct SignInView: View {
  @ObservedObject var model: SignInModel
  static let bodyHeight: CGFloat = 736
  static let cardWidth: CGFloat = 372

  @Environment(\.trayStaticRender) private var staticRender
  @Environment(\.accessibilityReduceMotion) private var systemReduceMotion
  @FocusState private var focused: SignInField?
  @State private var shown = false

  private var reduceMotion: Bool { staticRender || systemReduceMotion }

  var body: some View {
    withPalette { palette in
      ZStack(alignment: .leading) {
        SignInMotif(state: model.state, busy: model.busy || model.state == .pairing || model.state == .securing,
          dim: model.state == .rateLimited, shown: shown || staticRender, reduceMotion: reduceMotion)
          .frame(width: 420, height: 470)
          .frame(maxWidth: .infinity, alignment: .trailing)
          .padding(.trailing, 4)
          .allowsHitTesting(false)
          .zIndex(0)
        card(palette)
          .frame(width: Self.cardWidth, alignment: .leading)
          .zIndex(1)
      }
      .padding(.horizontal, 28)
      .frame(height: Self.bodyHeight)
      .frame(maxWidth: .infinity)
      .background(GeometryReader { Color.clear.preference(key: ListHeightKey.self, value: $0.size.height) })
      .onAppear { appear() }
      .onChange(of: model.entrance) { _, _ in appear() }
      .onChange(of: model.focus) { _, field in
        guard !staticRender, let field else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.18) { focused = field }
      }
      .accessibilityElement(children: .contain)
      .accessibilityLabel("Sign in to your dashboard")
    }
  }

  private func appear() {
    guard !staticRender else { shown = true; return }
    shown = false
    DispatchQueue.main.async {
      if reduceMotion { shown = true } else { withAnimation(.easeOut(duration: 0.42)) { shown = true } }
    }
    if let field = model.focus {
      DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { focused = field }
    }
  }

  // MARK: Card

  @ViewBuilder private func card(_ palette: TrayPalette) -> some View {
    let spec = SignInSpec.make(model)
    VStack(alignment: .leading, spacing: 0) {
      head(spec, palette).entrance(0, shown: shown, reduce: reduceMotion)
      VStack(alignment: .leading, spacing: 0) {
        if let banner = spec.banner {
          SignInBanner(banner: banner, limitUntil: model.limitUntil, palette: palette, onEnded: model.limitEnded)
            .transition(region)
        }
      }
      .entrance(1, shown: shown, reduce: reduceMotion)
      VStack(alignment: .leading, spacing: 0) {
        VStack(alignment: .leading, spacing: 0) { regions(spec, palette) }
          .modifier(ShakeEffect(progress: CGFloat(model.shake)))
          .animation(reduceMotion ? nil : .linear(duration: 0.44), value: model.shake)
        messageSlot(palette)
        if spec.primary != nil { actions(spec, palette).transition(region) }
      }
      .entrance(2, shown: shown, reduce: reduceMotion)
      if let note = spec.foot {
        footView(note, palette).entrance(3, shown: shown, reduce: reduceMotion)
      }
    }
    .padding(.horizontal, 24).padding(.top, 22).padding(.bottom, 18)
    .background(palette.group, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
    .overlay {
      if palette.reduceTransparency || palette.increasedContrast {
        RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(palette.separator, lineWidth: 0.5)
      }
    }
    .animation(reduceMotion ? nil : .smooth(duration: 0.26), value: model.state)
    .animation(reduceMotion ? nil : .smooth(duration: 0.26), value: model.step)
    .animation(reduceMotion ? nil : .smooth(duration: 0.26), value: model.message)
  }

  private var region: AnyTransition {
    reduceMotion ? .opacity : .opacity.combined(with: .move(edge: .top))
  }

  @ViewBuilder private func head(_ spec: SignInSpec, _ palette: TrayPalette) -> some View {
    VStack(alignment: .leading, spacing: 6) {
      Text(spec.title).font(.system(size: 22, weight: .semibold)).foregroundStyle(palette.label)
        .fixedSize(horizontal: false, vertical: true)
        .id("title-\(spec.title)")
        .transition(reduceMotion ? .opacity : .asymmetric(insertion: .opacity.combined(with: .offset(y: 4)), removal: .opacity))
        .accessibilityAddTraits(.isHeader)
      spec.lede(palette, model)
        .id("lede-\(model.state.rawValue)-\(model.repair)")
        .transition(reduceMotion ? .opacity : .asymmetric(insertion: .opacity.combined(with: .offset(y: 4)), removal: .opacity))
    }
    .padding(.bottom, 16)
  }

  @ViewBuilder private func regions(_ spec: SignInSpec, _ palette: TrayPalette) -> some View {
    if spec.addr {
      SignInInput(model: model, field: .addr, label: "Dashboard address", placeholder: "http://",
        focused: $focused, palette: palette).transition(region)
    }
    if spec.addrHint {
      hint(Text("The dashboard shows it under Settings › Dashboard sign-in."), palette).transition(region)
    }
    if spec.guide { guide(palette).transition(region) }
    if spec.creds {
      SignInInput(model: model, field: .user, label: "Username", aside: model.state == .setupCode ? "letters, numbers, - and _" : nil,
        focused: $focused, palette: palette).transition(region)
      SignInInput(model: model, field: .pass, label: "Password", secure: true, focused: $focused, palette: palette)
        .transition(region)
    }
    if spec.setup {
      StrengthMeter(strength: PasswordStrength.evaluate(model.password), palette: palette).transition(region)
      SignInInput(model: model, field: .confirm, label: "Confirm password", secure: true,
        matches: !model.confirm.isEmpty && model.confirm == model.password, focused: $focused, palette: palette)
        .transition(region)
      if model.codeRequired {
        SignInInput(model: model, field: .code, label: "Setup code", placeholder: "XXXX-XXXX",
          aside: "printed in the server's terminal", mono: true, focused: $focused, palette: palette).transition(region)
        hint(Text("Also saved on the server in \(Text(verbatim: "~/.ccs/auth/setup-code").font(.system(size: 11, design: .monospaced)))"), palette)
          .transition(region)
      }
    }
    if spec.device {
      hint(Text("Pairs as \(Text(model.deviceKind).fontWeight(.semibold).foregroundColor(palette.label)) on \(Text(verbatim: model.deviceHost))"), palette)
        .padding(.top, -2).transition(region)
    }
    if let steps = spec.steps {
      SignInSteps(steps: steps, step: model.state == .success ? steps.count : model.step, palette: palette,
        reduceMotion: reduceMotion)
        .padding(.top, 2).padding(.bottom, 6).transition(region)
    }
  }

  private func hint(_ text: Text, _ palette: TrayPalette) -> some View {
    text.font(.system(size: 12)).foregroundStyle(palette.label2).lineSpacing(2)
      .fixedSize(horizontal: false, vertical: true)
      .padding(.top, -4).padding(.bottom, 12)
  }

  /// State 4's guidance: the dashboard's local address (with Use this when the tray knows it), or the home VPN first.
  @ViewBuilder private func guide(_ palette: TrayPalette) -> some View {
    VStack(alignment: .leading, spacing: 0) {
      HStack(alignment: .center, spacing: 10) {
        Image(systemName: "house").font(.system(size: 13, weight: .medium)).foregroundStyle(palette.label2)
          .frame(width: 16).alignmentGuide(.top) { $0[.top] }
        VStack(alignment: .leading, spacing: 2) {
          Text("Use the dashboard's local address").font(.system(size: 12.5, weight: .semibold)).foregroundStyle(palette.label)
          if let local = model.lastLocalAddress {
            Text("\(Text(verbatim: SignInModel.host(local)).fontWeight(.semibold).foregroundColor(palette.label)), as shown under Settings › Dashboard sign-in")
              .font(.system(size: 12)).foregroundStyle(palette.label2).fixedSize(horizontal: false, vertical: true)
          } else {
            Text("The dashboard shows it under Settings › Dashboard sign-in.")
              .font(.system(size: 12)).foregroundStyle(palette.label2).fixedSize(horizontal: false, vertical: true)
          }
        }
        Spacer(minLength: 6)
        if model.lastLocalAddress != nil {
          Button("Use this") { model.useLocalAddress() }
            .trayGlassButton()
            .disabled(model.busy)
        }
      }
      .padding(.bottom, 9)
      Rectangle().fill(palette.separator).frame(height: 0.5)
      HStack(alignment: .top, spacing: 10) {
        Image(systemName: "lock.shield").font(.system(size: 13, weight: .medium)).foregroundStyle(palette.label2).frame(width: 16)
        VStack(alignment: .leading, spacing: 2) {
          Text("Or connect through your home VPN first").font(.system(size: 12.5, weight: .semibold)).foregroundStyle(palette.label)
          Text("Away from home, turn on the VPN, then use the local address. It puts this Mac on your local network.")
            .font(.system(size: 12)).foregroundStyle(palette.label2).fixedSize(horizontal: false, vertical: true)
        }
      }
      .padding(.top, 9).padding(.bottom, 8)
    }
    .padding(.bottom, 4)
  }

  /// The message slot keeps its height, so an error never moves anything below it.
  @ViewBuilder private func messageSlot(_ palette: TrayPalette) -> some View {
    ZStack(alignment: .topLeading) {
      Color.clear.frame(height: 34)
      if let message = model.message {
        HStack(alignment: .top, spacing: 8) {
          Image(systemName: message.info ? "info.circle" : "exclamationmark.circle.fill")
            .font(.system(size: 13)).foregroundStyle(message.info ? palette.accentText : palette.critText)
            .padding(.top, 1)
          VStack(alignment: .leading, spacing: 0) {
            if let bold = message.bold { Text(bold).fontWeight(.semibold).foregroundStyle(message.info ? palette.label : palette.critText) }
            if let sub = message.sub { Text(sub).foregroundStyle(palette.label2) }
            if let text = message.text { Text(text).foregroundStyle(message.info ? palette.label2 : palette.critText) }
          }
          .font(.system(size: 12)).lineSpacing(1.5).fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("signin-message")
        .transition(.opacity.combined(with: .offset(y: -3)))
      }
    }
    .padding(.bottom, 10)
  }

  @ViewBuilder private func actions(_ spec: SignInSpec, _ palette: TrayPalette) -> some View {
    HStack(spacing: 8) {
      if let alt = spec.alt {
        Button { alt.action(model) } label: {
          Text(alt.label).font(.system(size: 13)).padding(.horizontal, 4)
        }
        .trayGlassButton(large: true)
        .disabled(model.state == .pairing || model.state == .success)
        .accessibilityIdentifier("signin-alt")
      }
      if let primary = spec.primary {
        let done = model.state == .success
        let busy = (model.busy || model.state == .pairing || model.state == .securing) && !done
        Button { model.submit() } label: {
          ZStack {
            Text(primary.label).opacity(busy || done ? 0 : 1).offset(y: busy || done ? -10 : 0)
            HStack(spacing: 8) {
              ProgressView().controlSize(.small).tint(palette.accentInk)
              Text(primary.busy)
            }
            .opacity(busy ? 1 : 0).offset(y: busy ? 0 : 10)
            HStack(spacing: 7) {
              Image(systemName: "checkmark.circle.fill").font(.system(size: 15))
              Text(primary.done)
            }
            .opacity(done ? 1 : 0).offset(y: done ? 0 : 10)
          }
          .font(.system(size: 13, weight: .semibold))
          .frame(maxWidth: .infinity)
          .animation(reduceMotion ? nil : .smooth(duration: 0.26), value: busy)
          .animation(reduceMotion ? nil : .smooth(duration: 0.26), value: done)
        }
        .trayGlassButton(prominent: true, tint: done ? palette.calm : palette.accent, large: true)
        .keyboardShortcut(.defaultAction)
        .disabled(spec.disabled)
        .allowsHitTesting(!busy && !done)
        .accessibilityLabel(done ? primary.done : busy ? primary.busy : primary.label)
        .accessibilityIdentifier("signin-primary")
      }
    }
  }

  private func footView(_ foot: SignInSpec.Foot, _ palette: TrayPalette) -> some View {
    VStack(alignment: .leading, spacing: 0) {
      Rectangle().fill(palette.separator).frame(height: 0.5)
      HStack(alignment: .top, spacing: 8) {
        Image(systemName: foot.icon).font(.system(size: 12.5)).foregroundStyle(palette.label2).frame(width: 15).padding(.top, 1)
        Text(foot.text).font(.system(size: 12)).foregroundStyle(palette.label2).lineSpacing(1.5)
          .fixedSize(horizontal: false, vertical: true)
          .id("foot-\(foot.text)")
          .transition(.opacity)
      }
      .padding(.top, 12)
    }
    .padding(.top, 14)
  }
}

// MARK: - Spec: one state, one description

/// What one state shows: title, lede, banner, open regions, message, buttons and note (the concept's `siSpec`).
struct SignInSpec {
  struct Banner: Equatable {
    var icon: String
    var tone: Tone
    var title: String
    var body: AttributedString
    var countdown = false
    enum Tone { case info, warn, crit }
  }
  struct Primary { var label: String; var busy: String; var done: String }
  struct Alt { var label: String; var action: @MainActor (SignInModel) -> Void }
  struct Foot { var icon: String; var text: String }

  var title = ""
  var ledeKind: Lede = .plain("")
  var banner: Banner?
  var addr = false, addrHint = false, guide = false, creds = false, setup = false, device = false
  var steps: [SignInSteps.Step]?
  var primary: Primary?
  var alt: Alt?
  var foot: Foot?
  var disabled = false

  enum Lede {
    case plain(String)
    /// "<host> ..." with the host in bold.
    case host(String, rest: String, prefix: String = "")
    /// The verified dashboard on one line with Change at its end, after an optional sentence.
    case at(lead: AttributedString?)
    case rich(AttributedString)
  }

  @MainActor @ViewBuilder func lede(_ palette: TrayPalette, _ model: SignInModel) -> some View {
    switch ledeKind {
    case .plain(let text):
      Text(text).font(.system(size: 13)).foregroundStyle(palette.label2).lineSpacing(2).fixedSize(horizontal: false, vertical: true)
    case .host(let host, let rest, let prefix):
      Text("\(prefix)\(Text(verbatim: host).fontWeight(.semibold).foregroundColor(palette.label))\(rest)")
        .font(.system(size: 13)).foregroundStyle(palette.label2).lineSpacing(2).fixedSize(horizontal: false, vertical: true)
    case .rich(let text):
      Text(text).font(.system(size: 13)).foregroundStyle(palette.label2).lineSpacing(2).fixedSize(horizontal: false, vertical: true)
    case .at(let lead):
      VStack(alignment: .leading, spacing: 8) {
        if let lead {
          Text(lead).font(.system(size: 13)).foregroundStyle(palette.label2).lineSpacing(2).fixedSize(horizontal: false, vertical: true)
        }
        HStack(alignment: .firstTextBaseline, spacing: 6) {
          Text(verbatim: model.host).font(.system(size: 13, weight: .semibold)).foregroundStyle(palette.label)
            .lineLimit(1).truncationMode(.middle)
            .help(model.verified?.absoluteString ?? "")
          Spacer(minLength: 10)
          Button("Change") { model.changeAddress() }
            .buttonStyle(.link).font(.system(size: 13))
            .disabled(model.busy || model.state == .pairing || model.state == .success)
            .accessibilityIdentifier("signin-change")
        }
      }
    }
  }

  static func bold(_ text: String) -> AttributedString {
    var part = AttributedString(text)
    part.inlinePresentationIntent = .stronglyEmphasized
    return part
  }

  static func clock(_ date: Date) -> String { date.formatted(date: .omitted, time: .shortened) }

  @MainActor static func make(_ model: SignInModel) -> SignInSpec {
    var spec = SignInSpec()
    let what = "Mac"
    let pairLabel = "Pair this \(what)"
    let lanNote = "Your password is sent once over your local network to pair this tray, then forgotten. The tray keeps a device key in a file only you can read (mode 0600)."
    let firstFoot = Foot(icon: "house", text: "Pairing works from your home network, or over your home VPN.")
    let cancel: Alt? = model.repair ? Alt(label: "Cancel", action: { $0.cancel() }) : nil
    let repairBanner: Banner? = model.repair
      ? Banner(icon: "key", tone: .info, title: "Re-pairing replaces this tray's device key",
        body: AttributedString("The current key keeps working until the new one works and is saved, so Cancel changes nothing."))
      : nil
    func passwordStep() {
      spec.title = model.repair ? "Sign in to re-pair" : "Sign in to pair"
      spec.ledeKind = .at(lead: nil)
      spec.banner = repairBanner
      spec.creds = true
      spec.device = true
      spec.primary = Primary(label: pairLabel, busy: "Checking password", done: "Paired")
      spec.alt = cancel
      spec.foot = Foot(icon: "key", text: lanNote)
    }
    let host = model.host
    let tried = SignInModel.host(model.tried ?? model.verified)
    switch model.state {
    case .firstRun:
      spec.title = model.repair ? "Change the address" : "Connect this \(what)"
      spec.ledeKind = .plain("Enter the address of your AI Account Center dashboard. You sign in once; this tray then keeps its own device key.")
      if let at = model.disconnectedAt {
        spec.banner = Banner(icon: "rectangle.portrait.and.arrow.right", tone: .info, title: "Disconnected at \(clock(at))",
          body: AttributedString(model.disconnectTold
            ? "This tray forgot its device key and the dashboard revoked it. Pair again to see usage."
            : "This tray forgot its device key. The dashboard could not be reached, so revoke Mac tray there under Settings › Dashboard sign-in."))
      } else {
        spec.banner = repairBanner
      }
      spec.addr = true
      spec.addrHint = true
      spec.primary = Primary(label: "Continue", busy: "Checking address", done: "Paired")
      spec.alt = cancel
      spec.foot = firstFoot
    case .password: passwordStep()
    case .wrongPassword: passwordStep()
    case .rateLimited:
      passwordStep()
      let until = model.limitUntil ?? Date().addingTimeInterval(900)
      let minutes = max(1, Int((until.timeIntervalSinceNow / 60).rounded(.up)))
      var body = AttributedString("Too many tries from this computer. Pairing, and sign-in from this computer's browser, open again at ")
      body += bold(clock(until))
      body += AttributedString(".")
      spec.banner = Banner(icon: "timer", tone: .warn, title: "Try again in \(minutes) minute\(minutes == 1 ? "" : "s")", body: body, countdown: true)
      spec.disabled = true
    case .setupCode:
      spec.title = "Set up sign-in"
      spec.ledeKind = .host(host, rest: " has no sign-in yet. Choose one here; your browser and both trays use it.")
      spec.creds = true
      spec.setup = true
      spec.primary = Primary(label: "Create sign-in and pair", busy: "Creating sign-in", done: "Paired")
      spec.alt = cancel
      spec.foot = Foot(icon: "apple.terminal", text: "The setup code proves you can reach the dashboard machine. It works once and expires after 60 minutes.")
    case .pairing:
      spec.title = "Pairing this \(what)"
      spec.ledeKind = .host(host, rest: " is issuing this \(what) its own device key.")
      spec.steps = steps(model)
      spec.primary = Primary(label: pairLabel, busy: "Pairing", done: "Paired")
      spec.foot = Foot(icon: "key", text: lanNote)
    case .securing:
      spec.title = "Securing this tray"
      spec.ledeKind = .plain("An earlier version kept your dashboard password here. The tray is trading it for a device key, once, by itself.")
      spec.steps = steps(model)
      spec.foot = Foot(icon: "key", text: "There is nothing to do. The device key is kept in a file only you can read (mode 0600).")
    case .notLocal:
      spec.title = "This address isn't on your local network"
      if let seen = model.seenAs {
        spec.ledeKind = .host(seen, rest: ", which isn't on your local network. Pairing sends your password over plain HTTP, so it works only from your home network or your home VPN.",
          prefix: "The dashboard sees this \(what) at ")
      } else {
        spec.ledeKind = .host(tried, rest: " is a public address. Pairing sends your password over plain HTTP, so it works only from your home network or your home VPN.")
      }
      spec.addr = true
      spec.guide = true
      spec.primary = Primary(label: "Try again", busy: "Checking address", done: "Paired")
      spec.alt = cancel
      spec.foot = Foot(icon: "lock", text: model.seenAs == nil
        ? "Nothing was sent: the tray checks the address before it asks for your password."
        : "Nothing was sent: the dashboard refused this connection before your password was asked for.")
    case .pairingOff:
      spec.title = "Pairing is turned off for remote computers"
      var lead = AttributedString("On the dashboard, open Settings › Dashboard sign-in and turn on ")
      lead += bold("Trust this local network")
      lead += AttributedString(".")
      spec.ledeKind = .at(lead: lead)
      spec.banner = repairBanner
      spec.primary = Primary(label: "Try again", busy: "Checking", done: "Paired")
      spec.alt = cancel
      spec.foot = Foot(icon: "info.circle", text: "Only a browser on the dashboard machine itself can turn it on.")
    case .unreachable:
      spec.title = "Can't reach that address"
      spec.ledeKind = .host(tried, rest: " didn't answer.")
      if model.repair, let current = model.owner?.connection?.baseURL {
        var body = AttributedString("Nothing was saved. This tray keeps using ")
        body += bold(SignInModel.host(current))
        body += AttributedString(" until a new address answers and pairs.")
        spec.banner = Banner(icon: "checkmark.shield", tone: .info, title: "Your current connection is unchanged", body: body)
      }
      spec.addr = true
      spec.primary = Primary(label: "Retry", busy: "Checking", done: "Paired")
      spec.alt = model.repair ? Alt(label: "Keep current connection", action: { $0.cancel() }) : nil
      spec.foot = Foot(icon: "lock", text: "The tray checks an address before it saves anything.")
    case .wrongAddress:
      spec.title = "That isn't a dashboard address"
      spec.ledeKind = .host(tried, rest: " answered, but not as AI Account Center.")
      spec.addr = true
      spec.primary = Primary(label: "Retry", busy: "Checking", done: "Paired")
      spec.alt = cancel
      spec.foot = firstFoot
    case .signedOut, .signedOutAll, .expired:
      spec.title = "This tray was signed out"
      spec.ledeKind = .rich(signedOutLede(model))
      spec.creds = true
      spec.device = true
      spec.primary = Primary(label: "Pair again", busy: "Checking password", done: "Paired")
      let stopped = model.lastSyncedAt.map { "Usage stopped updating at \(clock($0)). " } ?? ""
      spec.foot = Foot(icon: "rectangle.portrait.and.arrow.right",
        text: model.state == .expired ? "The tray won't retry on its own." : "\(stopped)The tray won't retry on its own.")
    case .success:
      spec.title = model.flow == .secure ? "This tray is secured" : "Paired"
      spec.ledeKind = .plain("Opening your accounts.")
      spec.steps = steps(model)
      spec.primary = model.flow == .secure ? nil : Primary(label: pairLabel, busy: "Pairing", done: "Paired")
      spec.foot = Foot(icon: "key", text: "The device key is saved and the password is gone.")
    }
    return spec
  }

  /// Who and when, when the dashboard sends them; otherwise only what the tray knows.
  @MainActor static func signedOutLede(_ model: SignInModel) -> AttributedString {
    let note = model.note
    let at = AccountFormatting.date(note?.at)
    let fromServer = note?.revokedBy != nil || note?.revokedReason != nil
    let when: String = {
      guard fromServer, let at else { return "" }
      return Calendar.current.isDateInToday(at) ? ", today at \(clock(at))" : ", \(at.formatted(date: .abbreviated, time: .shortened))"
    }()
    switch model.state {
    case .expired:
      if let at, fromServer { return AttributedString("Not used for 90 days, so its device key expired on \(at.formatted(date: .abbreviated, time: .omitted)).") }
      return AttributedString("Not used for 90 days, so its device key expired.")
    case .signedOutAll:
      if let by = note?.revokedBy {
        var text = bold(by)
        text += AttributedString(" chose Sign out all devices in the dashboard\(when).")
        return text
      }
      return AttributedString("Sign out all devices was chosen in the dashboard\(when).")
    default:
      if note?.reason == "invalid_token" {
        return AttributedString("The dashboard no longer accepts this tray's device key. It may have been paired again from another copy.")
      }
      if let by = note?.revokedBy {
        var text = AttributedString("Revoked from the dashboard by ")
        text += bold(by)
        text += AttributedString("\(when).")
        return text
      }
      return AttributedString("Revoked from the dashboard\(when).")
    }
  }

  @MainActor static func steps(_ model: SignInModel) -> [SignInSteps.Step] {
    let host = model.host
    if model.flow == .secure {
      return [
        .init(title: "Rollback copy kept", sub: "\(model.rollbackName), until the new key has worked once"),
        .init(title: "Device key issued", sub: "For \(model.deviceKind) on \(model.deviceHost), traded once for the saved password"),
        .init(title: "Checking that the key works", sub: "One request to the dashboard with the new key"),
        .init(title: "Deleting the saved password", sub: "And the rollback copy"),
      ]
    }
    return [
      model.flow == .setup
        ? .init(title: "Sign-in created", sub: "Username \(model.username) on \(host)")
        : .init(title: "Password checked", sub: "Sent once to \(host)"),
      .init(title: "Device key issued", sub: "For \(model.deviceKind) on \(model.deviceHost); the dashboard keeps only a hash of it"),
      .init(title: "Saving the key", sub: "In \(model.keyFilePath), a file only you can read (mode 0600)"),
      .init(title: "Forgetting the password", sub: "It is never written to disk"),
    ]
  }
}

// MARK: - Parts

/// A field: label over a 34 pt translucent box, the focus halo, and (for a password) the eye that swaps the secure
/// field for a plain one.
struct SignInInput: View {
  @ObservedObject var model: SignInModel
  let field: SignInField
  let label: String
  var placeholder = ""
  var aside: String?
  var secure = false
  var mono = false
  var matches = false
  var focused: FocusState<SignInField?>.Binding
  let palette: TrayPalette
  @Environment(\.trayStaticRender) private var staticRender

  private var text: Binding<String> {
    switch field {
    case .addr: return $model.address
    case .user: return $model.username
    case .pass: return $model.password
    case .confirm: return $model.confirm
    case .code: return $model.setupCode
    }
  }

  var body: some View {
    let isFocused = focused.wrappedValue == field
    let bad = model.badField == field
    let disabled = model.fieldsDisabled
    let revealed = model.revealed.contains(field)
    VStack(alignment: .leading, spacing: 6) {
      HStack(alignment: .firstTextBaseline, spacing: 10) {
        Text(label).font(.system(size: 12, weight: .semibold)).foregroundStyle(palette.label2)
        Spacer(minLength: 0)
        if matches {
          HStack(spacing: 4) {
            Image(systemName: "checkmark.circle.fill").font(.system(size: 11.5))
            Text("Matches")
          }
          .font(.system(size: 11.5, weight: .semibold)).foregroundStyle(palette.goodText)
          .transition(.opacity)
        } else if let aside {
          Text(aside).font(.system(size: 11.5)).foregroundStyle(palette.label2).lineLimit(1)
        }
      }
      ZStack(alignment: .trailing) {
        Group {
          if secure && !revealed {
            SecureField(placeholder, text: text)
          } else {
            TextField(placeholder, text: text)
          }
        }
        .textFieldStyle(.plain)
        .font(mono ? .system(size: 12.5, design: .monospaced) : .system(size: 13))
        .foregroundStyle(palette.label)
        .focused(focused, equals: field)
        .disabled(disabled)
        .autocorrectionDisabled()
        .textContentType(contentType)
        .onSubmit { model.submit() }
        .onChange(of: text.wrappedValue) { _, _ in model.clearError(field) }
        .padding(.leading, 11).padding(.trailing, secure ? 40 : 11)
        .frame(height: 34)
        .accessibilityLabel(label)
        .accessibilityIdentifier("signin-\(field.rawValue)")
        if secure {
          Button { model.toggleReveal(field) } label: {
            Image(systemName: revealed ? "eye.slash" : "eye")
              .font(.system(size: 13, weight: .medium))
              .foregroundStyle(revealed ? palette.accentText : palette.label2)
              .contentTransition(.symbolEffect(.replace))
              .frame(width: 28, height: 28)
              .contentShape(Circle())
          }
          .buttonStyle(.plain)
          .disabled(disabled)
          .padding(.trailing, 4)
          .help(revealed ? "Hide password" : "Show password")
          .accessibilityLabel(revealed ? "Hide password" : "Show password")
        }
      }
      .background(fill, in: RoundedRectangle(cornerRadius: 9, style: .continuous))
      .overlay(RoundedRectangle(cornerRadius: 9, style: .continuous)
        .strokeBorder(edge(isFocused: isFocused, bad: bad), lineWidth: isFocused || bad || matches ? 1 : 0.5))
      .overlay {
        // The focus halo fades and settles in around the box.
        RoundedRectangle(cornerRadius: 13, style: .continuous)
          .stroke((bad ? palette.crit : palette.accent).opacity(bad ? 0.26 : 0.3), lineWidth: 3)
          .padding(-4)
          .opacity(isFocused && !staticRender ? 1 : 0)
          .scaleEffect(isFocused ? 1 : 0.985)
          .animation(.easeOut(duration: 0.26), value: isFocused)
          .allowsHitTesting(false)
      }
      .opacity(disabled ? 0.72 : 1)
    }
    .padding(.bottom, 12)
  }

  private var contentType: NSTextContentType? {
    switch field {
    case .addr: return .URL
    case .user: return .username
    case .pass, .confirm: return .password
    case .code: return .oneTimeCode
    }
  }

  private var fill: Color {
    if palette.reduceTransparency { return palette.solidControl }
    if model.fieldsDisabled { return palette.controlInner }
    return palette.control
  }

  private func edge(isFocused: Bool, bad: Bool) -> Color {
    if bad { return palette.crit }
    if isFocused { return palette.accent }
    if matches { return palette.calm.opacity(0.75) }
    return palette.reduceTransparency ? palette.label4 : palette.controlEdge
  }
}

/// A banner: an icon in the state colour, a title, a body and a hairline (no tinted box, no side bar). The rate-limit
/// banner adds the countdown and its draining bar.
struct SignInBanner: View {
  let banner: SignInSpec.Banner
  let limitUntil: Date?
  let palette: TrayPalette
  let onEnded: () -> Void

  var body: some View {
    VStack(alignment: .leading, spacing: 0) {
      HStack(alignment: .top, spacing: 10) {
        Image(systemName: iconName).font(.system(size: 14, weight: .medium)).foregroundStyle(tone).frame(width: 18).padding(.top, 1)
        VStack(alignment: .leading, spacing: 2) {
          Text(banner.title).font(.system(size: 13, weight: .semibold)).foregroundStyle(palette.label)
          Text(banner.body).font(.system(size: 12.5)).foregroundStyle(palette.label2).lineSpacing(1.5)
            .fixedSize(horizontal: false, vertical: true)
        }
        Spacer(minLength: 6)
        if banner.countdown, let limitUntil {
          TimelineView(.periodic(from: .now, by: 1)) { context in
            let left = max(0, limitUntil.timeIntervalSince(context.date))
            let seconds = Int(left.rounded(.up))
            VStack(alignment: .trailing, spacing: 4) {
              Text(verbatim: String(format: "%d:%02d", seconds / 60, seconds % 60))
                .font(.system(size: 22, weight: .semibold)).monospacedDigit().foregroundStyle(palette.warnText)
              Text("left").font(.system(size: 11.5)).foregroundStyle(palette.label2)
            }
          }
        }
      }
      if banner.countdown, let limitUntil {
        TimelineView(.periodic(from: .now, by: 1)) { context in
          let left = max(0, limitUntil.timeIntervalSince(context.date))
          GeometryReader { geometry in
            ZStack(alignment: .leading) {
              Capsule().fill(palette.track)
              Capsule().fill(LinearGradient(colors: [palette.fill(.warn).start, palette.fill(.warn).end],
                startPoint: .leading, endPoint: .trailing))
                .frame(width: geometry.size.width * min(1, left / (15 * 60)))
                .animation(.linear(duration: 1), value: left)
            }
          }
          .frame(height: 5)
        }
        .padding(.leading, 28).padding(.top, 9)
        .task(id: limitUntil) {
          let wait = limitUntil.timeIntervalSinceNow
          if wait > 0 { try? await Task.sleep(nanoseconds: UInt64(wait * 1_000_000_000)) }
          if !Task.isCancelled { onEnded() }
        }
      }
      Rectangle().fill(palette.separator).frame(height: 0.5).padding(.top, 14)
    }
    .padding(.bottom, 14)
  }

  private var iconName: String {
    switch banner.icon {
    case "key": return "key.fill"
    default: return banner.icon
    }
  }

  private var tone: Color {
    switch banner.tone {
    case .info: return palette.accentText
    case .warn: return palette.warnText
    case .crit: return palette.critText
    }
  }
}

/// The setup step's strength meter: one track, severity colours, the fill eases to its width (never past it).
struct StrengthMeter: View {
  let strength: PasswordStrength
  let palette: TrayPalette

  var body: some View {
    VStack(alignment: .leading, spacing: 5) {
      GeometryReader { geometry in
        ZStack(alignment: .leading) {
          Capsule().fill(palette.track)
          ForEach([0.25, 0.5, 0.75], id: \.self) { mark in
            Rectangle().fill(palette.tick).frame(width: 1).padding(.vertical, 1)
              .offset(x: geometry.size.width * mark)
          }
          Capsule().fill(LinearGradient(colors: [colors.start, colors.end], startPoint: .leading, endPoint: .trailing))
            .frame(width: geometry.size.width * strength.fill)
        }
      }
      .frame(height: 5)
      .animation(.trayValue(duration: 0.42), value: strength.fill)
      HStack(alignment: .firstTextBaseline, spacing: 12) {
        Text(strength.hint).font(.system(size: 11.5)).foregroundStyle(palette.label2).lineLimit(1).truncationMode(.tail)
        Spacer(minLength: 0)
        Text(strength.word).font(.system(size: 12, weight: .semibold)).foregroundStyle(wordColor)
      }
    }
    .padding(.top, -4).padding(.bottom, 12)
  }

  private var colors: (start: Color, end: Color) {
    switch strength.level {
    case 3: return palette.fill(.warn)
    case 4, 5: return palette.fill(.calm)
    default: return palette.fill(.crit)
    }
  }

  private var wordColor: Color {
    switch strength.level {
    case 1, 2: return palette.critText
    case 3: return palette.warnText
    case 4, 5: return palette.goodText
    default: return palette.label2
    }
  }
}

/// Progress steps: a pending ring, then the progress indicator, then a check that scales in.
struct SignInSteps: View {
  struct Step: Equatable { let title: String; let sub: String }
  let steps: [Step]
  let step: Int
  let palette: TrayPalette
  let reduceMotion: Bool

  var body: some View {
    VStack(alignment: .leading, spacing: 11) {
      ForEach(Array(steps.enumerated()), id: \.offset) { index, item in
        let done = index < step, active = index == step
        HStack(alignment: .top, spacing: 10) {
          ZStack {
            if done {
              Image(systemName: "checkmark.circle.fill").font(.system(size: 16)).foregroundStyle(palette.calm)
                .transition(reduceMotion ? .opacity : .scale(scale: 0.6).combined(with: .opacity))
            } else if active {
              ProgressView().controlSize(.small).transition(.opacity)
            } else {
              Image(systemName: "circle").font(.system(size: 16, weight: .light)).foregroundStyle(palette.label4)
                .transition(.opacity)
            }
          }
          .frame(width: 18, height: 18)
          VStack(alignment: .leading, spacing: 1) {
            Text(item.title).font(.system(size: 13, weight: .medium))
              .foregroundStyle(done || active ? palette.label : palette.label3)
            Text(item.sub).font(.system(size: 12)).foregroundStyle(palette.label2).lineSpacing(1)
              .fixedSize(horizontal: false, vertical: true)
          }
        }
        .accessibilityElement(children: .combine)
        .accessibilityValue(done ? "Done" : active ? "In progress" : "Waiting")
      }
    }
  }
}

/// The atlas motif behind the card: nine contour rings of one summit (the dashboard's rounded-triangle rings and
/// wobble, scaled by 0.62), the Apex mark at the top, three elevation figures and a four-segment scale bar.
struct SignInMotif: View {
  let state: SignInState
  let busy: Bool
  let dim: Bool
  let shown: Bool
  let reduceMotion: Bool
  @State private var sweep = false

  static let summit = CGPoint(x: 226, y: 196)
  static let base = CGPoint(x: 210, y: 236)
  static let r0: CGFloat = 178
  static let rings = 9

  static func ringPoints(_ index: Int) -> [CGPoint] {
    let scale = 1 - CGFloat(index) * 0.1
    let vertices = [-90.0, 30.0, 150.0].map { angle -> CGPoint in
      let radians = angle * .pi / 180
      return CGPoint(x: base.x + cos(radians) * r0 * 1.12, y: base.y + sin(radians) * r0 * 0.88)
    }
    var points: [CGPoint] = []
    for edge in 0..<3 {
      let a = vertices[edge], b = vertices[(edge + 1) % 3]
      for (k, t) in [0.14, 0.38, 0.62, 0.86].enumerated() {
        let x = a.x + (b.x - a.x) * t, y = a.y + (b.y - a.y) * t
        let kk = Double(edge * 4 + k), i = Double(index)
        let wobble = 0.05 * sin(kk * 1.7 + i * 0.3) + 0.024 * sin(kk * 3.3 - i * 0.5) - 0.012 * cos(kk * 0.9 + i * 0.9)
        let m = Double(scale) * (1 + wobble * (0.55 + 0.45 * Double(scale)))
        points.append(CGPoint(x: summit.x + (x - summit.x) * m, y: summit.y + (y - summit.y) * m))
      }
    }
    return points
  }

  /// Closed Catmull-Rom through the points, as cubic Béziers.
  static func ringPath(_ index: Int) -> Path {
    let points = ringPoints(index), n = points.count
    var path = Path()
    path.move(to: points[0])
    for i in 0..<n {
      let p0 = points[(i - 1 + n) % n], p1 = points[i], p2 = points[(i + 1) % n], p3 = points[(i + 2) % n]
      path.addCurve(to: p2,
        control1: CGPoint(x: p1.x + (p2.x - p0.x) / 6, y: p1.y + (p2.y - p0.y) / 6),
        control2: CGPoint(x: p2.x - (p3.x - p1.x) / 6, y: p2.y - (p3.y - p1.y) / 6))
    }
    path.closeSubpath()
    return path
  }

  var body: some View {
    withPalette { palette in
      let success = state == .success
      ZStack(alignment: .topLeading) {
        ForEach(0..<Self.rings, id: \.self) { index in
          let idx = index % 2 == 0
          Self.ringPath(index)
            .stroke(ringColor(palette, idx: idx, success: success), lineWidth: idx ? 1.3 : 1)
            .opacity(shown ? (dim ? 0.5 : 1) : 0)
            .scaleEffect(shown ? 1 : 0.94, anchor: UnitPoint(x: Self.summit.x / 420, y: Self.summit.y / 470))
            .animation(reduceMotion ? nil : .easeOut(duration: 0.9).delay(Double(Self.rings - 1 - index) * 0.05 + 0.14), value: shown)
            .animation(.easeInOut(duration: 0.42), value: success)
            .animation(.easeInOut(duration: 0.42), value: dim)
        }
        ForEach([2, 4, 6], id: \.self) { index in
          let p = Self.ringPoints(index)[6], o = Self.ringPoints(index - 1)[6]
          Text(verbatim: ["2": "25", "4": "50", "6": "75"]["\(index)"] ?? "")
            .font(.system(size: 10.5, design: .monospaced)).monospacedDigit()
            .foregroundStyle(palette.label3)
            .position(x: (p.x + o.x) / 2, y: (p.y + o.y) / 2)
            .opacity(shown ? (dim ? 0.5 : 1) : 0)
            .animation(reduceMotion ? nil : .easeOut(duration: 0.9).delay(Double(Self.rings - 1 - index) * 0.05 + 0.38), value: shown)
        }
        ApexMark(size: 30)
          .position(x: Self.summit.x, y: Self.summit.y - 1)
          .scaleEffect(shown ? (success ? 1.08 : 1) : 0.6, anchor: UnitPoint(x: Self.summit.x / 420, y: Self.summit.y / 470))
          .opacity(shown ? 1 : 0)
          .animation(reduceMotion ? nil : .spring(duration: 0.52, bounce: 0.3).delay(shown ? 0.6 : 0), value: shown)
          .animation(.spring(duration: 0.52, bounce: 0.3), value: success)
        scaleBar(palette, success: success)
      }
      .frame(width: 420, height: 470, alignment: .topLeading)
    }
    .accessibilityHidden(true)
    .onAppear { sweep = busy }
    .onChange(of: busy) { _, value in sweep = value }
  }

  private func ringColor(_ palette: TrayPalette, idx: Bool, success: Bool) -> Color {
    let base = palette.label.opacity(idx ? 0.26 / 0.9 : 0.15 / 0.9)
    guard success else { return base }
    return palette.accent.opacity(idx ? 0.62 : 0.42)
  }

  @ViewBuilder private func scaleBar(_ palette: TrayPalette, success: Bool) -> some View {
    let width: CGFloat = 168, height: CGFloat = 6, segment = width / 4
    let x = Self.base.x - width / 2, y: CGFloat = 378
    let paper = palette.scheme == .dark ? Color.black.opacity(0.32) : Color.white.opacity(0.55)
    ZStack(alignment: .topLeading) {
      HStack(spacing: 0) {
        ForEach(0..<4, id: \.self) { k in
          Rectangle().fill(k % 2 == 0 ? palette.label2 : paper)
            .overlay(Rectangle().strokeBorder(k % 2 == 0 ? palette.label2 : palette.label.opacity(0.26 / 0.9), lineWidth: 1))
            .frame(width: segment, height: height)
        }
      }
      .scaleEffect(x: shown ? 1 : 0, y: 1, anchor: .leading)
      .animation(reduceMotion ? nil : .easeOut(duration: 0.9).delay(0.72), value: shown)
      LinearGradient(colors: [Color(red: 0x2F / 255, green: 0x6B / 255, blue: 1), Color(red: 0x1F / 255, green: 0xC6 / 255, blue: 0xEE / 255)],
        startPoint: .leading, endPoint: .trailing)
        .frame(width: width, height: height)
        .scaleEffect(x: success ? 1 : 0, y: 1, anchor: .leading)
        .animation(.easeOut(duration: 0.52), value: success)
      if busy && !reduceMotion {
        TimelineView(.animation) { context in
          let phase = context.date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: 1.1) / 1.1
          let eased = phase < 0.5 ? 2 * phase * phase : 1 - pow(-2 * phase + 2, 2) / 2
          LinearGradient(stops: [.init(color: Color(red: 0x2F / 255, green: 0x6B / 255, blue: 1).opacity(0), location: 0),
            .init(color: Color(red: 0x2F / 255, green: 0x6B / 255, blue: 1).opacity(0.85), location: 0.6),
            .init(color: Color(red: 0x1F / 255, green: 0xC6 / 255, blue: 0xEE / 255), location: 1)],
            startPoint: .leading, endPoint: .trailing)
            .frame(width: 56, height: height)
            .offset(x: -56 + (width + 56) * eased)
        }
        .frame(width: width, height: height, alignment: .leading)
        .clipped()
        .transition(.opacity)
      }
      ForEach(0..<5, id: \.self) { k in
        Text(verbatim: "\(k * 25)").font(.system(size: 10.5, design: .monospaced)).monospacedDigit()
          .foregroundStyle(palette.label3)
          .position(x: CGFloat(k) * segment, y: height + 14)
      }
      .frame(width: width, height: height + 20, alignment: .topLeading)
    }
    .frame(width: width, height: height + 20, alignment: .topLeading)
    .offset(x: x, y: y)
  }
}

/// The error shake: x = 7 (1 - p)^2 sin(8 pi p) over one run of 0.44 s per bump.
struct ShakeEffect: GeometryEffect {
  var progress: CGFloat
  var animatableData: CGFloat {
    get { progress }
    set { progress = newValue }
  }
  func effectValue(size: CGSize) -> ProjectionTransform {
    let p = progress - progress.rounded(.down)
    guard p > 0 else { return ProjectionTransform(.identity) }
    let x = 7 * (1 - p) * (1 - p) * sin(8 * .pi * p)
    return ProjectionTransform(CGAffineTransform(translationX: x, y: 0))
  }
}

private struct SignInEntrance: ViewModifier {
  let index: Int
  let shown: Bool
  let reduce: Bool
  func body(content: Content) -> some View {
    content
      .opacity(shown ? 1 : 0)
      .offset(y: shown || reduce ? 0 : 12)
      .animation(reduce ? .easeOut(duration: 0.15) : .trayValue(duration: 0.42).delay(0.06 + Double(index) * 0.064), value: shown)
  }
}

extension View {
  fileprivate func entrance(_ index: Int, shown: Bool, reduce: Bool) -> some View {
    modifier(SignInEntrance(index: index, shown: shown, reduce: reduce))
  }
}
