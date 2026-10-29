// Real icons in session replay, without trusting a single pixel of the screen.
//
// A small layer with a bitmap could be one of the app's SF Symbols, or a
// sender's avatar initial, a date, a favicon, an emoji: the layer type does
// not tell them apart (SwiftUI draws "TI", "Dec 4" and the gear the same
// way). So the bitmap is read HERE, on this machine, only to ask one question:
// does its shape match one of the app's own icon symbols (`ReplayIconNames`)
// clearly, with a margin over the next best? If so, the frame gets OUR render
// of that symbol, in the fixed grey; if not, the layer stays a grey box. The
// pixels never leave; what leaves is a picture of a symbol from a closed list.
//
// Three guards keep that list from describing mail:
//   * the list holds only symbols Sources/ uses as icons, minus any shaped like
//     a letter, digit, punctuation mark or bare shape (replay-icons.py), so an
//     initial or a ringed avatar has nothing letter-like to match;
//   * a bitmap with more than one hue is never matched: the app tints a symbol
//     one color, while emoji and logos are multicolor;
//   * only leaf layers are read, so a container cannot mix its children in.
// What a match still says is which of the app's icons is on screen, which the
// privacy copy states.
//
// Matching: the bitmap's alpha, cropped to its ink, averaged onto a 20×20 grid
// and lightly blurred (so stroke weight matters less), compared by cosine to
// the same of each symbol, with the ink's aspect ratio as a gate. Measured on
// the sitrep: real icons score 0.97 to 0.995; text scores under 0.8; solid
// shapes match several filled symbols almost equally and fail the margin.

