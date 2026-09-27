import SwiftUI

/// A user-paced introduction, shared by first connection and the isolated
/// rehearsal. The illustrated messages are examples, never account data.
struct OnboardingIntroView: View {
    let onContinue: () -> Void
    var continueTitle = "Bring your inbox"
    var continueHint = "Continue to account connection"

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase
    @State private var page = 0
    @State private var backgrounded = false

    private var organized: Bool { page > 0 }

    var body: some View {
        // A ZStack, not a Group: a Group hands its modifiers to each branch,
        // so a fallback swap mid-intro would fire onDisappear/onAppear and
        // replay the opening burst.
        ZStack {
            if !SquelchScene.shared.isUnavailable {
                squelchLayout
            } else {
                cardLayout
            }
        }
        #if os(iOS)
            // THE PHONE PAGES BY HAND. No continue button and no skip: the
            // whole screen is the control, a swipe left moves on and a swipe
            // right goes back, and the hint at the bottom says so. Each beat is
            // also felt, for as long as it is on screen (IntroHaptics): static
            // while the squelch is open, a steady sine once it closes.
            .contentShape(Rectangle())
            // Simultaneous, so the card fallback's ScrollView keeps its pan.
            .simultaneousGesture(pageSwipe)
            .onAppear { IntroHaptics.shared.switchOn() }
            .onDisappear { IntroHaptics.shared.shutdown() }
            // Only a real trip to the background stops the engine. Pulling
            // down Control Center is inactive -> active and stops nothing, and
            // resuming there would cut the opening burst for no reason.
            .onChange(of: scenePhase) { _, phase in
                if phase == .background { backgrounded = true }
                if phase == .active, backgrounded {
                    backgrounded = false
                    IntroHaptics.shared.resume()
                }
            }
            .onChange(of: page) { _, now in
                if now == 1 { IntroHaptics.shared.sine() } else { IntroHaptics.shared.noise() }
            }
        #endif
    }

    private func advance() {
        if page == 1 { onContinue() } else { page += 1 }
    }

    #if os(iOS)
        /// Horizontal, decisive, and judged on where the fling was headed as
        /// much as where it stopped, so a quick flick counts as readily as a
        /// long drag. A mostly vertical drag is not a page turn.
        private var pageSwipe: some Gesture {
            DragGesture(minimumDistance: 24)
                .onEnded { value in
                    let dx = value.translation.width, dy = value.translation.height
                    let aimed = value.predictedEndTranslation.width
                    guard abs(dx) > abs(dy) * 1.2 else { return }
                    if dx < -50 || aimed < -140 {
                        advance()
                    } else if (dx > 50 || aimed > 140), page > 0 {
                        page -= 1
                    }
                }
        }
    #endif

    /// The radio beats: raw noise pours through a dormant gate, then the
    /// continue button closes the squelch and only the passband comes out.
    /// Night draws the waves as glow, day as ink on paper.
    private var squelchLayout: some View {
        GeometryReader { geometry in
            let wide = geometry.size.width >= 820
            ZStack(alignment: .topLeading) {
                IntroSceneBackdrop(engaged: organized, wide: wide)
                VStack(alignment: .leading, spacing: 0) {
                    masthead
                        .modifier(IntroEntrance(delay: 0, distance: 4))
                    // Narrow is the phone (the Mac window floors at 980pt).
                    // Split evenly with the spacer below, the leftover height
                    // set the story down on the crest of the nearest bands,
                    // where the body line was unreadable; capped, it stays in
                    // the dark sky the top gradient keeps for it.
                    Spacer(minLength: 24)
                        .frame(maxHeight: wide ? .infinity : 40)
                    story(wide: wide)
                        .frame(maxWidth: wide ? 480 : .infinity, alignment: .leading)
                    Spacer(minLength: wide ? 24 : 260)
                    controls
                }
                .padding(wide ? 44 : 24)
            }
        }
    }

    /// The fallback when Metal is unavailable: illustrated example mail.
    private var cardLayout: some View {
        GeometryReader { geometry in
            let wide = geometry.size.width >= 820
            ScrollView {
                VStack(alignment: .leading, spacing: wide ? 40 : 24) {
                    masthead
                        .modifier(IntroEntrance(delay: 0, distance: 4))

                    if wide {
                        HStack(alignment: .center, spacing: 44) {
                            story(wide: true)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            mailIllustration
                                .frame(width: 380, height: 380)
                        }
                    } else {
                        story(wide: false)
                        mailIllustration
                            .frame(maxWidth: 440)
                            .frame(height: 290)
                            .frame(maxWidth: .infinity)
                    }

                    controls
                }
                .padding(wide ? 44 : 24)
                .frame(maxWidth: 1060)
                .frame(minHeight: geometry.size.height, alignment: .center)
                .frame(maxWidth: .infinity)
            }
        }
        .background { IntroFallbackGlow() }
    }

