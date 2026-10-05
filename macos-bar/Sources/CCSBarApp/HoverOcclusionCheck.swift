import SwiftUI
import AppKit
import CCSBarCore

/// Which hover tag actually presents under the pointer, with the real panel hosted offscreen from a
/// sanitized fixture. The pointer is a stand-in (`HoverProbe`): the real pointer never moves, nothing is
/// clicked and no event is posted to the system. For every probed point the check finds each tag view whose
/// tracking area AppKit would fire there (the tag views use `.inVisibleRect` tracking), sends it the same
/// mouse-entered call AppKit makes, runs the tag's delayed show at once, and reads which tags present and
/// with what text. With Settings open, a point over a Settings control must present exactly that control's
/// own tag, and no point may present a tag from the account list hidden underneath.
@MainActor
enum HoverOcclusionCheck {
  struct Tag {
    let view: NSView
    let id: String
    let presenter: HelpPresenter
    var isRow: Bool { view is DetailsRowButton }
  }

  struct Presentation {
    var shown: [(id: String, text: String)] = []
    var windowTexts: [String] = []
    var entered: [String] = []
    /// Rows whose highlight is on while the pointer rests there.
    var highlighted: [String] = []
    var leftover = 0
  }

  /// Tags that stay visible above Settings: the panel header and footer, and Settings itself.
  static func allowedOverSettings(_ id: String) -> Bool {
    id.hasPrefix("settings-") || ["header-menu", "footer-dashboard", "footer-refresh", "footer-settings",
      "footer-quit", "codex-auto-info"].contains(id)
  }

  static func run(input: String) -> Never {
    do {
      let dashboard = try JSONDecoder().decode(AccountDashboard.self, from: Data(contentsOf: URL(fileURLWithPath: input)))
      var failures: [String] = []
      func fail(_ message: String) { if failures.count < 400 { failures.append(message) } }

      // The fixture must have Jared's real shape: three Codex accounts with full emails, the first in list
      // order NOT active and the active one after it, with Claude above and other providers below.
      let codex = dashboard.visibleAccounts.filter { $0.provider == "codex" }
      let activeIndex = codex.firstIndex(where: \.isActive) ?? -1
      let shapeOK = codex.count >= 3 && activeIndex >= 1 && codex.allSatisfy { $0.identity.contains("@") }
        && dashboard.visibleAccounts.contains { $0.provider == "claude" }
        && dashboard.providerGroups.contains { !["claude", "codex", "antigravity"].contains($0.id) }
      if !shapeOK {
        // Every later step indexes the Codex accounts by this shape: report and stop rather than trap.
        let report: [String: Any] = ["fixture": URL(fileURLWithPath: input).lastPathComponent, "passed": false,
          "failures": ["fixture shape: need 3+ Codex accounts with emails, first not active, plus Claude and another provider"],
          "realPointerMoved": false, "eventsPostedToSystem": false, "accountActionsInvoked": false]
        print(String(decoding: try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys, .prettyPrinted]), as: UTF8.self))
        exit(1)
      }

      AlignmentProbe.live = true
      let (host, state, window) = PreviewRenderer.host(dashboard, options: PreviewRenderer.Options([]), live: true)
      let model = host.rootView.model
      let prefs = host.rootView.prefs
      prefs.menuBarProvider = "codex"
      prefs.menuBarMode = .remaining
      prefs.menuBarClaudeAccountID = nil

      func pump(_ seconds: Double) { RunLoop.main.run(until: Date().addingTimeInterval(seconds)) }
      func fit() {
        let height = max(200, state.desiredHeight)
        if abs(window.frame.height - height) > 0.5 {
          window.setContentSize(NSSize(width: state.panelWidth, height: height))
          host.frame = NSRect(x: 0, y: 0, width: state.panelWidth, height: height)
        }
        host.layoutSubtreeIfNeeded()
        pump(0.15)
      }
      pump(0.6)
      fit()

