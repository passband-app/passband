// SIDEBAR — the slim icon rail: Sitrep / Emails / Auth on the digit keys, with
// Calendar second when it is offered (2, and every later page one higher; see
// `MainView.railViews`). Rules and Groups left for Settings. Update + Usage +
// Settings sit below a divider, out of that sequence so adding one never
// renumbers it. The discovery
// ring sits just ABOVE that divider, alone after the spacer: a real destination
// with no number key, its RailButton drawing the ring where the others draw a
// symbol. ONE long-lived pane behind the icons is the selector, moved by
// GEOMETRY not identity: per-icon glass drags the glyph along and
// swallows clicks, and a GlassEffectContainer would hoist the pane over it.

import SwiftUI

/// The rail's coordinate space. FILE-SCOPE, not a static on the view: the
/// buttons read it inside `onGeometryChange`'s Sendable closure, where a static
/// on a `@MainActor` View is main-actor isolated.
private let railSpace = "sidebar-rail"

struct SidebarRail: View {
    @Environment(AppStore.self) private var store
    @Environment(\.colorScheme) private var colorScheme
    let namespace: Namespace.ID

    /// Where each icon sits in the rail's own space, written by the buttons as
    /// they lay out, so the selector can be placed over the active one.
    @State private var slots: [MainView: CGRect] = [:]
    /// True for the span of a slide, so the pane can firm up while it moves.
    @State private var traveling = false
    @State private var updateHovering = false
    /// Bumped once when the update tile appears, for its single bounce.
    @State private var updateArrived = 0
    @ObservedObject private var updater = Updater.shared
    @State private var accountDeniedAttempts = 0

    /// THE TILE AND THE RAIL HOLD STILL ACROSS DENSITY. The glyphs grow with
    /// the type and the gaps between tiles open with the ladder, but a 17pt
    /// symbol already sits in a 44x36 tile with room to spare, and the rail's
    /// width is spoken for by every surface that insets past it (the reader,
    /// the expanded composer, the tour). Comfortable is about room between
    /// things; the rail was never where the crowding was.
    static let railWidth: CGFloat = 60
    static let iconWidth: CGFloat = 44
    static let iconHeight: CGFloat = 36
    static let selectorRadius: CGFloat = 11
    /// The space between a tile and the rail's edge, (60 - 44) / 2.
    static let sideGutter: CGFloat = (railWidth - iconWidth) / 2

    /// How long the pane takes to cross — short enough to read as a response to
    /// the click rather than a thing you wait out.
    private static let travel: Animation = .smooth(duration: 0.32)
    /// Tint the pane carries at rest, and the extra it picks up mid-travel.
    private static let restTint: Double = 0.26
    private static let travelTint: Double = 0.16

    var body: some View {
        railStack
            .coordinateSpace(.named(railSpace))
            // BEHIND the icons, and ordered before the rail material below so
            // that material stays further back still.
            .background(alignment: .topLeading) { selector }
            // WHAT MAKES IT SLIDE, and only for a pointer route: the travel
            // reads as the answer to a continuous gesture, while a digit is
            // instantaneous and the same travel would only delay it. Scoped HERE
            // rather than `withAnimation` at the call site — the main content
            // swap hangs off the same property and would animate too.
            .animation(store.routeWasPointer ? Self.travel : nil, value: store.activeView)
            .onChange(of: store.activeView) { _, _ in
                guard store.routeWasPointer else { return }
                // Firmer in flight: the pane gains opacity while it crosses and
                // gives it back on arrival, rising faster than it falls so the
                // weight sits at the start of the travel where the eye is.
                withAnimation(.easeOut(duration: 0.10)) { traveling = true }
                Task {
                    try? await Task.sleep(for: .seconds(0.12))
                    withAnimation(.easeIn(duration: 0.20)) { traveling = false }
                }
            }
            .background { railMaterial }
    }

