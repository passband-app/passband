// The content-free picture behind session replay: a window reduced to boxes.
//
// A replay can be a recording of LAYOUT rather than of pixels: each frame is a
// tiny HTML page of grey boxes that PostHog's web replay player renders. This
// file builds those boxes from a window's Core Animation layer tree, which
// SwiftUI and AppKit both render into, and it is the whole of what a replay
// can see.
//
// PRIVACY, which is the point of the file. A layer is read for its geometry and
// nothing else:
//
//   * frame, visibility, opacity, clipping: where a box is
//   * WHETHER it has a fill, a border or a corner radius: what kind of box
//
// Paint never leaves. Every box is drawn in one of a few FIXED greys chosen
// here, whatever color the app painted it, because not every fill is theme: a
// reading tile takes its background from the sender's logo, an avatar is hued
// from the sender's name, and a red or green pill says what triage made of a
// mail. Even reduced to grey, luminance told those apart, so the color is not
// read past "is there one". The window's own background is the one exception
// (dark or light theme, see `backgroundPaint`).
//
// It never sends `contents` (the bitmap a layer shows: rendered text, an
// avatar, an image in a mail). A small bitmap may be READ locally by the
// `icon` hook, which only answers "this is the app's own symbol X" and draws
// its own copy (see ReplayIcons.swift); otherwise it is a box. It never reads
// a `CATextLayer`'s `string`, and
// never descends into a web view (where a mail body renders) or a text field
// or text view (where a reply or a search is typed): each is one box at the
// view's own frame. A run of text becomes a bar snapped OUTWARD to an 8pt
// grid on every edge, so neither its length nor its line count is exact.
//
// `ReplayWireframe.validate` checks that on the way out: every String in a
// frame must be a tag from `closedWords`, a style string in exactly our own
// grammar with grey paints, the screen URL, or an image this process rendered
// itself (`registerImage`), or the frame is not sent. It is the same rule as `Analytics.allowedStrings`, applied to a tree.

import Foundation
import QuartzCore

enum ReplayWireframe {
    /// One box on the canvas, in points from the main window's top-left.
    struct Node: Equatable {
        /// `opaque` is a web view or a text field: a box nothing is read inside.
        /// `icon` is one of the app's own symbols, drawn from our render.
        enum Kind { case fill, text, image, opaque, icon }
        var x: Int
        var y: Int
        var w: Int
        var h: Int
        var kind: Kind
        var fill: String? = nil
        var border: String? = nil
        var borderWidth: Int = 0
        var radius: Int = 0
        /// A data URL from `registerImage`, for `.icon` only.
        var image: String? = nil
    }

    /// Answers whether a small bitmap layer is one of the app's own symbols,
    /// with our render of it as a data URL. Mac only (ReplayIcons); nil
    /// leaves every bitmap a box.
    typealias IconLookup = (CALayer, CGSize) -> String?

    /// Every image this process rendered for a replay. The validator passes
    /// an image string only if it is in here, so no other picture can ride.
    private static let imageLock = NSLock()
    nonisolated(unsafe) private static var images: Set<String> = []
    static func registerImage(_ url: String) {
        imageLock.withLock { _ = images.insert(url) }
    }
    static func isRegistered(_ url: String) -> Bool {
        imageLock.withLock { images.contains(url) }
    }

    /// Text bars snap outward to this grid on all four edges, so a bar says
    /// "about this long" and not "exactly this many characters".
    static let textQuantum = 8
    /// A frame stops growing here. A screen that needs more boxes than this is
    /// a list far longer than the window, and the clip has already failed.
    static let nodeCap = 1200

    /// The whole palette. Mid grey at partial alpha reads on both a light and
    /// a dark window, and nested panels darken or lighten by stacking, which
    /// is enough to see a card inside a page inside a window.
    static let panelPaint = "#8080801F"
    static let borderPaint = "#80808059"
    static let textPaint = "#8E8E8E73"
    static let imagePaint = "#8E8E8E33"
    static let opaquePaint = "#8E8E8E1F"
    static let opaqueBorder = "#8E8E8E66"

    /// Walk one layer tree. `root` is the window's content layer; `flipped`
    /// says whether its y already runs downward (a flipped content view);
    /// `offset` places this window on the main window's canvas. `clip` is in
    /// canvas points too, so a scroll view's off-screen rows fall away.
    static func nodes(
        root: CALayer, flipped: Bool, offset: CGPoint, clip: CGRect,
        icon: IconLookup? = nil, into out: inout [Node]
    ) {
        let height = root.bounds.height
        func toCanvas(_ r: CGRect) -> CGRect {
            let y = flipped ? r.minY : height - r.maxY
            return CGRect(x: r.minX + offset.x, y: y + offset.y, width: r.width, height: r.height)
        }
        walk(root, root: root, clip: clip, toCanvas: toCanvas, icon: icon, into: &out)
    }

