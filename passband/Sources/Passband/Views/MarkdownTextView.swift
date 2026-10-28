// The live markdown editor: an NSTextView that re-styles itself on every edit
// from `Markdown.spans`, with the syntax markers kept visible — `**asdf**`
// shows its stars and reads bold. An NSViewRepresentable rather than a
// TextEditor because live re-attribution is NSTextStorage's home turf, and the
// key monitor's input guard already speaks NSTextView (see KeyMonitor.isEditing).
//
// The contract with the composers: it binds the SAME Binding<String> the plain
// TextEditor did — every keystroke lands in ComposeState through the caller's
// setter, so the DraftSaver hook there keeps arming autosaves. Plain Enter
// stays a newline (the ceremony's Enter binding declines in edit phase and the
// event falls through to this view).
//
// THE APPKIT HALF, AND ONLY THAT. The span → attributes pass lives in
// Views/MarkdownStyle.swift and is literally the same code on both platforms;
// the UIKit twin is Sources/PassbandiOS/Views/MarkdownTextViewiOS.swift, which
// declares `MarkdownTextView` under this exact name so no composer forks on
// which OS it is running.

import AppKit
import SwiftUI

struct MarkdownTextView: NSViewRepresentable {
    @Binding var text: String
    /// Grab the cursor when the editor appears. Same affordance as the plain
    /// editors this replaces: `r` must land the caret in the body.
    var autofocus = false
    /// Take the caret AGAIN, after mount: each new value is one request
    /// (`EditorRefocus.take`). The inline reply bumps it when the time palette
    /// closes over it.
    var refocus = 0
    var disabled = false
    /// Files dropped ON THE EDITOR, with the UTF-16 offset of the drop point,
    /// so a picture lands where it was let go rather than at the end. nil
    /// leaves AppKit's own file-drop behaviour (which pastes the path).
    var onDropFiles: (([URL], Int?) -> Void)? = nil
    /// A picture pasted with ⌘V — a screenshot, a copied image — as PNG bytes
    /// with the caret's offset. Text pastes are untouched.
    var onPasteImage: ((Data, Int?) -> Void)? = nil
    /// A file is being dragged over the editor (true) or has left it (false),
    /// so the composer can draw the same border it draws for the rest of the
    /// pane — AppKit claims the drag here, and SwiftUI's `isTargeted` never
    /// hears about it.
    var onDropHover: ((Bool) -> Void)? = nil
    /// GROW WITH THE TEXT INSTEAD OF SCROLLING IT. Given this, the editor never
    /// scrolls inside itself: it reports the height its text needs, on every
    /// edit and every re-wrap, and the caller sizes the frame to that. The
    /// wheel passes straight through to whatever scrolls the page around it,
    /// and the caret is kept in view by scrolling THAT (see `revealCaret`).
    /// The reader's inline reply is the one caller: its composer is a row of
    /// the conversation, and a box with its own scroll inside a scrolling
    /// column is two scrolls fighting over one wheel.
    var onContentHeight: ((CGFloat) -> Void)? = nil

    /// The phone's editor carries the paperclip on its keyboard bar and needs
    /// to know which draft it feeds. The Mac's paperclip is a chip in the
    /// composer's footer, so this is here only so the composers do not fork.
    func attaching(_ slot: DraftSaver.Slot?) -> MarkdownTextView { self }

