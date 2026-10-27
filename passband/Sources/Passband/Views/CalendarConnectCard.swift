// THE CONNECT CARD: the calendar page asking, once per account, to read the
// user's Google Calendar. Built like `ReconnectAccountPrompt` (an
// `OverlayScrim` + `ModalCard` in the action layer, its own modal key context,
// Escape and Enter) because it is the same kind of moment: a browser is about
// to open on a Google screen that does not say which mailbox this is for.
//
// WHAT IT MUST SAY, and nothing more:
//   - what connecting gets you, in one sentence and without a promise the
//     product does not keep yet;
//   - that Google's screen will ask for SEE AND EDIT. The scope is
//     `calendar.events`, and a consent screen that says more than the card did
//     reads as a bait and switch;
//   - which account to pick, because Google's chooser lists every account in
//     the browser and the wrong one is refused at the callback.
//
// ONE BUTTON, two routes behind it: hosted opens the control plane's consent;
// a self-host daemon is asked for Google's link and that is opened. The user
// sees the same card either way, and the card then WAITS, and the stats poll
// ends the wait (`AppStore.calendarGrantArrived`). The terminal path
// (`squelchd auth --calendar`, for a headless box) is a line of fine print
// pointing at the docs, never a button.

#if os(macOS)
    import SwiftUI

    struct CalendarConnectCard: View {
        @Environment(AppStore.self) private var store
        let ask: CalendarConnectAsk

        var body: some View {
            OverlayScrim(onDismiss: { store.dismissCalendarConnect() }) {
                ModalCard(width: 420) {
                    // The wait has a deadline (the consent session's life), so
                    // the card re-asks the clock now and then rather than
                    // waiting forever on a tab that has died.
                    TimelineView(.periodic(from: .now, by: 15)) { context in
                        let waiting = store.calendarWaiting(now: context.date)
                        VStack(alignment: .leading, spacing: Space.s12) {
                            if waiting {
                                waitingBody
                            } else {
                                askBody
                            }
                        }
                    }
                }
            }
            .keyContext(.modal)
            .keyBindings(.modal, [
                KeyBinding("Escape", "not now", allowInInput: true) { store.dismissCalendarConnect() },
                KeyBinding("Enter", "connect calendar", allowInInput: true) { primary() },
            ])
        }

        /// Enter: the card's default button, whichever state it is in.
        private func primary() {
            if store.calendarWaiting() {
                store.dismissCalendarConnect()
            } else {
                store.confirmCalendarConnect()
            }
        }

        // MARK: asking

        @ViewBuilder
        private var askBody: some View {
            Text("Connect your Google Calendar")
                .font(Typo.modalTitle)
                .foregroundStyle(Palette.ink)
            Text("See your events beside the plans Passband finds in your mail.")
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkDim)
                .fixedSize(horizontal: false, vertical: true)
            // Honest about the ask: Google's screen will say see AND edit,
            // and Passband edits only on the user's own say-so.
            Text("Google will ask to let Passband see and edit your calendar events. Passband changes an event only when you create, edit or delete it here.")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
                .fixedSize(horizontal: false, vertical: true)
            if ask.route == .daemon {
                // For the box with no browser: where the shell route is
                // written down. Fine print, never a button.
                Text("Running your own daemon on a headless box? The self-host docs cover `squelchd auth --calendar`.")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let email = ask.email {
                accountBox(label: "Pick this account when Google asks", email: email)
            }
            ModalButtons {
                Button("Not now") { store.dismissCalendarConnect() }
                    .buttonStyle(.glass)
                Button("Connect calendar") { store.confirmCalendarConnect() }
                    .buttonStyle(.glassProminent)
                    // A self-host daemon is being asked for the link.
                    .disabled(store.calendarConnectStarting)
            }
        }

        // MARK: waiting

        @ViewBuilder
        private var waitingBody: some View {
            HStack(spacing: Space.s8) {
                WaitDots(tone: Palette.inkFaint)
                Text("Finish in your browser")
                    .font(Typo.modalTitle)
                    .foregroundStyle(Palette.ink)
            }
            Text("Google is asking for permission in your browser. This closes on its own once your calendar is connected.")
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkDim)
                .fixedSize(horizontal: false, vertical: true)
            if let email = ask.email {
                accountBox(label: "Pick this account", email: email)
            }
            ModalButtons {
                // Self-host asks the daemon for a fresh link: the first may
                // have expired.
                Button("Open Google again") { store.reopenCalendarConsent() }
                    .buttonStyle(.glass)
                    .disabled(store.calendarConnectStarting)
                Button("Close") { store.dismissCalendarConnect() }
                    .buttonStyle(.glassProminent)
            }
        }

        // MARK: the account

        /// The address to pick, set the way the reconnect prompt sets it, but
        /// on a neutral well: this is a choice to get right, not a warning.
        private func accountBox(label: String, email: String) -> some View {
            VStack(alignment: .leading, spacing: Space.s4) {
                Text(label)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                // Email-derived, so plain Text and never markup.
                Text(verbatim: email)
                    .font(Typo.mono(13, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, Space.s10)
                    .padding(.vertical, Space.scaled(7))
                    .background(Palette.hairline.opacity(0.6), in: RoundedRectangle(cornerRadius: Radius.well, style: .continuous))
            }
        }
    }
#endif