    /// The travelling pane: absent only until the first layout pass reports a
    /// slot, then ONE view for the life of the rail — which is what lets its
    /// offset animate instead of it being rebuilt somewhere else.
    @ViewBuilder
    private var selector: some View {
        if let slot = slots[store.activeView] {
            Color.clear
                .frame(width: slot.width, height: slot.height)
                .glassEffect(
                    .regular.tint(Palette.accent.opacity(Self.restTint)),
                    in: .rect(cornerRadius: Self.selectorRadius, style: .continuous)
                )
                // The travel tint is an OVERLAY, not a change to the Glass value:
                // `Glass` is not animatable, so tinting the material itself would
                // jump. A colour's opacity interpolates.
                .overlay {
                    RoundedRectangle(cornerRadius: Self.selectorRadius, style: .continuous)
                        .fill(Palette.accent.opacity(traveling ? Self.travelTint : 0))
                }
                // NOT `.interactive()`: that material takes the hits it tracks,
                // and this pane sits over whichever button is active, so an
                // interactive selector eats that tab's clicks.
                .allowsHitTesting(false)
                .offset(x: slot.minX, y: slot.minY)
        }
    }

    private var railStack: some View {
            VStack(spacing: Space.s6) {
                // NO TOP SPACER: the clearance is the top bar's, applied to the
                // whole stack below. The first icon starts UNDER the page header
                // beside it rather than on its line — the rail is what the bar
                // runs above, so an icon level with the title would be an icon
                // sitting in the bar.
                ForEach(MainView.railViews(calendar: store.calendarPageAvailable), id: \.view) { row in
                    let view = row.view
                    RailButton(
                        view: view, keyHint: row.key, active: store.activeView == view,
                        showRings: view == .auth, onSlot: { slots[view] = $0 })
                }

                Spacer(minLength: Space.s8)

                // WAYS THROUGH YOUR MAIL: the discovery ring, alone between the
                // spacer and the divider. It is about how you use the five
                // destinations above, not about your account like the group
                // below, so it sits on the line between them. A RailButton
                // like any other, so the selector parks on it when its page
                // is up; the divider's own vertical padding is the gap under
                // it, the same gap the bottom group gets above.
                RailButton(
                    view: .discovery, keyHint: nil, active: store.activeView == .discovery,
                    showRings: false, onSlot: { slots[.discovery] = $0 }
                ) { _ in DiscoveryRailGlyph() }
                .accessibilityValue(DiscoveryRailGlyph.spokenValue)

                Rectangle()
                    .fill(Palette.hairline)
                    .frame(width: 22, height: 0.5)
                    .padding(.vertical, Space.s4)

                updateButton

                ForEach(MainView.bottomViews, id: \.self) { view in
                    RailButton(
                        view: view, keyHint: nil, active: store.activeView == view,
                        showRings: false, onSlot: { slots[view] = $0 })
                }

                accountBadge
            }
            // The app draws from the window's true top edge now, so this is the
            // rail's whole clearance for the title strip — not a nudge on top of
            // a safe area that no longer applies.
            //
            // FULLSCREEN HAS NO STRIP to clear (see `railMaterial`), so the
            // stack goes to the corner: Sitrep sits the same gutter from the top
            // as the tiles sit from the rail's sides, and nothing above it
            // reads as a margin waiting for buttons that are not there.
            .padding(.top, WindowState.shared.isFullscreen ? Self.sideGutter : TopBar.height + Space.s10)
            .padding(.bottom, Space.s10)
            .frame(width: Self.railWidth)
            .frame(maxHeight: .infinity)
            .ignoresSafeArea(edges: .top)
    }

