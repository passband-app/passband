// THE PHONE'S WHAT'S NEW: the whole screen, one feature a page, paged by hand.
//
// NOT THE MAC'S CARD ON A SMALLER SCREEN. The Mac's is a modal over a window
// (Views/WhatsNewCard.swift): a glass card, a scrim, Next and Back, and a
// record of every surface's bullets at the end, because a Mac has the room and
// its reader has a daemon to wonder about. A phone has one screen and a thumb.
// So this takes the screen, the way the cold open does
// (Views/OnboardingIntroView.swift), and is turned the way the cold open is: a
// swipe to the left moves on, a swipe to the right goes back, no button
// anywhere, and the same line at the bottom saying so. Somebody who met
// Passband through that intro already knows how this works.
//
// A PAGE IS A PICTURE AND TWO LINES. The top of the screen is a night screen
// hung from the top edge, in both themes, for the reason the Mac's vignettes
// are night in both: it is a screen inside the app, and a dark screen over
// paper reads as a product shot. On it the feature's vignette plays
// (PhoneVignettes.swift), the gesture done by a thumb. Under it, on the app's
// own canvas, the feature's name in the serif and one sentence on how. The
// picture crossfades between pages, one screen changing channel; the words
// turn like a page, in from the side the walk is heading.
//
// THE LAST PAGE is what did not need a page: the brand's own light where the
// vignettes were, and a short list. There is no record of the Mac's or the
// daemon's notes here. This walk is about the phone in your hand, and its
// table (Lib/PhoneReleaseNotes.swift) is written that way.
//
// THE WAY OUT IS ALWAYS THERE, top right, and off the end: a swipe past the
// last page closes it, as the intro's last swipe begins. Closing stamps what
// was shown (Model/WhatsNew.swift), from either door.
//
// THE STATUS BAR STEPS ASIDE while this is up. The night runs to the top edge
// in both themes, and a light theme's dark clock would sit on it unreadable.
// It is a few seconds of a brand's page, not a place anyone works.

import SwiftUI

struct WhatsNewWalk: View {
    /// What the walk draws, newest release first. Handed in rather than read
    /// off the store: closing empties the store's copy at once, and the walk
    /// is still on screen, fading, for a moment after that.
    let notes: [PhoneReleaseNote]

    @Environment(AppStore.self) private var store
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase

    /// Where the walk is: an index into `pages`.
    @State private var page: Int
    /// Which way the last move went, so the words leave by the right side.
    @State private var forward = true

    private enum Page {
        case feature(PhoneFeature, PhoneReleaseNote)
        case closing
    }

    /// Every feature of every release shown, newest release first, then the
    /// one closing page.
    private let pages: [Page]

    init(notes: [PhoneReleaseNote], openingPage: Int = 0) {
        self.notes = notes
        let pages = notes.flatMap { note in note.features.map { Page.feature($0, note) } } + [.closing]
        self.pages = pages
        _page = State(initialValue: min(max(0, openingPage), pages.count - 1))
    }

    private var onLast: Bool { page >= pages.count - 1 }
    private var live: Bool { scenePhase == .active }

    /// The cold open's margin, so the wordmark sits where it sat there.
    private static let margin: CGFloat = 24
    private static let shadeRadius: CGFloat = 36

