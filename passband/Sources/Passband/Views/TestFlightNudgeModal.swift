// THE IPHONE AD. Two days into the Mac app, once, for somebody whose mailbox
// has never seen the phone app: a card in the bottom-right corner that sells
// the phone app the way the landing page would, in the app's own materials.
//
// THE HERO IS THE PITCH. The live P7 phosphor (the quiet board's scene, so the
// brand's own light) runs behind an iPhone rising out of the card's lower
// edge. On its screen is a tiny sitrep, and every few seconds a push lands on
// it: the one thing the phone app does that the Mac cannot. Reduce Motion
// holds the notification on screen and lets the phosphor settle.
//
// A QR CODE, NOT JUST A LINK. The link is for a phone, and opening it on the
// Mac lands on a page that says to open it on a phone. Pointing the camera at
// the screen is the shortest way there; "Copy link" is the fallback.
//
// NOT MODAL. A corner card, closed by its ✕, that leaves the inbox usable
// underneath. Bottom-RIGHT on the main window: the toast stack owns
// bottom-left and the update card owns bottom-centre. NO `.defaultAction`
// anywhere on it: an unbidden card does not get to own Return.

import AppKit
import CoreImage.CIFilterBuiltins
import SwiftUI

struct TestFlightNudgeModal: View {
    private var nudge: TestFlightNudge { TestFlightNudge.shared }

    var body: some View {
        if nudge.showingNudge {
            TestFlightAdCard { copied in
                nudge.showingNudge = false
                Analytics.capture("ios_nudge_closed", ["copied": copied])
            }
            .padding(20)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing)
        }
    }
}

private struct TestFlightAdCard: View {
    let close: (_ copied: Bool) -> Void

    @Environment(\.controlActiveState) private var activeState
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var arrived = false
    @State private var leaving = false
    @State private var copied = false

    private static let width: CGFloat = 340
    private static let heroHeight: CGFloat = 196