    /// A NEWER PASSBAND IS WAITING. First in the bottom group, so it is the
    /// first thing there the eye lands on, and present for as long as a version
    /// is pending: Later hides the card, never this. It goes when the app
    /// relaunches into the update.
    ///
    /// Accent rather than a red dot: red in this rail means Auth wants an
    /// answer now, and an update never does. One bounce on arrival, then still;
    /// a rail tile that keeps moving is a tile you learn to hate.
    ///
    /// The click brings the card back rather than restarting: a relaunch from a
    /// 44pt tile is one stray click away from quitting someone mid-email, and
    /// the card says what Restart does before it is pressed.
    @ViewBuilder
    private var updateButton: some View {
        if let version = updater.pending, !RehearsalMode.isEnabled {
            Button { updater.showAgain() } label: {
                Image(systemName: "arrow.down.circle")
                    .font(Typo.ui(17, weight: .medium))
                    .foregroundStyle(Palette.accent)
                    .symbolEffect(.bounce, value: updateArrived)
                    .frame(width: Self.iconWidth, height: Self.iconHeight)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .background {
                RoundedRectangle(cornerRadius: Self.selectorRadius, style: .continuous)
                    .fill(Palette.hairline.opacity(updateHovering ? 0.7 : 0))
                    .frame(width: Self.iconWidth, height: Self.iconHeight)
            }
            .onHover { updateHovering = $0 }
            .onAppear { updateArrived += 1 }
            .help("Passband \(version) is available")
            .accessibilityLabel("Passband \(version) is available")
            .transition(.scale(scale: 0.6).combined(with: .opacity))
        }
    }

    /// WHICH MAILBOX YOU ARE IN, at the foot of the rail — and the fastest way
    /// to another one. Absent with a single account: a switcher between one
    /// thing is chrome, and the rail's whole argument is that it holds only
    /// what earns its 60 points.
    ///
    /// Deliberately NOT a `RailButton`: it routes nowhere, so it takes no
    /// selector pane and reports no slot.
    @ViewBuilder
    private var accountBadge: some View {
        let manager = AccountManager.shared
        if manager.accounts.count > 1 {
            if RehearsalMode.isEnabled {
                Button { accountDeniedAttempts += 1 } label: { accountMark }
                    .buttonStyle(.plain)
                    .frame(width: Self.iconWidth, height: Self.iconHeight)
                    .help("Account switching is available after onboarding")
                    .accessibilityLabel("switch account")
                    .accessibilityHint("Available after onboarding.")
                    .modifier(PracticeRailFeedback(attempts: accountDeniedAttempts))
            } else {
            Menu {
                // No `.keyboardShortcut` on these, deliberately: the Accounts
                // menu in the menu bar owns the ⌘numbers, and a second
                // registration of the same chord is a conflict, not a shortcut.
                ForEach(manager.accounts) { account in
                    accountMenuRow(account, isActive: account.id == manager.activeId)
                }
                Divider()
                Button("Add Account…") { store.addAccountSheetOpen = true }
            } label: {
                accountMark
            }
            // The default menu chrome is a bordered well with a chevron, which
            // in a 60pt icon rail reads as a broken button. The badge IS the
            // affordance.
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .frame(width: Self.iconWidth, height: Self.iconHeight)
            .help(manager.active.map { "account: \($0.displayName)" } ?? "accounts")
            .accessibilityLabel("switch account")
            }
        }
    }

    /// The live account's profile photo, or its initial when none was imported.
    ///
    /// THE PHOTO GOES IN AS A BITMAP. This view is a `Menu`'s label, and AppKit
    /// draws an image in a menu button at its natural size with no regard for
    /// a SwiftUI frame or clip shape: a resizable photo here came out as a
    /// 256pt square over the bottom of the rail. A bitmap that is already a
    /// 26pt circle has nothing left to get wrong. The initial stays a real
    /// view, as it always was: text in a menu label is laid out by SwiftUI.
    ///
    /// THE ACCOUNT BEING SWITCHED TO, while a switch is asking after it: the
    /// press is answered here at once, whatever the daemon takes. A refused
    /// switch puts the live account's mark back, beside the toast saying why.
    @ViewBuilder
    private var accountMark: some View {
        let manager = AccountManager.shared
        let active = manager.accounts.first { $0.id == store.switchingTo } ?? manager.active
        if let active, AccountPhotos.shared.image(for: active.id) != nil,
            let bitmap = markBitmap(active, size: 26, font: Typo.ui(13, weight: .semibold))
        {
            Image(nsImage: bitmap).renderingMode(.original)
        } else {
            AccountMark(account: active, font: Typo.ui(13, weight: .semibold))
        }
    }

    /// One row of the switcher menu: the account's mark beside its name, with
    /// the menu's own checkmark on the live one.
    ///
    /// A `Toggle`, because that is how a SwiftUI menu spells a checked item:
    /// the tick lands in the state column AppKit reserves for it, which leaves
    /// the image slot free for the face. Every row gets a mark, photo or
    /// initial, so the names line up whether or not each account has a picture.
    private func accountMenuRow(_ account: AccountRecord, isActive: Bool) -> some View {
        Toggle(
            isOn: Binding(
                get: { isActive },
                // Either direction is "go to this account": `switchTo` already
                // declines the one that is live.
                set: { _ in Task { await AccountManager.shared.switchTo(account.id) } })
        ) {
            Label {
                Text(account.displayName)
            } icon: {
                if let mark = markBitmap(account, size: 18, font: Typo.ui(10, weight: .semibold)) {
                    Image(nsImage: mark).renderingMode(.original)
                }
            }
        }
    }

    /// An account's mark as a bitmap, for the two places AppKit draws it
    /// instead of SwiftUI (see `accountMark`). Rendering the same `AccountMark`
    /// the rest of the app wears is what keeps them from drifting apart; the
    /// color scheme is handed over because a renderer has no window to inherit
    /// one from and would draw the light palette into a dark rail.
    private func markBitmap(_ account: AccountRecord, size: CGFloat, font: Font) -> NSImage? {
        // Read HERE, in the rail's own body, and not only inside the renderer:
        // a renderer's view graph is not this view's, so a photo that arrived
        // for a row in the menu would otherwise never redraw it.
        let photo = AccountPhotos.shared.image(for: account.id)
        // `body` runs on every hover and slot change, and a rasterisation per
        // account per pass is not free. The key is everything the bitmap is
        // drawn from; the photo by identity, since a new one is a new object.
        let key =
            "\(account.id)|\(photo.map { "\(ObjectIdentifier($0).hashValue)" } ?? account.initial)|\(size)|\(colorScheme)"
            as NSString
        if let hit = Self.markCache.object(forKey: key) { return hit }
        let renderer = ImageRenderer(
            content: AccountMark(account: account, size: size, font: font)
                .environment(\.colorScheme, colorScheme))
        renderer.scale = NSScreen.main?.backingScaleFactor ?? 2
        guard let image = renderer.nsImage else { return nil }
        Self.markCache.setObject(image, forKey: key)
        return image
    }

    private static let markCache: NSCache<NSString, NSImage> = {
        let cache = NSCache<NSString, NSImage>()
        cache.countLimit = 64
        return cache
    }()

    /// The one rounded corner the rail owns: the top-trailing shoulder, where
    /// the material tucks under the top bar. The other three corners meet
    /// window edges and stay square.
    private static let shoulderRadius: CGFloat = 12

    private var railMaterial: some View {
            // The most translucent surface in the window, but not arbitrarily
            // thin: it must hold its value over any wallpaper or its icons stop
            // being legible. It runs up into the top safe area and stops under
            // the TOP BAR — the same line the page's own header ends on, so the
            // rail's top edge and that header's rule read as one rule across the
            // window, dipping only for the shoulder. Above it is the strip the
            // traffic lights live in, left unpainted: material running up BEHIND
            // the dots reads as the dots clipping the rail.
            //
            // FULLSCREEN HAS NO TRAFFIC LIGHTS — the strip they live in is not
            // on screen, so the rail runs to the true top instead of reserving
            // it, and the shoulder goes square: a corner on a window edge, like
            // the other three.
            let fullscreen = WindowState.shared.isFullscreen
            let radius = fullscreen ? 0 : Self.shoulderRadius
            let shape = UnevenRoundedRectangle(
                topLeadingRadius: 0, bottomLeadingRadius: 0,
                bottomTrailingRadius: 0, topTrailingRadius: radius,
                style: .continuous)
            return shape
                .fill(.thinMaterial)
                .overlay(shape.fill(Palette.glassTint.opacity(0.5)))
                // The hairline follows the silhouette — trailing edge, shoulder,
                // top — where a straight trailing rule would cut the curve.
                .overlay(
                    RailEdge(radius: radius)
                        .stroke(Palette.hairline, lineWidth: 0.5)
                )
                .padding(.top, fullscreen ? 0 : TopBar.height)
                .ignoresSafeArea(edges: [.top, .bottom])
    }
}

/// The rail material's visible edge: up the trailing side, around the
/// top-trailing shoulder, and along the top to the window's edge. The leading
/// and bottom edges sit on window edges and draw nothing.
private struct RailEdge: Shape {
    var radius: CGFloat