    func makeNSView(context: Context) -> NSScrollView {
        let view = HighlightingTextView()
        view.delegate = context.coordinator
        view.onDropFiles = onDropFiles
        view.onPasteImage = onPasteImage
        view.onDropHover = onDropHover
        view.string = text
        view.allowsUndo = true
        view.isRichText = false  // attributes are OURS; no pasted fonts
        view.textColor = NSColor(Palette.ink)
        view.insertionPointColor = NSColor(Palette.accent)
        view.drawsBackground = false
        view.isAutomaticQuoteSubstitutionEnabled = false  // curly quotes break markers
        view.isAutomaticDashSubstitutionEnabled = false
        view.autoresizingMask = [.width]
        view.textContainer?.widthTracksTextView = true
        // The base font, the inset and the first styling pass: the same call a
        // density flip makes later, so mount and flip cannot disagree.
        view.applyDensity()
        // A body can open non-empty (the seeded signature); the caret belongs
        // at the top, above it, not after it where setString parks it.
        view.setSelectedRange(NSRange(location: 0, length: 0))

        let fits = onContentHeight != nil
        if fits {
            view.fitsContent = true
            view.isVerticallyResizable = true
            view.maxSize = NSSize(
                width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
            view.onContentHeight = { [coordinator = context.coordinator] height in
                coordinator.deliver(height)
            }
        }

        let scroll = fits ? FittedEditorScrollView() : EditorScrollView()
        scroll.documentView = view
        scroll.hasVerticalScroller = !fits
        scroll.drawsBackground = false
        scroll.verticalScrollElasticity = fits ? .none : .automatic

        if autofocus {
            // The window exists only after mount; one hop later is soon enough.
            DispatchQueue.main.async { view.window?.makeFirstResponder(view) }
        }
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let view = scroll.documentView as? HighlightingTextView else { return }
        // The representable is a fresh value every render; the coordinator is
        // not. Without this the delegate writes into the first render's binding.
        context.coordinator.parent = self
        // A height measured in here is reported a hop later: see `deliver`.
        context.coordinator.updating = true
        defer { context.coordinator.updating = false }
        view.isEditable = !disabled
        // Fresh closures every render, like the binding.
        view.onDropFiles = onDropFiles
        view.onPasteImage = onPasteImage
        view.onDropHover = onDropHover
        if EditorRefocus.take(refocus, seen: &context.coordinator.focusSeen) {
            // After the overlay that had the caret has unmounted.
            DispatchQueue.main.async { view.window?.makeFirstResponder(view) }
        }
        // THE DENSITY SETTING FLIPPED WITH A DRAFT OPEN. The text view built
        // its fonts from the density live at the last pass and observes
        // nothing, so the representable is what notices: reading the setting
        // here makes this update run when it changes, and the coordinator
        // remembers which density the view was last styled at so the pass
        // runs once per flip, not once per render. Attributes only, through
        // `applyDensity`: the characters, the selection and the undo stack
        // are never touched.
        let density = Prefs.shared.density
        if context.coordinator.density != density {
            context.coordinator.density = density
            view.applyDensity()
            view.noteContentHeight()
        }
        // Only external changes (a draft restore) land here — the coordinator
        // wrote user edits into the binding already, and re-setting the string
        // for those would throw away the selection.
        if view.string != text {
            // TWO KINDS OF EXTERNAL CHANGE, and they want opposite carets. A
            // draft RESTORE lands a whole body into an untouched editor, and
            // reading starts from the top. A DROPPED PICTURE inserts one
            // marker into text somebody is typing, and the caret has to end
            // up after the insertion — sending it to the top would drop the
            // next keystroke above everything written so far.
            //
            // AND THEY WANT DIFFERENT MECHANICS. A restore may simply set the
            // string. An edit under a live editor goes through the text
            // system as a REPLACEMENT of the changed run, so it joins the
            // undo stack in order: `string =` bypasses the undo manager, and
            // the groups recorded before it keep ranges that no longer exist
            // — ⌘Z after a drop would then cut a marker in half, or throw
            // past the end of a body a tray click had shortened.
            if Prefs.shared.isBodyUntouched(view.string) {
                // `string =` bypasses the undo manager; anything recorded
                // before it names ranges in a body that is no longer here.
                context.coordinator.undo.removeAllActions()
                view.string = text
                view.rehighlight()
                view.setSelectedRange(NSRange(location: 0, length: 0))
                view.scrollRangeToVisible(NSRange(location: 0, length: 0))
                // `string =` tells no delegate, so nobody else will.
                view.noteContentHeight()
            } else {
                view.replaceExternally(with: text)
            }
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    @MainActor final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: MarkdownTextView
        /// The last `refocus` answered; starts at the mount's, which
        /// `autofocus` covers.
        var focusSeen: Int
        /// The density the text view was last styled at (see `updateNSView`).
        var density: DensityChoice
        /// True for the length of `updateNSView`.
        var updating = false
        init(_ parent: MarkdownTextView) {
            self.parent = parent
            self.focusSeen = parent.refocus
            self.density = Prefs.shared.density
        }

        /// Hand a measured height to the caller, who keeps it in view state.
        /// A keystroke reports AT ONCE, so the frame grows in the same pass
        /// that draws the new line and the text never spends a frame clipped.
        /// A height found during a SwiftUI update (a restore, a density flip)
        /// waits one hop, because state must not be written from inside one.
        func deliver(_ height: CGFloat) {
            if updating {
                DispatchQueue.main.async { [weak self] in self?.parent.onContentHeight?(height) }
            } else {
                parent.onContentHeight?(height)
            }
        }

        /// THIS EDITOR'S OWN HISTORY. Left to itself the text view registers
        /// into the window's undo manager, the one SwiftUI hands the whole
        /// shell, and ⌘Z typed here did not take back what was typed here.
        /// One stack per editor also means undo never reaches past this
        /// message into a field, or a composer, that is gone.
        let undo = UndoManager()

        func undoManager(for view: NSTextView) -> UndoManager? { undo }

        func textDidChange(_ notification: Notification) {
            guard let view = notification.object as? HighlightingTextView else { return }
            parent.text = view.string
            view.rehighlight()
            view.noteContentHeight()
            view.revealCaretSoon()
        }

        /// The arrows, Page Down, ⌘↓: the caret moved and nothing was typed.
        /// A pointer is left alone. A click lands on text that is on screen
        /// by definition, and a drag autoscrolls on its own.
        func textViewDidChangeSelection(_ notification: Notification) {
            guard let view = notification.object as? HighlightingTextView, view.fitsContent
            else { return }
            switch NSApp.currentEvent?.type {
            case .leftMouseDown, .leftMouseDragged, .leftMouseUp: return
            default: view.revealCaretSoon()
            }
        }
    }
}

/// THE EDITOR IS NOT A WINDOW HANDLE, and the only answer AppKit hears in
/// time is the WINDOW's. The shell window moves by its background, and with
/// `isMovableByWindowBackground` on at the moment of the mouse-down, a drag
/// into the editor moved the window — even with the text view, its clip view
/// and this scroller all answering no to `mouseDownCanMoveWindow` (logged on
/// a real mouse: every view under the pointer said no, and the window moved).
/// The decision is made before the event reaches any view or monitor, so it
/// has to be settled BEFORE the click: the flag goes off while the pointer is
/// over the editor, and back on when it leaves.
class EditorScrollView: NSScrollView {
    private var hover: NSTrackingArea?

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let hover { removeTrackingArea(hover) }
        let area = NSTrackingArea(
            rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect],
            owner: self)
        addTrackingArea(area)
        hover = area
    }

