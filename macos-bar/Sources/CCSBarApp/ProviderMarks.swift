import SwiftUI
import AppKit

struct ProviderMark: View {
  let provider: String
  var body: some View {
    Group {
      if let url = Bundle.main.url(forResource: "provider-\(provider)", withExtension: "png"),
        let image = NSImage(contentsOf: url) {
        Image(nsImage: image).resizable().scaledToFit()
      } else if provider == "muse" {
        Text("M").font(.system(size: 30, weight: .black, design: .rounded))
          .foregroundStyle(LinearGradient(colors: [.purple, Color(red: 0.36, green: 0.24, blue: 0.93)], startPoint: .top, endPoint: .bottom))
      } else if provider == "opencode-go" {
        Image(systemName: "infinity").resizable().scaledToFit().foregroundStyle(Color(red: 0.39, green: 0.38, blue: 1))
      } else {
        Image(systemName: "cpu").resizable().scaledToFit().foregroundStyle(AccountsPalette.accent)
      }
    }
  }
}

struct WindowsMark: View {
  var body: some View {
    VStack(spacing: 1.5) {
      HStack(spacing: 1.5) { Rectangle(); Rectangle() }
      HStack(spacing: 1.5) { Rectangle(); Rectangle() }
    }
  }
}

struct CCSStackMark: View {
  var body: some View {
    GeometryReader { geometry in
      ForEach(0..<3) { index in
        Path { path in
          let width = geometry.size.width
          let height = geometry.size.height
          let y = CGFloat(index) * 0.22 + 0.08
          path.move(to: CGPoint(x: width * 0.5, y: height * y))
          path.addLine(to: CGPoint(x: width * 0.92, y: height * (y + 0.20)))
          path.addQuadCurve(to: CGPoint(x: width * 0.92, y: height * (y + 0.27)), control: CGPoint(x: width, y: height * (y + 0.235)))
          path.addLine(to: CGPoint(x: width * 0.5, y: height * (y + 0.47)))
          path.addLine(to: CGPoint(x: width * 0.08, y: height * (y + 0.27)))
          path.addQuadCurve(to: CGPoint(x: width * 0.08, y: height * (y + 0.20)), control: CGPoint(x: 0, y: height * (y + 0.235)))
          path.closeSubpath()
        }.fill(index == 0 ? Color(red: 0.32, green: 0.71, blue: 1) : AccountsPalette.accent)
      }
    }
  }
}