    var body: some View {
        VStack(spacing: 0) {
            hero
            words
        }
        .frame(width: Self.width)
        .passbandGlass(.pane, cornerRadius: 20, tint: Palette.glassTintStrong)
        .shadow(color: .black.opacity(0.35), radius: 28, y: 14)
        .offset(x: arrived && !leaving ? 0 : 40, y: arrived && !leaving ? 0 : 24)
        .opacity(arrived && !leaving ? 1 : 0)
        .scaleEffect(arrived && !leaving ? 1 : 0.96, anchor: .bottomTrailing)
        .onAppear {
            withAnimation(reduceMotion ? .easeOut(duration: 0.2) : .spring(response: 0.55, dampingFraction: 0.82).delay(0.35)) {
                arrived = true
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Get Passband for iPhone")
    }

    // MARK: - the hero

    private var hero: some View {
        ZStack(alignment: .bottom) {
            PhosphorBackdrop(paused: activeState != .key, scene: .card)
                .allowsHitTesting(false)
                .accessibilityHidden(true)

            // The phosphor's floor bleeds into the card rather than ending at
            // a seam: the hero fades into the glass under it.
            LinearGradient(
                colors: [.clear, .black.opacity(0.35)],
                startPoint: .center, endPoint: .bottom
            )
            .allowsHitTesting(false)

            PhoneMock(reduceMotion: reduceMotion, live: activeState == .key)
                .offset(y: arrived ? 66 : 220)
                .animation(
                    reduceMotion ? nil : .spring(response: 0.75, dampingFraction: 0.78).delay(0.6),
                    value: arrived)
        }
        .frame(height: Self.heroHeight)
        .clipShape(UnevenRoundedRectangle(topLeadingRadius: 20, topTrailingRadius: 20))
        .overlay(alignment: .topTrailing) { closeButton }
        .overlay(alignment: .topLeading) { betaPill }
    }

    private var closeButton: some View {
        Button(action: dismiss) {
            Image(systemName: "xmark")
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(.white.opacity(0.9))
                .frame(width: 24, height: 24)
                .background(.black.opacity(0.35), in: Circle())
                .overlay(Circle().strokeBorder(.white.opacity(0.18), lineWidth: 0.5))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .padding(10)
        .help("Close")
        .accessibilityLabel("Close")
    }

    private var betaPill: some View {
        Text("NEW  ·  IPHONE ALPHA")
            .font(.system(size: 9, weight: .bold).width(.expanded))
            .tracking(0.6)
            .foregroundStyle(.white.opacity(0.92))
            .padding(.horizontal, 9)
            .padding(.vertical, 5)
            .background(.black.opacity(0.32), in: Capsule())
            .overlay(Capsule().strokeBorder(Color(hex: 0x82BAF5).opacity(0.45), lineWidth: 0.5))
            .padding(12)
    }

    // MARK: - the words

    private var words: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Passband, in your pocket.")
                    .font(Typo.hero(25))
                    .foregroundStyle(Palette.ink)
                Text("Your phone buzzes for the mail that needs you and stays quiet for everything else.")
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack(spacing: 14) {
                if let qr = Self.qrCode {
                    Image(nsImage: qr)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 66, height: 66)
                        .padding(5)
                        // Pinned white in both themes: a phone camera reads
                        // dark-on-light, and an inverted code is a coin flip.
                        .background(.white, in: RoundedRectangle(cornerRadius: 9, style: .continuous))
                        .accessibilityLabel("QR code for the TestFlight invite")
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text("Scan with your iPhone")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                    Text("Join the alpha on TestFlight.")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                        .fixedSize(horizontal: false, vertical: true)
                    Button(action: copyLink) {
                        Label(copied ? "Link copied" : "Copy link", systemImage: copied ? "checkmark" : "link")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Palette.accent)
                            .contentTransition(.symbolEffect(.replace))
                    }
                    .buttonStyle(.plain)
                    .padding(.top, 3)
                }
            }
        }
        .padding(.horizontal, 18)
        .padding(.top, 16)
        .padding(.bottom, 18)
    }

    // MARK: - verbs

    private func dismiss() {
        withAnimation(.easeIn(duration: 0.2)) { leaving = true }
        Task {
            try? await Task.sleep(for: .milliseconds(200))
            close(copied)
        }
    }

    /// Copy and say so; the card stays up, because the person may still want
    /// the code.
    private func copyLink() {
        let board = NSPasteboard.general
        board.clearContents()
        board.setString(TestFlightNudge.joinURL.absoluteString, forType: .string)
        withAnimation(Motion.toast) { copied = true }
    }

    /// Rendered once per process. Nil only if CoreImage cannot make a QR code
    /// of a 40-character URL, in which case the card is copy-only.
    private static let qrCode: NSImage? = {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(TestFlightNudge.joinURL.absoluteString.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage,
            let cg = CIContext().createCGImage(output, from: output.extent)
        else { return nil }
        return NSImage(cgImage: cg, size: output.extent.size)
    }()
}

// MARK: - the phone

/// An iPhone drawn in SwiftUI, showing a tiny sitrep and, on a loop, the push
/// that is the whole point. Always dark: it is a screen inside a night scene,
/// whatever the Mac's appearance.
private struct PhoneMock: View {
    let reduceMotion: Bool
    let live: Bool

    @State private var banner = false

    private static let size = CGSize(width: 118, height: 236)
    private static let screenInk = Color(hex: 0xE9EEF5)
    private static let screenDim = Color(hex: 0x808E9F)
    private static let accent = Color(hex: 0x4E9BEA)

    var body: some View {
        ZStack(alignment: .top) {
            // The body: titanium edge, glass black inside.
            RoundedRectangle(cornerRadius: 24, style: .continuous)
                .fill(Color(hex: 0x0B0F16))
                .overlay(
                    RoundedRectangle(cornerRadius: 24, style: .continuous)
                        .strokeBorder(
                            LinearGradient(
                                colors: [Color(hex: 0x9AA7B8), Color(hex: 0x3A4452), Color(hex: 0x7D8A9C)],
                                startPoint: .topLeading, endPoint: .bottomTrailing),
                            lineWidth: 2.5))

            screen
                .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
                .padding(5)

            // The island.
            Capsule()
                .fill(.black)
                .frame(width: 34, height: 10)
                .padding(.top, 11)
        }
        .frame(width: Self.size.width, height: Self.size.height)
        .shadow(color: Color(hex: 0x4E9BEA).opacity(0.35), radius: 22)
        .shadow(color: .black.opacity(0.5), radius: 10, y: 6)
        .rotation3DEffect(.degrees(-8), axis: (x: 0.15, y: 1, z: 0), perspective: 0.6)
        .task(id: live) { await loop() }
        .accessibilityHidden(true)
    }