    override func mouseEntered(with event: NSEvent) {
        super.mouseEntered(with: event)
        window?.isMovableByWindowBackground = false
    }

    override func mouseExited(with event: NSEvent) {
        super.mouseExited(with: event)
        window?.isMovableByWindowBackground = true
    }

    /// Leaving the window mid-hover (the composer closing under the pointer)
    /// sends no exit; the window is a handle again without us.
    override func viewWillMove(toWindow newWindow: NSWindow?) {
        if newWindow == nil { window?.isMovableByWindowBackground = true }
        super.viewWillMove(toWindow: newWindow)
    }
}

/// THE EDITOR THAT GROWS (`MarkdownTextView.onContentHeight`). Its frame is
/// always at least as tall as its text, so there is nothing in here to scroll
/// and three things follow from that.
final class FittedEditorScrollView: EditorScrollView {
    /// The wheel belongs to the page. A scroll view takes every wheel event
    /// over it, with or without anywhere to go, so a pointer resting on the
    /// reply would stop the conversation scrolling.
    override func scrollWheel(with event: NSEvent) {
        nextResponder?.scrollWheel(with: event)
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        guard let text = documentView as? HighlightingTextView else { return }
        // The text view fills the frame even when the text does not, so a
        // click in the empty part of a short reply still lands the caret.
        text.minSize = NSSize(width: 0, height: contentSize.height)
        text.sizeToFit()
        // AppKit scrolled the new line into view inside here before the frame
        // had grown to hold it. The frame holds it now.
        contentView.scroll(to: .zero)
        reflectScrolledClipView(contentView)
        text.revealCaretSoon()
    }
}

