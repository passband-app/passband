// A login code, floated over whatever app is in front.
//
// The code arrives while the human is in a BROWSER, looking at a field that
// wants six digits. A banner makes them read the digits off a corner of the
// screen and type them; fronting Passband takes the field they were about to
// type into away from them. This is neither: a small panel above every app,
// with the code and a Copy button, that never becomes key and never activates
// Passband. Whatever they were typing into keeps the keyboard, and the click
// on Copy is the only thing this ever asks for.
//
// The code is the one the daemon already put in the event's banner copy
// (`EventBanner.loginCode`), so nothing is revealed here that a banner would
// not have shown. It is held in this panel's state only and dropped on close.
//
// A panel nobody answered turns back into the ordinary banner (`onMissed`), so
// a code that arrived while the human was across the room is still in
// Notification Center when they come back.

import AppKit
import SwiftUI

@MainActor
final class LoginCodePanel {
    static let shared = LoginCodePanel()

    /// How long an unanswered code floats. Longer than the in-app modal's 30s:
    /// this one is out of the way, and the human may be mid-form.
    static let seconds = 60

    struct Entry: Identifiable, Equatable {
        let id = UUID()
        var login: EventBanner.LoginCode
        var sender: String
        /// The mailbox it arrived in, named only when there is more than one.
        var account: String?
    }

    private var panel: FloatingPanel?
    private var onMissed: (() -> Void)?
    /// Which entry the panel on screen belongs to. A card's callbacks outlive
    /// it (Copy closes on a short delay), and one that fires after a newer
    /// code has taken the panel must not close the newer code.
    private var showing: UUID?

    private init() {}

    /// Float `login`. A code already on screen is replaced by the newer one,
    /// and goes the way of any other unanswered code.
    func show(_ login: EventBanner.LoginCode, sender: String, account: String?, onMissed: @escaping () -> Void) {
        close(showing, answered: false)
        self.onMissed = onMissed
        let entry = Entry(login: login, sender: sender, account: account)
        let id = entry.id
        showing = id
        let host = FirstClickHostingView(
            rootView: LoginCodeCard(
                entry: entry,
                onCopied: { [weak self] in self?.close(id, answered: true) },
                onCopy: { [weak self] in self?.markAnswered(id) },
                onDismiss: { [weak self] in self?.close(id, answered: true) },
                onTimeout: { [weak self] in self?.close(id, answered: false) }))
        // The card has one size and the panel is given it once. A hosting
        // view installed AS a borderless window's content view is the only
        // view in that window, and it keeps asking for another constraint
        // pass from inside the pass: AppKit allows one per view and then
        // throws. So it sits in a plain container, sized by frame, and is
        // told not to manage anybody's constraints.
        let size = host.fittingSize
        host.sizingOptions = []
        let container = NSView(frame: NSRect(origin: .zero, size: size))
        host.frame = container.bounds
        host.autoresizingMask = [.width, .height]
        container.addSubview(host)
        let panel = FloatingPanel(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered, defer: false)
        panel.identifier = NSUserInterfaceItemIdentifier("login-code")
        panel.isReleasedWhenClosed = false
        panel.level = .floating
        // Every Space, and over a full-screen app: the browser asking for the
        // code is as likely to be full screen as not.
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .ignoresCycle]
        panel.hidesOnDeactivate = false
        panel.becomesKeyOnlyIfNeeded = true
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.isMovableByWindowBackground = true
        // The theme is the app's own setting, not the system's. Set on the
        // window: `preferredColorScheme` inside a hosting view changes the
        // window's appearance mid-layout.
        panel.appearance = switch Prefs.shared.theme {
        case .dark: NSAppearance(named: .darkAqua)
        case .light: NSAppearance(named: .aqua)
        case .system: nil
        }
        panel.contentView = container
        panel.setFrameOrigin(Self.origin(for: size))
        panel.alphaValue = 0
        self.panel = panel
        // Regardless: Passband is, by design, not the active app right now.
        panel.orderFrontRegardless()
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.18
            panel.animator().alphaValue = 1
        }
    }

    /// Top centre of the screen the pointer is on, which is the screen the
    /// human is working on. Clear of the corner banners arrive in.
    private static func origin(for size: NSSize) -> NSPoint {
        let mouse = NSEvent.mouseLocation
        let screen =
            NSScreen.screens.first { NSMouseInRect(mouse, $0.frame, false) } ?? NSScreen.main
        guard let frame = screen?.visibleFrame else { return .zero }
        return NSPoint(x: frame.midX - size.width / 2, y: frame.maxY - size.height - 28)
    }

    /// Copying ANSWERS a code the moment it happens, not when the card gets
    /// round to closing: a newer code that replaces a just-copied one must
    /// not send the copied one back out as a banner.
    fileprivate func markAnswered(_ id: UUID) {
        if id == showing { onMissed = nil }
    }

    private func close(_ id: UUID?, answered: Bool) {
        guard let panel, id == showing else { return }
        self.panel = nil
        showing = nil
        let missed = onMissed
        onMissed = nil
        panel.orderOut(nil)
        // Dropping the hosting view drops the code with it.
        panel.contentView = nil
        if !answered { missed?() }
    }
}

