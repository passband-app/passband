// THE IPHONE ASK, IN TWO PLACES. On day one it is a second panel beside the
// first-load progress (`FirstLoadIPhoneCard`). Otherwise, once, two days into
// the Mac app, for somebody whose mailbox has never seen the phone app, it is
// a card in the bottom-right corner (`TestFlightCornerCard`). When the veil
// closes by itself the corner card takes over, so the code never vanishes from
// under a camera; see TestFlightNudge's header for every rule.
//
// PLAIN ON PURPOSE. Both are the app's own glass and type: the app icon
// beside a serif "Passband on iOS", one line, a flat picture of the one notification the phone
// gets, and the code. No phosphor, no drawn phone, no pill. An earlier corner
// card had all three and read as an advertisement.
//
// A QR CODE, NOT JUST A LINK. The link is for a phone, and opening it on the
// Mac lands on a page that says to open it on a phone. The camera is the
// shortest way there; "Copy link" is the fallback.
//
// NOT MODAL. The corner card is closed by its ✕ and leaves the inbox usable
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
            TestFlightCornerCard { copied in
                nudge.showingNudge = false
                Analytics.capture("ios_nudge_closed", ["copied": copied, "source": nudge.source])
                // What's New waits behind this card (WhatsNew.maybeShow); its turn.
                AppStore.shared.whatsNew.maybeShow()
            }
            .adCardPlacement()
        }
    }
}

/// The corner card: the veil card's contents at the corner's width, with a ✕.
/// Its sizes are fixed (`fixed: true`), like every corner card's.
private struct TestFlightCornerCard: View {
    let close: (_ copied: Bool) -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var arrived = false
    @State private var leaving = false
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: AdCard.wordsGap) {
            HStack(alignment: .center, spacing: 10) {
                TestFlightQR.AppIcon(side: 32)
                Text(TestFlightQR.headline)
                    .font(Typo.serif(22, weight: .medium, fixed: true))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                Spacer(minLength: 0)
                Button(action: dismiss) {
                    Image(systemName: "xmark")
                        .font(Typo.ui(11, weight: .semibold, fixed: true))
                        .foregroundStyle(Palette.inkFaint)
                        .frame(width: 22, height: 22)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Close")
                .accessibilityLabel("Close")
            }
            Text(TestFlightQR.pitch)
                .font(Typo.ui(12, fixed: true))
                .foregroundStyle(Palette.inkDim)
                .fixedSize(horizontal: false, vertical: true)
            IPhoneNotificationPreview(size: 12, fixed: true)
            HStack(spacing: AdCard.wordsGap) {
                if let qr = TestFlightQR.image {
                    Image(nsImage: qr)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 66, height: 66)
                        .padding(5)
                        // White in both themes: a phone camera reads dark on light.
                        .background(.white, in: RoundedRectangle(cornerRadius: Radius.well, style: .continuous))
                        .accessibilityLabel("QR code for the TestFlight invite")
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text("Scan to join")
                        .font(Typo.ui(13, weight: .semibold, fixed: true))
                        .foregroundStyle(Palette.ink)
                    Text("the alpha on TestFlight")
                        .font(Typo.ui(11, fixed: true))
                        .foregroundStyle(Palette.inkDim)
                        .fixedSize(horizontal: false, vertical: true)
                    Button(action: copyLink) {
                        Label(copied ? "Link copied" : "Copy link", systemImage: copied ? "checkmark" : "link")
                            .font(Typo.ui(11, weight: .semibold, fixed: true))
                            .foregroundStyle(Palette.accent)
                            .contentTransition(.symbolEffect(.replace))
                    }
                    .buttonStyle(.plain)
                    .padding(.top, 3)
                }
            }
        }
        .adWordsPadding()
        .adCardChrome(arrived: arrived, leaving: leaving, label: "Get Passband for iPhone")
        .onAppear {
            withAnimation(reduceMotion ? .easeOut(duration: 0.2) : .spring(response: 0.55, dampingFraction: 0.82).delay(0.35)) {
                arrived = true
            }
        }
    }

    private func dismiss() {
        // A double click lands inside the fade: one close, one event.
        guard !leaving else { return }
        withAnimation(.easeIn(duration: 0.2)) { leaving = true }
        Task {
            try? await Task.sleep(for: .milliseconds(200))
            close(copied)
        }
    }

    /// Copy and say so; the card stays up, because the person may still want
    /// the code.
    private func copyLink() {
        TestFlightQR.copyLink()
        withAnimation(Motion.toast) { copied = true }
    }
}

/// The invite as a QR code and on the pasteboard, and the words both iPhone
/// cards share.
enum TestFlightQR {
    static let headline = "Passband on iOS"
    static let pitch = "Your phone tells you what matters and stays quiet for everything else."

    /// The notification in the picture on both iPhone cards. Made up: one
    /// person, one thing to do, and the work already done for it.
    static let notificationSample = "Jamie needs a reply today, you have a draft ready to review."

    /// The app's icon. The Mac and the iPhone ship the same Icon Composer
    /// document (project.yml), so the Mac's own icon is the iPhone's too.
    struct AppIcon: View {
        let side: CGFloat

        var body: some View {
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .interpolation(.high)
                .frame(width: side, height: side)
                .accessibilityHidden(true)
        }
    }

    static func copyLink() {
        let board = NSPasteboard.general
        board.clearContents()
        board.setString(TestFlightNudge.joinURL.absoluteString, forType: .string)
    }

