// THE DISCOVERY RING'S WIRING: the rail button, the menu's mount and keys, and
// the found moment's timing. The pictures are DiscoveryRing / DiscoveryMenu /
// DiscoveryVignette, which take every input as a parameter; this file is the
// only one that reads the store, the ledger and the window.
//
// MAC ONLY in v1, like the rest of the folder (see project.yml's iOS excludes).

import SwiftUI

/// What is on screen. Separate from `Discovery` on purpose: the ledger is the
/// model and crosses to iOS; whether a menu is open is this shell's business.
@MainActor
@Observable
final class DiscoveryPresenter {
    static let shared = DiscoveryPresenter()

    /// `PASSBAND_DISCOVERY_OPEN=1` opens the menu at launch, the same posture
    /// as `PASSBAND_FORCE_TOUR`: a launch environment nobody sets by accident.
    /// The menu still waits for the shell (no practice inbox, no tour).
    var menuOpen = ProcessInfo.processInfo.environment["PASSBAND_DISCOVERY_OPEN"] == "1"
    /// The found moment on screen, if any.
    var moment: FeatureSet?
    /// Where the ring sits in `tourSpace`, so the menu and the moment can grow
    /// out of it.
    var ringFrame: CGRect = .zero

    func openMenu() {
        moment = nil
        menuOpen = true
    }

    func closeMenu() { menuOpen = false }
}

// MARK: - the rail button

/// The ring on the rail, just above the bottom group's divider. Borrows
/// `RailButton`'s 44x36 tile and hover wash, but takes no selector pane,
/// reports no slot and routes nowhere: it opens a menu.
///
/// ALWAYS THERE, practice inbox included, and it never retires: all kept is a
/// resting state, not a farewell, and the menu still opens from it. During the
/// tour it refuses like every other rail tile (`PracticeRailFeedback`), so the
/// rail stays one consistent, inert thing while the lesson runs.
struct DiscoveryRailButton: View {
    @Environment(\.controlActiveState) private var activeState
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var hovering = false
    @State private var deniedAttempts = 0

    private var discovery: Discovery { Discovery.shared }
    private var presenter: DiscoveryPresenter { DiscoveryPresenter.shared }

    var body: some View {
        Button {
            guard !RehearsalMode.isEnabled else {
                deniedAttempts += 1
                return
            }
            presenter.menuOpen ? presenter.closeMenu() : presenter.openMenu()
        } label: {
            DiscoveryRing(
                states: FeatureSet.ringStates(discovery.state(of:)),
                fresh: discovery.ledger.fresh,
                diameter: 24,
                breathing: activeState == .key && !reduceMotion)
                .frame(width: SidebarRail.iconWidth, height: SidebarRail.iconHeight)
                // The whole tile is the target: the ring is mostly hole.
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background {
            RoundedRectangle(cornerRadius: SidebarRail.selectorRadius, style: .continuous)
                .fill(Palette.hairline.opacity(hovering || presenter.menuOpen ? 0.7 : 0))
                .frame(width: SidebarRail.iconWidth, height: SidebarRail.iconHeight)
        }
        .onHover { hovering = $0 }
        .onGeometryChange(for: CGRect.self) { $0.frame(in: .named(tourSpace)) } action: {
            presenter.ringFrame = $0
        }
        .help(DiscoveryCopy.ringLabel)
        .accessibilityLabel(DiscoveryCopy.ringLabel)
        .accessibilityValue(FeatureSet.ringDescription(discovery.state(of:)))
        .accessibilityHint(
            RehearsalMode.isEnabled
                ? "Available after onboarding."
                : discovery.ledger.fresh.isEmpty ? "" : "Something new was found.")
        .modifier(PracticeRailFeedback(attempts: deniedAttempts))
    }
}

// MARK: - the layer

/// The menu and the moment, mounted in ActionLayer under its modal palettes.
struct DiscoveryLayer: View {
    @Environment(AppStore.self) private var store
    @Environment(\.controlActiveState) private var activeState
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var discovery: Discovery { Discovery.shared }
    private var presenter: DiscoveryPresenter { DiscoveryPresenter.shared }

    /// How long a moment stays when nobody touches it.
    private static let momentHold: Duration = .seconds(7)
    /// How long the app must have been at rest before a moment may arrive:
    /// long enough that it lands after the action, never on it.
    private static let settle: Duration = .milliseconds(1400)

    var body: some View {
        GeometryReader { geo in
            let origin = geo.frame(in: .named(tourSpace)).origin
            let ring = presenter.ringFrame.offsetBy(dx: -origin.x, dy: -origin.y)
            ZStack(alignment: .topLeading) {
                if let set = presenter.moment {
                    momentCard(set, ring: ring, in: geo.size)
                }
                if showsMenu {
                    menu(ring: ring, in: geo.size)
                }
            }
            .frame(width: geo.size.width, height: geo.size.height, alignment: .topLeading)
        }
        .animation(reduceMotion ? .easeOut(duration: 0.15) : .spring(response: 0.42, dampingFraction: 0.86), value: showsMenu)
        .animation(reduceMotion ? .easeOut(duration: 0.15) : .spring(response: 0.5, dampingFraction: 0.84), value: presenter.moment)
        // LEAVING THE PAGE CLOSES THE MENU. The digits are global and keep
        // firing from under it, and a menu left floating over a page nobody
        // opened it from reads as stuck.
        .onChange(of: store.activeView) { _, _ in presenter.closeMenu() }
        // THE WAIT FOR REST. Re-keyed on everything that can break it, so a
        // reader opening during the settle cancels the sleep and the moment
        // waits for the next quiet stretch.
        .task(id: RestKey(pending: discovery.pendingCelebration, atRest: atRest)) {
            guard let set = discovery.pendingCelebration, atRest else { return }
            do { try await Task.sleep(for: Self.settle) } catch { return }
            presenter.moment = set
            discovery.celebrationShown()
        }
    }