/// The NSTextView half: owns the re-style pass. Attribute changes never touch
/// the characters, so the selection and undo stack survive every pass.
final class HighlightingTextView: NSTextView {
    var onDropFiles: (([URL], Int?) -> Void)?
    var onPasteImage: ((Data, Int?) -> Void)?
    var onDropHover: ((Bool) -> Void)?

    // MARK: - growing with the text

    /// See `MarkdownTextView.onContentHeight`. Off for every other editor.
    var fitsContent = false
    var onContentHeight: ((CGFloat) -> Void)?
    private var reportedHeight: CGFloat = -1

    /// Air kept around the caret when the page is scrolled to it, and the
    /// extra kept under the LAST line, which is where a reply is written: the
    /// composer's own foot (its files, its send keys) stays on screen with it.
    private static var caretRoom: CGFloat { Space.s24 }
    private static var footRoom: CGFloat { Space.scaled(64) }

    /// Measure what the text needs at the width it has, and say so if that
    /// changed. The used rect counts the empty line after a trailing newline,
    /// so pressing Return grows the editor before anything is typed on it.
    func noteContentHeight() {
        guard fitsContent, let onContentHeight, let layoutManager, let textContainer else { return }
        layoutManager.ensureLayout(for: textContainer)
        let height = ceil(
            layoutManager.usedRect(for: textContainer).height + textContainerInset.height * 2)
        guard abs(height - reportedHeight) > 0.5 else { return }
        reportedHeight = height
        onContentHeight(height)
    }

    /// A new width is a new wrap. Measured a hop later: this runs inside a
    /// layout pass, and the measurement lays text out itself.
    override func setFrameSize(_ newSize: NSSize) {
        let rewrapped = newSize.width != frame.width
        super.setFrameSize(newSize)
        if fitsContent, rewrapped {
            DispatchQueue.main.async { [weak self] in self?.noteContentHeight() }
        }
    }

    /// After the frame has caught up with the edit that moved the caret.
    func revealCaretSoon() {
        guard fitsContent else { return }
        DispatchQueue.main.async { [weak self] in self?.revealCaret() }
    }

    /// KEEP THE CARET ON SCREEN BY SCROLLING THE PAGE. A text view does this
    /// for itself inside its own scroll view, and this one has nothing to
    /// scroll there, so the request goes to the nearest clip view ABOVE the
    /// editor: the conversation's. Only while this view has the caret, so an
    /// editor nobody is typing in never moves the mail.
    private func revealCaret() {
        guard let window, window.firstResponder === self, let host = enclosingScrollView,
            var rect = caretLine()
        else { return }
        let onLastLine = rect.maxY >= reportedHeight - textContainerInset.height - rect.height
        rect = rect.insetBy(dx: 0, dy: -Self.caretRoom)
        if onLastLine { rect.size.height += Self.footRoom }
        host.scrollToVisible(convert(rect, to: host))
    }