      func tags() -> [Tag] {
        var found: [Tag] = []
        func visit(_ view: NSView) {
          if let help = view as? HoverHelpView {
            found.append(Tag(view: help, id: help.identifier?.rawValue ?? "", presenter: help.presenter))
          } else if let row = view as? DetailsRowButton {
            found.append(Tag(view: row, id: row.identifier?.rawValue ?? "", presenter: row.presenter))
          }
          view.subviews.forEach(visit)
        }
        visit(host)
        return found
      }
      func helpWindows() -> [NSWindow] {
        NSApplication.shared.windows.filter { $0.isVisible && $0.identifier?.rawValue == "account-center-hover-help" }
      }
      func windowText(_ help: NSWindow) -> String {
        func find(_ view: NSView) -> String? {
          if let field = view as? NSTextField, field.accessibilityIdentifier() == "account-center-tooltip-text" { return field.stringValue }
          for child in view.subviews { if let text = find(child) { return text } }
          return nil
        }
        return help.contentView.flatMap(find) ?? ""
      }
      func frameInWindow(_ view: NSView) -> NSRect { view.convert(view.bounds, to: nil) }
      /// AppKit fires a tracking area for a view in the window that is not hidden and whose tracking rect
      /// (its visible rect, for `.inVisibleRect`) contains the pointer. Opacity and SwiftUI hit testing do not count.
      func tracks(_ view: NSView, _ point: NSPoint) -> Bool {
        guard view.window === window, !view.isHiddenOrHasHiddenAncestor else { return false }
        let local = view.convert(point, from: nil)
        return view.trackingAreas.contains { area in
          area.options.contains(.mouseEnteredAndExited)
            && (area.options.contains(.inVisibleRect) ? view.visibleRect : area.rect).contains(local)
        }
      }
      func event(_ type: NSEvent.EventType, _ point: NSPoint) -> NSEvent? {
        NSEvent.enterExitEvent(with: type, location: point, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
          windowNumber: window.windowNumber, context: nil, eventNumber: 0, trackingNumber: 0, userData: nil)
      }
      /// AppKit keeps tracking areas current only for windows on a screen; this offscreen window gets the same
      /// `updateTrackingAreas` call AppKit makes when a view's geometry or visibility changes.
      func refreshTracking() { for tag in tags() { tag.view.updateTrackingAreas() } }
      func highlightedRows(_ all: [Tag]) -> [String] {
        all.compactMap { tag in (tag.view as? DetailsRowButton)?.hovering == true ? tag.id : nil }
      }
      /// `everyone`: every tag view hears the enter, as if each tracking rect spanned the whole panel (FW4-HOVER
      /// measured a content-sized host's visible rects that far; the real pointer could not settle which holds).
      func present(at point: NSPoint, everyone: Bool = false) -> Presentation {
        var result = Presentation()
        HoverProbe.pointer = window.convertPoint(toScreen: point)
        refreshTracking()
        let all = tags()
        let entered = everyone ? all.filter { $0.view.window === window && !$0.view.isHiddenOrHasHiddenAncestor }
          : all.filter { tracks($0.view, point) }
        result.entered = entered.map(\.id)
        for tag in entered { if let enter = event(.mouseEntered, point) { tag.view.mouseEntered(with: enter) } }
        for tag in entered { tag.presenter.flushPending() }
        result.shown = all.filter { $0.presenter.isShowing }.map { ($0.id, $0.presenter.text) }
        result.windowTexts = helpWindows().map(windowText)
        result.highlighted = highlightedRows(all)
        for tag in entered { if let exit = event(.mouseExited, point) { tag.view.mouseExited(with: exit) } }
        for tag in all where tag.presenter.isShowing { tag.presenter.hide() }
        result.leftover = helpWindows().count
        HoverProbe.pointer = nil
        return result
      }
      /// A pointer path through AppKit's own tracking rules: a tag view gets mouse-entered when one of its tracking
      /// rects starts to hold the pointer, mouse-moved while it holds it (for `.mouseMoved` areas) and mouse-exited
      /// when it stops. Each stop dwells long enough for a tag to show (the delayed show runs at once). Nothing is
      /// reset between stops, so a tag left up from an earlier stop counts against the later one.
      func walk(_ phase: String, _ path: [(label: String, point: NSPoint, expect: (id: String, text: String)?)],
        everyone: Bool = false) -> [[String: Any]] {
        var inside = Set<ObjectIdentifier>()
        var steps: [[String: Any]] = []
        func move(to point: NSPoint) -> [Tag] {
          HoverProbe.pointer = window.convertPoint(toScreen: point)
          refreshTracking()
          let all = tags()
          var entering: [Tag] = [], moving: [Tag] = []
          for tag in all {
            let key = ObjectIdentifier(tag.view)
            let local = tag.view.convert(point, from: nil)
            let areas = tag.view.trackingAreas
            let holds = tag.view.window === window && !tag.view.isHiddenOrHasHiddenAncestor && (everyone || areas.contains { area in
              (area.options.contains(.inVisibleRect) ? tag.view.visibleRect : area.rect).contains(local)
            })
            if holds && !inside.contains(key) {
              inside.insert(key)
              entering.append(tag)
            } else if holds {
              if areas.contains(where: { $0.options.contains(.mouseMoved) }) { moving.append(tag) }
            } else if inside.contains(key) {
              inside.remove(key)
              if let exit = event(.mouseExited, point) { tag.view.mouseExited(with: exit) }
            }
          }
          for tag in entering { if let enter = event(.mouseEntered, point) { tag.view.mouseEntered(with: enter) } }
          for tag in moving {
            if let moved = NSEvent.mouseEvent(with: .mouseMoved, location: point, modifierFlags: [],
              timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil,
              eventNumber: 0, clickCount: 0, pressure: 0) {
              tag.view.mouseMoved(with: moved)
            }
          }
          for tag in all { tag.presenter.flushPending() }
          return all
        }
        for stop in path {
          let all = move(to: stop.point)
          let shown = all.filter { $0.presenter.isShowing }.map { ($0.id, $0.presenter.text) }
          let texts = helpWindows().map(windowText)
          var ok = true
          if let expect = stop.expect {
            ok = shown.count == 1 && shown[0].0 == expect.id && shown[0].1 == expect.text && texts == [expect.text]
            if !ok {
              fail("\(phase): walking to \(stop.label) expected only \"\(expect.text)\", presented "
                + (shown.isEmpty ? "nothing" : shown.map { "\($0.0): \"\($0.1)\"" }.joined(separator: " + ")))
            }
          } else if !shown.isEmpty || !texts.isEmpty {
            ok = false
            fail("\(phase): walking to \(stop.label) expected no tag, presented \(shown.map(\.0))")
          }
          steps.append(["at": stop.label, "presented": shown.map { ["id": $0.0, "text": $0.1] }, "highlighted": highlightedRows(all),
            "passed": ok])
        }
        // Leave the panel: every tag goes away.
        _ = move(to: NSPoint(x: -40, y: -40))
        for tag in tags() where tag.presenter.isShowing { tag.presenter.hide() }
        HoverProbe.pointer = nil
        return steps
      }
      let outside = NSPoint(x: -40, y: -40)
      func rowPoint(_ id: String) -> NSPoint? {
        guard let tag = tags().first(where: { $0.id == id }) else { return nil }
        let frame = frameInWindow(tag.view)
        return tag.isRow ? NSPoint(x: frame.minX + frame.width * 0.03, y: frame.midY) : NSPoint(x: frame.midX, y: frame.midY)
      }
      func describe(_ p: Presentation) -> String {
        p.shown.isEmpty ? "nothing" : p.shown.map { "\($0.id): \"\($0.text)\"" }.joined(separator: " + ")
      }
      func bands() -> (headerBottom: CGFloat, footerTop: CGFloat) {
        let all = tags()
        let header = all.first { $0.id == "header-menu" }.map { frameInWindow($0.view).minY - 9 } ?? host.bounds.maxY - 48
        let footer = all.filter { $0.id.hasPrefix("footer-") }.map { frameInWindow($0.view).maxY + 8 }.max() ?? 60
        return (header, footer)
      }

      // MARK: Settings closed: every account and provider row presents its own tag; nested controls theirs.
      func checkList(_ phase: String) -> [String: Any] {
        let (headerBottom, footerTop) = bands()
        var probed = 0, passed = 0
        for tag in tags() where tag.view.visibleRect.width > 1 && tag.view.visibleRect.height > 1 {
          let frame = frameInWindow(tag.view)
          let point = tag.isRow ? NSPoint(x: frame.minX + frame.width * 0.03, y: frame.midY) : NSPoint(x: frame.midX, y: frame.midY)
          if !allowedOverSettings(tag.id) && (point.y >= headerBottom || point.y <= footerTop) { continue }
          probed += 1
          let p = present(at: point)
          var expected = tag.presenter.text
          if tag.id.hasPrefix("account-row-") {
            let id = String(tag.id.dropFirst("account-row-".count))
            if let account = dashboard.visibleAccounts.first(where: { $0.id == id }) {
              expected = "Usage details for \(account.identity): windows, balances and reset times"
            } else { fail("\(phase): row \(tag.id) has no fixture account") }
          }
          let ok = p.shown.count == 1 && p.shown[0].id == tag.id && p.shown[0].text == expected
            && p.windowTexts == [expected] && p.leftover == 0
          if ok { passed += 1 } else { fail("\(phase): at \(tag.id) expected only \"\(expected)\", presented \(describe(p))") }
        }
        if probed < codex.count { fail("\(phase): only \(probed) list tags were probed") }
        return ["probed": probed, "passed": passed]
      }