    private var showsMenu: Bool { presenter.menuOpen && !RehearsalMode.isEnabled && !store.tour.active }

    /// AT REST: the board on screen and nothing in flight over it. Undo chips
    /// count as in flight: the find that earned a moment is very often an `e`,
    /// and its undo is still part of that action.
    private var atRest: Bool {
        store.activeView == .sitrep && store.threadId == nil && store.scheduledPreviewRow == nil
            && !store.composeOpen && !store.modalOverlayOpen && !store.tour.active
            && !store.sideView.isOpen && !store.shareSheetOpen && !store.addAccountSheetOpen
            && store.undos.isEmpty && !presenter.menuOpen && presenter.moment == nil
            && !RehearsalMode.isEnabled && activeState == .key
    }

    private struct RestKey: Equatable {
        var pending: FeatureSet?
        var atRest: Bool
    }

    // MARK: menu

    private func menu(ring: CGRect, in size: CGSize) -> some View {
        let leading = SidebarRail.railWidth + 10
        let columns = size.width - leading - 16 >= DiscoveryMenu.width(columns: 4) ? 4 : 2
        return ZStack(alignment: .topLeading) {
            // A light dim, no blur: the board stays legible behind the card.
            Rectangle()
                .fill(.black.opacity(0.12))
                .ignoresSafeArea()
                .contentShape(Rectangle())
                .onTapGesture { presenter.closeMenu() }
                .transition(.opacity)

            DiscoveryMenuAnchor(ring: ring, leading: leading, area: size) {
                let card = DiscoveryMenu(
                    state: discovery.state(of:), fresh: discovery.ledger.fresh,
                    live: activeState == .key, columns: columns,
                    onClose: { presenter.closeMenu() })
                // The card at its own height when the window has it; a scroll
                // only when it does not. A bare ScrollView would always take
                // the whole cap and pin the card to the top of the window.
                ViewThatFits(in: .vertical) {
                    card
                    ScrollView(.vertical) { card }
                        .scrollIndicators(.never)
                }
            }
            .transition(
                .scale(scale: 0.92, anchor: .bottomLeading).combined(with: .opacity))
        }
        .keyContext(.modal)
        .keyBindings(.modal, [
            KeyBinding("Escape", "close ways through your mail") { presenter.closeMenu() }
        ])
        .onAppear {
            // Opening is the acknowledgement, whoever opened it (the ring, or
            // `PASSBAND_DISCOVERY_OPEN`): the glow has done its job.
            discovery.acknowledge()
            Analytics.capture("discovery_opened")
        }
    }

    // MARK: moment

    private func momentCard(_ set: FeatureSet, ring: CGRect, in size: CGSize) -> some View {
        DiscoveryMomentHost(set: set, live: activeState == .key, hold: Self.momentHold) {
            presenter.moment = nil
        } open: {
            presenter.openMenu()
        }
        // Beside the ring, centred on it, so it reads as the ring speaking.
        .alignmentGuide(.leading) { _ in -(SidebarRail.railWidth + 10) }
        .alignmentGuide(.top) { d in -(max(16, min(size.height - d.height - 16, ring.midY - d.height / 2))) }
        .transition(
            .asymmetric(
                insertion: .scale(scale: 0.9, anchor: .leading).combined(with: .opacity)
                    .combined(with: .offset(x: -14)),
                removal: .opacity))
    }
}

/// The menu card placed beside the rail with its bottom at the ring's bottom,
/// pushed up only as far as it must be to fit, and capped to the window's
/// height (the ScrollView inside takes over below that).
private struct DiscoveryMenuAnchor<Content: View>: View {
    let ring: CGRect
    let leading: CGFloat
    let area: CGSize
    @ViewBuilder var content: Content

    @State private var height: CGFloat = 0

    var body: some View {
        let margin: CGFloat = 16
        let cap = max(200, area.height - 2 * margin)
        let bottom = ring == .zero ? area.height - margin : min(area.height - margin, ring.maxY + 8)
        let top = max(margin, bottom - min(height, cap))
        content
            .frame(maxHeight: cap)
            .fixedSize(horizontal: true, vertical: false)
            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0 }
            .offset(x: leading, y: top)
    }
}

/// The moment with its own clock: held while hovered, gone after `hold`.
private struct DiscoveryMomentHost: View {
    let set: FeatureSet
    let live: Bool
    let hold: Duration
    let dismiss: () -> Void
    let open: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: open) {
            DiscoveryMoment(set: set, live: live)
                .contentShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help("See every way through your mail")
        // Restarted by hover: leaving the card gives it its full time again.
        .task(id: hovering) {
            guard !hovering else { return }
            do { try await Task.sleep(for: hold) } catch { return }
            dismiss()
        }
    }
}