    /// The line the caret is on, in this view's coordinates. Asked of the
    /// layout and not of `firstRect(forCharacterRange:)`, which answers only
    /// for text that is on screen and says zero for the rest: the one case
    /// this exists for is a caret that is NOT on screen (⌘↓ from the top of a
    /// long reply).
    private func caretLine() -> NSRect? {
        guard let layoutManager, let textContainer else { return nil }
        layoutManager.ensureLayout(for: textContainer)
        let caret = selectedRange().location
        let length = (string as NSString).length
        var line = NSRect.zero
        if caret >= length {
            // After the last character: the empty line a trailing newline
            // opens (or an empty editor has), else the last line of text.
            line = layoutManager.extraLineFragmentRect
            if line.height == 0, length > 0 {
                line = layoutManager.lineFragmentRect(
                    forGlyphAt: layoutManager.glyphIndexForCharacter(at: length - 1),
                    effectiveRange: nil)
            }
        } else {
            line = layoutManager.lineFragmentRect(
                forGlyphAt: layoutManager.glyphIndexForCharacter(at: caret), effectiveRange: nil)
        }
        guard line.height > 0 else { return nil }
        return line.offsetBy(dx: textContainerOrigin.x, dy: textContainerOrigin.y)
    }

    // MARK: - the binding writing under a live editor

    /// Land `text` as ONE replacement of the run that differs, through the
    /// text system, so the undo manager records it in sequence and the caret
    /// ends after the change (see `updateNSView`). The run is the smallest
    /// span between a common prefix and a common suffix; for the marker a
    /// drop inserts, that is the marker and its line breaks.
    func replaceExternally(with text: String) {
        let old = string
        let o = Array(old.utf16), n = Array(text.utf16)
        var prefix = 0
        while prefix < o.count, prefix < n.count, o[prefix] == n[prefix] { prefix += 1 }
        var suffix = 0
        while suffix < o.count - prefix, suffix < n.count - prefix,
            o[o.count - 1 - suffix] == n[n.count - 1 - suffix]
        {
            suffix += 1
        }
        let range = NSRange(location: prefix, length: o.count - prefix - suffix)
        let replacement = String(utf16CodeUnits: Array(n[prefix..<(n.count - suffix)]), count: n.count - prefix - suffix)
        let caret = selectedRange().location
        guard shouldChangeText(in: range, replacementString: replacement) else {
            // Refused (a field editor mid-edit, in theory): fall back to the
            // blunt set rather than leaving the editor out of step.
            string = text
            rehighlight()
            return
        }
        textStorage?.replaceCharacters(in: range, with: replacement)
        didChangeText()  // -> the delegate's textDidChange: binding + restyle
        let target = ComposeMarkers.caretAfterEdit(old: old, new: text, caret: caret)
        setSelectedRange(NSRange(location: target, length: 0))
        scrollRangeToVisible(NSRange(location: target, length: 0))
    }

    // MARK: - undo