    private static func walk(
        _ layer: CALayer, root: CALayer, clip: CGRect,
        toCanvas: (CGRect) -> CGRect, icon: IconLookup?, into out: inout [Node]
    ) {
        guard out.count < nodeCap, !layer.isHidden, layer.opacity > 0.02 else { return }
        let frame = toCanvas(root.convert(layer.bounds, from: layer))
        let visible = frame.intersection(clip)
        // A box that does not land on the canvas as plain numbers is treated
        // as off-screen: every edge here ends in an Int, and Int(Double)
        // traps on NaN, infinity, or anything past Int.max.
        let empty = visible.isNull || visible.width < 1 || visible.height < 1 || !onCanvas(visible)

        // A web view is where a mail renders; a text field or text view is
        // where a reply, a search or an address is typed. One box at the
        // view's frame, and the walk stops at its edge, visible or not: an
        // off-screen boundary must not let its insides through either.
        if isOpaqueBoundary(layer) {
            if !empty {
                out.append(
                    node(visible, .opaque, fill: opaquePaint, border: opaqueBorder,
                        borderWidth: 1, radius: layer.cornerRadius > 0 ? 6 : 0))
            }
            return
        }

        guard !empty else {
            // SwiftUI hangs real content off 0×0 container layers that do not
            // clip, so an empty or off-screen layer prunes its subtree only
            // when it masks; otherwise its children are still on the canvas.
            if !layer.masksToBounds {
                for sub in layer.sublayers ?? [] {
                    walk(sub, root: root, clip: clip, toCanvas: toCanvas, icon: icon, into: &out)
                }
            }
            return
        }

        // Liquid Glass: SwiftUI's SDF layer draws its shape through private
        // `CASDF…` sublayers with no plain fill, so the shape stands in as a
        // faint panel. Its OTHER children are the card's content and are
        // walked as usual; only the shape's own machinery is skipped.
        if NSStringFromClass(type(of: layer)) == "SwiftUI.SDFLayer" {
            out.append(
                node(visible, .fill, fill: panelPaint,
                    radius: Int(min(visible.height / 2, 12).rounded())))
            let childClip = layer.masksToBounds ? visible : clip
            for sub in layer.sublayers ?? []
            where !NSStringFromClass(type(of: sub)).hasPrefix("CASDF") {
                walk(sub, root: root, clip: childClip, toCanvas: toCanvas, icon: icon, into: &out)
            }
            return
        }

        // Paint is read only as "is there any": the box gets a fixed grey.
        // Layer geometry is SwiftUI's, not ours, and is not promised finite:
        // 0.0.16 crashed here on a radius Int() could not hold.
        let radius = cornerRadius(layer.cornerRadius)
        if visibleInk(layer.backgroundColor) {
            out.append(node(visible, .fill, fill: panelPaint, radius: radius))
        }
        if layer.borderWidth > 0, visibleInk(layer.borderColor) {
            out.append(node(visible, .fill, border: borderPaint, borderWidth: 1, radius: radius))
        }
        if let shape = layer as? CAShapeLayer, visibleInk(shape.fillColor),
            let path = shape.path
        {
            let box = toCanvas(root.convert(path.boundingBoxOfPath, from: layer)).intersection(clip)
            if !box.isNull, box.width >= 1, box.height >= 1, onCanvas(box) {
                out.append(node(box, .fill, fill: panelPaint, radius: radius))
            }
        }
        // Whatever the layer DRAWS is never sent. A small bitmap that is
        // clearly one of the app's own symbols is drawn as OUR copy of it;
        // anything else is only the fact that it draws something, and which
        // kind of thing that probably is.
        if layer.contents != nil, let icon, frame.width == visible.width,
            frame.height == visible.height, let url = icon(layer, layer.bounds.size)
        {
            var n = node(visible, .icon)
            n.image = url
            out.append(n)
        } else if layer is CATextLayer || (layer.contents != nil && looksLikeText(layer)) {
            out.append(textBar(visible))
        } else if layer.contents != nil {
            out.append(node(visible, .image, fill: imagePaint, radius: radius))
        }

        let childClip = layer.masksToBounds ? visible : clip
        for sub in layer.sublayers ?? [] {
            walk(sub, root: root, clip: childClip, toCanvas: toCanvas, icon: icon, into: &out)
        }
    }