      // MARK: Settings open
      /// A control's centre from its laid-out SwiftUI frame (the same probe the alignment checks use), so it is
      /// found the same way whether or not the control carries a hover-tag view.
      func locate(_ id: String) -> (point: NSPoint, via: String)? {
        let key = "settings|" + id.dropFirst("settings-".count)
        if let frame = AlignmentProbe.frames[key], frame.width > 0 {
          let local = NSPoint(x: frame.midX, y: host.isFlipped ? frame.midY : host.bounds.height - frame.midY)
          return (host.convert(local, to: nil), "laid-out frame \(key)")
        }
        if let tag = tags().first(where: { $0.id == id }) {
          let frame = frameInWindow(tag.view)
          return (NSPoint(x: frame.midX, y: frame.midY), "hover-tag view")
        }
        return nil
      }
      var controlPoints: [String: NSPoint] = [:]
      func checkSettings(_ phase: String, gridStep: CGFloat) -> [String: Any] {
        state.setSettings(true)
        pump(0.9)
        fit()
        if !helpWindows().isEmpty { fail("\(phase): \(helpWindows().count) hover tag(s) still showing after Settings opened") }
        let reading = model.menuBarReading(prefs)
        var controls: [(id: String, text: String)] = [
          ("settings-close", "Close settings (Esc)"),
          ("settings-show", MenuBarReading.showHelp(for: reading)),
        ]
        if prefs.menuBarProvider == "claude" {
          controls.append(("settings-claude-account", MenuBarReading.claudeAccountHelp))
        }
        controls.append(("settings-value", MenuBarReading.valueHelp(for: reading)))
        var controlResults: [[String: Any]] = []
        // The probe frames and the AppKit views must agree on where a control is (the X carries both).
        if let probe = locate("settings-close"), let tag = tags().first(where: { $0.id == "settings-close" }) {
          let frame = frameInWindow(tag.view)
          if abs(probe.point.x - frame.midX) > 2 || abs(probe.point.y - frame.midY) > 2 {
            fail("\(phase): the X's laid-out frame and its tag view disagree (\(probe.point) vs \(frame))")
          }
        }
        for control in controls {
          guard let (point, via) = locate(control.id) else {
            fail("\(phase): could not find the \(control.id) control")
            controlResults.append(["control": control.id, "found": false])
            continue
          }
          controlPoints[control.id] = point
          let p = present(at: point)
          let ok = p.shown.count == 1 && p.shown[0].id == control.id && p.shown[0].text == control.text
            && p.windowTexts == [control.text] && p.leftover == 0
          if !ok { fail("\(phase): at \(control.id) expected only \"\(control.text)\", presented \(describe(p))") }
          controlResults.append(["control": control.id, "locatedBy": via, "x": Double(point.x), "y": Double(point.y),
            "expected": control.text, "presented": p.shown.map { ["id": $0.id, "text": $0.text] },
            "tagViewsWhoseTrackingHoldsThePoint": p.entered.count, "passed": ok])
        }
        // Walk the pointer in from outside, across the Settings controls and back, as a person would.
        var settingsPath: [(label: String, point: NSPoint, expect: (id: String, text: String)?)] = [("outside the panel", outside, nil)]
        for control in controls + controls.reversed().dropFirst() {
          if let point = controlPoints[control.id] { settingsPath.append((control.id, point, (control.id, control.text))) }
        }
        let walked = walk("\(phase) walk", settingsPath)
        // A grid over the whole panel: nothing from the hidden list may present anywhere, and never two tags at once.
        var points = 0, leaks = 0, doubles = 0
        var examples: [String] = []
        var y = gridStep / 2
        while y < host.bounds.height {
          var x = gridStep / 2
          while x < host.bounds.width {
            let point = NSPoint(x: x, y: y)
            let p = present(at: point)
            points += 1
            let hidden = p.shown.filter { !allowedOverSettings($0.id) }
            if !hidden.isEmpty {
              leaks += 1
              if examples.count < 12 { examples.append("(\(Int(x)),\(Int(y))) \(describe(p))") }
            }
            if p.shown.count > 1 || p.windowTexts.count > 1 { doubles += 1 }
            if p.leftover != 0 { fail("\(phase): a tag stayed up after the pointer left (\(Int(x)),\(Int(y)))") }
            x += gridStep
          }
          y += gridStep
        }
        if leaks > 0 { fail("\(phase): \(leaks) of \(points) grid points presented a hover tag from the hidden account list") }
        if doubles > 0 { fail("\(phase): \(doubles) grid points presented two tags at once") }
        // Native help tags (SwiftUI .help sets a view tool tip): none may sit under Settings outside the header.
        let (headerBottom, _) = bands()
        var native: [[String: Any]] = []
        func visit(_ view: NSView) {
          if let tip = view.toolTip, !tip.isEmpty, !view.isHiddenOrHasHiddenAncestor, view.window === window,
            frameInWindow(view).midY < headerBottom {
            native.append(["class": String(describing: type(of: view)), "text": tip])
          }
          view.subviews.forEach(visit)
        }
        visit(host)
        if !native.isEmpty { fail("\(phase): \(native.count) native tool tip(s) under Settings: \(native.prefix(4).map { $0["text"] ?? "" })") }
        // How the hidden list's tag views stand while Settings covers them.
        let hiddenTags = tags().filter { !allowedOverSettings($0.id) }
        func chain(_ view: NSView) -> (alpha: CGFloat, layer: Float) {
          var alpha: CGFloat = 1, layer: Float = 1
          var current: NSView? = view
          while let next = current, next !== host.superview {
            alpha *= next.alphaValue
            layer = min(layer, next.layer?.opacity ?? 1)
            current = next.superview
          }
          return (alpha, layer)
        }
        let trackingHidden = hiddenTags.filter { tag in
          let frame = frameInWindow(tag.view)
          return tracks(tag.view, NSPoint(x: frame.midX, y: frame.midY))
        }
        let sample = hiddenTags.first.map { tag -> [String: Any] in
          let c = chain(tag.view)
          return ["id": tag.id, "isHiddenOrHasHiddenAncestor": tag.view.isHiddenOrHasHiddenAncestor,
            "chainAlpha": Double(c.alpha), "chainLayerOpacity": Double(c.layer),
            "trackingAreas": tag.view.trackingAreas.count, "visibleRect": NSStringFromRect(tag.view.visibleRect),
            "frame": NSStringFromRect(tag.view.frame), "bounds": NSStringFromRect(tag.view.bounds),
            "frameInWindow": NSStringFromRect(frameInWindow(tag.view))]
        } ?? [:]
        return ["controls": controlResults, "walk": walked, "gridPoints": points, "gridStep": Double(gridStep), "gridLeaks": leaks,
          "gridLeakExamples": examples, "gridDoubles": doubles, "nativeToolTipsUnderSettings": native,
          "hiddenListTags": hiddenTags.count, "hiddenListTagsWhoseTrackingHoldsTheirCentre": trackingHidden.count,
          "hiddenListSample": sample]
      }
      func closeSettings() {
        state.setSettings(false)
        pump(0.9)
        fit()
      }

