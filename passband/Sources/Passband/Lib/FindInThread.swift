// FIND IN THE OPEN EMAIL (⌘F in the reader). Three pieces, and this file is the
// part of each that needs no view system, so it builds alone for its suite:
//
//   * `FindScript`: what runs INSIDE a mail frame. A thread is one sandboxed
//     web frame per html message, so there is no single document to search;
//     each frame searches its own, paints its matches, and answers with a
//     count and where the active one is.
//   * `TextFind`: the same question asked of a plain string, for the messages
//     that are not html and for the ones the lazy stack has not mounted yet.
//   * `FindWalk` + `ReaderFind`: the cursor that walks matches ACROSS those
//     messages, which is the only place the thread is one thing.

import Foundation
import Observation

/// What a body is asked to show: the term, and which of its own matches is the
/// active one (`none` when the cursor is in some other message).
struct FindRequest: Equatable, Sendable {
    var query: String
    var active: Int

    static let none = -1
}

/// What a mail frame answers with.
struct FindResult: Equatable, Sendable {
    var count: Int
    /// The match the frame actually made active: the request's, clamped to
    /// what the document holds. `FindRequest.none` when it was asked for none.
    var active: Int
    /// The active match's top edge and height in the document's own points.
    var y: CGFloat
    var height: CGFloat

    /// From an `evaluateJavaScript` result. nil for a frame that could not
    /// answer at all (no highlight support), which is not the same as zero.
    init?(payload: Any?) {
        guard let dict = payload as? [String: Any],
            let count = (dict["count"] as? NSNumber)?.intValue
        else { return nil }
        self.count = count
        active = (dict["active"] as? NSNumber)?.intValue ?? FindRequest.none
        y = CGFloat((dict["y"] as? NSNumber)?.doubleValue ?? 0)
        height = CGFloat((dict["h"] as? NSNumber)?.doubleValue ?? 0)
    }

    init(count: Int, active: Int = FindRequest.none, y: CGFloat = 0, height: CGFloat = 0) {
        self.count = count
        self.active = active
        self.y = y
        self.height = height
    }
}

/// Matches in a plain string: case-insensitive, non-overlapping, in order.
enum TextFind {
    /// The same ceiling the frames keep, so a one-letter term in a long thread
    /// costs a bounded amount of painting wherever it is asked.
    static let limit = 500

    static func ranges(of query: String, in text: String) -> [Range<String.Index>] {
        guard !query.isEmpty else { return [] }
        var found: [Range<String.Index>] = []
        var from = text.startIndex
        while found.count < limit, from < text.endIndex,
            let hit = text.range(of: query, options: [.caseInsensitive], range: from..<text.endIndex)
        {
            found.append(hit)
            // An empty hit cannot happen for a non-empty term, but a loop that
            // trusts that is a loop that can spin.
            from = hit.isEmpty ? text.index(after: hit.lowerBound) : hit.upperBound
        }
        return found
    }

    static func count(of query: String, in text: String) -> Int {
        ranges(of: query, in: text).count
    }
}

/// Where the cursor is: a message, and a match inside it.
struct FindCursor: Equatable, Sendable {
    var message: Int
    var match: Int
}

/// Stepping the cursor over a thread. Pure: the messages in display order, and
/// a way to ask how many matches each one holds.
enum FindWalk {
    /// "The last match in that message", for a step backwards into a message
    /// whose real count is not known until its frame answers. The frame clamps
    /// it and reports the index it landed on.
    static let last = Int.max

    /// The first match at or after display position `start`, wrapping.
    static func first(in order: [Int], from start: Int, count: (Int) -> Int) -> FindCursor? {
        seek(in: order, from: start, by: 1, count: count)
    }

    static func step(
        _ cursor: FindCursor?, by delta: Int, in order: [Int], from start: Int,
        count: (Int) -> Int
    ) -> FindCursor? {
        guard let cursor, let position = order.firstIndex(of: cursor.message) else {
            return seek(in: order, from: start, by: delta, count: count)
        }
        let held = count(cursor.message)
        if held > 0 {
            let next = min(cursor.match, held - 1) + delta
            if next >= 0, next < held { return FindCursor(message: cursor.message, match: next) }
        }
        // Off the end of this message: the next one that holds anything, and
        // round to this one again when it is the only one that does.
        return seek(in: order, from: position + delta, by: delta, count: count)
    }

