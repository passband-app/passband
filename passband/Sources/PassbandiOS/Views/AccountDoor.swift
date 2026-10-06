// THE TWO PANES THAT COME IN FROM THE LEFT, and the door that opens them.
//
// A person glyph sits at the top LEFT of the sitrep, and a swipe from left to
// right is a pull on that edge, so what either opens arrives from that side:
// the picker as a drawer over the board, the full Account page as a pane that
// covers it. A navigation push comes in from the right, which is the direction
// of "deeper"; these are not deeper than the sitrep, they are beside it.
//
//   - A SWIPE opens the PICKER: which mailbox, one tap to change it, and the
//     two doors onward (add one, or the whole Account page). It is the phone's
//     answer to the Mac's rail of accounts and its ⌘1..⌘9.
//   - THE GLYPH opens the ACCOUNT PAGE: the same page it always was, in a
//     stack of its own so its panes still push the ordinary way inside it.
//
// The shell mounts both (`AccountPanes`, in MobileRootView) over the tab bar.

import SwiftUI

/// Which of the two is open. One shared instance, like the other pieces of
/// shell state a view three levels down has to reach (the sitrep opens it, the
/// page closes it, the shell draws it).
@MainActor
@Observable
final class AccountDoor {
    static let shared = AccountDoor()

    enum Pane: Hashable { case picker, page }

    var pane: Pane?

    func close() { pane = nil }
}

// MARK: - the list of mailboxes

/// WHICH MAILBOX, and the tap that changes it. Shared by the picker and the
/// Account page, which leads with the same list.
///
/// It renders for a single account too. One row is not a switcher, but it is
/// still the answer to "who am I signed in as", and a control that appears only
/// once a second account exists is one nobody finds before they need it.
struct AccountSwitcher: View {
    /// After a row is tapped; `changed` is false for the mailbox already on
    /// screen. Both surfaces get out of the way of a mailbox that was just
    /// chosen, so the board it switches to is what is looked at next.
    var onPicked: (_ changed: Bool) -> Void = { _ in }

    private var accounts: [AccountRecord] { AccountManager.shared.accounts }

    var body: some View {
        VStack(spacing: 0) {
            ForEach(accounts) { account in
                let isActive = account.id == AccountManager.shared.activeId
                Button {
                    // `switchTo` declines the account already live, so a tap on
                    // the checked row costs nothing: no flush, no epoch bump,
                    // no world rebuilt to arrive where it started. Which is why
                    // the row is not `.disabled`: dimming the ACTIVE account
                    // would say the opposite of what the checkmark says.
                    Task { await AccountManager.shared.switchTo(account.id) }
                    onPicked(!isActive)
                } label: {
                    row(account, isActive: isActive)
                }
                .buttonStyle(.plain)
                if account.id != accounts.last?.id { Hairline() }
            }
        }
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .passbandGlass(.pane, cornerRadius: 18, tint: Palette.glassTint)
    }