    var body: some View {
        GeometryReader { geometry in
            let safe = geometry.safeAreaInsets
            let height = geometry.size.height + safe.top + safe.bottom
            ZStack(alignment: .top) {
                Palette.canvas
                VStack(spacing: 0) {
                    shade(top: safe.top)
                        .frame(height: shadeHeight(screen: height, width: geometry.size.width))
                    ZStack(alignment: .topLeading) {
                        words
                            .id(page)
                            .transition(pageTurn)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                    .clipped()
                    controls
                        .padding(.horizontal, Self.margin)
                        .padding(.bottom, max(safe.bottom, Space.s12) + Space.s8)
                }
                masthead
                    .padding(.leading, Self.margin)
                    .padding(.trailing, Space.s16)
                    .padding(.top, safe.top + Space.s4)
            }
            .ignoresSafeArea()
        }
        // THE WHOLE SCREEN IS THE CONTROL, as in the cold open. Simultaneous,
        // so the closing page's list keeps its own pan.
        .contentShape(Rectangle())
        .simultaneousGesture(pageSwipe)
        // Each turn is felt, once: a page arriving, not a pattern.
        .sensoryFeedback(.selection, trigger: page)
        .statusBarHidden(true)
        // The closing page's light takes a few seconds to build its renderer
        // and settle; started now, it is lit by the time anyone swipes to it.
        .onAppear { PhosphorScene.whatsNew.warm() }
        .accessibilityAddTraits(.isModal)
        .accessibilityAction(.escape) { close() }
    }

    // MARK: - turning

    private func close() { store.whatsNew.dismiss() }

    /// A swipe to the left: the next page, and off the last one, out.
    private func advance() {
        if onLast { close() } else { go(to: page + 1) }
    }

    private func go(to target: Int) {
        let clamped = min(max(target, 0), pages.count - 1)
        guard clamped != page else { return }
        // The direction lands a beat BEFORE the page does, as on the Mac's
        // card: the words leaving take the transition they were last drawn
        // with, so a back after a forward would otherwise leave the wrong way.
        forward = clamped > page
        DispatchQueue.main.async {
            withAnimation(reduceMotion ? .easeInOut(duration: 0.2) : .smooth(duration: 0.42)) { page = clamped }
        }
    }

    /// The cold open's gesture, to the number: horizontal, decisive, and
    /// judged on where the fling was headed as much as where it stopped. A
    /// mostly vertical drag is not a page turn.
    private var pageSwipe: some Gesture {
        DragGesture(minimumDistance: 24)
            .onEnded { value in
                let dx = value.translation.width, dy = value.translation.height
                let aimed = value.predictedEndTranslation.width
                guard abs(dx) > abs(dy) * 1.2 else { return }
                if dx < -50 || aimed < -140 {
                    advance()
                } else if dx > 50 || aimed > 140 {
                    go(to: page - 1)
                }
            }
    }

    /// In from the side the walk is heading, out the other, with a fade.
    /// Reduce Motion keeps only the fade.
    private var pageTurn: AnyTransition {
        guard !reduceMotion else { return .opacity }
        let shift: CGFloat = 44
        return .asymmetric(
            insertion: .offset(x: forward ? shift : -shift).combined(with: .opacity),
            removal: .offset(x: forward ? -shift : shift).combined(with: .opacity))
    }

    // MARK: - the night screen

    /// Most of a feature's page; a strip of the closing one, which has a list
    /// to fit under it. Capped by the width so a tall phone does not grow a
    /// picture taller than its scene can fill.
    private func shadeHeight(screen: CGFloat, width: CGFloat) -> CGFloat {
        if case .closing = pages[page] { return (screen * 0.33).rounded() }
        return min(screen * 0.57, width * 1.28).rounded()
    }

    /// The picture, hung from the top edge and round at the bottom. No border:
    /// three of its sides are the screen's, and a line along the fourth alone
    /// would be a highlight on one edge.
    private func shade(top: CGFloat) -> some View {
        ZStack {
            Group {
                switch pages[page] {
                case .feature(let feature, _):
                    if let vignette = feature.vignette {
                        VignetteStage(
                            scene: vignette.scene, live: live, fixedTime: Self.pinnedTime, cornerRadius: 0,
                            fitsCanvas: false,
                            // Clear of the wordmark and the way out above it.
                            sceneInsets: EdgeInsets(top: top + 50, leading: 0, bottom: 14, trailing: 0))
                    } else {
                        symbolArt(feature.symbol)
                    }
                case .closing:
                    hero
                }
            }
            .id(page)
            .transition(.opacity)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .clipShape(
            UnevenRoundedRectangle(
                bottomLeadingRadius: Self.shadeRadius, bottomTrailingRadius: Self.shadeRadius,
                style: .continuous)
        )
        .shadow(color: .black.opacity(0.22), radius: 22, y: 8)
        .environment(\.colorScheme, .dark)
        .accessibilityHidden(true)
    }

    /// A feature with no vignette drawn for it: its symbol on the same night.
    private func symbolArt(_ symbol: String) -> some View {
        ZStack {
            LinearGradient(
                colors: [VignettePalette.groundTop, VignettePalette.groundBottom], startPoint: .top,
                endPoint: .bottom)
            Image(systemName: symbol)
                .font(.system(size: 84, weight: .light))
                .foregroundStyle(VignettePalette.phosphorHot)
                .shadow(color: VignettePalette.phosphor.opacity(0.7), radius: 20)
        }
    }

    /// The closing page's picture: the brand's own light, the quiet board's
    /// scene, as the Mac's card wears it.
    private var hero: some View {
        ZStack(alignment: .bottomLeading) {
            Group {
                if PhosphorScene.whatsNew.isUnavailable {
                    IntroSceneBackdrop(engaged: true, wide: false, paused: !live)
                } else {
                    PhosphorBackdrop(paused: !live, scene: .whatsNew)
                }
            }
            .allowsHitTesting(false)

            LinearGradient(colors: [.clear, .black.opacity(0.3)], startPoint: .center, endPoint: .bottom)
                .allowsHitTesting(false)

            if let newest = notes.first {
                Text("NEW IN PASSBAND \(newest.version)")
                    .font(Typo.ui(10, weight: .bold).width(.expanded))
                    .tracking(0.6)
                    .foregroundStyle(.white.opacity(0.92))
                    .padding(.horizontal, Space.s10)
                    .padding(.vertical, Space.s6)
                    .background(.black.opacity(0.32), in: Capsule())
                    .overlay(Capsule().strokeBorder(Color(hex: 0x82BAF5).opacity(0.45), lineWidth: 0.5))
                    .padding(.leading, Self.margin)
                    .padding(.bottom, Space.s20)
            }
        }
    }

    // MARK: - chrome

    /// The wordmark where the cold open sets it, and the way out. Both stand
    /// on the night, so both are drawn for the dark whatever the theme.
    private var masthead: some View {
        HStack {
            IntroWordmark()
                .accessibilityHidden(true)
            Spacer()
            Button(action: close) {
                Image(systemName: "xmark")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Palette.ink.opacity(0.85))
                    .frame(width: 44, height: 44)
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .passbandGlass(.chrome, cornerRadius: 22, tint: nil, interactive: true)
            .accessibilityLabel("Close what's new")
        }
        .environment(\.colorScheme, .dark)
    }

    /// The cold open's foot: where you are on the left, how to move on the
    /// right.
    private var controls: some View {
        HStack(spacing: Space.s16) {
            HStack(spacing: 6) {
                ForEach(pages.indices, id: \.self) { index in
                    Capsule().fill(page == index ? Palette.accent : Palette.hairlineStrong)
                        .frame(width: page == index ? 24 : 8, height: 5)
                }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("What's new, page \(page + 1) of \(pages.count)")
            .animation(reduceMotion ? nil : .easeInOut(duration: 0.28), value: page)

            Spacer(minLength: 0)
            SwipeHint(text: onLast ? "Swipe to finish" : "Swipe for the next")
                // VoiceOver cannot flick the page, so the hint is where that
                // gesture lives for it, as in the cold open.
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(onLast ? "Finish" : "Next")
                .accessibilityAddTraits(.isButton)
                .accessibilityAction { advance() }
                .accessibilityAction(named: "Back") { go(to: page - 1) }
        }
    }

    // MARK: - the words

    @ViewBuilder private var words: some View {
        switch pages[page] {
        case .feature(let feature, let note):
            VStack(alignment: .leading, spacing: 0) {
                eyebrow(note)
                Text(feature.title)
                    .font(Typo.hero(34))
                    .foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, Space.s10)
                    .accessibilityAddTraits(.isHeader)
                Text(feature.detail)
                    .font(Typo.ui(16))
                    .foregroundStyle(Palette.inkDim)
                    .lineSpacing(4)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, Space.s10)
            }
            .padding(.horizontal, Self.margin)
            .padding(.top, Space.s24)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
        case .closing:
            closing
        }
    }

    /// The release's own name and number, on every page, so the walk reads as
    /// one update rather than as four announcements.
    private func eyebrow(_ note: PhoneReleaseNote) -> some View {
        Text("\(note.headline) · \(note.version)")
            .font(Typo.ui(10, weight: .bold).width(.expanded))
            .tracking(0.8)
            .textCase(.uppercase)
            .foregroundStyle(Palette.accent)
            .lineLimit(1)
    }

    /// Everything the walk did not give a page, every release shown in one
    /// list, the same words said once.
    private var asides: [PhoneAside] {
        var said = Set<String>()
        return notes.flatMap(\.asides).filter { said.insert(ReleaseDigest.key($0.text)).inserted }
    }

    private var closing: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                if let newest = notes.first {
                    eyebrow(newest)
                    Text(notes.count == 1 ? "Also in \(newest.version)" : "And the rest")
                        .font(Typo.hero(34))
                        .foregroundStyle(Palette.ink)
                        .padding(.top, Space.s10)
                        .accessibilityAddTraits(.isHeader)
                }
                VStack(alignment: .leading, spacing: Space.s14) {
                    ForEach(asides) { aside in
                        HStack(alignment: .firstTextBaseline, spacing: Space.s12) {
                            Image(systemName: aside.symbol ?? "circle.fill")
                                .font(.system(size: aside.symbol == nil ? 5 : 14, weight: .medium))
                                .foregroundStyle(Palette.accent)
                                .frame(width: 22)
                            Text(aside.text)
                                .font(Typo.ui(15))
                                .foregroundStyle(Palette.inkDim)
                                .lineSpacing(3)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        .accessibilityElement(children: .combine)
                    }
                }
                .padding(.top, Space.s20)
                // The filler every release carries, said once and quietly.
                if let footnote = notes.first?.footnote {
                    Text(footnote)
                        .font(Typo.ui(13))
                        .foregroundStyle(Palette.inkFaint)
                        .padding(.top, Space.s16)
                        .padding(.leading, 22 + Space.s12)
                }
                // WHERE IT LIVES AFTERWARDS: the one place somebody can go
                // looking for this a second time.
                Text("This is under the person icon, in General, whenever you want it again.")
                    .font(Typo.ui(12))
                    .foregroundStyle(Palette.inkFaintest)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, Space.s20)
            }
            .padding(.horizontal, Self.margin)
            .padding(.top, Space.s24)
            .padding(.bottom, Space.s12)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .scrollBounceBehavior(.basedOnSize)
    }

    /// A screenshot's stopped clock; see FixtureStage. Nil in anything shipped.
    private static var pinnedTime: Double? {
        #if DEBUG
            FixtureStage.vignetteTime
        #else
            nil
        #endif
    }
}
