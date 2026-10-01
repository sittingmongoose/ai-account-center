import SwiftUI
import AppKit

struct ProviderMark: View {
  let provider: String
  var body: some View {
    Group {
      if let url = Bundle.main.url(forResource: "provider-\(provider)", withExtension: "png"),
        let image = NSImage(contentsOf: url) {
        Image(nsImage: image).resizable().scaledToFit()
      } else {
        Image(systemName: "cpu").resizable().scaledToFit().foregroundStyle(AccountsPalette.accent)
      }
    }.help(provider == "muse" ? "Muse Code (Meta publisher mark)" : "\(provider) provider mark")
  }
}

struct WindowsMark: View {
  static var nativeImage: NSImage {
    let image = NSImage(size: NSSize(width: 15, height: 15), flipped: false) { _ in
      NSColor.white.setFill()
      for x in [CGFloat(0), CGFloat(8.25)] {
        for y in [CGFloat(0), CGFloat(8.25)] {
          NSRect(x: x, y: y, width: 6.75, height: 6.75).fill()
        }
      }
      return true
    }
    image.isTemplate = true
    return image
  }
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