    func path(in rect: CGRect) -> Path {
        var p = Path()
        p.move(to: CGPoint(x: rect.maxX, y: rect.maxY))
        p.addLine(to: CGPoint(x: rect.maxX, y: rect.minY + radius))
        // Square (fullscreen): the top edge IS the window's edge, and window
        // edges draw nothing.
        guard radius > 0 else { return p }
        p.addQuadCurve(
            to: CGPoint(x: rect.maxX - radius, y: rect.minY),
            control: CGPoint(x: rect.maxX, y: rect.minY))
        p.addLine(to: CGPoint(x: rect.minX, y: rect.minY))
        return p
    }
}

/// One rail tile. The glyph is a parameter, an SF Symbol for every
/// destination but discovery, which draws its ring: everything else about a
/// tile (the target, the wash, the slot, the refusal) is the same thing.
private struct RailButton<Icon: View>: View {
    @Environment(AppStore.self) private var store
    let view: MainView
    /// The key the tooltip teaches: a digit, or nothing.
    let keyHint: String?
    let active: Bool
    let showRings: Bool
    /// Where this icon landed, in the rail's space, so the rail can park the
    /// selector on it.
    let onSlot: (CGRect) -> Void
    /// The glyph, given whether the tile is the active one.
    @ViewBuilder let icon: (Bool) -> Icon

