import SwiftUI

struct AboutView: View {
  @Environment(\.dismiss) private var dismiss

  var body: some View {
    VStack(alignment: .leading, spacing: 14) {
      HStack(spacing: 12) {
        CCSStackMark().frame(width: 40, height: 40)
        VStack(alignment: .leading, spacing: 3) {
          Text("AI Account Center").font(.system(size: 21, weight: .semibold))
          Text("Version \(Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "2.0.0")")
            .font(.caption).foregroundStyle(AccountsPalette.muted)
        }
      }
      Text("Your AI accounts, usage and account controls in one place.")
        .font(.system(size: 12))
      Text("Based on CCS and CCS Bar (kaitranntt/ccs). Copyright (c) 2025 CCS Contributors. Distributed under the MIT license; original attribution and license are included in this app.")
        .font(.system(size: 11)).foregroundStyle(AccountsPalette.muted)
      Link("AI Account Center source", destination: URL(string: "https://github.com/sittingmongoose/ai-account-center")!)
        .font(.system(size: 11))
      Link("Upstream CCS project", destination: URL(string: "https://github.com/kaitranntt/ccs")!)
        .font(.system(size: 11))
      HStack { Spacer(); Button("Close") { dismiss() }.keyboardShortcut(.defaultAction) }
    }.padding(22).frame(width: 430)
      .foregroundStyle(AccountsPalette.text).background(AccountsPalette.plate)
      .environment(\.colorScheme, .dark).preferredColorScheme(.dark)
  }
}