    /// ⌘Z AND ⇧⌘Z ARE THE EDITOR'S while it has the caret. The Inbox menu
    /// binds ⌘Z too ("Undo Last Action", for a done or a send), and a menu
    /// key equivalent is a race a text view should not have to win: the key
    /// window is asked before the menu bar, so answering here settles it.
    /// Claimed even with nothing to undo — a spare ⌘Z while writing must not
    /// quietly bring back a thread marked done a moment ago.
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard window?.firstResponder === self,
            event.charactersIgnoringModifiers?.lowercased() == "z"
        else { return super.performKeyEquivalent(with: event) }
        let chord = event.modifierFlags.intersection([.command, .shift, .option, .control])
        // Read-only while a send is in flight: the chord is still ours (it
        // must not fall through to the Inbox undo), but the body stays put.
        if !isEditable, chord == .command || chord == [.command, .shift] { return true }
        switch chord {
        case .command:
            if undoManager?.canUndo == true { undoManager?.undo() }
            return true
        case [.command, .shift]:
            if undoManager?.canRedo == true { undoManager?.redo() }
            return true
        default:
            return super.performKeyEquivalent(with: event)
        }
    }

    // MARK: - files in

    // THE EDITOR IS A DROP TARGET FOR FILES, and it has to be this view that
    // says so: AppKit delivers a drag to the deepest view under the pointer,
    // and NSTextView already accepts file URLs — by inserting their PATHS as
    // text, which is never what dropping a photo on a mail means. So the drag
    // is claimed here when there is a handler and a file, and handed up with
    // the character the pointer is over; everything else falls through to
    // AppKit (text drags still work).

    private func hasFiles(_ info: NSDraggingInfo) -> Bool {
        onDropFiles != nil && !ComposeDrop.fileURLs(on: info.draggingPasteboard).isEmpty
    }

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        guard hasFiles(sender) else { return super.draggingEntered(sender) }
        onDropHover?(true)
        return .copy
    }

    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation {
        hasFiles(sender) ? .copy : super.draggingUpdated(sender)
    }

    override func draggingExited(_ sender: NSDraggingInfo?) {
        onDropHover?(false)
        super.draggingExited(sender)
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        onDropHover?(false)
        guard let onDropFiles, hasFiles(sender) else {
            return super.performDragOperation(sender)
        }
        let urls = ComposeDrop.fileURLs(on: sender.draggingPasteboard)
        let point = convert(sender.draggingLocation, from: nil)
        onDropFiles(urls, characterIndexForInsertion(at: point))
        return true
    }

    /// ⌘V with a file copied in the Finder attaches that file; ⌘V with a
    /// picture on the clipboard attaches it. A FILE WINS OVER TEXT: the Finder
    /// puts the file's NAME on the pasteboard as a string beside the url, and
    /// a copied text selection never carries a file url, so the url is the
    /// tell. A picture beside text is a text paste (a copied web selection) —
    /// see `ComposeDrop.imagePNG`.
    override func paste(_ sender: Any?) {
        let pasteboard = NSPasteboard.general
        let files = onDropFiles == nil ? [] : ComposeDrop.fileURLs(on: pasteboard)
        let png = files.isEmpty && onPasteImage != nil ? ComposeDrop.imagePNG(on: pasteboard) : nil
        guard !files.isEmpty || png != nil else {
            super.paste(sender)
            return
        }
        // A paste REPLACES the selection, as every paste does: the selected
        // words go through the text system (undoable) and the file lands
        // where they were.
        let selection = selectedRange()
        if selection.length > 0 { insertText("", replacementRange: selection) }
        let caret = selectedRange().location
        if !files.isEmpty, let onDropFiles {
            onDropFiles(files, caret)
        } else if let png, let onPasteImage {
            onPasteImage(png, caret)
        }
    }

    /// Everything about the editor that is sized by the density setting, set
    /// from the current one, then the styling pass over the text already here.
    /// Run at mount and again on every flip (`MarkdownTextView.updateNSView`).
    ///
    /// The horizontal inset stays 2 in both: with the line fragment's 5 it is
    /// the 7 the composer subtracts to put the body's left edge on the header
    /// labels' edge, an alignment rather than room. The typing attributes are
    /// reset because AppKit last derived them from the old fonts, and in an
    /// empty editor they are what sizes the caret and the first letter.
    func applyDensity() {
        font = MarkdownStyle.baseFont
        textContainerInset = NSSize(width: 2, height: Space.s4)
        rehighlight()
        typingAttributes = MarkdownStyle.base
    }

    func rehighlight() {
        guard let storage = textStorage else { return }
        let all = NSRange(location: 0, length: storage.length)
        storage.beginEditing()
        storage.setAttributes(MarkdownStyle.base, range: all)
        for span in Markdown.spans(of: string) {
            guard span.range.location + span.range.length <= storage.length else { continue }
            MarkdownStyle.apply(span, to: storage)
        }
        storage.endEditing()
    }

    // Palette colors are appearance-dynamic providers, but font CHOICES made
    // per-span are not — restyle when the theme flips so nothing goes stale.
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        rehighlight()
    }
}