    private func row(_ account: AccountRecord, isActive: Bool) -> some View {
        HStack(spacing: 12) {
            // The same initial-in-a-circle the Mac's rail wears, so one person
            // on both devices recognizes an account by the same mark.
            Text(account.initial)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Palette.accent)
                .frame(width: 26, height: 26)
                .background(Circle().fill(Palette.accentSoft))
            VStack(alignment: .leading, spacing: 2) {
                Text(account.displayName)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                // Only when it would SAY something new: `displayName` already
                // falls back to the host, and a row reading "baddiebox:8848"
                // twice is noise dressed as detail.
                if let host = subtitle(account) {
                    Text(host)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            if isActive {
                Image(systemName: "checkmark")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Palette.accent)
                    .accessibilityLabel("Active account")
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 11)
        .contentShape(Rectangle())
    }

    /// The daemon behind a NAMED account. Nil when the name is already the host
    /// (or when the host is not known yet, which is a record written before the
    /// field existed; `noteDisplayHost` fills those in at boot).
    private func subtitle(_ account: AccountRecord) -> String? {
        let named = account.label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !named.isEmpty, !account.displayHost.isEmpty else { return nil }
        return account.displayHost
    }
}

// MARK: - the picker

/// The drawer's contents: the mailboxes, and the two ways onward.
struct AccountPicker: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("accounts")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.accent)
                        .textCase(.uppercase)
                        .tracking(0.6)
                    Text("Which mailbox.")
                        .font(Typo.hero(26))
                        .foregroundStyle(Palette.ink)
                }
                .padding(.top, 12)

                // The board behind the drawer becomes the mailbox that was
                // tapped, so the drawer gets out of its way.
                AccountSwitcher { _ in AccountDoor.shared.close() }

                VStack(spacing: 0) {
                    door("plus", "Add an account") {
                        // The sheet is the shell's, and a sheet raised under an
                        // open drawer would be a form behind a pane.
                        AccountDoor.shared.close()
                        store.addAccountSheetOpen = true
                    }
                    Hairline()
                    door("person.crop.circle", "Account and settings") {
                        AccountDoor.shared.pane = .page
                    }
                }
                .padding(.vertical, 4)
                .passbandGlass(.pane, cornerRadius: 18, tint: Palette.glassTint)
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 28)
        }
        .scrollBounceBehavior(.basedOnSize)
    }

    private func door(_ symbol: String, _ title: String, _ action: @escaping () -> Void)
        -> some View
    {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: symbol)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(Palette.accent)
                    .frame(width: 26)
                Text(title)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.ink)
                Spacer(minLength: 8)
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Palette.inkFaintest)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 11)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

// MARK: - a pane that comes in from the left

/// Slides its content in from the leading edge and lets a drag push it back
/// out the way it came. `width` nil is the whole screen (the Account page); a
/// number is a drawer, with the board dimmed and tappable beside it.
struct LeadingPane<Content: View>: View {
    let width: CGFloat?
    /// Whether a leftward drag on the content closes it. The Account page says
    /// no and closes from its own first page, since a drag here would also be
    /// heard on every page pushed inside it.
    var dragCloses = true
    let onClose: () -> Void
    @ViewBuilder var content: Content

    @State private var drag: CGFloat = 0

    var body: some View {
        content
            .frame(maxWidth: width ?? .infinity, maxHeight: .infinity, alignment: .topLeading)
            // A drawer shows its trailing edge, rounded and lifted off the
            // board; a full pane has no edge to show. The shape is the
            // background and not a clip, so it runs under the status bar and
            // the home indicator while the content keeps to the safe area.
            .background {
                UnevenRoundedRectangle(
                    bottomTrailingRadius: width == nil ? 0 : 30,
                    topTrailingRadius: width == nil ? 0 : 30, style: .continuous
                )
                .fill(Palette.canvas)
                .shadow(color: .black.opacity(width == nil ? 0 : 0.35), radius: 30, x: 10)
                .ignoresSafeArea()
            }
            .offset(x: min(0, drag))
            .simultaneousGesture(dragCloses ? closeDrag : nil)
            .frame(maxWidth: .infinity, alignment: .leading)
            .transition(.move(edge: .leading))
    }

    private var closeDrag: some Gesture {
        LeadingPaneClose.drag(
            onChanged: { drag = $0 },
            onEnded: { close in
                if close {
                    onClose()
                } else {
                    withAnimation(Motion.pane) { drag = 0 }
                }
            })
    }
}

/// The closing drag, shared by the drawer (which follows the finger) and the
/// Account page's first screen (which only needs the verdict).
@MainActor
enum LeadingPaneClose {
    /// Mostly sideways, leftward, and either far enough or fast enough to have
    /// been meant.
    static func drag(
        onChanged: @escaping (CGFloat) -> Void = { _ in },
        onEnded: @escaping (_ close: Bool) -> Void
    ) -> some Gesture {
        DragGesture(minimumDistance: 16)
            .onChanged { value in
                let dx = value.translation.width
                guard dx < 0, abs(value.translation.height) < abs(dx) else { return }
                onChanged(dx)
            }
            .onEnded { value in
                let dx = value.translation.width
                let sideways = abs(value.translation.height) < abs(dx) * 0.6
                let meant = dx < -80 || value.predictedEndTranslation.width < -220
                onEnded(dx < 0 && sideways && meant)
            }
    }
}
