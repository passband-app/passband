// THE DISCOVERY RING'S WIRING: the ring's glyph on the rail, the routed page
// that reads the store, and the found moment's timing. The pictures are
// DiscoveryRing / DiscoveryPage / DiscoveryVignette, which take every input as
// a parameter; this file is the only one that reads the store, the ledger and
// the window.
//
// MAC ONLY in v1, like the rest of the folder (see project.yml's iOS excludes).

import AppKit
import SwiftUI

/// What is on screen. Separate from `Discovery` on purpose: the ledger is the
/// model and crosses to iOS; where the moment sits is this shell's business.
@MainActor
@Observable
final class DiscoveryPresenter {
    static let shared = DiscoveryPresenter()

    /// `PASSBAND_DISCOVERY_OPEN=1` opens the page at launch, the same posture
    /// as `PASSBAND_FORCE_TOUR`: a launch environment nobody sets by accident.
    /// It still waits for the shell (no practice inbox, no tour).
    var openAtLaunch = ProcessInfo.processInfo.environment["PASSBAND_DISCOVERY_OPEN"] == "1"
    /// The found moment on screen, if any.
    var moment: FeatureSet?
    /// Where the ring sits in `tourSpace`, so the moment can speak from it.
    var ringFrame: CGRect = .zero
}

// MARK: - the ring on the rail

/// The ring as the rail's icon for `.discovery`. The tile, the selector pane,
/// the hover wash and the practice refusal are all `RailButton`'s; this is
/// only the glyph, plus the frame the found moment is placed from.
///
/// ALWAYS THERE, practice inbox included, and it never retires: all kept is a
/// resting state, not a farewell.
struct DiscoveryRailGlyph: View {
    @Environment(\.controlActiveState) private var activeState
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var discovery: Discovery { Discovery.shared }

    var body: some View {
        DiscoveryRing(
            progress: DiscoveryProgress(discovery.state(of:), fresh: discovery.ledger.fresh),
            diameter: 24,
            breathing: activeState == .key && !reduceMotion)
            .onGeometryChange(for: CGRect.self) { $0.frame(in: .named(tourSpace)) } action: {
                DiscoveryPresenter.shared.ringFrame = $0
            }
    }

    /// For VoiceOver on the rail tile: the ring's progress in words.
    @MainActor static var spokenValue: String {
        let discovery = Discovery.shared
        return DiscoveryProgress(discovery.state(of:), fresh: discovery.ledger.fresh).spoken
    }
}

// MARK: - the page

/// The routed page: the store-reading shell around `DiscoveryPage`.
struct DiscoveryView: View {
    @Environment(AppStore.self) private var store
    @Environment(\.controlActiveState) private var activeState

    /// What was fresh when the page opened. Taken in `init`, before the first
    /// frame, because arriving is what clears it in the ledger and the first
    /// frame has to put fresh rows in their OLD place for the arrival to read.
    @State private var arrivedFresh: Set<FeatureSet>
    @State private var settled: Bool

    private var discovery: Discovery { Discovery.shared }

    init() {
        let fresh = Discovery.shared.ledger.fresh
        _arrivedFresh = State(initialValue: fresh)
        // Reduce Motion: already settled, so nothing travels. Read from the
        // workspace because the environment is not available yet in init.
        let still = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        _settled = State(initialValue: fresh.isEmpty || still)
    }

    var body: some View {
        VStack(spacing: 0) {
            RoutedHeader(title: DiscoveryCopy.pageTitle, subtitle: DiscoveryCopy.pageSubtitle)
            DiscoveryPage(
                state: discovery.state(of:), fresh: arrivedFresh,
                live: activeState == .key, settled: settled)
        }
        // Esc leaves, as on Usage: a full routed page needs an exit other
        // than the rail.
        .keyContext(.modal)
        .keyBindings(.modal, [
            KeyBinding("Escape", "back to sitrep") { store.setView(.sitrep) }
        ])
        .task {
            // Visiting is the acknowledgement, whoever routed here (the ring,
            // the moment, `PASSBAND_DISCOVERY_OPEN`): the glow has done its job.
            discovery.acknowledge()
            guard !settled else { return }
            // THE ARRIVAL. Long enough for the page to be seen with the row
            // in its old place, then it settles into Unlocked.
            do { try await Task.sleep(for: .milliseconds(700)) } catch { return }
            withAnimation(.spring(response: 0.75, dampingFraction: 0.86)) { settled = true }
        }
    }
}

// MARK: - the layer

/// The found moment, mounted in ActionLayer under its modal palettes.
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
            }
            .frame(width: geo.size.width, height: geo.size.height, alignment: .topLeading)
        }
        .animation(reduceMotion ? .easeOut(duration: 0.15) : .spring(response: 0.5, dampingFraction: 0.84), value: presenter.moment)
        // THE MOMENT IS THE BOARD'S. It only arrives on the sitrep, and a
        // route away (the page included) takes it with it rather than leaving
        // it floating over a page it was not about.
        .onChange(of: store.activeView) { _, view in
            if view != .sitrep { presenter.moment = nil }
        }
        // THE WAIT FOR REST. Re-keyed on everything that can break it, so a
        // reader opening during the settle cancels the sleep and the moment
        // waits for the next quiet stretch.
        .task(id: RestKey(pending: discovery.pendingCelebration, atRest: atRest)) {
            guard let set = discovery.pendingCelebration, atRest else { return }
            do { try await Task.sleep(for: Self.settle) } catch { return }
            presenter.moment = set
            discovery.celebrationShown()
        }
        // `PASSBAND_DISCOVERY_OPEN`, once the shell is the real one.
        .task(id: store.tour.active) {
            guard presenter.openAtLaunch, !RehearsalMode.isEnabled, !store.tour.active else { return }
            presenter.openAtLaunch = false
            store.setView(.discovery)
        }
    }

    /// AT REST: the board on screen and nothing in flight over it. Undo chips
    /// count as in flight: the find that earned a moment is very often an `e`,
    /// and its undo is still part of that action.
    private var atRest: Bool {
        store.activeView == .sitrep && store.threadId == nil && store.scheduledPreviewRow == nil
            && !store.composeOpen && !store.modalOverlayOpen && !store.tour.active
            && !store.sideView.isOpen && !store.shareSheetOpen && !store.addAccountSheetOpen
            && store.undos.isEmpty && presenter.moment == nil
            && !RehearsalMode.isEnabled && activeState == .key
    }

    private struct RestKey: Equatable {
        var pending: FeatureSet?
        var atRest: Bool
    }

    // MARK: moment

    private func momentCard(_ set: FeatureSet, ring: CGRect, in size: CGSize) -> some View {
        // Read out here, not in the guides: they are Sendable closures, and
        // the rail's width is main-actor state.
        let leading = SidebarRail.railWidth + 10
        return DiscoveryMomentHost(set: set, live: activeState == .key, hold: Self.momentHold) {
            presenter.moment = nil
        } open: {
            presenter.moment = nil
            store.setView(.discovery, viaPointer: true)
        }
        // Beside the ring, centred on it, so it reads as the ring speaking.
        .alignmentGuide(.leading) { _ in -leading }
        .alignmentGuide(.top) { d in -(max(16, min(size.height - d.height - 16, ring.midY - d.height / 2))) }
        .transition(
            .asymmetric(
                insertion: .scale(scale: 0.9, anchor: .leading).combined(with: .opacity)
                    .combined(with: .offset(x: -14)),
                removal: .opacity))
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
