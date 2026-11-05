// THE IPHONE AD. Two days into the Mac app, once, for somebody whose mailbox
// has never seen the phone app: a card in the bottom-right corner that sells
// the phone app the way the landing page would, in the app's own materials.
//
// THE HERO IS THE PITCH. The live P7 phosphor (the quiet board's scene, so the
// brand's own light) runs behind an iPhone rising out of the card's lower
// edge. On its screen is a tiny sitrep, and every few seconds a push lands on
// it: the one thing the phone app does that the Mac cannot. Reduce Motion
// holds the notification on screen and lets the phosphor settle. The card,
// the hero, the pill and the ✕ are the ad family's shared materials
// (AdCard.swift), which the agent-door ad wears too.
//
// A QR CODE, NOT JUST A LINK. The link is for a phone, and opening it on the
// Mac lands on a page that says to open it on a phone. Pointing the camera at
// the screen is the shortest way there; "Copy link" is the fallback.
//
// A FIXED COMPOSITION, so none of its type follows the density setting: the
// card, the hero and the phone drawn in it are set sizes, and the headline is
// cut to that width. Every font here is a literal or `fixed:` on purpose.
//
// ON DAY ONE, A PLAIN PANEL. The first-load veil sets its own iPhone card
// beside the progress (`FirstLoadIPhoneCard`, below): the app's own type and
// glass, no hero, sharing only the code and the link with this card. When the
// veil closes by itself this corner card takes over, so the code never
// vanishes from under a camera; see TestFlightNudge's header for every rule.
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
                Analytics.capture("ios_nudge_closed", ["copied": copied, "source": nudge.source])
                // What's New waits behind this card (WhatsNew.maybeShow); its turn.
                AppStore.shared.whatsNew.maybeShow()
            }
            .adCardPlacement()
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

    var body: some View {
        VStack(spacing: 0) {
            hero
            words
        }
        .adCardChrome(arrived: arrived, leaving: leaving, label: "Get Passband for iPhone")
        .onAppear {
            withAnimation(reduceMotion ? .easeOut(duration: 0.2) : .spring(response: 0.55, dampingFraction: 0.82).delay(0.35)) {
                arrived = true
            }
        }
    }

    // MARK: - the hero

    /// The shared hero (`AdHero`), with the phone rising out of it.
    private var hero: some View {
        AdHero(live: activeState == .key, pill: "NEW  ·  IPHONE ALPHA", close: dismiss) {
            PhoneMock(reduceMotion: reduceMotion, live: activeState == .key)
                .offset(y: arrived ? 66 : 220)
                .animation(
                    reduceMotion ? nil : .spring(response: 0.75, dampingFraction: 0.78).delay(0.6),
                    value: arrived)
        }
    }

    // MARK: - the words

    private var words: some View {
        VStack(alignment: .leading, spacing: AdCard.wordsGap) {
            VStack(alignment: .leading, spacing: AdCard.headlineGap) {
                Text("Passband, in your pocket.")
                    .font(Typo.hero(25, fixed: true))
                    .foregroundStyle(Palette.ink)
                Text("Your phone tells you what matters and stays quiet for everything else.")
                    .font(Typo.ui(12, fixed: true))
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack(spacing: AdCard.wordsGap) {
                if let qr = TestFlightQR.image {
                    Image(nsImage: qr)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 66, height: 66)
                        .padding(5)
                        // Pinned white in both themes: a phone camera reads
                        // dark-on-light, and an inverted code is a coin flip.
                        .background(.white, in: RoundedRectangle(cornerRadius: Radius.well, style: .continuous))
                        .accessibilityLabel("QR code for the TestFlight invite")
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text("Scan to join")
                        .font(Typo.ui(13, weight: .semibold, fixed: true))
                        .foregroundStyle(Palette.ink)
                    Text("the alpha on TestFlight")
                        .font(Typo.ui(10, weight: .medium, fixed: true))
                        .foregroundStyle(Palette.inkFaint)
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
    }

    // MARK: - verbs

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

/// The invite as a QR code and on the pasteboard, for the card and the veil.
enum TestFlightQR {
    /// What the phone's morning brief looks like, for the pictures of it on
    /// both iPhone cards. Made up, and calm on purpose: what the day holds,
    /// no counts, nothing "for your eyes", nothing that "needs you".
    static let briefSample =
        "PG&E bill due tomorrow. A package arrives today. First up: Dentist at 9:30 AM."

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

// MARK: - the veil's iPhone card

/// THE IPHONE ASK BESIDE THE FIRST-LOAD PROGRESS, as a second panel of the
/// same screen rather than an ad: the progress card's glass, padding,
/// wordmark, headline and body type, and the code on a plain white tile. No
/// hero, no phone, no glow. No ✕ and no button of its own: it goes with the
/// veil, at Start now, Enter or Esc. It fills the height the veil gives it,
/// with the code on the floor, level with Start now beside it.
struct FirstLoadIPhoneCard: View {
    @Environment(\.colorScheme) private var colorScheme
    @State private var copied = false

    var body: some View {
        ModalCard(width: 380) {
            VStack(alignment: .leading, spacing: Space.scaled(22)) {
                Text("passband")
                    .font(Typo.serif(19, weight: .medium))
                    .foregroundStyle(Palette.ink)
                VStack(alignment: .leading, spacing: Space.s10) {
                    Text("Passband, in your pocket.")
                        .font(Typo.serif(32, weight: .medium))
                        .foregroundStyle(Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)
                    Text("Your phone tells you what matters and stays quiet for everything else.")
                        .font(Typo.ui(15))
                        .foregroundStyle(Palette.inkDim)
                        .fixedSize(horizontal: false, vertical: true)
                }
                brief
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

    /// What the phone gets, drawn flat in the app's own palette: one
    /// notification, Passband's mark, no title, the day in a sentence.
    private var brief: some View {
        HStack(alignment: .top, spacing: Space.s10) {
            Group {
                // The light mark on dark glass, where the dark one vanishes.
                if let mark = colorScheme == .dark ? (IntroBrand.lightMark ?? IntroBrand.mark) : IntroBrand.mark {
                    mark.resizable().scaledToFit().padding(4)
                } else {
                    Image(systemName: "waveform").font(Typo.ui(12, weight: .bold))
                }
            }
            .frame(width: 26, height: 26)
            .background(Palette.accent.opacity(0.12), in: RoundedRectangle(cornerRadius: 7, style: .continuous))
            Text(TestFlightQR.briefSample)
                .font(Typo.ui(13))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
            Text("now")
                .font(Typo.ui(11))
                .foregroundStyle(Palette.inkFaint)
        }
        .padding(Space.s12)
        .background(Palette.ink.opacity(0.04), in: RoundedRectangle(cornerRadius: Radius.well, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .strokeBorder(Palette.hairline, lineWidth: 1))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("A Passband notification: \(TestFlightQR.briefSample)")
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

    /// A lock screen: the date and the clock, and the morning brief landing
    /// under them. No counts on it anywhere: the brief itself dropped them
    /// because a number of things "for your eyes" reads as pressure.
    private var screen: some View {
        ZStack(alignment: .top) {
            LinearGradient(
                colors: [Color(hex: 0x111A27), Color(hex: 0x090D16)],
                startPoint: .top, endPoint: .bottom)

            VStack(spacing: 1) {
                Text("Thursday, November 5")
                    .font(.system(size: 6, weight: .semibold))
                    .foregroundStyle(Self.screenInk.opacity(0.8))
                Text("8:02")
                    .font(.system(size: 30, weight: .semibold).width(.condensed))
                    .foregroundStyle(Self.screenInk)
            }
            .padding(.top, 22)
            .frame(maxWidth: .infinity)

            notification
                .padding(.horizontal, 5)
                .padding(.top, 72)
                .offset(y: banner ? 0 : -20)
                .opacity(banner ? 1 : 0)
                .scaleEffect(banner ? 1 : 0.94, anchor: .top)
        }
    }

    /// The push: the morning brief as the phone gets it, a Passband
    /// notification with no title, a calm summary of the day.
    private var notification: some View {
        HStack(alignment: .top, spacing: 5) {
            Group {
                if let mark = IntroBrand.lightMark ?? IntroBrand.mark {
                    mark.resizable().scaledToFit().padding(2)
                } else {
                    Image(systemName: "waveform").font(.system(size: 7, weight: .bold))
                }
            }
            .frame(width: 15, height: 15)
            .background(Color(hex: 0x14243A), in: RoundedRectangle(cornerRadius: 4, style: .continuous))

            Text(TestFlightQR.briefSample)
                .font(.system(size: 5.5))
                .foregroundStyle(Self.screenInk.opacity(0.9))
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
            Text("now")
                .font(.system(size: 5))
                .foregroundStyle(Self.screenDim)
        }
        .padding(5)
        .background(Color(hex: 0x2A3442).opacity(0.92), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(.white.opacity(0.08), lineWidth: 0.5))
    }

    /// Land, hold, leave, rest, forever, while the window is key. Reduce Motion,
    /// and a window in the background, get the banner held still instead: a
    /// phone at rest still shows what it is for.
    private func loop() async {
        if reduceMotion || !live {
            banner = true
            return
        }
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