    private var screen: some View {
        ZStack(alignment: .top) {
            LinearGradient(
                colors: [Color(hex: 0x111A27), Color(hex: 0x090D16)],
                startPoint: .top, endPoint: .bottom)

            VStack(alignment: .leading, spacing: 5) {
                Text("GOOD MORNING")
                    .font(.system(size: 5, weight: .semibold).width(.expanded))
                    .foregroundStyle(Self.accent)
                Text("2 items for\nyour eyes.")
                    .font(Typo.hero(13))
                    .foregroundStyle(Self.screenInk)
                    .lineSpacing(-2)
                    .fixedSize(horizontal: false, vertical: true)
                VStack(spacing: 4) {
                    row(width: 0.9, money: true)
                    row(width: 0.7, money: false)
                    row(width: 0.8, money: false)
                }
                .padding(.top, 4)
            }
            .padding(.horizontal, 10)
            // Under the banner's lane, so the push lands in its own space
            // rather than on the headline.
            .padding(.top, 62)
            .frame(maxWidth: .infinity, alignment: .leading)

            notification
                .padding(.horizontal, 5)
                .padding(.top, 24)
                .offset(y: banner ? 0 : -44)
                .opacity(banner ? 1 : 0)
                .scaleEffect(banner ? 1 : 0.94, anchor: .top)
        }
    }

    private func row(width: CGFloat, money: Bool) -> some View {
        HStack(spacing: 4) {
            Circle().fill(Self.screenDim.opacity(0.5)).frame(width: 7, height: 7)
            RoundedRectangle(cornerRadius: 1.5)
                .fill(Self.screenDim.opacity(0.45))
                .frame(height: 3.5)
                .frame(maxWidth: .infinity, alignment: .leading)
                .scaleEffect(x: width, anchor: .leading)
            if money {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(Self.accent.opacity(0.85))
                    .frame(width: 12, height: 3.5)
            }
        }
        .padding(.horizontal, 5)
        .padding(.vertical, 5)
        .background(Color.white.opacity(0.045), in: RoundedRectangle(cornerRadius: 4, style: .continuous))
    }

    /// The push. Real sender, real shape: what the lock screen actually shows
    /// when the For Your Eyes lane lets something through.
    private var notification: some View {
        HStack(spacing: 5) {
            Group {
                if let mark = IntroBrand.lightMark ?? IntroBrand.mark {
                    mark.resizable().scaledToFit().padding(2)
                } else {
                    Image(systemName: "waveform").font(.system(size: 7, weight: .bold))
                }
            }
            .frame(width: 15, height: 15)
            .background(Color(hex: 0x14243A), in: RoundedRectangle(cornerRadius: 4, style: .continuous))

            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 0) {
                    Text("Chase")
                        .font(.system(size: 6.5, weight: .semibold))
                    Spacer(minLength: 2)
                    Text("now")
                        .font(.system(size: 5.5))
                        .foregroundStyle(Self.screenDim)
                }
                Text("Card payment of $482 due Friday")
                    .font(.system(size: 5.5))
                    .foregroundStyle(Self.screenInk.opacity(0.85))
                    .lineLimit(1)
            }
            .foregroundStyle(Self.screenInk)
        }
        .padding(5)
        .background(Color(hex: 0x2A3442).opacity(0.92), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(.white.opacity(0.08), lineWidth: 0.5))
    }

    /// Land, hold, leave, rest, forever, while the window is key. Reduce Motion
    /// gets the banner held still instead.
    private func loop() async {
        if reduceMotion {
            banner = true
            return
        }
        guard live else { return }
        try? await Task.sleep(for: .milliseconds(1700))
        while !Task.isCancelled {
            withAnimation(.spring(response: 0.42, dampingFraction: 0.72)) { banner = true }
            try? await Task.sleep(for: .milliseconds(3400))
            guard !Task.isCancelled else { return }
            withAnimation(.easeIn(duration: 0.25)) { banner = false }
            try? await Task.sleep(for: .milliseconds(1800))
        }
    }
}
