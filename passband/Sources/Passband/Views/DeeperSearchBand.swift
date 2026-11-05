// THE DEEPER SEARCH, WHERE THE READER CAN SEE IT (docs/SEARCH.md §6.4). A band
// above the hits in the 460pt strip, the left-hand column beside them when the
// panel is expanded: same content either way, because it is the same
// conversation and only the room it has changes.
//
// What it draws is deliberately thin. One status line while the lane works (the
// tool chips reduced to their summaries: searching "abstract wifi", reading
// "Abstract is today."), then the email cards, then AT MOST one line of prose.
// That is the lane's whole answer shape, and drawing anything more would invite
// a model to write more.
//
// THE ONE LINE IS PLAIN TEXT, never markdown, and that is a security decision
// rather than a typographic one: the model's input is mail somebody else wrote,
// so a link in its prose is a link a stranger asked for. The ⌘K tray renders
// markdown and routes every click through `Opener`; this surface simply has no
// links at all.

import SwiftUI

struct DeeperSearchBand: View {
    /// Whether the panel is expanded. Only the frame differs; the content is
    /// identical, which is the point of one view rather than two.
    let expanded: Bool

    /// Where "continue in chat" goes, for a shell whose chat is not the Mac's
    /// ⌘K bar. nil is the Mac: `openSearchLaneInChat` opens the bar, and the bar
    /// takes a still-running lane over when it finishes. The phone has no bar
    /// to watch the lane from, so it passes its own push here and the button
    /// only offers itself once the lane is idle and the chat can adopt it.
    var openInChat: (() -> Void)? = nil

    @Environment(AppStore.self) private var store
    @Environment(Prefs.self) private var prefs
    private var session: AssistantSession { store.searchLane }
    private var search: SearchSession { store.search }

    /// The trigger a not-yet-started band would run under, for the on-request
    /// button. nil once the lane is going: then the trigger that matters is the
    /// one it actually started on.
    private var offeredTrigger: SearchIntent.Trigger? {
        search.laneStarted ? nil : search.lastVerdict?.trigger
    }