/// Borderless panels refuse key by default, and a panel that can never be key
/// makes SwiftUI draw its buttons as disabled. It CAN be key and is told to
/// become so only when a control needs it, which no control here does.
private final class FloatingPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

/// The first click on a window that is not key is normally spent on making it
/// key. This window is never key, so that click has to be the button press.
private final class FirstClickHostingView<Content: View>: NSHostingView<Content> {
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
}

private struct LoginCodeCard: View {
    let entry: LoginCodePanel.Entry
    let onCopied: () -> Void
    var onCopy: () -> Void = {}
    let onDismiss: () -> Void
    let onTimeout: () -> Void

    @State private var copied = false
    @State private var secondsLeft = LoginCodePanel.seconds

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: Space.s10) {
                Avatar(sender: entry.sender, size: 26)
                VStack(alignment: .leading, spacing: 1) {
                    Text(entry.login.service)
                        .font(Typo.ui(13, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                    Label(subtitle, systemImage: "key.fill")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.lock)
                        .lineLimit(1)
                }
                Spacer(minLength: Space.s6)
                Text("\(secondsLeft)s")
                    .font(Typo.num(10))
                    .foregroundStyle(Palette.inkFaintest)
            }

            Text(entry.login.code)
                .font(Typo.mono(38, weight: .semibold))
                .foregroundStyle(Palette.ink)
                .tracking(5)
                .lineLimit(1)
                .minimumScaleFactor(0.5)
                .textSelection(.disabled)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Space.s14)
                .accessibilityLabel("login code \(entry.login.code)")

            // Drawn by hand, both of them. The system's glass buttons dim
            // themselves in a window that is not key, and this window never
            // is: they would sit there looking disabled for the whole minute.
            HStack(spacing: Space.s8) {
                Button(action: copy) {
                    Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                        .font(Typo.ui(13, weight: .semibold))
                        .foregroundStyle(.white)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, Space.s8)
                        .background(
                            RoundedRectangle(cornerRadius: 11, style: .continuous)
                                .fill(copied ? Palette.positive : Palette.lock))
                        .contentShape(RoundedRectangle(cornerRadius: 11, style: .continuous))
                }
                Button(action: onDismiss) {
                    Text("Dismiss")
                        .font(Typo.ui(13, weight: .medium))
                        .foregroundStyle(Palette.inkDim)
                        .padding(.horizontal, Space.s14)
                        .padding(.vertical, Space.s8)
                        .background(
                            RoundedRectangle(cornerRadius: 11, style: .continuous)
                                .fill(Palette.ink.opacity(0.1)))
                        .contentShape(RoundedRectangle(cornerRadius: 11, style: .continuous))
                }
            }
            .buttonStyle(.plain)
        }
        .padding(Space.s16)
        .frame(width: 300)
        .passbandGlass(.pane, cornerRadius: 22, tint: Palette.lockSoft.opacity(0.9))
        // Room for the glass's own shadow: the window has none, because a
        // window shadow is cut to the frame and this card has round corners.
        .padding(Space.s20)
        .task { await runTimer() }
    }

    private var subtitle: String {
        entry.account.map { "login code · \($0)" } ?? "login code"
    }

    private func copy() {
        guard !copied, Clip.copy(entry.login.pasteable) else { return }
        copied = true
        onCopy()
        // Long enough to read "Copied", short enough that the panel is gone
        // by the time the pointer is back on the field.
        Task {
            try? await Task.sleep(for: .milliseconds(700))
            onCopied()
        }
    }

    private func runTimer() async {
        while secondsLeft > 0 {
            try? await Task.sleep(for: .seconds(1))
            if Task.isCancelled || copied { return }
            secondsLeft -= 1
        }
        onTimeout()
    }
}