    private static func seek(
        in order: [Int], from start: Int, by delta: Int, count: (Int) -> Int
    ) -> FindCursor? {
        guard !order.isEmpty else { return nil }
        let direction = delta < 0 ? -1 : 1
        for offset in 0..<order.count {
            let raw = (start + direction * offset) % order.count
            let position = raw < 0 ? raw + order.count : raw
            let id = order[position]
            if count(id) > 0 {
                return FindCursor(message: id, match: direction < 0 ? last : 0)
            }
        }
        return nil
    }

    /// "3" of "3 of 12": every match in the messages above the cursor's, plus
    /// its own place in its message.
    static func ordinal(_ cursor: FindCursor, in order: [Int], count: (Int) -> Int) -> Int {
        var before = 0
        for id in order {
            if id == cursor.message { break }
            before += count(id)
        }
        return before + min(cursor.match, max(0, count(cursor.message) - 1)) + 1
    }
}

/// The reader's find session: one per open reader, handed down the environment
/// so the bodies (a mail frame, a plain-text block) can read what they are
/// asked to show and report what they found WITHOUT the message cards between
/// them and the reader having to carry any of it. A card diffs on its data
/// (see MessageCard's `==`); a find term is not its data.
@MainActor
@Observable
final class ReaderFind {
    /// The scroll target the body holding the active match pins to it. One at
    /// a time, because only one body holds the cursor.
    nonisolated static let anchorID = "reader-find-anchor"

    private(set) var open = false
    var query = ""
    private(set) var cursor: FindCursor?
    /// The active match has been placed (or moved): the reader should look at
    /// where the anchor is and bring it into the window if it is not.
    private(set) var aimTick = 0
    /// ⌘F again while the bar is already up puts the caret back in it.
    private(set) var focusTick = 0

    private var order: [Int] = []
    @ObservationIgnored private var texts: [Int: String] = [:]
    /// What each message's TEXT says it holds, for the ones no body has
    /// answered for: the lazy stack mounts a handful of cards, and a count
    /// that only knew about those would change as you scrolled.
    private var estimates: [Int: Int] = [:]
    /// What a mounted body actually found, which is the truth and outranks the
    /// estimate: the frame searches what is drawn, folded history left out.
    private var reported: [Int: Int] = [:]
    /// The display position a search starts from: the message being read.
    @ObservationIgnored private var start = 0

    /// Where the active match is in the window right now, written by the
    /// anchor's own geometry. Not observed: it changes on every scroll tick.
    @ObservationIgnored private(set) var anchorMinY: CGFloat?
    @ObservationIgnored private var anchorOwner: UUID?
    /// The caret is in the bar's field, which is when Enter means "next match"
    /// rather than whatever the reader binds it to.
    @ObservationIgnored var fieldFocused = false
    /// The reader moved the selection to follow the cursor and has already
    /// scrolled for it; the selection's own scroll must stand down once.
    @ObservationIgnored var steering = false

    func begin(messages: [(id: Int, text: String)], from position: Int) {
        adopt(messages)
        start = position
        open = true
        focusTick += 1
        if !query.isEmpty { research() }
    }

    func close() {
        open = false
        cursor = nil
        reported = [:]
        anchorOwner = nil
        anchorMinY = nil
    }

    /// The thread under the session changed: a reply landed, a refetch.
    func adopt(_ messages: [(id: Int, text: String)]) {
        order = messages.map(\.id)
        texts = Dictionary(messages.map { ($0.id, $0.text) }, uniquingKeysWith: { a, _ in a })
        guard open else { return }
        estimates = texts.mapValues { TextFind.count(of: query, in: $0) }
        if let cursor, !order.contains(cursor.message) { self.cursor = nil }
    }

    func queryChanged(from position: Int) {
        start = position
        research()
    }

    private func research() {
        reported = [:]
        estimates = texts.mapValues { TextFind.count(of: query, in: $0) }
        cursor = FindWalk.first(in: order, from: start, count: matches)
    }

    private func matches(_ id: Int) -> Int { reported[id] ?? estimates[id] ?? 0 }

    var total: Int { order.reduce(0) { $0 + matches($1) } }
    var ordinal: Int? {
        cursor.map { FindWalk.ordinal($0, in: order, count: matches) }
    }

    func step(_ delta: Int) {
        guard open, !query.isEmpty else { return }
        cursor = FindWalk.step(cursor, by: delta, in: order, from: start, count: matches)
    }