    private var masthead: some View {
        HStack {
            IntroWordmark()
            Spacer()
            if RehearsalMode.launchedStandalone {
                DemoAppearancePicker()
                    .padding(.trailing, 12)
            }
        }
    }

    private func story(wide: Bool) -> some View {
        VStack(alignment: .leading, spacing: 22) {
            VStack(alignment: .leading, spacing: 18) {
                if organized {
                    Text("Know what needs you.")
                        .font(Typo.hero(wide ? 49 : 36))
                        .modifier(IntroEntrance(delay: 0.04))
                } else {
                    Text("Inbox zero every day was never realistic.")
                        .font(Typo.serif(wide ? 33 : 27))
                        .foregroundStyle(Palette.inkDim)
                        .modifier(IntroEntrance(delay: 0.04))
                    Text("You’re only human.")
                        .font(Typo.hero(wide ? 49 : 36))
                        .modifier(IntroEntrance(delay: 0.14))
                }
            }
            .foregroundStyle(Palette.ink)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)

            Text(organized
                 ? "Passband brings the important things forward, so you can give them your attention and get on with your day."
                 : "Your attention is valuable. You deserve an inbox that treats it that way.")
                .font(.system(size: 15))
                .foregroundStyle(Palette.inkDim)
                .lineSpacing(5)
                .fixedSize(horizontal: false, vertical: true)
                .modifier(IntroEntrance(delay: organized ? 0.16 : 0.24))
        }
        // Recreate just the copy on a new beat, so it settles in once while
        // the existing mail cards continue into their new positions.
        .id(page)
    }

    private var mailIllustration: some View {
        GeometryReader { geometry in
            let width = min(geometry.size.width - 24, 330.0)
            let centerX = geometry.size.width / 2
            let centerY = geometry.size.height / 2 - 8

            ZStack {
                Circle()
                    .fill(Palette.accentSoft.opacity(0.4))
                    .frame(width: min(geometry.size.width, 340))
                    .overlay(Circle().strokeBorder(Palette.hairline, lineWidth: 0.75))
                    .position(x: centerX, y: centerY)

                ForEach(Array(IntroMail.examples.enumerated()), id: \.element.id) { index, mail in
                    IntroMailCard(mail: mail, organized: organized)
                        .frame(width: width)
                        .rotationEffect(.degrees(organized ? 0 : Double(index - 1) * 9))
                        .scaleEffect(organized ? (index == 0 ? 1 : 0.94) : 1 - Double(index) * 0.035)
                        .position(
                            x: centerX + (organized ? 0 : CGFloat(index - 1) * 9),
                            y: centerY + (organized ? CGFloat(index - 1) * 89 : CGFloat(index - 1) * 29))
                        .animation(
                            reduceMotion ? nil : .easeInOut(duration: 0.65).delay(Double(index) * 0.045),
                            value: organized)
                        .modifier(IntroEntrance(delay: 0.10 + Double(index) * 0.07, distance: 10))
                        .zIndex(Double(3 - index))
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(organized
            ? "Example mail organized: a request needs you, a receipt is saved in records, and a newsletter is set aside for later."
            : "An overlapping pile of example emails: a request, a receipt, and a newsletter.")
    }

    private var controls: some View {
        HStack(spacing: 16) {
            HStack(spacing: 6) {
                ForEach(0..<2) { index in
                    Capsule().fill(page == index ? Palette.accent : Palette.hairlineStrong)
                        .frame(width: page == index ? 24 : 8, height: 5)
                }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Introduction, step \(page + 1) of 2")
            .animation(reduceMotion ? nil : .easeInOut(duration: 0.28), value: page)

            // The phone goes back by swiping right; only the Mac needs a button.
            #if os(macOS)
                if organized {
                    Button("Back") { page -= 1 }
                        .buttonStyle(.plain)
                        .foregroundStyle(Palette.inkDim)
                }
            #endif

            Spacer(minLength: 0)
            #if os(iOS)
                SwipeHint(text: organized ? "Swipe to begin" : "Swipe for a little breathing room")
                    // VoiceOver cannot flick the page, so the hint is where
                    // that gesture lives for it.
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(organized ? "Begin" : "Next")
                    .accessibilityHint(organized ? continueHint : "")
                    .accessibilityAddTraits(.isButton)
                    .accessibilityAction { advance() }
                    // And the swipe back, for the same reason.
                    .accessibilityAction(named: "Back") { if page > 0 { page -= 1 } }
            #else
            Button("Skip intro", action: onContinue)
                .buttonStyle(.plain)
                .font(.system(size: 11))
                .foregroundStyle(Palette.inkFaintest)
                .padding(.vertical, 8)
                .accessibilityHint(continueHint)
            Button {
                if page == 1 { onContinue() }
                else { page += 1 }
            } label: {
                HStack(spacing: 9) {
                    Text(organized ? continueTitle : "Find a little breathing room")
                    Image(systemName: "arrow.right")
                }
                .font(.system(size: 13, weight: .semibold))
                .padding(.vertical, 7)
                .padding(.horizontal, 8)
            }
            .buttonStyle(.borderedProminent)
            .tint(Palette.accent)
            .keyboardShortcut(.defaultAction)
            #endif
        }
        .padding(.bottom, 24)
    }
}

#if os(iOS)
    /// Where the continue button is on the Mac: a line of copy and a run of
    /// chevrons that keep drifting right, pointing at where the next page is.
    /// It is a hint, not a target; the gesture is the whole screen.
    private struct SwipeHint: View {
        let text: String
        @Environment(\.accessibilityReduceMotion) private var reduceMotion

        var body: some View {
            HStack(spacing: 8) {
                Text(text)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(Palette.ink.opacity(0.8))
                TimelineView(.animation(paused: reduceMotion)) { context in
                    let t = context.date.timeIntervalSinceReferenceDate
                    HStack(spacing: -1) {
                        ForEach(0..<3) { i in
                            Image(systemName: "chevron.right")
                                .font(.system(size: 14, weight: .bold))
                                .opacity(reduceMotion ? 0.8 : Self.pulse(t, index: i))
                        }
                    }
                    // Ink, not the accent: the nearest bands are accent blue,
                    // and a blue crest over blue waves could not be seen.
                    .foregroundStyle(Palette.ink)
                }
            }
            .padding(.vertical, 10)
        }

        /// A crest that travels left to right across the three chevrons,
        /// once every 1.6s.
        private static func pulse(_ t: TimeInterval, index: Int) -> Double {
            let phase = (t / 1.6).truncatingRemainder(dividingBy: 1) * 4 - Double(index)
            return 0.25 + 0.75 * max(0, 1 - abs(phase - 0.5) * 1.6)
        }
    }
#endif

/// The scene's own backdrop color, so the scrim behind the copy melts into it
/// rather than tinting it.
private enum IntroNight {
    static let backdrop = SquelchSceneView.backdrop
}

/// The squelch scene with the scrims that keep copy legible over it. Shared by
/// the intro and the sign-in gate after it, so leaving the intro changes the
/// words on screen and nothing behind them.
struct IntroSceneBackdrop: View {
    let engaged: Bool
    let wide: Bool

    var body: some View {
        ZStack {
            SquelchSceneView(engaged: engaged)
                .ignoresSafeArea()
            LinearGradient(
                stops: [
                    .init(color: IntroNight.backdrop.opacity(0.92), location: 0),
                    .init(color: IntroNight.backdrop.opacity(0.6), location: wide ? 0.32 : 0.4),
                    .init(color: .clear, location: wide ? 0.62 : 0.75),
                ],
                startPoint: wide ? .leading : .top,
                endPoint: wide ? .trailing : .bottom)
                .ignoresSafeArea()
                .allowsHitTesting(false)
            // Keeps the controls legible over the nearest, brightest bands.
            LinearGradient(
                colors: [.clear, IntroNight.backdrop.opacity(0.85)],
                startPoint: UnitPoint(x: 0.5, y: 0.72), endPoint: .bottom)
                .ignoresSafeArea()
                .allowsHitTesting(false)
        }
    }
}

/// The backdrop when Metal is unavailable: an accent glow from the trailing edge.
struct IntroFallbackGlow: View {
    var body: some View {
        RadialGradient(
            colors: [Palette.accentSoft.opacity(0.65), .clear],
            center: .trailing, startRadius: 10, endRadius: 650)
            .allowsHitTesting(false)
    }
}

/// The mark and the name, as the intro's masthead sets them.
struct IntroWordmark: View {
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(spacing: 9) {
            if let mark = colorScheme == .dark ? IntroBrand.lightMark : IntroBrand.mark {
                mark.resizable()
                    .scaledToFit()
                    .frame(width: 42, height: 24)
                    .accessibilityHidden(true)
            }
            Text("passband")
                .font(Typo.serif(24, weight: .medium))
        }
        .foregroundStyle(Palette.ink)
    }
}

/// The intro's stage for the screens after it: the closed squelch behind, the
/// wordmark up top, and one column of content on the scrimmed side. Every
/// first-run step past the intro sits on this, so moving between them changes
/// the column and nothing else.
///
/// The column is centered in a scroll view rather than between spacers: a long
/// form (an error, an open disclosure) in a short window has to scroll, and
/// spacers inside a scroll view never grow.
struct IntroStage<Content: View>: View {
    var columnWidth: CGFloat = 440
    @ViewBuilder var content: (_ wide: Bool) -> Content

    var body: some View {
        GeometryReader { geometry in
            let wide = geometry.size.width >= 820
            let inset: CGFloat = wide ? 44 : 24
            ZStack(alignment: .topLeading) {
                if SquelchScene.shared.isUnavailable {
                    IntroFallbackGlow()
                } else {
                    // Engaged: the intro's last beat left the squelch closed,
                    // and every step after it picks up exactly there.
                    IntroSceneBackdrop(engaged: true, wide: wide)
                }
                ScrollView {
                    content(wide)
                        .frame(maxWidth: wide ? columnWidth : .infinity, alignment: .leading)
                        .padding(.horizontal, inset)
                        // Clears the wordmark above and the corner toggle below.
                        .padding(.vertical, inset + 56)
                        .frame(maxWidth: .infinity, minHeight: geometry.size.height, alignment: .leading)
                }
                .scrollBounceBehavior(.basedOnSize)
                IntroWordmark()
                    .modifier(IntroEntrance(delay: 0, distance: 4))
                    .padding(inset)
            }
        }
    }
}

/// A single quiet entrance per view identity. No timers or repeating motion;
/// leaving a beat cancels its task, and Reduce Motion renders it immediately.
struct IntroEntrance: ViewModifier {
    let delay: Double
    var distance: CGFloat = 7

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var appeared = false

    func body(content: Content) -> some View {
        content
            .opacity(reduceMotion || appeared ? 1 : 0)
            .offset(y: reduceMotion || appeared ? 0 : distance)
            .animation(reduceMotion ? nil : .easeOut(duration: 0.48).delay(delay), value: appeared)
            .task {
                await Task.yield()
                guard !Task.isCancelled else { return }
                appeared = true
            }
    }
}

/// Verbatim transparent exports from brand/png, with ink and lit variants.
/// Native image decoding works in the CLI Mac bundle and the iOS bundle.
@MainActor
private enum IntroBrand {
    static let mark = load("passband-mark")
    static let lightMark = load("passband-mark-light")

    private static func load(_ name: String) -> Image? {
        let url = Bundle.main.url(forResource: name, withExtension: "png", subdirectory: "Brand")
            ?? Bundle.main.url(forResource: name, withExtension: "png", subdirectory: "Resources/Brand")
        guard let url, let data = try? Data(contentsOf: url),
              let nativeImage = PlatformImage(data: data) else { return nil }
        return Image(platformImage: nativeImage)
    }
}

private struct IntroMail: Identifiable {
    let id: Int
    let sender: String
    let subject: String
    let destination: String
    let symbol: String
    let color: Color

    static let examples: [IntroMail] = [
        .init(id: 0, sender: "Jamie", subject: "A quick decision before Friday",
              destination: "Needs you", symbol: "arrow.turn.up.left", color: Palette.warn),
        .init(id: 1, sender: "Your neighborhood café", subject: "Your receipt. Thanks for stopping by.",
              destination: "Receipts", symbol: "tray", color: Palette.inkDim),
        .init(id: 2, sender: "The Sunday Edit", subject: "A few things we thought you’d like",
              destination: "For later", symbol: "book", color: Palette.inkDim),
    ]
}

private struct IntroMailCard: View {
    let mail: IntroMail
    let organized: Bool

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: organized ? mail.symbol : "envelope")
                .font(.system(size: 16, weight: .medium))
                .foregroundStyle(organized ? mail.color : Palette.inkDim)
                .frame(width: 35, height: 35)
                .background((organized ? mail.color : Palette.inkDim).opacity(0.09), in: RoundedRectangle(cornerRadius: 11))
            VStack(alignment: .leading, spacing: 5) {
                HStack {
                    Text(mail.sender)
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                    Spacer(minLength: 4)
                    if organized {
                        Text(mail.destination)
                            .font(.system(size: 10, weight: .medium))
                            .foregroundStyle(mail.color)
                    }
                }
                Text(mail.subject)
                    .font(.system(size: 11))
                    .foregroundStyle(Palette.inkDim)
                    .lineLimit(1)
            }
        }
        .padding(16)
        .frame(height: 76)
        .background(Color(light: 0xF7FAFD, dark: 0x293747), in: RoundedRectangle(cornerRadius: 17))
        .overlay(RoundedRectangle(cornerRadius: 17).strokeBorder(Palette.hairlineStrong, lineWidth: 0.75))
        .shadow(color: .black.opacity(organized ? 0.07 : 0.12), radius: organized ? 12 : 20, y: 8)
    }
}
