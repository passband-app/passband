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
// TWO ROUTES, as the disconnected banner has: hosted opens the control plane's
// consent; self-host is `squelchd auth --calendar` at a shell, which no button
// can run, so the card hands over the command instead. Either way the card
// then WAITS, and the stats poll ends the wait (`AppStore.calendarGrantArrived`).

#if os(macOS)
    import SwiftUI

    struct CalendarConnectCard: View {
        @Environment(AppStore.self) private var store
        let ask: CalendarConnectAsk
        @State private var copied = false

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
                return
            }
            switch ask.route {
            case .link: store.confirmCalendarConnect()
            case .command: copy()
            }
        }

        private func copy() {
            store.copyCalendarCommand()
            copied = true
            Task {
                try? await Task.sleep(for: Clip.flashWindow)
                copied = false
            }
        }

        // MARK: asking

        @ViewBuilder
        private var askBody: some View {
            Text("Connect your Google Calendar")
                .font(Typo.ui(14, weight: .semibold))
                .foregroundStyle(Palette.ink)
            Text("See your events beside the plans Passband finds in your mail.")
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkDim)
                .fixedSize(horizontal: false, vertical: true)
            Text(permissionLine)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
                .fixedSize(horizontal: false, vertical: true)

            switch ask.route {
            case .link:
                if let email = ask.email {
                    accountBox(label: "Pick this account when Google asks", email: email)
                }
                HStack(spacing: Space.s8) {
                    Spacer(minLength: 0)
                    Button("Not now") { store.dismissCalendarConnect() }
                        .buttonStyle(.glass)
                    Button("Connect calendar") { store.confirmCalendarConnect() }
                        .buttonStyle(.glassProminent)
                }
            case .command(let command):
                commandBox(command)
                if let email = ask.email {
                    accountBox(label: "Sign in as this account when the browser opens", email: email)
                }
                HStack(spacing: Space.s8) {
                    Spacer(minLength: 0)
                    Button("Not now") { store.dismissCalendarConnect() }
                        .buttonStyle(.glass)
                    Button(copied ? "Copied" : "Copy command") { copy() }
                        .buttonStyle(.glassProminent)
                }
            }
        }

        /// Honest about the ask: Google's screen will say see AND edit, and
        /// this wave of Passband only reads.
        private var permissionLine: String {
            switch ask.route {
            case .link:
                "Google will ask to let Passband see and edit your calendar events. For now Passband only reads them."
            case .command:
                "Run this where your daemon runs. Google will ask to let Passband see and edit your calendar events. For now Passband only reads them."
            }
        }

        // MARK: waiting

        @ViewBuilder
        private var waitingBody: some View {
            HStack(spacing: Space.s8) {
                WaitDots(tone: Palette.inkFaint)
                Text(ask.route.isLink ? "Finish in your browser" : "Waiting for squelchd")
                    .font(Typo.ui(14, weight: .semibold))
                    .foregroundStyle(Palette.ink)
            }
            Text(waitingLine)
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkDim)
                .fixedSize(horizontal: false, vertical: true)
            if case .command(let command) = ask.route {
                commandBox(command)
            } else if let email = ask.email {
                accountBox(label: "Pick this account", email: email)
            }
            HStack(spacing: Space.s8) {
                Spacer(minLength: 0)
                if case .link(let url) = ask.route {
                    Button("Open Google again") { Opener.open(url) }
                        .buttonStyle(.glass)
                }
                Button("Close") { store.dismissCalendarConnect() }
                    .buttonStyle(.glassProminent)
            }
        }

        private var waitingLine: String {
            switch ask.route {
            case .link:
                "Google is asking for permission in your browser. This closes on its own once your calendar is connected."
            case .command:
                "Run the command where your daemon runs. This closes on its own once your calendar is connected."
            }
        }

        // MARK: boxes

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
                    .background(Palette.hairline.opacity(0.6), in: RoundedRectangle(cornerRadius: 7))
            }
        }

        private func commandBox(_ command: String) -> some View {
            HStack(spacing: Space.s8) {
                Text(verbatim: command)
                    .font(Typo.mono(12.5, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .textSelection(.enabled)
                    .lineLimit(1)
                Spacer(minLength: Space.s8)
                Button {
                    copy()
                } label: {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        .font(Typo.ui(11))
                }
                .buttonStyle(.textAction)
                .help("Copy")
                .accessibilityLabel("Copy command")
            }
            .padding(.horizontal, Space.s10)
            .padding(.vertical, Space.scaled(7))
            .background(Palette.hairline.opacity(0.6), in: RoundedRectangle(cornerRadius: 7))
        }
    }

    extension CalendarGate.ConnectRoute {
        fileprivate var isLink: Bool {
            if case .link = self { return true }
            return false
        }
    }
#endif