    func request(for id: Int) -> FindRequest? {
        guard open, !query.isEmpty else { return nil }
        let active = cursor.flatMap { $0.message == id ? $0.match : nil }
        return FindRequest(query: query, active: active ?? FindRequest.none)
    }

    /// A body's answer for the term in force. `active` is the index it made
    /// active, when it was asked for one.
    func report(_ id: Int, query answered: String, count found: Int, active: Int = FindRequest.none) {
        guard open, answered == query else { return }
        if reported[id] != found { reported[id] = found }
        guard let held = cursor else {
            // Nothing had a match by its text, and this body found one anyway.
            if found > 0 { cursor = FindCursor(message: id, match: 0) }
            return
        }
        guard held.message == id else { return }
        if found == 0 {
            // The text promised a match the drawn document does not have.
            let position = order.firstIndex(of: id) ?? start
            cursor = FindWalk.first(in: order, from: position + 1, count: matches)
        } else if active >= 0, held.match != active {
            cursor = FindCursor(message: id, match: active)
        } else if held.match >= found {
            cursor = FindCursor(message: id, match: found - 1)
        }
    }

    func aim() { aimTick += 1 }

    func noteAnchor(_ owner: UUID, minY: CGFloat) {
        anchorOwner = owner
        anchorMinY = minY
    }

    /// Only the anchor that last spoke may withdraw the position.
    func dropAnchor(_ owner: UUID) {
        guard anchorOwner == owner else { return }
        anchorOwner = nil
        anchorMinY = nil
    }
}