    /// A corner radius as the 0...24 the wire carries. Infinite means fully
    /// round, so it takes the cap; NaN has no shape and takes none.
    static func cornerRadius(_ r: CGFloat) -> Int {
        guard !r.isNaN else { return 0 }
        return Int(min(max(r, 0), 24).rounded())
    }

    /// Whether every edge of a box is a number an Int can hold once snapped
    /// to the grid. A real window is a few thousand points; a million is
    /// already nonsense, and well inside Int.
    private static func onCanvas(_ r: CGRect) -> Bool {
        let limit: CGFloat = 1_000_000
        return [r.minX, r.minY, r.maxX, r.maxY].allSatisfy { $0.isFinite && abs($0) < limit }
    }

    /// SwiftUI draws Text (and vector glyphs) into `CGDrawingLayer`s and real
    /// images into `ImageLayer`s; a `CATextLayer` is text by definition. The
    /// class NAME is the only signal, which is fine: guessing wrong turns an
    /// icon into a bar or a bar into a box, and neither shows anything.
    private static func looksLikeText(_ layer: CALayer) -> Bool {
        let name = NSStringFromClass(type(of: layer))
        return name.contains("Drawing") || name.contains("Text")
    }

    /// The views whose insides are never walked, resolved by class at
    /// runtime so this file needs neither WebKit nor AppKit (and builds for
    /// iOS, where the AppKit names simply resolve to nothing). Matched with
    /// `isKind(of:)` on the layer's delegate, which is the view that owns a
    /// backing layer, so the app's own subclasses (EmailWebView's
    /// PassthroughWebView, any NSTextField) are caught as what they are.
    private static let boundaryClasses: [AnyClass] = [
        "WKWebView", "NSTextView", "NSTextField", "NSTokenField", "QLPreviewView",
        "PDFView", "UITextView", "UITextField",
    ].compactMap { NSClassFromString($0) }

    /// A web view, a text field or text view, or (as a second line, should a
    /// web view ever be embedded some other way) any of WebKit's own layers.
    private static func isOpaqueBoundary(_ layer: CALayer) -> Bool {
        if let view = layer.delegate as? NSObject,
            boundaryClasses.contains(where: { view.isKind(of: $0) })
        {
            return true
        }
        return NSStringFromClass(type(of: layer)).hasPrefix("WK")
    }

    /// Whether a paint would show at all. The only question a color is asked.
    private static func visibleInk(_ color: CGColor?) -> Bool {
        guard let color else { return false }
        return color.alpha > 0.01
    }

    private static func textBar(_ r: CGRect) -> Node {
        // Every edge snaps OUTWARD to the grid, so x and width together no
        // longer give the exact length of a right-aligned or centred run, and
        // height gives lines only to the nearest 8pt. The bar is inset a
        // little inside that box, like a loading skeleton.
        let q = CGFloat(textQuantum)
        let minX = (r.minX / q).rounded(.down) * q
        let maxX = (r.maxX / q).rounded(.up) * q
        let minY = (r.minY / q).rounded(.down) * q
        let maxY = (r.maxY / q).rounded(.up) * q
        return Node(
            x: Int(minX), y: Int(minY) + 2, w: Int(maxX - minX), h: max(4, Int(maxY - minY) - 4),
            kind: .text, fill: textPaint, radius: 3)
    }

    /// Every box snaps OUTWARD to a grid, so its exact size says nothing:
    /// a recipient pill no longer gives the length of the address in it, nor
    /// a badge the number of digits. `opaqueQuantum` is coarser because a
    /// web view sizes itself to the mail it shows, and its exact height
    /// would fingerprint a newsletter. Icons are not snapped: they are the
    /// app's own symbols at the app's own sizes.
    static let boxQuantum = 8
    static let opaqueQuantum = 32

    private static func node(
        _ r: CGRect, _ kind: Node.Kind, fill: String? = nil, border: String? = nil,
        borderWidth: Int = 0, radius: Int = 0
    ) -> Node {
        let q: CGFloat
        switch kind {
        case .icon:
            return Node(
                x: Int(r.minX.rounded()), y: Int(r.minY.rounded()),
                w: Int(r.width.rounded()), h: Int(r.height.rounded()), kind: kind)
        case .opaque: q = CGFloat(opaqueQuantum)
        default: q = CGFloat(boxQuantum)
        }
        let minX = (r.minX / q).rounded(.down) * q
        let minY = (r.minY / q).rounded(.down) * q
        let maxX = (r.maxX / q).rounded(.up) * q
        let maxY = (r.maxY / q).rounded(.up) * q
        return Node(
            x: Int(minX), y: Int(minY), w: Int(maxX - minX), h: Int(maxY - minY), kind: kind,
            fill: fill, border: border, borderWidth: borderWidth, radius: radius)
    }

