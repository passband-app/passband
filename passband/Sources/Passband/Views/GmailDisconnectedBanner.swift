// The mailbox-disconnected banner: the one thing standing between a dead
// refresh token and a person who thinks they are simply having a quiet week.
//
// A dead token is invisible from everywhere anybody looks. Mail stops arriving,
// which is indistinguishable from nobody writing to you; the app is otherwise
// perfectly healthy, so nothing is spinning, nothing is red, and nothing is
// wrong except that Passband has quietly stopped being a mail client. In August
// 2026 three of four hosted mailboxes sat like that, and the only reason
// anybody found out was an operator reading Prometheus by hand.
//
// NOT DISMISSIBLE, and that is the whole design. Every other card in this app
// is something to read once and put away; this is a statement about the app not
// working, and it stays true until somebody fixes it. A dismiss button would be
// pressed within a second and the mailbox would go back to looking merely
// quiet, which is the exact failure this exists to end.
//
// NOT A MODAL either, for the opposite reason: the app still works. Mail that
// already synced is readable, searchable and answerable, and a scrim over all
// of it would take away the half that functions in order to complain about the
// half that does not.
//
// TWO ENDINGS, because two products end here. A hosted mailbox re-consents
// through the control plane and the button opens it, after one dialog naming
// the address to pick in Google's chooser. A self-host one is
// repaired with `squelchd auth` at a shell, which is not a link anything can
// offer, so it gets the sentence instead of the button. The daemon decides
// which by whether it sent a `reconnect_url`; the app never guesses.

import SwiftUI

struct GmailDisconnectedBanner: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        HStack(alignment: .top, spacing: 11) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(Palette.warn)
                .font(.system(size: 14, weight: .semibold))
                .padding(.top, 1)

            VStack(alignment: .leading, spacing: 3) {
                Text("Passband cannot read your mail")
                    .font(Typo.zoneTitle)
                    .foregroundStyle(Palette.ink)

                // WHAT IT MEANS, not what went wrong. "Your Google sign in
                // expired" is the fact; "new mail is not arriving" is the
                // consequence, and the consequence is the half that matters,
                // because it is what they have been living with without
                // knowing.
                Text(detail)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Spacer(minLength: 8)

            if store.gmailReconnectURL != nil {
                Button("Reconnect") { store.askReconnect() }
                    .controlSize(.small)
            }
        }
        .padding(.horizontal, 13)
        .padding(.vertical, 10)
        .background(Palette.warnSoft, in: RoundedRectangle(cornerRadius: 9))
        .overlay(
            RoundedRectangle(cornerRadius: 9)
                .strokeBorder(Palette.warn.opacity(0.30), lineWidth: 1)
        )
        // One announcement for a screen reader, not four fragments.
        .accessibilityElement(children: .combine)
    }

    /// The sentence under the heading. Says how long only when the daemon said
    /// so: "disconnected" is bad enough without inventing a duration for it.
    private var detail: String {
        let repair =
            store.gmailReconnectURL != nil
            ? "Reconnect to start it again."
            : "Run squelchd auth on the machine running your daemon to sign in again."
        guard let since = store.gmailDisconnectedSince else {
            return "Your Google sign in expired, so new mail is not arriving. \(repair)"
        }
        let ago = Self.elapsed.localizedString(for: since, relativeTo: Date())
        return "Your Google sign in expired \(ago), so no new mail has arrived since. \(repair)"
    }

    private static let elapsed: RelativeDateTimeFormatter = {
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .full
        return f
    }()
}

/// Which Google account to pick, said before the browser opens. Google's
/// chooser lists every account signed in there and none of them is marked as
/// this mailbox; the wrong pick consents for somebody else's mail and leaves
/// the banner up. See `AppStore.reconnectAsk`.
struct ReconnectAccountPrompt: View {
    @Environment(AppStore.self) private var store
    let ask: AppStore.ReconnectAsk

    var body: some View {
        OverlayScrim(onDismiss: { store.cancelReconnectAsk() }) {
            ModalCard(width: 400) {
                Text("Be sure to select this email address")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                // Email-derived, so plain Text and never markup.
                Text(verbatim: ask.email)
                    .font(.system(size: 13, weight: .medium, design: .monospaced))
                    .foregroundStyle(Palette.ink)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 7)
                    .background(Palette.warnSoft, in: RoundedRectangle(cornerRadius: 7))
                Text("Google will show every account signed in to your browser. Choosing a different one will not reconnect this mailbox.")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                HStack(spacing: 8) {
                    Spacer(minLength: 0)
                    Button("Cancel") { store.cancelReconnectAsk() }
                        .buttonStyle(.glass)
                    Button("Continue to Google") { store.confirmReconnect() }
                        .buttonStyle(.glassProminent)
                }
            }
        }
        .keyContext(.modal)
        .keyBindings(.modal, [
            KeyBinding("Escape", "cancel", allowInInput: true) { store.cancelReconnectAsk() },
            KeyBinding("Enter", "continue", allowInInput: true) { store.confirmReconnect() },
        ])
    }
}