#if os(macOS)
    import AppKit

    @MainActor
    final class ReplayIcons {
        static let shared = ReplayIcons()

        nonisolated private static let grid = 20
        private static let minScore: Float = 0.95
        private static let minMargin: Float = 0.02
        private static let maxAspectRatio: Float = 1.3
        /// Icons are small; anything bigger is a picture and stays a box.
        static let maxSide: CGFloat = 48
        /// New bitmaps matched per frame. An animating or scrolling screen can
        /// show dozens of fresh bitmaps a tick; past this they stay boxes
        /// until a later frame gets to them.
        private static let matchesPerFrame = 20
        /// Grey, as every paint in a replay is (`ReplayWireframe`).
        private static let tint = CGColor(gray: 0.6, alpha: 1)

        private struct Ref {
            let name: String
            let mask: [Float]
            let aspect: Float
        }
        private var refs: [Ref]?
        private var building = false

        /// One answer per bitmap object. The entry holds the object itself, so
        /// its address cannot be freed and reused by another bitmap while the
        /// answer is cached, and the size it was read at, since a layer can
        /// show one bitmap at two sizes.
        private struct Entry {
            let contents: AnyObject
            let size: CGSize
            let name: String?
            var seen: Int
        }
        private var entries: [ObjectIdentifier: Entry] = [:]
        private var frame = 0
        private var matchedThisFrame = 0
        private var renders: [String: String] = [:]

        /// Build the symbol masks off the main thread; until they are ready
        /// every icon simply stays a box.
        func prepare() {
            guard refs == nil, !building else { return }
            building = true
            DispatchQueue.global(qos: .utility).async {
                var out: [Ref] = []
                for name in Set(ReplayIconNames.candidates).sorted() {
                    guard
                        let image = NSImage(systemSymbolName: name, accessibilityDescription: nil)?
                            .withSymbolConfiguration(.init(pointSize: 40, weight: .regular)),
                        let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil),
                        let m = Self.shape(cg)
                    else { continue }
                    out.append(Ref(name: name, mask: m.mask, aspect: m.aspect))
                }
                let built = out
                DispatchQueue.main.async {
                    MainActor.assumeIsolated {
                        self.refs = built
                        self.building = false
                    }
                }
            }
        }

        /// Call once per captured frame: forgets bitmaps not seen for two
        /// frames and resets the per-frame matching budget.
        func beginFrame() {
            frame += 1
            matchedThisFrame = 0
            let cutoff = frame - 2
            entries = entries.filter { $0.value.seen >= cutoff }
        }

        /// The data URL of our render of the matching symbol, sized to the
        /// layer, or nil to leave the layer a box.
        func icon(for layer: CALayer, size: CGSize) -> String? {
            guard let refs, let contents = layer.contents as AnyObject?,
                layer.sublayers?.isEmpty ?? true,
                size.width >= 6, size.height >= 6,
                size.width <= Self.maxSide, size.height <= Self.maxSide
            else { return nil }
            let key = ObjectIdentifier(contents)
            let name: String?
            if var known = entries[key], known.contents === contents, known.size == size {
                known.seen = frame
                entries[key] = known
                name = known.name
            } else {
                guard matchedThisFrame < Self.matchesPerFrame else { return nil }
                matchedThisFrame += 1
                name = match(layer, refs)
                entries[key] = Entry(contents: contents, size: size, name: name, seen: frame)
            }
            guard let name else { return nil }
            return render(name, size)
        }

        private func match(_ layer: CALayer, _ refs: [Ref]) -> String? {
            let scale: CGFloat = 2
            let w = Int((layer.bounds.width * scale).rounded())
            let h = Int((layer.bounds.height * scale).rounded())
            guard w > 0, h > 0,
                let ctx = CGContext(
                    data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                    space: CGColorSpace(name: CGColorSpace.sRGB)!,
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return nil }
            // `render(in:)` lands upside down in a bitmap context; flip so the
            // ink compares upright against the symbols.
            ctx.translateBy(x: 0, y: CGFloat(h))
            ctx.scaleBy(x: scale, y: -scale)
            layer.render(in: ctx)
            guard let cg = ctx.makeImage(), let m = Self.shape(cg), !m.multicolor else {
                return nil
            }

            var best: (name: String, score: Float) = ("", 0)
            var second: Float = 0
            for r in refs {
                let ratio = max(r.aspect, m.aspect) / min(r.aspect, m.aspect)
                guard ratio <= Self.maxAspectRatio else { continue }
                let s = Self.cosine(r.mask, m.mask)
                if s > best.score {
                    second = best.score
                    best = (r.name, s)
                } else if s > second {
                    second = s
                }
            }
            guard best.score >= Self.minScore, best.score - second >= Self.minMargin else {
                return nil
            }
            return best.name
        }

        /// Our own picture of a symbol: aspect-fit in the layer's box, at
        /// exactly 2x pixels, grey, no color profile (about 1.5 KB for an
        /// 18pt icon). Registered with `ReplayWireframe` so the outbound
        /// validator knows it is one of ours.
        private func render(_ name: String, _ size: CGSize) -> String? {
            let key = "\(name)@\(Int(size.width))x\(Int(size.height))"
            if let done = renders[key] { return done }
            let w = Int(size.width * 2)
            let h = Int(size.height * 2)
            guard w > 0, h > 0,
                let symbol = NSImage(systemSymbolName: name, accessibilityDescription: nil)?
                    .withSymbolConfiguration(.init(pointSize: size.height * 2, weight: .regular)),
                let glyph = symbol.cgImage(forProposedRect: nil, context: nil, hints: nil),
                let ctx = CGContext(
                    data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                    space: CGColorSpaceCreateDeviceRGB(),
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return nil }
            let gw = CGFloat(glyph.width)
            let gh = CGFloat(glyph.height)
            let fit = min(CGFloat(w) / gw, CGFloat(h) / gh)
            let rect = CGRect(
                x: (CGFloat(w) - gw * fit) / 2, y: (CGFloat(h) - gh * fit) / 2,
                width: gw * fit, height: gh * fit)
            // The glyph as a mask, filled with the replay grey.
            ctx.clip(to: rect, mask: glyph)
            ctx.setFillColor(Self.tint)
            ctx.fill(rect)
            guard let out = ctx.makeImage(),
                let png = NSBitmapImageRep(cgImage: out).representation(using: .png, properties: [:])
            else { return nil }
            let url = "data:image/png;base64," + png.base64EncodedString()
            ReplayWireframe.registerImage(url)
            renders[key] = url
            return url
        }

        // MARK: - shape

        /// Alpha cropped to its ink, box-averaged onto the grid, lightly
        /// blurred; the ink's aspect ratio; and whether the ink holds more
        /// than one hue (emoji, logos), which is never matched.
        nonisolated private static func shape(_ img: CGImage)
            -> (mask: [Float], aspect: Float, multicolor: Bool)?
        {
            let w = img.width
            let h = img.height
            guard w > 0, h > 0 else { return nil }
            var px = [UInt8](repeating: 0, count: w * h * 4)
            guard
                let ctx = CGContext(
                    data: &px, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                    space: CGColorSpace(name: CGColorSpace.sRGB)!,
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return nil }
            ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
            var minX = w, minY = h, maxX = -1, maxY = -1
            var hues = Set<Int>()
            for y in 0..<h {
                for x in 0..<w {
                    let i = (y * w + x) * 4
                    let a = px[i + 3]
                    guard a > 40 else { continue }
                    minX = min(minX, x)
                    maxX = max(maxX, x)
                    minY = min(minY, y)
                    maxY = max(maxY, y)
                    if a > 128, let hue = hueBucket(px[i], px[i + 1], px[i + 2], a) {
                        hues.insert(hue)
                    }
                }
            }
            guard maxX >= minX, maxY >= minY else { return nil }
            let cw = maxX - minX + 1
            let ch = maxY - minY + 1
            let n = grid
            var v = [Float](repeating: 0, count: n * n)
            for j in 0..<n {
                for i in 0..<n {
                    let x0 = minX + i * cw / n
                    let x1 = max(x0 + 1, minX + (i + 1) * cw / n)
                    let y0 = minY + j * ch / n
                    let y1 = max(y0 + 1, minY + (j + 1) * ch / n)
                    var sum: Float = 0
                    var count: Float = 0
                    for y in y0..<min(y1, h) {
                        for x in x0..<min(x1, w) {
                            sum += Float(px[(y * w + x) * 4 + 3]) / 255
                            count += 1
                        }
                    }
                    v[j * n + i] = count > 0 ? sum / count : 0
                }
            }
            var blurred = v
            for j in 0..<n {
                for i in 0..<n {
                    var sum: Float = 0
                    var count: Float = 0
                    for dj in -1...1 {
                        for di in -1...1 {
                            let x = i + di
                            let y = j + dj
                            if x >= 0, y >= 0, x < n, y < n {
                                sum += v[y * n + x]
                                count += 1
                            }
                        }
                    }
                    blurred[j * n + i] = sum / count
                }
            }
            return (blurred, Float(cw) / Float(ch), spread(hues) > 2)
        }

        /// How many 30° buckets the hues span, the short way round the
        /// wheel. One tint lands in one or two neighbouring buckets (its
        /// antialiased edge can tip into the next); two colours span more.
        nonisolated private static func spread(_ buckets: Set<Int>) -> Int {
            guard buckets.count > 1 else { return buckets.count }
            let sorted = buckets.sorted()
            var largestGap = 12 - sorted.last! + sorted.first!
            for (a, b) in zip(sorted, sorted.dropFirst()) { largestGap = max(largestGap, b - a) }
            return 12 - largestGap + 1
        }

        /// A saturated pixel's hue in one of 12 buckets, or nil for a grey.
        nonisolated private static func hueBucket(_ r8: UInt8, _ g8: UInt8, _ b8: UInt8, _ a8: UInt8)
            -> Int?
        {
            let a = Float(a8)
            let r = Float(r8) / a
            let g = Float(g8) / a
            let b = Float(b8) / a
            let mx = max(r, g, b)
            let mn = min(r, g, b)
            guard mx > 0.15, (mx - mn) / mx > 0.3 else { return nil }
            var hue: Float
            if mx == r {
                hue = (g - b) / (mx - mn)
            } else if mx == g {
                hue = 2 + (b - r) / (mx - mn)
            } else {
                hue = 4 + (r - g) / (mx - mn)
            }
            hue = (hue * 60).truncatingRemainder(dividingBy: 360)
            if hue < 0 { hue += 360 }
            return Int(hue / 30) % 12
        }

        nonisolated private static func cosine(_ a: [Float], _ b: [Float]) -> Float {
            var dot: Float = 0
            var na: Float = 0
            var nb: Float = 0
            for i in 0..<a.count {
                dot += a[i] * b[i]
                na += a[i] * a[i]
                nb += b[i] * b[i]
            }
            return dot / ((na * nb).squareRoot() + 1e-6)
        }
    }
#endif