    /// The WINDOW's background as a grey #LLLLLLAA, which is the theme (dark
    /// or light) and nothing else: the one real paint in a frame. Nothing
    /// inside the window is ever passed through here. Converted through sRGB
    /// so a pattern or an exotic space either lands as a plain color or is
    /// dropped.
    static func backgroundPaint(_ color: CGColor?) -> String? {
        guard let color, color.alpha > 0.01,
            let space = CGColorSpace(name: CGColorSpace.sRGB),
            let rgb = color.converted(to: space, intent: .defaultIntent, options: nil),
            let c = rgb.components, c.count >= 3
        else { return nil }
        func byte(_ v: CGFloat) -> Int { Int((min(max(v, 0), 1) * 255).rounded()) }
        let l = byte(0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2])
        return String(format: "#%02X%02X%02X%02X", l, l, l, byte(rgb.alpha))
    }

    // MARK: - the wire

    /// rrweb node ids that never change: the document, <html>, <head>, and
    /// <body>, which is the canvas and the target of every pointer record.
    static let bodyId = 5

    /// One full rrweb snapshot (type 2) of the canvas, in PostHog's WEB
    /// replay format: a tiny HTML page whose <body> is the main window,
    /// painted `background`, with one absolutely positioned <div> per box
    /// (or <img> per icon) in paint order. Web, not PostHog's mobile
    /// "wireframes": PostHog accepts mobile-format recordings from a custom
    /// client and then silently never stores them (verified 2026-10-28: a
    /// web-format probe played, the same session in mobile format did not).
    /// Ids are per frame; a full snapshot replaces the last one wholesale.
    static func fullSnapshot(
        _ nodes: [Node], width: Int, height: Int, background: String, timestamp: Int64
    ) -> [String: Any] {
        var children: [[String: Any]] = []
        children.reserveCapacity(nodes.count)
        for (i, n) in nodes.enumerated() {
            let id = bodyId + 1 + i
            if let image = n.image {
                children.append([
                    "type": 2, "tagName": "img", "id": id, "childNodes": [[String: Any]](),
                    "attributes": ["src": image, "style": boxStyle(n, paint: false)],
                ])
            } else {
                children.append([
                    "type": 2, "tagName": "div", "id": id, "childNodes": [[String: Any]](),
                    "attributes": ["style": boxStyle(n, paint: true)],
                ])
            }
        }
        let body: [String: Any] = [
            "type": 2, "tagName": "body", "id": bodyId, "childNodes": children,
            "attributes": ["style": bodyStyle(width: width, height: height, background: background)],
        ]
        let head: [String: Any] = [
            "type": 2, "tagName": "head", "id": 4, "childNodes": [[String: Any]](),
            "attributes": [String: Any](),
        ]
        let html: [String: Any] = [
            "type": 2, "tagName": "html", "id": 3, "childNodes": [head, body],
            "attributes": [String: Any](),
        ]
        let document: [String: Any] = ["type": 0, "id": 1, "childNodes": [html]]
        return [
            "type": 2, "timestamp": timestamp,
            "data": ["node": document, "initialOffset": ["top": 0, "left": 0]],
        ]
    }

    /// The meta record (type 4) that opens every full snapshot: the canvas
    /// size, and the screen as a URL PostHog shows above the player. The
    /// screen name is one `Analytics` already allows.
    static func meta(screen: String, width: Int, height: Int, timestamp: Int64) -> [String: Any] {
        let href = screen.isEmpty ? hrefRoot : "\(hrefRoot)/\(screen)"
        return ["type": 4, "timestamp": timestamp, "data": ["href": href, "width": width, "height": height]]
    }
    private static let hrefRoot = "https://passband.app/app"

    private static func boxStyle(_ n: Node, paint: Bool) -> String {
        var css = "position:absolute;left:\(n.x)px;top:\(n.y)px;width:\(n.w)px;height:\(n.h)px;box-sizing:border-box;"
        guard paint else { return css }
        if let fill = n.fill { css += "background:\(fill);" }
        if let border = n.border { css += "border:\(n.borderWidth)px solid \(border);" }
        if n.radius > 0 { css += "border-radius:\(n.radius)px;" }
        return css
    }

    private static func bodyStyle(width: Int, height: Int, background: String) -> String {
        "margin:0;position:relative;overflow:hidden;width:\(width)px;height:\(height)px;background:\(background);"
    }

    /// A cheap fingerprint of a frame, so an unchanged screen is not re-sent.
    static func fingerprint(_ nodes: [Node], width: Int, height: Int) -> Int {
        var h = Hasher()
        h.combine(width)
        h.combine(height)
        for n in nodes {
            h.combine(n.x)
            h.combine(n.y)
            h.combine(n.w)
            h.combine(n.h)
            h.combine(n.fill)
            h.combine(n.border)
            h.combine(n.borderWidth)
            h.combine(n.radius)
            h.combine(n.image)
        }
        return h.finalize()
    }

    // MARK: - the guard

    /// The only words a replay record may carry as values.
    static let closedWords: Set<String> = ["html", "head", "body", "div", "img"]

    /// True when every String anywhere in `value` is one of: a closed word, a
    /// style string in EXACTLY the grammar `boxStyle`/`bodyStyle` write with
    /// grey paints only, an image this process rendered (`registerImage`),
    /// or the screen URL for a name in `extra`. Every key must be an rrweb
    /// field name. Anything else (and so anything that could be text off the
    /// screen) fails the whole record.
    static func validate(_ value: Any, extra: Set<String>) -> Bool {
        switch value {
        case let s as String:
            return closedWords.contains(s) || isOurStyle(s) || isOurHref(s, extra)
                || (s.hasPrefix("data:image/png;base64,") && isRegistered(s))
        case let d as [String: Any]:
            return d.allSatisfy { closedKeys.contains($0.key) && validate($0.value, extra: extra) }
        case let a as [Any]:
            return a.allSatisfy { validate($0, extra: extra) }
        case is Int, is Int64, is Double, is Bool:
            return true
        default:
            return false
        }
    }

    /// Keys are strings too. These are rrweb's own field names.
    private static let closedKeys: Set<String> = [
        "type", "timestamp", "data", "node", "childNodes", "tagName", "attributes",
        "style", "src", "id", "initialOffset", "top", "left", "href", "width", "height",
        "source", "x", "y", "positions", "timeOffset",
    ]

    private static let boxGrammar = try! NSRegularExpression(
        pattern:
            "^position:absolute;left:-?[0-9]{1,5}px;top:-?[0-9]{1,5}px;width:[0-9]{1,5}px;height:[0-9]{1,5}px;box-sizing:border-box;(?:background:(#[0-9A-F]{8});)?(?:border:[0-9]{1,2}px solid (#[0-9A-F]{8});)?(?:border-radius:[0-9]{1,3}px;)?\\z"
    )
    private static let bodyGrammar = try! NSRegularExpression(
        pattern:
            "^margin:0;position:relative;overflow:hidden;width:[0-9]{1,5}px;height:[0-9]{1,5}px;background:(#[0-9A-F]{8});\\z"
    )

    /// A style string in our grammar, every color in it a grey.
    static func isOurStyle(_ s: String) -> Bool {
        let range = NSRange(s.startIndex..., in: s)
        for grammar in [boxGrammar, bodyGrammar] {
            guard let m = grammar.firstMatch(in: s, range: range) else { continue }
            for g in 1..<m.numberOfRanges {
                let r = m.range(at: g)
                guard r.location != NSNotFound, let sr = Range(r, in: s) else { continue }
                if !isHexPaint(String(s[sr])) { return false }
            }
            return true
        }
        return false
    }

    private static func isOurHref(_ s: String, _ screens: Set<String>) -> Bool {
        if s == hrefRoot { return true }
        guard s.hasPrefix(hrefRoot + "/") else { return false }
        return screens.contains(String(s.dropFirst(hrefRoot.count + 1)))
    }

    /// #LLLLLLAA, uppercase, with the three channels equal: a grey and
    /// nothing else, so a hue cannot ride out even if `paint` regresses.
    static func isHexPaint(_ s: String) -> Bool {
        let c = Array(s)
        guard c.count == 9, c[0] == "#",
            c.dropFirst().allSatisfy({ $0.isHexDigit && !$0.isLowercase })
        else { return false }
        return c[1] == c[3] && c[3] == c[5] && c[2] == c[4] && c[4] == c[6]
    }
}
