// WHETHER A PAGE IS THE ONE ON SCREEN, apart from whether it is mounted.
//
// The Sitrep and Emails pages stay mounted across rail navigation (RootView's
// `RoutedPages`): building Emails again on every visit was ~400 ms before it
// appeared, measured, and it lost the scroll and the cursor each time. A kept
// page is drawn at zero opacity and takes no clicks, but it is still mounted,
// so everything that used to mean "on screen" by being mounted has to ask this
// instead: key bindings and contexts (KeyDispatch), the seen-signals, the
// warms and refreshes a visit starts. `onPageShown` / `onPageHidden` are the
// mount-or-visit versions of `onAppear` / `onDisappear` for those.
//
// Every page that is not kept is mounted only while it is the active one, and
// reads the default, true, so nothing changes for it.

import SwiftUI

private struct PageIsActiveKey: EnvironmentKey {
    static let defaultValue = true
}

extension EnvironmentValues {
    /// False inside a page the shell keeps mounted while another is on screen.
    var pageIsActive: Bool {
        get { self[PageIsActiveKey.self] }
        set { self[PageIsActiveKey.self] = newValue }
    }
}

extension View {
    /// A page the shell keeps mounted: drawn and reachable only while active.
    /// Hidden the way the smart clear list hides the page under it, plus the
    /// environment value everything inside reads.
    func keptPage(active: Bool) -> some View {
        environment(\.pageIsActive, active)
            .opacity(active ? 1 : 0)
            .allowsHitTesting(active)
            .accessibilityHidden(!active)
            // No field in a page off screen keeps the keyboard.
            .disabled(!active)
            .zIndex(active ? 1 : 0)
    }

    /// Runs when the view is mounted in the active page, and again each time
    /// its page becomes active.
    func onPageShown(_ action: @escaping () -> Void) -> some View {
        modifier(PagePresenceModifier(shown: action, hidden: {}))
    }

    /// Runs when the view leaves the screen: unmounted from the active page,
    /// or its page no longer active.
    func onPageHidden(_ action: @escaping () -> Void) -> some View {
        modifier(PagePresenceModifier(shown: {}, hidden: action))
    }
}

private struct PagePresenceModifier: ViewModifier {
    let shown: () -> Void
    let hidden: () -> Void
    @Environment(\.pageIsActive) private var active
    @State private var mounted = false

    func body(content: Content) -> some View {
        content
            .onAppear {
                mounted = true
                if active { shown() }
            }
            .onDisappear {
                if active { hidden() }
                mounted = false
            }
            .onChange(of: active) { _, now in
                guard mounted else { return }
                if now { shown() } else { hidden() }
            }
    }
}