    @State private var hovering = false
    @State private var deniedAttempts = 0

    var body: some View {
        Button {
            guard !RehearsalMode.isEnabled else {
                deniedAttempts += 1
                return
            }
            store.setView(view, viaPointer: true)
        } label: {
            ZStack {
                icon(active)
                    .frame(width: SidebarRail.iconWidth, height: SidebarRail.iconHeight)
                    // The rail's selector is placed from this. Nothing about the
                    // glyph changes with selection except weight and colour, so
                    // it never moves and its subtree is never rebuilt.
                    .onGeometryChange(for: CGRect.self) {
                        $0.frame(in: .named(railSpace))
                    } action: { onSlot($0) }

                if showRings {
                    AuthRingsOverlay()
                    AuthDecisionBadge()
                }
            }
            // THE WHOLE TILE IS THE TARGET. Without this the hit region is the
            // GLYPH, and every rail symbol is an OUTLINE — a click through the
            // middle of the envelope lands in a hole and the tab silently ignores
            // it. The hover wash hides the bug: `.onHover` sits outside the
            // button and lights up the full tile either way.
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background {
            // Opacity rather than an `if`: a conditional is two view identities,
            // and swapping them on hover throws away the button's subtree.
            RoundedRectangle(cornerRadius: SidebarRail.selectorRadius, style: .continuous)
                .fill(Palette.hairline.opacity(hovering && !active ? 0.7 : 0))
                .frame(width: SidebarRail.iconWidth, height: SidebarRail.iconHeight)
        }
        .onHover { hovering = $0 }
        .help(keyHint.map { "\(view.label) · \($0)" } ?? view.label)
        .accessibilityLabel(view.label)
        .accessibilityHint(RehearsalMode.isEnabled ? "Available after onboarding." : "")
        .modifier(PracticeRailFeedback(attempts: deniedAttempts))
    }
}

extension RailButton where Icon == RailSymbol {
    init(view: MainView, keyHint: String?, active: Bool, showRings: Bool, onSlot: @escaping (CGRect) -> Void) {
        self.init(
            view: view, keyHint: keyHint, active: active, showRings: showRings, onSlot: onSlot
        ) { RailSymbol(symbol: view.symbol, active: $0) }
    }
}

/// The ordinary tile glyph. Nothing about it changes with selection except
/// weight and colour.
private struct RailSymbol: View {
    let symbol: String
    let active: Bool

    var body: some View {
        Image(systemName: symbol)
            .font(Typo.ui(17, weight: active ? .semibold : .regular))
            .foregroundStyle(active ? Palette.accent : Palette.inkDim)
    }
}

/// Keep practice navigation recognizable and responsive without leaving the
/// lesson. With Reduce Motion the same refusal is a short, stationary outline.
struct PracticeRailFeedback: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let attempts: Int
    @State private var highlighted = false