      var report: [String: Any] = ["fixture": URL(fileURLWithPath: input).lastPathComponent,
        "codexOrder": codex.map { ["identity": $0.identity, "active": $0.isActive] }]
      report["1-list-settings-closed"] = checkList("list, Settings closed")
      var listPath: [(label: String, point: NSPoint, expect: (id: String, text: String)?)] = [("outside the panel", outside, nil)]
      for id in ["account-row-" + codex[0].id, "activate-" + codex[0].id, "account-row-" + codex[activeIndex].id,
        "account-row-" + codex[codex.count - 1].id, "footer-settings"] {
        if let point = rowPoint(id), let tag = tags().first(where: { $0.id == id }) {
          listPath.append((id, point, (id, tag.presenter.text)))
        } else { fail("list walk: no \(id) tag") }
      }
      report["1b-list-walk"] = walk("list walk, Settings closed", listPath)
      report["2-settings-open-codex"] = checkSettings("Settings open (Codex)", gridStep: 20)

      // Mid-transition: right after Settings starts to open, the fading list may not present a tag; right
      // after it starts to close, the leaving Settings may not present one.
      closeSettings()
      var transition: [[String: Any]] = []
      state.setSettings(true)
      pump(0.03)
      for id in ["settings-show", "settings-value"] {
        guard let point = controlPoints[id] else { continue }
        let p = present(at: point)
        let hidden = p.shown.filter { !allowedOverSettings($0.id) }
        if !hidden.isEmpty { fail("opening transition: at \(id) the fading list presented \(describe(p))") }
        transition.append(["moment": "opening", "at": id, "presented": p.shown.map(\.id)])
      }
      pump(0.9)
      fit()
      state.setSettings(false)
      pump(0.03)
      for id in ["settings-show", "settings-value", "settings-close"] {
        guard let point = controlPoints[id] else { continue }
        let p = present(at: point)
        let leaving = p.shown.filter { $0.id.hasPrefix("settings-") }
        if !leaving.isEmpty { fail("closing transition: at \(id) the leaving Settings presented \(describe(p))") }
        transition.append(["moment": "closing", "at": id, "presented": p.shown.map(\.id)])
      }
      pump(0.9)
      fit()
      report["3-transitions"] = transition
      report["4-list-after-close"] = checkList("list, after Settings closed")

      // Open, close and open the panel again: the content is rebuilt, nothing stale shows, rows still tag.
      state.settingsOpen = false
      state.openGeneration += 1
      pump(0.9)
      fit()
      if !helpWindows().isEmpty { fail("reopen: \(helpWindows().count) stale hover tag(s) showing") }
      report["5-list-after-reopen"] = checkList("list, after reopen")
      report["6-settings-after-reopen"] = checkSettings("Settings open after reopen", gridStep: 40)
      closeSettings()

      // The panel closing: a tag up at that moment goes at once, none starts during the fade, and the next
      // open tags again. (PanelController.close and open shut and reopen this gate.)
      var closing: [String: Any] = [:]
      let rowID = "account-row-" + codex[activeIndex].id
      if let point = rowPoint(rowID) {
        let before = present(at: point)
        HoverProbe.pointer = window.convertPoint(toScreen: point)
        refreshTracking()
        if let tag = tags().first(where: { $0.id == rowID }) {
          if let enter = event(.mouseEntered, point) { tag.view.mouseEntered(with: enter) }
          tag.presenter.flushPending()
          let upBefore = tag.presenter.isShowing
          state.panelHover.set(true)
          let goneAtClose = !tag.presenter.isShowing && helpWindows().isEmpty
          HoverProbe.pointer = nil
          let during = present(at: point)
          state.panelHover.set(false)
          pump(0.1)
          let after = present(at: point)
          let ok = before.shown.map(\.id) == [rowID] && upBefore && goneAtClose && during.shown.isEmpty
            && after.shown.map(\.id) == [rowID]
          if !ok {
            fail("panel closing: tag up before \(upBefore), gone at close \(goneAtClose), during the fade \(describe(during)), after reopening \(describe(after))")
          }
          closing = ["row": rowID, "upBeforeClose": upBefore, "goneAtClose": goneAtClose,
            "presentedDuringFade": during.shown.map(\.id), "presentedAfterOpen": after.shown.map(\.id), "passed": ok]
          if let exit = event(.mouseExited, point) { tag.view.mouseExited(with: exit) }
        }
      } else { fail("panel closing: no \(rowID) row") }
      report["6b-panel-closing"] = closing

