import SwiftUI
import UIKit

/// The Mac app's palette (src/theme.rs), light and dark.
enum Theme {
    static let background = dynamic(0xfcfcfc, 0x24272d)
    static let chrome = dynamic(0xf3f3f4, 0x2b2e35)
    static let panel = dynamic(0xf6f6f7, 0x2f333a)
    static let border = dynamic(0xe0e0e3, 0x3a3e47)
    static let text = dynamic(0x2c2d31, 0xdcdfe4)
    static let muted = dynamic(0x76787f, 0x959ba6)
    static let quiet = dynamic(0xa2a4aa, 0x6c727d)
    static let blue = dynamic(0x2f6fde, 0x74ade8)
    static let green = dynamic(0x3b7d3e, 0x98c379)
    static let amber = dynamic(0xa86a00, 0xe5c07b)
    static let orange = dynamic(0xd9620f, 0xf0883e)
    static let red = dynamic(0xc2414b, 0xe06c75)
    static let blueTint = dynamic(0xe7effc, 0x2a3647)
    static let greenTint = dynamic(0xe8f3e9, 0x2c3a2e)
    static let amberTint = dynamic(0xfbf3e3, 0x3b3427)
    static let redTint = dynamic(0xfbe9ea, 0x3e2b2f)

    private static let lanesLight: [UInt32] = [0x2f6fde, 0x8a3fc7, 0x3b8a3e, 0xb07800, 0xc2414b, 0x1f8a99, 0xc0601a, 0x5b62c9]
    private static let lanesDark: [UInt32] = [0x74ade8, 0xc678dd, 0x98c379, 0xe5c07b, 0xe06c75, 0x56b6c2, 0xd19a66, 0xa0a8f0]

    /// A project's colour, cycled as on the Mac's rail.
    static func lane(_ i: Int) -> Color {
        dynamic(lanesLight[i % 8], lanesDark[i % 8])
    }

    static let mono = Font.system(size: 12, design: .monospaced)

    private static func dynamic(_ light: UInt32, _ dark: UInt32) -> Color {
        Color(UIColor { $0.userInterfaceStyle == .dark ? uiColor(dark) : uiColor(light) })
    }

    private static func uiColor(_ hex: UInt32) -> UIColor {
        UIColor(red: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255, blue: CGFloat(hex & 0xff) / 255, alpha: 1)
    }
}

/// A short tinted label: "Direct", "Needs you".
struct Pill: View {
    let text: String
    let fg: Color
    let bg: Color

    var body: some View {
        Text(text)
            .font(.caption2.weight(.medium))
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .foregroundStyle(fg)
            .background(bg, in: Capsule())
    }
}

/// A folder: a tab over a body, the body squared off where the tab joins it so
/// the two read as one shape (as `face` draws it on the Mac's rail).
struct FolderShape: Shape {
    func path(in rect: CGRect) -> Path {
        let tab = rect.height * 0.14
        let tabWidth = rect.width * 0.42
        var p = Path()
        p.addPath(UnevenRoundedRectangle(topLeadingRadius: 3, topTrailingRadius: 3)
            .path(in: CGRect(x: rect.minX, y: rect.minY, width: tabWidth, height: tab + 1)))
        p.addPath(UnevenRoundedRectangle(topLeadingRadius: 0, bottomLeadingRadius: 6, bottomTrailingRadius: 6, topTrailingRadius: 6)
            .path(in: CGRect(x: rect.minX, y: rect.minY + tab, width: rect.width, height: rect.height - tab)))
        return p
    }
}

/// A project's folder tile, as on the Mac's rail.
struct ProjectTile: View {
    let project: ProjectInfo

    var body: some View {
        let color = Theme.lane(project.lane)
        ZStack {
            // One path for tab and body, filled once: no seam, no darker overlap.
            FolderShape().fill(color.opacity(0.18))
            Text(project.initials)
                .font(.system(size: 11, weight: .bold))
                .foregroundStyle(color)
                .offset(y: 2)
        }
        .frame(width: 32, height: 28)
    }
}

/// The folderless chats: a speech bubble where projects have folders.
struct ChatsTile: View {
    var body: some View {
        Image(systemName: "bubble.left.and.bubble.right.fill")
            .font(.system(size: 13))
            .foregroundStyle(Theme.muted)
            .frame(width: 32, height: 28)
            .background(Theme.muted.opacity(0.14), in: RoundedRectangle(cornerRadius: 7))
    }
}

/// How a thread's state reads as a mark.
struct StateMark: View {
    let state: String
    let unread: Bool

    var body: some View {
        switch state {
        case "running", "waiting":
            ProgressView().controlSize(.mini).tint(Theme.orange)
        case "failed":
            Circle().fill(Theme.red).frame(width: 7, height: 7)
        default:
            Circle().fill(unread ? Theme.blue : Theme.quiet).frame(width: 7, height: 7)
        }
    }
}

func relative(_ ms: Int64?) -> String {
    guard let ms else { return "" }
    let date = Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
    return date.formatted(.relative(presentation: .named))
}

extension View {
    /// Liquid Glass where the system has it; frosted material before that.
    @ViewBuilder
    func glass<S: Shape>(_ shape: S) -> some View {
        if #available(iOS 26.0, *) {
            self.glassEffect(.regular.interactive(), in: shape)
        } else {
            self.background(.ultraThinMaterial, in: shape)
                .overlay(shape.stroke(Theme.border.opacity(0.6)))
        }
    }
}