    func body(content: Content) -> some View {
        content
            .overlay {
                RoundedRectangle(cornerRadius: SidebarRail.selectorRadius)
                    .strokeBorder(Palette.accent.opacity(highlighted ? 0.65 : 0), lineWidth: 1.5)
                    .allowsHitTesting(false)
            }
            .modifier(PracticeRailShake(progress: reduceMotion ? 0 : CGFloat(attempts)))
            .animation(reduceMotion ? nil : .linear(duration: 0.28), value: attempts)
            .task(id: attempts) {
                guard attempts > 0, reduceMotion else { return }
                highlighted = true
                do { try await Task.sleep(for: .milliseconds(350)) }
                catch { return }
                highlighted = false
            }
    }
}

private struct PracticeRailShake: GeometryEffect {
    var progress: CGFloat
    var animatableData: CGFloat {
        get { progress }
        set { progress = newValue }
    }

    func effectValue(size: CGSize) -> ProjectionTransform {
        ProjectionTransform(CGAffineTransform(translationX: 3 * sin(progress * .pi * 4), y: 0))
    }
}

/// OPEN AUTH DECISIONS: how many questions are still waiting on "was this
/// you?". Filtered by `AuthDecisions.isOpen` and grouped by `AuthPolicy.cards`,
/// the same two the Auth page's rail lists by, so the number here is the number
/// of decisions there, counted the same way the page's header counts them
/// (a burst of new-device alerts is one, laid out for review or not). Danger-toned while a
/// critical change is among them. Nothing drawn at zero: an empty badge is noise
/// on the one icon that should only ever get loud for a reason.
private struct AuthDecisionBadge: View {
    @Environment(AppStore.self) private var store
    private var decisions = AuthDecisions.shared

    private var open: [SealedMeta] { store.sitrep.sealed.filter { decisions.isOpen($0) } }

    var body: some View {
        let open = open
        let count = open.isEmpty ? 0 : AuthPolicy.cards(open).count
        let critical = open.contains(where: \.isCritical)
        if count > 0 {
            Text(count > 9 ? "9+" : "\(count)")
                .font(Typo.num(9, weight: .bold))
                .foregroundStyle(.white)
                .padding(.horizontal, 4)
                .frame(minWidth: 15, minHeight: 15)
                .background(Capsule().fill(critical ? Palette.danger : Palette.lock))
                .frame(
                    width: SidebarRail.iconWidth, height: SidebarRail.iconHeight,
                    alignment: .topTrailing
                )
                .offset(x: -3, y: -1)
                .allowsHitTesting(false)
                .accessibilityLabel(count == 1 ? "1 decision open" : "\(count) decisions open")
        }
    }
}

/// AUTH COUNTDOWN RINGS — a 60s sweep over the auth rail icon per active
/// AuthRing in the store; each removes itself when its sweep completes.
private struct AuthRingsOverlay: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        ForEach(store.authRings) { ring in
            AuthRingView(ring: ring)
        }
    }
}

private struct AuthRingView: View {
    @Environment(AppStore.self) private var store
    let ring: AuthRing
    @State private var progress: CGFloat = 1

    private static let size: CGFloat = 34

    var body: some View {
        Circle()
            .trim(from: 0, to: progress)
            .stroke(Palette.lock, style: StrokeStyle(lineWidth: 2, lineCap: .round))
            .rotationEffect(.degrees(-90))
            .frame(width: Self.size, height: Self.size)
            .allowsHitTesting(false)
            .task {
                // A late mount starts partway, so the ring still finishes ~60s
                // after it was armed and never over-runs.
                let elapsed = Date().timeIntervalSince(ring.startedAt)
                let remaining = max(0, ringSeconds - elapsed)
                guard remaining > 0 else {
                    store.expireAuthRing(ring.id)
                    return
                }
                progress = CGFloat(remaining / ringSeconds)
                withAnimation(.linear(duration: remaining)) { progress = 0 }
                try? await Task.sleep(for: .seconds(remaining))
                store.expireAuthRing(ring.id)
            }
    }
}