/// The half that runs inside a mail frame, injected beside the measuring
/// script. Page script is off in these frames (layer 2); this is ours.
///
/// THE HIGHLIGHTS ARE PAINT, NOT MARKUP. The CSS Custom Highlight API colours
/// ranges without touching the DOM, so a find never changes a document's
/// height: a frame is sized from what its document reports, and a `<mark>`
/// wrapped round every hit would re-measure every open message on every
/// keystroke.
///
/// The haystack is the document's text as it READS: runs of whitespace are one
/// space (html source wraps where the sentence does not), a block boundary or
/// a `<br>` is a space too, and the invisible padding newsletters stuff their
/// preheaders with is dropped. A match that is not drawn (folded history, a
/// hidden preheader) is not a match.
enum FindScript {
    static let source = """
        (function () {
          if (window.__passbandFind) return;
          var ALL = 'passband-find', ACTIVE = 'passband-find-active';
          var LIMIT = \(TextFind.limit), PROBES = 4000;
          var BLOCKS = 'address,article,aside,blockquote,body,center,dd,div,dl,dt,' +
            'fieldset,figure,footer,form,h1,h2,h3,h4,h5,h6,header,hr,li,main,nav,ol,p,' +
            'pre,section,table,tbody,td,tfoot,th,thead,tr,ul';
          var SKIP = { STYLE: 1, SCRIPT: 1, NOSCRIPT: 1, TEMPLATE: 1, TITLE: 1, HEAD: 1 };
          var state = { query: null, ranges: [] };

          function supported() {
            return !!(window.CSS && CSS.highlights && window.Highlight);
          }
          function isSpace(code) {
            return code <= 32 || code === 160 || (code >= 8192 && code <= 8202) ||
              code === 8232 || code === 8233 || code === 8239 || code === 12288;
          }
          // Soft hyphen, zero-width space / non-joiner / joiner, word joiner, BOM.
          function isDropped(code) {
            return code === 173 || (code >= 8203 && code <= 8205) || code === 8288 ||
              code === 65279;
          }
          function lower(c) {
            var l = c.toLowerCase();
            return l.length === 1 ? l : c;
          }
          function fold(text) {
            var out = '', space = false;
            for (var i = 0; i < text.length; i++) {
              var code = text.charCodeAt(i);
              if (isDropped(code)) continue;
              if (isSpace(code)) {
                if (!space) out += ' ';
                space = true;
                continue;
              }
              space = false;
              out += lower(text.charAt(i));
            }
            return out;
          }

          function index(body) {
            var chars = [], nodes = [], starts = [], ends = [];
            var space = true, lastNode = null, lastBlock = null;
            // A boundary reads as a space. It belongs to no character, so it is
            // pinned to the end of the text before it.
            function gap() {
              if (space || !lastNode) return;
              var n = lastNode.nodeValue.length;
              chars.push(' '); nodes.push(lastNode); starts.push(n); ends.push(n);
              space = true;
            }
            var walker = document.createTreeWalker(
              body, NodeFilter.SHOW_TEXT | NodeFilter.SHOW_ELEMENT, {
                acceptNode: function (n) {
                  if (n.nodeType !== 1) return NodeFilter.FILTER_ACCEPT;
                  if (SKIP[n.nodeName]) return NodeFilter.FILTER_REJECT;
                  return n.nodeName === 'BR' ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP;
                }
              });
            var node;
            while ((node = walker.nextNode())) {
              if (node.nodeType === 1) { gap(); continue; }
              var parent = node.parentElement;
              var block = parent ? parent.closest(BLOCKS) : null;
              if (block !== lastBlock) { gap(); lastBlock = block; }
              var text = node.nodeValue;
              for (var i = 0; i < text.length; i++) {
                var code = text.charCodeAt(i);
                if (isDropped(code)) continue;
                var c;
                if (isSpace(code)) {
                  if (space) continue;
                  space = true;
                  c = ' ';
                } else {
                  space = false;
                  c = lower(text.charAt(i));
                }
                chars.push(c); nodes.push(node); starts.push(i); ends.push(i + 1);
                lastNode = node;
              }
            }
            return { hay: chars.join(''), nodes: nodes, starts: starts, ends: ends };
          }

          function drawn(range) {
            var el = range.startContainer.parentElement;
            if (el && el.checkVisibility && !el.checkVisibility({
              checkOpacity: true, checkVisibilityCSS: true,
              opacityProperty: true, visibilityProperty: true
            })) return false;
            var rects = range.getClientRects();
            for (var i = 0; i < rects.length; i++) {
              if (rects[i].width > 0.5 && rects[i].height > 0.5) return true;
            }
            return false;
          }

          function search(query) {
            var body = document.body;
            var needle = fold(query);
            if (!body || !needle) return [];
            var map = index(body);
            var ranges = [], from = 0, probes = 0, at;
            while (ranges.length < LIMIT && probes < PROBES &&
                   (at = map.hay.indexOf(needle, from)) !== -1) {
              probes++;
              var end = at + needle.length - 1;
              var range = document.createRange();
              range.setStart(map.nodes[at], map.starts[at]);
              range.setEnd(map.nodes[end], map.ends[end]);
              if (drawn(range)) ranges.push(range);
              from = at + needle.length;
            }
            return ranges;
          }

          function ensureStyle() {
            if (document.getElementById('passband-find-style')) return;
            var style = document.createElement('style');
            style.id = 'passband-find-style';
            style.textContent =
              '::highlight(' + ALL + '){background-color:#ffe58a;color:#1a1a1a}' +
              '::highlight(' + ACTIVE + '){background-color:#ff9f1c;color:#000}';
            (document.head || document.documentElement).appendChild(style);
          }

          function paint(active) {
            CSS.highlights.delete(ALL);
            CSS.highlights.delete(ACTIVE);
            if (!state.ranges.length) return;
            var rest = new Highlight();
            for (var i = 0; i < state.ranges.length; i++) {
              if (i !== active) rest.add(state.ranges[i]);
            }
            CSS.highlights.set(ALL, rest);
            if (active >= 0) {
              var one = new Highlight(state.ranges[active]);
              one.priority = 1;
              CSS.highlights.set(ACTIVE, one);
            }
          }

          // `fresh` re-reads the document for a term already searched: what is
          // DRAWN changed under it (the quoted history folded or unfolded).
          window.__passbandFind = function (query, active, fresh) {
            if (!supported()) return null;
            if (!query) {
              CSS.highlights.delete(ALL);
              CSS.highlights.delete(ACTIVE);
              state = { query: null, ranges: [] };
              return { count: 0, active: -1 };
            }
            ensureStyle();
            if (fresh || state.query !== query) {
              state = { query: query, ranges: search(query) };
            }
            var count = state.ranges.length;
            var at = (typeof active === 'number' && active >= 0 && count)
              ? Math.min(active, count - 1) : -1;
            paint(at);
            var out = { count: count, active: at };
            if (at >= 0) {
              var rect = state.ranges[at].getBoundingClientRect();
              out.y = rect.top + (window.scrollY || 0);
              out.h = rect.height;
            }
            return out;
          };
        })();
        """
}