    /// One line of small type naming the signal that started this, so a reader
    /// who did not want the lane can see why it ran and shorten the query.
    private var reason: String? {
        if let trigger = search.laneTrigger {
            return SearchIntent.reason(trigger, query: search.laneQuery)
        }
        if let trigger = offeredTrigger {
            return SearchIntent.reason(trigger, query: search.fetchedQuery ?? search.query)
        }
        return nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            header
            content
        }
        .padding(Space.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .fill(Palette.accentSoft.opacity(0.35))
        )
        .overlay(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .strokeBorder(Palette.hairline, lineWidth: 0.75)
        )
        // THE WHOLE BOX IS THE WAY INTO THE CHAT: a click anywhere but on an
        // email card, or on one of the box's own buttons, opens the
        // conversation (full screen on the Mac, a push on the phone). Those
        // are Buttons and take their own clicks first; this hears only what
        // lands between them. Only once there is a conversation to open.
        .contentShape(RoundedRectangle(cornerRadius: Radius.well, style: .continuous))
        .onTapGesture {
            guard tapOpensChat else { return }
            (openInChat ?? store.openSearchLaneInChat)()
        }
        #if os(macOS)
            .pointerStyle(tapOpensChat ? .link : nil)
        #endif
        .padding(.horizontal, expanded ? 0 : Space.s14)
        .padding(.bottom, expanded ? 0 : Space.s8)
        // STRUCTURE ONLY, for AskBar's reason: rows arriving is a change worth
        // animating, text growing inside one at token rate is a smear.
        .animation(.smooth(duration: 0.25), value: session.transcript.count)
    }

    // MARK: - chrome

    private var header: some View {
        HStack(spacing: Space.scaled(7)) {
            Image(systemName: "sparkles")
                .font(Typo.ui(11, weight: .semibold))
                .foregroundStyle(Palette.accent)
            Text("Deeper search")
                .font(Typo.row)
                .foregroundStyle(Palette.inkFaint)
            if let reason {
                Text(reason)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            Spacer(minLength: Space.s4)
            if session.isPaused {
                Image(systemName: "pause.circle")
                    .font(Typo.ui(11, weight: .semibold))
                    .foregroundStyle(Palette.inkFaint)
                    .help("Held where it was. It picks up from there, nothing is lost.")
            }
            // THE WAY OUT OF THE ONE-LINE ANSWER. The band is kept thin on
            // purpose; somebody who wants the whole answer, or to ask a
            // follow-up, takes the conversation to ⌘K, where it has room.
            if search.laneStarted && !session.transcript.isEmpty && (openInChat == nil || hostedChatOffered) {
                Button { (openInChat ?? store.openSearchLaneInChat)() } label: {
                    Image(systemName: "arrow.up.left.and.arrow.down.right")
                        #if os(macOS)
                            .font(Typo.ui(10, weight: .semibold))
                            .foregroundStyle(Palette.inkFaint)
                            .frame(width: Space.scaled(18), height: Space.scaled(18))
                        #else
                            // A thumb's target: the way into the whole answer.
                            .font(Typo.ui(14, weight: .semibold))
                            .foregroundStyle(Palette.inkDim)
                            .frame(width: 32, height: 32)
                            .background(Circle().fill(Palette.hairline))
                        #endif
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(openInChat == nil ? !store.canOpenSearchLaneInChat : !hostedChatReady)
                .help(expandHelp)
                .accessibilityLabel("Continue in chat")
            }
        }
    }

    private var expandHelp: String {
        if openInChat != nil {
            return hostedChatReady
                ? "Continue in chat: the full answer, and room for follow-ups."
                : "The chat is busy answering something else."
        }
        if !store.canOpenSearchLaneInChat { return "The chat is busy answering something else." }
        return "Continue in chat: the full answer, and room for follow-ups. ⌘Return"
    }

    /// The box's background tap goes to the chat: the same moment the expand
    /// button is offered and enabled. The Mac's bar can watch a lane that is
    /// still answering, so there that is any time the button is live.
    private var tapOpensChat: Bool {
        guard search.laneStarted && !session.transcript.isEmpty else { return false }
        if openInChat == nil { return store.canOpenSearchLaneInChat }
        return hostedChatOffered && hostedChatReady
    }

    /// A caller-owned chat is offered once the lane has stopped talking: there
    /// is no bar to hand a running lane to, and `adopt` refuses a live one.
    private var hostedChatOffered: Bool {
        !session.running && !store.preparingSearchEvidence
    }

    /// And it can take the conversation right now: it already holds this one,
    /// or it is not busy with another.
    private var hostedChatReady: Bool {
        store.chatHoldsSearchLane || store.canExpandSearchLane
    }

    // MARK: - the lane's own rows

    @ViewBuilder
    private var content: some View {
        if !search.laneStarted {
            offer
        } else {
            // The status line, while there is work to say something about.
            if session.running, let tool = latestTool {
                ToolChipRow(tool: tool)
            } else if session.running {
                HStack(spacing: Space.s8) {
                    ProgressView().controlSize(.mini)
                    Text("reading your mail…")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                }
            }
            ForEach(cardRows) { row in
                EmailCardList(cards: row.emails) { threadId in
                    // The panel is not CLOSED, unlike AskBar's cards: the
                    // reader is still searching, and the results beside the
                    // thread are the whole reason the strip exists. An EXPANDED
                    // panel does fold back to that strip on the way in, because
                    // `openThread` collapses it as every path into the reader
                    // must. What survives the click is the search, not the
                    // layout it was being read in.
                    store.openThread(threadId, from: .deeperSearch)
                }
            }
            if let line = answerLine, !line.isEmpty {
                // Selectable on the phone, where a long press selects and a tap
                // still opens the chat. On the Mac a click into selectable text
                // is a selection, not the box's click, and the full answer is
                // one click away in the chat, selectable there.
                Text(line)
                    .font(Typo.row)
                    .foregroundStyle(Palette.ink)
                    #if os(iOS)
                        .textSelection(.enabled)
                    #endif
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if let failure = errorLine {
                BandNote(failure)
            }
        }
    }

    /// The on-request posture: the band says what it could do and waits to be
    /// asked. Nothing has been spent at this point, and nothing will be until
    /// this is pressed.
    @ViewBuilder
    private var offer: some View {
        HStack(spacing: Space.s8) {
            Text("Read the mail itself and answer this?")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
            Spacer(minLength: Space.s4)
            if let trigger = offeredTrigger {
                #if os(macOS)
                    Button("ask the agent") { store.startDeeperSearch(trigger: trigger) }
                        .buttonStyle(.glass)
                        .font(Typo.chip)
                        .help("Runs the assistant over these results. It costs a model call.")
                #else
                    // PLAIN, with the glass painted by hand. On the phone the
                    // band is a List row, and a Button of any other style in a
                    // row can take a tap anywhere on the row: a stray tap would
                    // spend a model call the reader never asked for.
                    Button { store.startDeeperSearch(trigger: trigger) } label: {
                        Text("ask the agent")
                            .font(Typo.chip)
                            .foregroundStyle(Palette.ink)
                            .padding(.horizontal, Space.s12)
                            .padding(.vertical, Space.s6)
                            .glassCapsule()
                            .contentShape(Capsule())
                    }
                    .buttonStyle(.plain)
                #endif
            }
        }
    }

    /// The newest tool chip: what the lane is doing right now, reduced to its
    /// own one-line summary.
    private var latestTool: ToolActivity? {
        session.transcript.last { $0.kind == .tool }?.tool
    }

    /// Every batch of cards the lane has shown, oldest first. Identifiable
    /// rows, because a refined search shows a second set beneath the first.
    private var cardRows: [ChatItem] {
        session.transcript.filter { $0.kind == .emails && !$0.emails.isEmpty }
    }

    /// The one line of prose, which is the last thing the lane said. Earlier
    /// assistant text belongs to an earlier narrowing of the same search and is
    /// answered by the line that replaced it.
    private var answerLine: String? {
        session.transcript.last { $0.kind == .assistant }?.text
    }

    /// A failed run, in the panel's own note style.
    private var errorLine: String? {
        session.transcript.last { $0.kind == .error }?.text
    }
}