    /// Rendered once per process. Nil only if CoreImage cannot make a QR code
    /// of a 40-character URL, in which case the card is copy-only.
    static let image: NSImage? = {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(TestFlightNudge.joinURL.absoluteString.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage,
            let cg = CIContext().createCGImage(output, from: output.extent)
        else { return nil }
        return NSImage(cgImage: cg, size: output.extent.size)
    }()
}

/// What the phone gets, drawn flat in the app's own palette: one
/// notification, Passband's mark, no title.
private struct IPhoneNotificationPreview: View {
    let size: CGFloat
    var fixed = false
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        let mark = size * 2
        HStack(alignment: .top, spacing: size * 0.75) {
            Group {
                // The light mark on dark glass, where the dark one vanishes.
                if let image = colorScheme == .dark ? (IntroBrand.lightMark ?? IntroBrand.mark) : IntroBrand.mark {
                    image.resizable().scaledToFit().padding(size * 0.3)
                } else {
                    Image(systemName: "waveform").font(Typo.ui(size * 0.9, weight: .bold, fixed: fixed))
                }
            }
            .frame(width: mark, height: mark)
            .background(Palette.accent.opacity(0.12), in: RoundedRectangle(cornerRadius: size * 0.55, style: .continuous))
            Text(TestFlightQR.notificationSample)
                .font(Typo.ui(size, fixed: fixed))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
            Text("now")
                .font(Typo.ui(size - 2, fixed: fixed))
                .foregroundStyle(Palette.inkFaint)
        }
        .padding(size * 0.9)
        .background(Palette.ink.opacity(0.04), in: RoundedRectangle(cornerRadius: Radius.well, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .strokeBorder(Palette.hairline, lineWidth: 1))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("A Passband notification: \(TestFlightQR.notificationSample)")
    }
}

// MARK: - the veil's iPhone card

/// THE IPHONE ASK BESIDE THE FIRST-LOAD PROGRESS, as a second panel of the
/// same screen rather than an ad: the progress card's glass, padding,
/// headline and body type, and the code on a plain white tile. No
/// hero, no phone, no glow. No ✕ and no button of its own: it goes with the
/// veil, at Start now, Enter or Esc. It fills the height the veil gives it,
/// with the code on the floor, level with Start now beside it.
struct FirstLoadIPhoneCard: View {
    @State private var copied = false

    var body: some View {
        ModalCard(width: 380) {
            VStack(alignment: .leading, spacing: Space.scaled(22)) {
                // No wordmark: the progress card beside it already wears one.
                VStack(alignment: .leading, spacing: Space.s10) {
                    HStack(alignment: .center, spacing: Space.s12) {
                        TestFlightQR.AppIcon(side: 44)
                        Text(TestFlightQR.headline)
                            .font(Typo.serif(28, weight: .medium))
                            .foregroundStyle(Palette.ink)
                            .lineLimit(1)
                            .minimumScaleFactor(0.8)
                    }
                    Text(TestFlightQR.pitch)
                        .font(Typo.ui(15))
                        .foregroundStyle(Palette.inkDim)
                        .fixedSize(horizontal: false, vertical: true)
                }
                IPhoneNotificationPreview(size: 13)
                VStack(alignment: .leading, spacing: 0) {
                    Spacer(minLength: 0)
                    code
                }
            }
            .padding(Space.s14)
            .frame(maxHeight: .infinity, alignment: .top)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Get Passband for iPhone")
        .onAppear { TestFlightNudge.shared.veilAdDrawn = true }
    }

    private var code: some View {
        HStack(spacing: Space.s14) {
            if let qr = TestFlightQR.image {
                Image(nsImage: qr)
                    .interpolation(.none)
                    .resizable()
                    .frame(width: 84, height: 84)
                    .padding(6)
                    // White in both themes: a phone camera reads dark on light.
                    .background(.white, in: RoundedRectangle(cornerRadius: Radius.well, style: .continuous))
                    .accessibilityLabel("QR code for the TestFlight invite")
            }
            VStack(alignment: .leading, spacing: Space.s4) {
                Text("Scan to join")
                    .font(Typo.ui(15, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                Text("the alpha on TestFlight")
                    .font(Typo.ui(13))
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
                Button(action: copyLink) {
                    Label(copied ? "Link copied" : "Copy link", systemImage: copied ? "checkmark" : "link")
                        .font(Typo.ui(13, weight: .semibold))
                        .foregroundStyle(Palette.accent)
                        .contentTransition(.symbolEffect(.replace))
                }
                .buttonStyle(.plain)
                .padding(.top, Space.s2)
            }
        }
    }

    private func copyLink() {
        TestFlightQR.copyLink()
        TestFlightNudge.shared.copiedInVeil = true
        withAnimation(Motion.toast) { copied = true }
    }
}

// MARK: - the fallback, over the live app

extension TestFlightNudge {
    /// Every other one-time card that can be on screen. The iPhone card waits
    /// for them rather than piling on.
    static func othersShowing(_ store: AppStore) -> Bool {
        ShareNudge.shared.showingNudge
            || AgentNudge.shared.showingNudge
            || store.firstLoad.active
            || store.whatsNew.active
            || Updater.shared.phase != .idle
    }

    /// A first look that never needed the veil has ended: the corner card a
    /// short beat later, once nothing else is up (a newcomer's What's New
    /// usually is), and only if the rule still holds then. Gives up after ten
    /// minutes of other cards; the two-day rule is still there behind it.
    func askAfterOnboarding(_ store: AppStore) {
        Task { @MainActor in
            try? await Task.sleep(for: Self.afterOnboardingDelay)
            for _ in 0..<300 where Self.othersShowing(store) {
                try? await Task.sleep(for: .seconds(2))
            }
            guard !Self.othersShowing(store), !showingNudge, store.connStatus == .connected,
                Self.earnedAfterOnboarding(veilShown: false, iosConnected: store.iosConnected, asked: asked)
            else { return }
            ask(source: "onboarding")
        }
    }
}