      // Claude: the picked account (not the first) names the Show and Value tags; the Claude picker has its own.
      let claude = dashboard.visibleAccounts.filter { $0.provider == "claude" }
      if claude.count >= 2 {
        prefs.menuBarProvider = "claude"
        prefs.menuBarClaudeAccountID = claude[1].id
        let claudeResult = checkSettings("Settings open (Claude, second account picked)", gridStep: 40)
        report["7-settings-open-claude"] = claudeResult
        let reading = model.menuBarReading(prefs)
        if reading?.accountName != claude[1].identity { fail("Claude: the reading must come from the picked account") }
        closeSettings()
      }
      // A tag already up follows the panel: the dashboard changes under it (the forced refresh at every open, the
      // 60 s timer, a Codex auto-switch) or a Settings choice changes, and the visible tag must say the same as its
      // presenter, naming the account Show now displays. Nothing is reset between the change and the read.
      let raw = try JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: input))) as? [String: Any] ?? [:]
      func variant(activeCodex id: String) throws -> AccountDashboard {
        var copy = raw
        copy["accounts"] = (raw["accounts"] as? [[String: Any]] ?? []).map { account -> [String: Any] in
          var account = account
          if account["provider"] as? String == "codex" { account["isActive"] = (account["id"] as? String) == id }
          return account
        }
        return try JSONDecoder().decode(AccountDashboard.self, from: JSONSerialization.data(withJSONObject: copy))
      }
      func expectedHelp(_ id: String) -> String {
        let reading = model.menuBarReading(prefs)
        return id == "settings-show" ? MenuBarReading.showHelp(for: reading) : MenuBarReading.valueHelp(for: reading)
      }
      /// Puts the stand-in pointer on a Settings control and brings up its tag, which stays up for the caller.
      func hold(_ id: String) -> (tag: Tag, point: NSPoint)? {
        guard let (point, _) = locate(id) else { fail("follow: could not find the \(id) control"); return nil }
        HoverProbe.pointer = window.convertPoint(toScreen: point)
        refreshTracking()
        let entered = tags().filter { tracks($0.view, point) }
        for tag in entered { if let enter = event(.mouseEntered, point) { tag.view.mouseEntered(with: enter) } }
        for tag in entered { tag.presenter.flushPending() }
        guard let tag = tags().first(where: { $0.id == id && $0.presenter.isShowing }) else {
          fail("follow: the \(id) tag did not present"); return nil
        }
        return (tag, point)
      }
      func release(_ held: (tag: Tag, point: NSPoint)) {
        if let exit = event(.mouseExited, held.point) { held.tag.view.mouseExited(with: exit) }
        for tag in tags() where tag.presenter.isShowing { tag.presenter.hide() }
        HoverProbe.pointer = nil
      }
      var follows: [[String: Any]] = []
      func follow(_ id: String, _ label: String, names identity: String?, change: () throws -> Void) rethrows {
        guard let held = hold(id) else { return }
        let before = held.tag.presenter.text
        try change()
        pump(0.3)
        let expected = expectedHelp(id)
        let texts = helpWindows().map(windowText)
        var ok = held.tag.presenter.isShowing && held.tag.presenter.text == expected && texts == [expected]
        if let identity { ok = ok && expected.contains(identity) && expected != before }
        if !ok {
          fail("follow: \(id) after \(label) should read \"\(expected)\", presenter \"\(held.tag.presenter.text)\", "
            + "showing \(held.tag.presenter.isShowing), visible tag \(texts)")
        }
        follows.append(["control": id, "change": label, "before": before, "expected": expected,
          "presenter": held.tag.presenter.text, "visible": texts, "passed": ok])
        release(held)
      }
      // MARK: 9. A panel shorter than its list (Jared's list is longer than his panel), scrolled
      // Rows scroll under the floating footer and up under the header. Over a footer button only that button's tag
      // presents and no row under it highlights; the gear over Settings' Value the same; no row scrolled out of
      // view presents; content scrolling under a still pointer, and a layer opening under one, re-check it.
      func scrolledPanel() -> [String: Any] {
        var out: [String: Any] = [:]
        prefs.menuBarProvider = "codex"
        prefs.menuBarMode = .remaining
        prefs.menuBarClaudeAccountID = nil
        if state.settingsOpen { closeSettings() }
        let panelHeight: CGFloat = 240
        state.maxHeight = panelHeight
        window.setContentSize(NSSize(width: state.panelWidth, height: panelHeight))
        host.frame = NSRect(x: 0, y: 0, width: state.panelWidth, height: panelHeight)
        host.layoutSubtreeIfNeeded()
        pump(0.4)
        out["panelHeight"] = Double(panelHeight)
        out["contentHeight"] = Double(state.desiredHeight)

        func tag(_ id: String) -> Tag? { tags().first { $0.id == id } }
        /// What a person sees of a tag view, in window coordinates: its frame cut to every scroll viewport around it.
        func seen(_ view: NSView) -> NSRect {
          var rect = frameInWindow(view)
          var current = view.superview
          while let next = current {
            if next is NSClipView { rect = rect.intersection(frameInWindow(next)) }
            current = next.superview
          }
          return rect
        }
        func scrollTo(_ scroll: NSScrollView, _ offset: CGFloat) {
          let clip = scroll.contentView
          var bounds = clip.bounds
          bounds.origin.y = offset
          clip.scroll(to: clip.constrainBoundsRect(bounds).origin)
          scroll.reflectScrolledClipView(clip)
          host.layoutSubtreeIfNeeded()
          pump(0.15)
        }
        func midY(_ id: String) -> CGFloat? { tag(id).map { frameInWindow($0.view).midY } }
        /// Scrolls so the tag view `id` has its centre at window height `y`; false when the content cannot scroll that far.
        func bring(_ id: String, to y: CGFloat) -> Bool {
          guard let start = tag(id), let scroll = start.view.enclosingScrollView else { return false }
          let origin = scroll.contentView.bounds.origin.y
          let delta = y - frameInWindow(start.view).midY
          scrollTo(scroll, origin + delta)
          if let now = midY(id), abs(now - y) > 1 { scrollTo(scroll, origin - delta) }
          return midY(id).map { abs($0 - y) <= 1 } ?? false
        }
        func columns(_ a: NSRect, _ b: NSRect) -> (lo: CGFloat, hi: CGFloat)? {
          let lo = max(a.minX, b.minX) + 2, hi = min(a.maxX, b.maxX) - 2
          return hi > lo ? (lo, hi) : nil
        }
        func cleanUp() {
          for tag in tags() {
            if let exit = event(.mouseExited, outside) { tag.view.mouseExited(with: exit) }
            if tag.presenter.isShowing { tag.presenter.hide() }
          }
          HoverProbe.pointer = nil
        }
        let footerIDs = ["footer-dashboard", "footer-refresh", "footer-settings"]
        let footerControls = footerIDs + ["codex-auto-info"]
        guard let listScroll = tag("account-row-" + codex[0].id)?.view.enclosingScrollView else {
          fail("scrolled panel: the account list has no scroll view")
          return out
        }
        out["listViewport"] = NSStringFromRect(frameInWindow(listScroll.contentView))

        // A footer button over a nested control that scrolled under it: only the footer's tag, no row highlight.
        func overFooter(_ phase: String, targets: [String], everyone: Bool) -> [[String: Any]] {
          var results: [[String: Any]] = []
          for targetID in targets {
            for footerID in footerIDs {
              guard let footer = tag(footerID), let target = tag(targetID) else { fail("\(phase): no \(targetID) or \(footerID) tag"); continue }
              let f = frameInWindow(footer.view)
              guard let span = columns(f, frameInWindow(target.view)) else { continue }
              guard bring(targetID, to: f.midY), let moved = tag(targetID) else {
                fail("\(phase): could not scroll \(targetID) under \(footerID)")
                continue
              }
              let point = NSPoint(x: (span.lo + span.hi) / 2, y: f.midY)
              guard seen(moved.view).contains(point), seen(footer.view).contains(point) else {
                fail("\(phase): \(targetID) is not under \(footerID) at \(point)")
                continue
              }
              let p = present(at: point, everyone: everyone)
              let text = footer.presenter.text
              let ok = p.shown.count == 1 && p.shown[0].id == footerID && p.shown[0].text == text && p.windowTexts == [text]
                && p.highlighted.isEmpty && p.leftover == 0
              if !ok {
                fail("\(phase): \(footerID) over \(targetID) expected only \"\(text)\" and no row highlight, presented "
                  + "\(describe(p)), highlighted \(p.highlighted)")
              }
              results.append(["footer": footerID, "under": targetID, "x": Double(point.x), "y": Double(point.y),
                "presented": p.shown.map(\.id), "highlighted": p.highlighted, "passed": ok])
            }
          }
          if results.isEmpty { fail("\(phase): no footer button reached \(targets)") }
          return results
        }
        // A nested control half under a footer button, walked: its own tag above the footer, only the footer's below.
        func halfUnder(_ phase: String, targetID: String, footerID: String, rowID: String?, everyone: Bool) -> [[String: Any]] {
          guard let footer = tag(footerID), let target = tag(targetID) else { fail("\(phase): no \(targetID) or \(footerID) tag"); return [] }
          let f = frameInWindow(footer.view)
          guard let span = columns(f, frameInWindow(target.view)), bring(targetID, to: f.maxY), let moved = tag(targetID) else {
            fail("\(phase): could not put \(targetID) half under \(footerID)")
            return []
          }
          let x = (span.lo + span.hi) / 2
          let reach = min(6, frameInWindow(moved.view).height / 2 - 2)
          let above = NSPoint(x: x, y: f.maxY + reach), under = NSPoint(x: x, y: f.maxY - reach)
          let mine = (id: targetID, text: moved.presenter.text), front = (id: footerID, text: footer.presenter.text)
          var path: [(label: String, point: NSPoint, expect: (id: String, text: String)?)] = [
            ("outside the panel", outside, nil), ("\(targetID) above the footer", above, mine),
            ("\(footerID) over \(targetID)", under, front), ("\(targetID) above the footer again", above, mine)]
          var lit: [[String]] = [[], rowID.map { [$0] } ?? [], [], rowID.map { [$0] } ?? []]
          if let rowID, let row = tag(rowID) {
            let r = frameInWindow(row.view)
            path.append(("\(rowID) above the footer", NSPoint(x: r.minX + r.width * 0.03, y: min(r.maxY - 3, f.maxY + 12)),
              (rowID, row.presenter.text)))
            lit.append([rowID])
          }
          path.append(("\(footerID) over \(targetID) again", under, front))
          lit.append([])
          let steps = walk(phase, path, everyone: everyone)
          for (index, step) in steps.enumerated() where index < lit.count {
            let highlighted = step["highlighted"] as? [String] ?? []
            if highlighted != lit[index] { fail("\(phase): at \(step["at"] ?? "") the row highlight was \(highlighted), expected \(lit[index])") }
          }
          return steps
        }
        // Content scrolled up under the header: nothing scrolled out of view presents or highlights there.
        func headerBand(_ phase: String, scroll: NSScrollView, allowed: Set<String>, everyone: Bool) -> [String: Any] {
          let viewport = frameInWindow(scroll.contentView)
          let height = scroll.documentView?.frame.height ?? 0
          scrollTo(scroll, (height - viewport.height) * 0.6)
          let top = host.bounds.height
          let underHeader = tags().filter { $0.view.enclosingScrollView === scroll }
            .filter { let f = frameInWindow($0.view); return f.maxY > viewport.maxY + 2 && f.minY < top - 2 }
          if underHeader.isEmpty { fail("\(phase): no tag view scrolled up under the header") }
          var points = 0, bad = 0
          var examples: [String] = []
          var y = viewport.maxY + 2
          while y < top - 1 {
            var x: CGFloat = 4
            while x < host.bounds.width {
              let p = present(at: NSPoint(x: x, y: y), everyone: everyone)
              points += 1
              if p.shown.contains(where: { !allowed.contains($0.id) }) || p.shown.count > 1 || !p.highlighted.isEmpty {
                bad += 1
                if examples.count < 8 { examples.append("(\(Int(x)),\(Int(y))) \(describe(p)) highlighted \(p.highlighted)") }
              }
              x += 16
            }
            y += 6
          }
          if bad > 0 { fail("\(phase): \(bad) of \(points) header points presented a tag or highlight from content scrolled out of view: \(examples.prefix(3))") }
          return ["tagViewsUnderHeader": underHeader.count, "points": points, "bad": bad, "examples": examples]
        }
        // A grid over the scrolled panel at several scroll positions: one tag at most, always the frontmost under the pointer.
        func grid(_ phase: String, scroll: NSScrollView, allowed: (String) -> Bool, everyone: Bool) -> [String: Any] {
          let viewport = frameInWindow(scroll.contentView)
          let height = scroll.documentView?.frame.height ?? 0
          var points = 0, doubles = 0, unseen = 0, footerMiss = 0, headerLeak = 0, badHighlight = 0, leaks = 0
          var examples: [String] = []
          func note(_ text: String) { if examples.count < 12 { examples.append(text) } }
          for fraction in [0.25, 0.5, 0.75] {
            scrollTo(scroll, (height - viewport.height) * fraction)
            let all = tags()
            var byID: [String: NSView] = [:]
            for tag in all { byID[tag.id] = tag.view }
            let footers = all.filter { footerControls.contains($0.id) }.map { ($0.id, frameInWindow($0.view)) }
            var y: CGFloat = 12
            while y < host.bounds.height {
              var x: CGFloat = 12
              while x < host.bounds.width {
                let point = NSPoint(x: x, y: y)
                let p = present(at: point, everyone: everyone)
                points += 1
                let at = "(\(Int(x)),\(Int(y))) at \(Int(fraction * 100))%"
                if p.shown.count > 1 || p.windowTexts.count > 1 { doubles += 1; note("two tags \(at): \(describe(p))") }
                for shown in p.shown {
                  if let view = byID[shown.id], !seen(view).contains(point) { unseen += 1; note("unseen \(shown.id) \(at)") }
                  if !allowed(shown.id) { leaks += 1; note("covered layer \(shown.id) \(at)") }
                }
                if let footer = footers.first(where: { $0.1.contains(point) }) {
                  if p.shown.map(\.id) != [footer.0] || !p.highlighted.isEmpty {
                    footerMiss += 1
                    note("over \(footer.0) \(at): \(describe(p)) highlighted \(p.highlighted)")
                  }
                }
                if y > viewport.maxY, p.shown.contains(where: { $0.id != "header-menu" }) || !p.highlighted.isEmpty {
                  headerLeak += 1
                  note("header \(at): \(describe(p)) highlighted \(p.highlighted)")
                }
                if p.highlighted.count > 1 || p.highlighted.contains(where: { byID[$0].map { !seen($0).contains(point) } ?? true }) {
                  badHighlight += 1
                  note("highlight \(at): \(p.highlighted)")
                }
                x += 24
              }
              y += 24
            }
          }
          let failed = doubles + unseen + footerMiss + headerLeak + badHighlight + leaks
          if failed > 0 {
            fail("\(phase): \(failed) problems over \(points) points (two tags \(doubles), tag from an unseen view \(unseen), "
              + "footer not frontmost \(footerMiss), header leak \(headerLeak), wrong highlight \(badHighlight), covered layer \(leaks)): "
              + "\(examples.prefix(4))")
          }
          return ["points": points, "twoTags": doubles, "unseen": unseen, "footerNotFrontmost": footerMiss, "headerLeak": headerLeak,
            "wrongHighlight": badHighlight, "coveredLayer": leaks, "examples": examples]
        }

        let activates = codex.filter { !$0.isActive }.map { "activate-" + $0.id }.filter { tag($0) != nil }
        let opens = Array(tags().filter { $0.id.hasPrefix("claude-mac-") || $0.id.hasPrefix("claude-windows-") }.suffix(2)).map(\.id)
        if activates.isEmpty || opens.isEmpty { fail("scrolled panel: need Activate and Claude Open buttons, found \(activates) \(opens)") }
        let walkTarget = activates.first ?? ""
        let walkRow = "account-row-" + String(walkTarget.dropFirst("activate-".count))
        for (name, everyone) in [("tracking", false), ("every-tag", true)] {
          var section: [String: Any] = [:]
          let phase = "scrolled list (\(name))"
          section["footerOverActivate"] = overFooter(phase, targets: activates, everyone: everyone)
          section["footerOverClaudeOpen"] = overFooter(phase, targets: opens, everyone: everyone)
          section["walkActivateHalfUnderRefresh"] = halfUnder(phase, targetID: walkTarget, footerID: "footer-refresh", rowID: walkRow, everyone: everyone)
          section["headerBand"] = headerBand(phase, scroll: listScroll, allowed: ["header-menu"], everyone: everyone)
          // A list control scrolled up behind the header menu: the menu's own tag (the clipped geometry, not raw bounds).
          if let menu = tag("header-menu"), let behind = tags().first(where: { tag in
            tag.view.enclosingScrollView === listScroll && !tag.isRow && columns(frameInWindow(tag.view), frameInWindow(menu.view)) != nil
          }) {
            let m = frameInWindow(menu.view)
            if bring(behind.id, to: m.midY), let span = columns(frameInWindow(behind.view), m) {
              let p = present(at: NSPoint(x: (span.lo + span.hi) / 2, y: m.midY), everyone: everyone)
              let ok = p.shown.map(\.id) == ["header-menu"] && p.highlighted.isEmpty
              if !ok { fail("\(phase): header menu over \(behind.id) scrolled behind it presented \(describe(p))") }
              section["headerMenuOverScrolledControl"] = ["behind": behind.id, "presented": p.shown.map(\.id), "passed": ok]
            } else { fail("\(phase): could not scroll \(behind.id) behind the header menu") }
          }
          section["grid"] = grid(phase, scroll: listScroll, allowed: { _ in true }, everyone: everyone)
          out["list-\(name)"] = section
        }

        // Content scrolling under a still pointer (AppKit sends no mouse event): the row that left no longer tags or
        // highlights, and the row now under the pointer does.
        do {
          let viewport = frameInWindow(listScroll.contentView)
          let footerTop = footerIDs.compactMap { tag($0).map { frameInWindow($0.view).maxY } }.max() ?? 60
          scrollTo(listScroll, 0)
          let point = NSPoint(x: 12 + (state.panelWidth - 24) * 0.03, y: (viewport.maxY + footerTop) / 2)
          func rowAt() -> Tag? { tags().first { $0.isRow && seen($0.view).contains(point) } }
          var result: [String: Any] = [:]
          var start = rowAt()
          var tries = 0
          while start == nil && tries < 6 {
            scrollTo(listScroll, listScroll.contentView.bounds.origin.y + 20)
            start = rowAt()
            tries += 1
          }
          if let start {
            HoverProbe.pointer = window.convertPoint(toScreen: point)
            refreshTracking()
            let entered = tags().filter { tracks($0.view, point) }
            for tag in entered { if let enter = event(.mouseEntered, point) { tag.view.mouseEntered(with: enter) } }
            for tag in entered { tag.presenter.flushPending() }
            let upBefore = start.presenter.isShowing && highlightedRows(tags()) == [start.id]
            if !upBefore { fail("scroll under a still pointer: \(start.id) did not tag and highlight first") }
            scrollTo(listScroll, listScroll.contentView.bounds.origin.y + 90)
            for tag in tags() { tag.presenter.flushPending() }
            let all = tags()
            let shown = all.filter { $0.presenter.isShowing }.map(\.id)
            let lit = highlightedRows(all)
            let now = rowAt()
            let expected = now.map { [$0.id] } ?? []
            let ok = upBefore && !shown.contains(start.id) && !lit.contains(start.id) && shown == expected && lit == expected
            if !ok {
              fail("scroll under a still pointer: \(start.id) scrolled away; presented \(shown), highlighted \(lit), expected \(expected)")
            }
            result = ["rowBefore": start.id, "rowAfter": now?.id ?? "none", "presentedAfter": shown, "highlightedAfter": lit, "passed": ok]
            cleanUp()
          } else { fail("scroll under a still pointer: no row under \(point)") }
          out["scrollUnderStillPointer"] = result
        }

        // Settings open in the short panel, scrolled: the gear over Value presents only the gear's tag.
        state.setSettings(true)
        pump(0.9)
        if let settingsScroll = tag("settings-value")?.view.enclosingScrollView {
          for (name, everyone) in [("tracking", false), ("every-tag", true)] {
            var section: [String: Any] = [:]
            let phase = "scrolled Settings (\(name))"
            section["footerOverPickers"] = overFooter(phase, targets: ["settings-value", "settings-show"], everyone: everyone)
            section["walkValueHalfUnderGear"] = halfUnder(phase, targetID: "settings-value", footerID: "footer-settings", rowID: nil, everyone: everyone)
            section["headerBand"] = headerBand(phase, scroll: settingsScroll, allowed: ["header-menu"], everyone: everyone)
            section["grid"] = grid(phase, scroll: settingsScroll, allowed: allowedOverSettings, everyone: everyone)
            out["settings-\(name)"] = section
          }
        } else { fail("scrolled Settings: no scroll view around the Value picker") }

        // A layer opening under a still pointer (Settings closing, the panel opening): the row under the pointer tags
        // and highlights after the delay, with no mouse move.
        do {
          let viewport = frameInWindow(listScroll.contentView)
          let footerTop = footerIDs.compactMap { tag($0).map { frameInWindow($0.view).maxY } }.max() ?? 60
          let point = NSPoint(x: 12 + (state.panelWidth - 24) * 0.03, y: (viewport.maxY + footerTop) / 2)
          var row = tags().first { $0.isRow && seen($0.view).contains(point) }
          var tries = 0
          while row == nil && tries < 6 {
            scrollTo(listScroll, listScroll.contentView.bounds.origin.y + 20)
            row = tags().first { $0.isRow && seen($0.view).contains(point) }
            tries += 1
          }
          var result: [String: Any] = [:]
          if row != nil {
            let covered = present(at: point)
            if covered.shown.contains(where: { !allowedOverSettings($0.id) }) { fail("reopen under a still pointer: Settings open presented \(describe(covered))") }
            func settle(_ label: String, rebuilds: Bool, _ change: () -> Void) {
              let viewsBefore = Set(tags().map { ObjectIdentifier($0.view) })
              HoverProbe.pointer = window.convertPoint(toScreen: point)
              change()
              pump(0.9)
              for tag in tags() { tag.presenter.flushPending() }
              let all = tags()
              // The row under the resting pointer now (a rebuilt panel starts at the top of the list).
              let under = all.first { $0.isRow && seen($0.view).contains(point) }?.id ?? "none"
              let shown = all.filter { $0.presenter.isShowing }.map(\.id)
              let lit = highlightedRows(all)
              // A rebuilt panel has new tag views only, so the old views' gate cannot be what tags it.
              let rebuilt = !all.contains { viewsBefore.contains(ObjectIdentifier($0.view)) }
              let ok = under != "none" && shown == [under] && lit == [under] && rebuilt == rebuilds
              if !ok { fail("\(label): the pointer rests on \(under); presented \(shown), highlighted \(lit), content rebuilt \(rebuilt)") }
              result[label] = ["row": under, "presented": shown, "highlighted": lit, "contentRebuilt": rebuilt, "passed": ok]
              cleanUp()
            }
            settle("Settings closed under a still pointer", rebuilds: false) { state.setSettings(false) }
            // As PanelController.close and open do: the panel gate shuts, then opens, and the content is rebuilt.
            settle("panel opened under a still pointer", rebuilds: true) {
              state.panelHover.set(true)
              pump(0.05)
              state.settingsOpen = false
              state.panelHover.set(false)
              state.openGeneration += 1
            }
          } else { fail("reopen under a still pointer: no row under \(point)") }
          out["reopenUnderStillPointer"] = result
        }
        if state.settingsOpen { closeSettings() }
        return out
      }

      let baseActive = codex[activeIndex]
      let other = codex[codex.count - 1].id == baseActive.id ? codex[0] : codex[codex.count - 1]
      prefs.menuBarProvider = "codex"
      prefs.menuBarMode = .remaining
      prefs.menuBarClaudeAccountID = nil
      state.setSettings(true)
      pump(0.9)
      fit()
      for id in ["settings-value", "settings-show"] {
        model.previewReplace(dashboard)
        pump(0.3)
        try follow(id, "the active Codex account changed to \(other.identity)", names: other.identity) {
          model.previewReplace(try variant(activeCodex: other.id))
        }
        try follow(id, "the active Codex account changed back to \(baseActive.identity)", names: baseActive.identity) {
          model.previewReplace(try variant(activeCodex: baseActive.id))
        }
        follow(id, "Value switched to Used", names: nil) { prefs.menuBarMode = .used }
        follow(id, "Value switched to Remaining", names: nil) { prefs.menuBarMode = .remaining }
      }
      model.previewReplace(dashboard)
      if claude.count >= 3 {
        prefs.menuBarProvider = "claude"
        prefs.menuBarClaudeAccountID = claude[1].id
        pump(0.6)
        fit()
        for id in ["settings-value", "settings-show"] {
          prefs.menuBarClaudeAccountID = claude[1].id
          pump(0.3)
          follow(id, "the picked Claude account changed to \(claude[2].identity)", names: claude[2].identity) {
            prefs.menuBarClaudeAccountID = claude[2].id
          }
        }
      } else { fail("follow: the fixture needs 3+ Claude accounts") }
      closeSettings()
      prefs.menuBarProvider = "codex"
      prefs.menuBarClaudeAccountID = nil
      report["8-tag-follows-changes"] = follows
      report["9-scrolled-panel"] = scrolledPanel()

      if !helpWindows().isEmpty { fail("end: \(helpWindows().count) hover tag(s) still showing") }

      let passed = failures.isEmpty
      report["passed"] = passed
      report["failures"] = failures
      report["realPointerMoved"] = false
      report["eventsPostedToSystem"] = false
      report["accountActionsInvoked"] = false
      print(String(decoding: try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys, .prettyPrinted]), as: UTF8.self))
      exit(passed ? 0 : 1)
    } catch {
      fputs("Hover occlusion check failed to start.\n", stderr)
      exit(1)
    }
  }
}
