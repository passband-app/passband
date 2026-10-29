// Session replay's privacy contract, held against a layer tree built to tempt
// it: a CATextLayer carrying a subject, an image, a colored pill, a REAL
// WKWebView subclass and a real NSTextField with things inside them. Green
// means the wireframe kept the geometry and dropped every one of those, and
// that the outbound validator refuses anything that is not a grey, a closed
// word or a number.
//
// Also: SwiftUI's 0×0 container layers (which do not clip) must not hide
// their children, a clipping scroll view must drop its off-screen rows, and
// the gzip member must be one a server can open.

import AppKit
import Foundation
import QuartzCore
import WebKit

/// The app's own web view is a subclass (EmailWebView's PassthroughWebView),
/// so the boundary must be found by kind, not by name.
final class AppStyleWebView: WKWebView {}

@main
@MainActor
struct ReplayWireframeTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        noTextLeaves()
        webContentIsOneOpaqueBox()
        textFieldsAreOpaque()
        offscreenBoundaryLetsNothingThrough()
        paintIsAFixedPalette()
        zeroSizedContainersKeepTheirChildren()
        clippingDropsOffscreenRows()
        flippedAndUnflippedAgree()
        textWidthsAreQuantized()
        validatorIsClosed()
        boxesSnapToTheGrid()
        aClippedIconStaysABox()
        pointerRecordsPass()
        iconListHasNoLetters()
        webSnapshotShape()
        onlyOurOwnImagesPass()
        gzipIsAGzipMember()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: - fixtures

    static func layer(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat) -> CALayer {
        let l = CALayer()
        l.frame = CGRect(x: x, y: y, width: w, height: h)
        return l
    }

    static func image() -> CGImage {
        let ctx = CGContext(
            data: nil, width: 4, height: 4, bitsPerComponent: 8, bytesPerRow: 16,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        ctx.setFillColor(CGColor(red: 1, green: 0, blue: 0, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: 4, height: 4))
        return ctx.makeImage()!
    }

    /// A window-sized root with one of everything a replay must not show.
    static func tempting() -> CALayer {
        let root = layer(0, 0, 800, 600)
        root.isGeometryFlipped = true
        let card = layer(20, 20, 400, 100)
        card.backgroundColor = CGColor(red: 0.9, green: 0.1, blue: 0.1, alpha: 1)
        card.cornerRadius = 12
        root.addSublayer(card)
        let subject = CATextLayer()
        subject.frame = CGRect(x: 10, y: 10, width: 203, height: 16)
        subject.string = "SECRET SUBJECT Your invoice from Acme"
        card.addSublayer(subject)
        let avatar = layer(300, 10, 40, 40)
        avatar.contents = image()
        card.addSublayer(avatar)
        let webView = AppStyleWebView(frame: CGRect(x: 0, y: 0, width: 500, height: 300))
        webView.wantsLayer = true
        let web = webView.layer!
        web.frame = CGRect(x: 20, y: 200, width: 500, height: 300)
        root.addSublayer(web)
        // A plain CALayer, not a WK one: only the view's kind stops the walk.
        let inside = layer(10, 10, 100, 20)
        inside.backgroundColor = CGColor(red: 0, green: 0.5, blue: 1, alpha: 1)
        let insideText = CATextLayer()
        insideText.string = "SECRET BODY"
        insideText.frame = CGRect(x: 0, y: 0, width: 90, height: 14)
        inside.addSublayer(insideText)
        web.addSublayer(inside)
        return root
    }

    static func walk(_ root: CALayer, flipped: Bool = true, clip: CGRect? = nil)
        -> [ReplayWireframe.Node]
    {
        var out: [ReplayWireframe.Node] = []
        ReplayWireframe.nodes(
            root: root, flipped: flipped, offset: .zero,
            clip: clip ?? root.bounds, into: &out)
        return out
    }

    static func webSnapshotShape() {
        let nodes = walk(tempting())
        let snap = ReplayWireframe.fullSnapshot(
            nodes, width: 800, height: 600, background: "#1E1E1EFF", timestamp: 1)
        let data = snap["data"] as? [String: Any]
        let doc = data?["node"] as? [String: Any]
        let html = (doc?["childNodes"] as? [[String: Any]])?.first
        let body = (html?["childNodes"] as? [[String: Any]])?.last
        expect(doc?["type"] as? Int == 0 && html?["tagName"] as? String == "html", "a document with <html>")
        expect(body?["tagName"] as? String == "body" && body?["id"] as? Int == ReplayWireframe.bodyId,
            "<body> carries the fixed id pointer records target")
        let kids = body?["childNodes"] as? [[String: Any]] ?? []
        expect(kids.count == nodes.count, "one element per box (\(kids.count) vs \(nodes.count))")
        let ids = Set(kids.compactMap { $0["id"] as? Int })
        expect(ids.count == kids.count && !ids.contains(ReplayWireframe.bodyId), "element ids are unique")
    }

    static func json(_ nodes: [ReplayWireframe.Node]) -> String {
        let snap = ReplayWireframe.fullSnapshot(
            nodes, width: 800, height: 600, background: "#1E1E1EFF", timestamp: 1)
        let data = try! JSONSerialization.data(withJSONObject: snap)
        return String(decoding: data, as: UTF8.self)
    }

    // MARK: - checks

    static func noTextLeaves() {
        let nodes = walk(tempting())
        let out = json(nodes)
        expect(!out.contains("SECRET"), "no layer text reaches the wire")
        expect(!out.contains("Acme"), "not even part of it")
        expect(nodes.contains { $0.kind == .text }, "the text layer is still drawn, as a bar")
        expect(nodes.contains { $0.kind == .image }, "the avatar is drawn, as an empty box")
        let snap = ReplayWireframe.fullSnapshot(
            nodes, width: 800, height: 600, background: "#1E1E1EFF", timestamp: 1)
        expect(ReplayWireframe.validate([snap], extra: []), "a real frame passes the validator")
    }

    static func webContentIsOneOpaqueBox() {
        let nodes = walk(tempting())
        let web = nodes.filter { $0.kind == .opaque }
        expect(web.count == 1, "the web view is exactly one box (got \(web.count))")
        // 20,200 500x300 snapped outward to the 32pt opaque grid.
        expect(web.first.map { $0.x == 0 && $0.y == 192 && $0.w == 544 && $0.h == 320 } ?? false,
            "and it covers the web view, on the coarse grid (got \(String(describing: web.first)))")
        // The blue fill and the body text inside it were never visited.
        let insideWeb = nodes.filter {
            $0.kind != .opaque && $0.x >= 20 && $0.y >= 200 && $0.x < 520 && $0.y < 500
        }
        expect(insideWeb.isEmpty, "nothing under the web view is walked (got \(insideWeb.count))")
    }

    static func textFieldsAreOpaque() {
        let root = layer(0, 0, 800, 600)
        root.isGeometryFlipped = true
        let field = NSTextField(string: "SECRET typed reply")
        field.frame = CGRect(x: 0, y: 0, width: 300, height: 24)
        field.wantsLayer = true
        field.layer!.frame = CGRect(x: 40, y: 40, width: 300, height: 24)
        root.addSublayer(field.layer!)
        let nodes = walk(root)
        expect(nodes.count == 1 && nodes[0].kind == .opaque,
            "a text field is one opaque box (got \(nodes.map(\.kind)))")
        expect(nodes.first.map { $0.x == 32 && $0.w == 320 } ?? false,
            "at the field's own frame on the coarse grid, not the text's (got \(String(describing: nodes.first)))")
    }

    static func offscreenBoundaryLetsNothingThrough() {
        // A web view scrolled out of the clip whose content overflows back in.
        let root = layer(0, 0, 800, 600)
        root.isGeometryFlipped = true
        let webView = AppStyleWebView(frame: .zero)
        webView.wantsLayer = true
        let web = webView.layer!
        web.frame = CGRect(x: 0, y: -400, width: 400, height: 300)
        root.addSublayer(web)
        let overflow = layer(0, 350, 200, 100)  // lands at canvas y -50...50
        overflow.backgroundColor = CGColor(gray: 0.5, alpha: 1)
        web.addSublayer(overflow)
        expect(walk(root).isEmpty, "nothing under an off-screen web view is drawn")
    }

    static func paintIsAFixedPalette() {
        let palette: Set<String> = [
            ReplayWireframe.panelPaint, ReplayWireframe.borderPaint, ReplayWireframe.textPaint,
            ReplayWireframe.imagePaint, ReplayWireframe.opaquePaint, ReplayWireframe.opaqueBorder,
        ]
        let root = layer(0, 0, 800, 600)
        root.isGeometryFlipped = true
        // Past due red, delivered green, a dark logo, a light logo.
        for (i, c) in [(1.0, 0.23, 0.19), (0.2, 0.78, 0.35), (0.1, 0.1, 0.1), (0.96, 0.96, 0.96)]
            .enumerated()
        {
            let pill = layer(CGFloat(i) * 100, 0, 80, 20)
            pill.backgroundColor = CGColor(srgbRed: c.0, green: c.1, blue: c.2, alpha: 1)
            pill.borderWidth = 1
            pill.borderColor = CGColor(srgbRed: c.0, green: c.1, blue: c.2, alpha: 1)
            root.addSublayer(pill)
        }
        let nodes = walk(root) + walk(tempting())
        let paints = Set(nodes.flatMap { [$0.fill, $0.border].compactMap { $0 } })
        expect(paints.isSubset(of: palette), "every paint is from the fixed palette (extra: \(paints.subtracting(palette)))")
        let fills = Set(nodes.filter { $0.kind == .fill && $0.fill != nil }.map(\.fill))
        expect(fills.count == 1, "red, green, dark and light fills are indistinguishable")
        for p in palette { expect(ReplayWireframe.isHexPaint(p), "\(p) passes the validator") }
        let red = ReplayWireframe.backgroundPaint(CGColor(srgbRed: 1, green: 0, blue: 0, alpha: 1))
        expect(red == "#363636FF", "a window background lands as its luminance (got \(red ?? "nil"))")
        expect(ReplayWireframe.backgroundPaint(CGColor(gray: 0, alpha: 0)) == nil, "clear draws nothing")
    }

    static func zeroSizedContainersKeepTheirChildren() {
        let root = layer(0, 0, 800, 600)
        root.isGeometryFlipped = true
        let container = layer(100, 100, 0, 0)
        root.addSublayer(container)
        let child = layer(0, 0, 50, 20)
        child.backgroundColor = CGColor(gray: 0.5, alpha: 1)
        container.addSublayer(child)
        expect(
            walk(root).contains { $0.x == 96 && $0.y == 96 && $0.w == 56 && $0.h == 24 },
            "a child of a 0x0 non-clipping container is still drawn")

        container.masksToBounds = true
        expect(walk(root).isEmpty, "but a 0x0 container that clips hides it")
    }

    static func clippingDropsOffscreenRows() {
        let root = layer(0, 0, 800, 600)
        root.isGeometryFlipped = true
        let scroll = layer(0, 0, 400, 300)
        scroll.masksToBounds = true
        root.addSublayer(scroll)
        let doc = layer(0, 0, 400, 3000)
        scroll.addSublayer(doc)
        for i in 0..<100 {
            let row = layer(0, CGFloat(i) * 30, 400, 28)
            row.backgroundColor = CGColor(gray: 0.3, alpha: 1)
            doc.addSublayer(row)
        }
        let rows = walk(root).filter { $0.kind == .fill }
        expect(rows.count == 10, "only the ten visible rows survive (got \(rows.count))")
        expect(
            rows.allSatisfy { $0.y + $0.h <= 300 + ReplayWireframe.boxQuantum },
            "and each is cut to the scroll view (to within one grid step)")
    }

    static func flippedAndUnflippedAgree() {
        // The same box, described in an up-y root and in a down-y root, lands
        // at the same canvas position.
        let up = layer(0, 0, 800, 600)
        let a = layer(10, 500, 100, 50)  // y up: 50pt from the top
        a.backgroundColor = CGColor(gray: 0.5, alpha: 1)
        up.addSublayer(a)
        let down = layer(0, 0, 800, 600)
        down.isGeometryFlipped = true
        let b = layer(10, 50, 100, 50)
        b.backgroundColor = CGColor(gray: 0.5, alpha: 1)
        down.addSublayer(b)
        let u = walk(up, flipped: false).first
        let d = walk(down, flipped: true).first
        expect(u != nil && u == d, "flip handling places both at the top (\(String(describing: u)) vs \(String(describing: d)))")
    }

    static func textWidthsAreQuantized() {
        let bars = walk(tempting()).filter { $0.kind == .text }
        expect(!bars.isEmpty, "there is a bar to measure")
        for b in bars {
            expect(b.w % ReplayWireframe.textQuantum == 0, "bar width \(b.w) is a multiple of the quantum")
            expect(b.x % ReplayWireframe.textQuantum == 0, "bar x \(b.x) is on the grid too")
        }
        // 203pt starting at x=30 spans 30...233, snapped out to 24...240.
        expect(bars.first?.w == 216, "203pt of text reads as 216 (got \(bars.first?.w ?? -1))")
        expect(bars.first?.x == 24, "and starts on the grid (got \(bars.first?.x ?? -1))")
    }

    static func validatorIsClosed() {
        let ok = ReplayWireframe.meta(screen: "sitrep", width: 1, height: 1, timestamp: 1)
        expect(ReplayWireframe.validate([ok], extra: ["sitrep"]), "a screen name from the vocabulary passes")
        expect(!ReplayWireframe.validate([ok], extra: []), "the same name outside the vocabulary fails")
        let bare = ReplayWireframe.meta(screen: "", width: 1, height: 1, timestamp: 1)
        expect(ReplayWireframe.validate([bare], extra: []), "no screen yet is fine")
        // The style grammar is exact: our own string passes, anything that
        // bends it does not.
        let box = "position:absolute;left:4px;top:-8px;width:10px;height:12px;box-sizing:border-box;background:#8080801F;border:1px solid #80808059;border-radius:6px;"
        expect(ReplayWireframe.isOurStyle(box), "our box style passes")
        expect(!ReplayWireframe.isOurStyle(box.replacingOccurrences(of: "#8080801F", with: "#FF3B30FF")),
            "a hue inside a style fails")
        expect(!ReplayWireframe.isOurStyle(box + "content:'hi';"), "an extra declaration fails")
        expect(!ReplayWireframe.isOurStyle(box.replacingOccurrences(of: "background:#8080801F;", with: "background:url(x);")),
            "a url() fails")
        let textNode: [String: Any] = ["type": 3, "textContent": "SECRET", "id": 9]
        expect(!ReplayWireframe.validate([textNode], extra: []), "an rrweb text node fails")
        let span: [String: Any] = ["type": 2, "tagName": "span", "id": 9, "attributes": [String: Any](), "childNodes": [Any]()]
        expect(!ReplayWireframe.validate([span], extra: []), "a tag outside the closed set fails")
        let text: [String: Any] = ["type": 2, "data": ["wireframes": [["type": "text", "text": "hi"]]]]
        expect(!ReplayWireframe.validate([text], extra: []), "a text wireframe fails")
        let hue: [String: Any] = ["style": ["backgroundColor": "#FF0000FF"]]
        expect(!ReplayWireframe.validate([hue], extra: []), "a hue fails")
        let lower: [String: Any] = ["style": ["backgroundColor": "#aaaaaaff"]]
        expect(!ReplayWireframe.validate([lower], extra: []), "a lowercase paint fails")
        let key: [String: Any] = ["subject": 1]
        expect(!ReplayWireframe.validate([key], extra: []), "an unknown key fails")
        let data: [String: Any] = ["data": Data([1, 2, 3])]
        expect(!ReplayWireframe.validate([data], extra: []), "a value of any other type fails")
    }

    static func gzipIsAGzipMember() {
        expect(Gzip.crc32(Data("123456789".utf8)) == 0xCBF4_3926, "CRC-32 check value")
        let input = Data(String(repeating: "{\"type\":\"rectangle\"},", count: 500).utf8)
        guard let gz = Gzip.compress(input) else {
            expect(false, "compresses")
            return
        }
        expect(gz.count < input.count / 5, "repetitive JSON shrinks (\(input.count) -> \(gz.count))")
        expect(gz.prefix(3) == Data([0x1F, 0x8B, 0x08]), "gzip magic and deflate method")
        let body = gz.subdata(in: 10..<(gz.count - 8))
        let inflated = try? (body as NSData).decompressed(using: .zlib) as Data
        expect(inflated == input, "the deflate stream round-trips")
        let trailer = [UInt8](gz.suffix(8))
        let crc = UInt32(trailer[0]) | UInt32(trailer[1]) << 8 | UInt32(trailer[2]) << 16 | UInt32(trailer[3]) << 24
        let size = UInt32(trailer[4]) | UInt32(trailer[5]) << 8 | UInt32(trailer[6]) << 16 | UInt32(trailer[7]) << 24
        expect(crc == Gzip.crc32(input) && size == UInt32(input.count), "trailer carries CRC and length")
        // And the system's own gunzip agrees, which is the real test.
        let tmp = FileManager.default.temporaryDirectory.appendingPathComponent("replay-gz-\(getpid()).gz")
        try? gz.write(to: tmp)
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/gunzip")
        p.arguments = ["-c", tmp.path]
        let pipe = Pipe()
        p.standardOutput = pipe
        try? p.run()
        let out = pipe.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        try? FileManager.default.removeItem(at: tmp)
        expect(p.terminationStatus == 0 && out == input, "gunzip opens it")
    }

    static func onlyOurOwnImagesPass() {
        // An icon lookup that claims every bitmap is a symbol: what reaches
        // the wire is the lookup's own picture, never the layer's.
        let ours = "data:image/png;base64,T1VSUw=="
        ReplayWireframe.registerImage(ours)
        let root = tempting()
        var nodes: [ReplayWireframe.Node] = []
        ReplayWireframe.nodes(
            root: root, flipped: true, offset: .zero, clip: root.bounds,
            icon: { _, _ in ours }, into: &nodes)
        let icons = nodes.filter { $0.kind == .icon }
        expect(icons.count == 1 && icons[0].image == ours, "the avatar becomes our icon (got \(icons.count))")
        let snap = ReplayWireframe.fullSnapshot(
            nodes, width: 800, height: 600, background: "#1E1E1EFF", timestamp: 1)
        expect(ReplayWireframe.validate([snap], extra: []), "a frame carrying our registered image passes")
        expect(!json(nodes).contains("SECRET"), "and still carries no text")

        let theirs: [String: Any] = ["type": "image", "base64": "data:image/png;base64,QVZBVEFS"]
        expect(!ReplayWireframe.validate([theirs], extra: []), "an image we did not render fails")
        let jpeg: [String: Any] = ["type": "image", "base64": "data:image/jpeg;base64,T1VSUw=="]
        expect(!ReplayWireframe.validate([jpeg], extra: []), "nor does any other encoding of it")
    }

    static func boxesSnapToTheGrid() {
        let root = layer(0, 0, 800, 600)
        root.isGeometryFlipped = true
        let pill = layer(3, 5, 203, 17)  // a recipient pill whose width is an address
        pill.backgroundColor = CGColor(gray: 0.5, alpha: 1)
        root.addSublayer(pill)
        let n = walk(root).first
        expect(n.map { $0.x == 0 && $0.y == 0 && $0.w == 208 && $0.h == 24 } ?? false,
            "a box snaps outward to the 8pt grid (got \(String(describing: n)))")
        for b in walk(tempting()) where b.kind != .icon && b.kind != .text {
            let q = b.kind == .opaque ? ReplayWireframe.opaqueQuantum : ReplayWireframe.boxQuantum
            expect(b.x % q == 0 && b.y % q == 0 && b.w % q == 0 && b.h % q == 0,
                "every box is on its grid (\(b))")
        }
    }

    static func aClippedIconStaysABox() {
        // The avatar spans x 320...360; a clip at 340 cuts it, and a half
        // icon must not be drawn as a whole one.
        let ours = "data:image/png;base64,T1VSUw=="
        ReplayWireframe.registerImage(ours)
        let root = tempting()
        var nodes: [ReplayWireframe.Node] = []
        ReplayWireframe.nodes(
            root: root, flipped: true, offset: .zero, clip: CGRect(x: 0, y: 0, width: 340, height: 600),
            icon: { _, _ in ours }, into: &nodes)
        expect(!nodes.contains { $0.kind == .icon }, "a clipped bitmap is not an icon")
        expect(nodes.contains { $0.kind == .image }, "it is a box instead")
    }

    static func pointerRecordsPass() {
        let move: [String: Any] = [
            "type": 3, "timestamp": 1,
            "data": ["source": 1, "positions": [["x": 4, "y": 5, "id": ReplayWireframe.bodyId, "timeOffset": 0]]],
        ]
        let click: [String: Any] = [
            "type": 3, "timestamp": 1,
            "data": ["source": 2, "type": 2, "id": ReplayWireframe.bodyId, "x": 4, "y": 5],
        ]
        expect(ReplayWireframe.validate([move, click], extra: []), "pointer and click records pass")
        let keyed: [String: Any] = ["type": 3, "timestamp": 1, "data": ["source": 5, "text": "k"]]
        expect(!ReplayWireframe.validate([keyed], extra: []), "an input record with text fails")
    }

    static func iconListHasNoLetters() {
        let banned: Set<String> = ["a", "k", "at", "bold", "italic", "info", "circle", "ring", "exclamationmark"]
        let names = ReplayIconNames.candidates
        expect(names.count > 50, "the icon list is populated (\(names.count))")
        for n in names {
            expect(!banned.contains(n), "\(n) is not letter- or shape-like")
            let parts = n.split(separator: ".")
            expect(!parts.contains { $0.count == 1 }, "\(n) has no single-character part")
        }
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
