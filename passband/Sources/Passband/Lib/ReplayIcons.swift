// Real icons in session replay, without trusting a single pixel of the screen.
//
// A small layer with a bitmap could be one of the app's SF Symbols, or a
// sender's avatar initials, a date, a carrier logo, a photo: the layer type
// does not tell them apart (SwiftUI draws "TI", "Dec 4" and the gear the same
// way). So the bitmap is read HERE, on this machine, only to ask one question:
// does its shape match one of the app's own symbols (`ReplayIconNames`)
// clearly, with a margin over the next best? If so, the frame gets OUR render
// of that symbol, in the fixed grey; if not, the layer stays a grey box. The
// pixels never leave; what leaves is a picture of a symbol from a closed list,
// so the most a wrong match can say is "something here looked like a gear".
//
// Matching: the bitmap's alpha, cropped to its ink, averaged onto a 20×20 grid
// and lightly blurred (so stroke weight matters less), compared by cosine to
// the same of each symbol, with the ink's aspect ratio as a gate. Measured on
// the sitrep: real icons score 0.97 to 0.995; text scores under 0.8; solid
// shapes (a photo in a circle) match several filled symbols almost equally
// and fail the margin.

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
        private static let tint = NSColor(white: 0.6, alpha: 1)

        private struct Ref {
            let name: String
            let mask: [Float]
            let aspect: Float
        }
        private var refs: [Ref]?
        private var building = false
        /// Per bitmap object: the symbol it matched, or nil for none.
        private var matches: [ObjectIdentifier: String?] = [:]
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
                        let m = Self.mask(cg)
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

        /// The data URL of our render of the matching symbol, sized to the
        /// layer, or nil to leave the layer a box.
        func icon(for layer: CALayer, size: CGSize) -> String? {
            guard let refs, let contents = layer.contents,
                size.width >= 6, size.height >= 6,
                size.width <= Self.maxSide, size.height <= Self.maxSide
            else { return nil }
            let key = ObjectIdentifier(contents as AnyObject)
            let name: String?
            if let known = matches[key] {
                name = known
            } else {
                name = match(layer, refs)
                if matches.count > 2000 { matches.removeAll() }
                matches[key] = name
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
            guard let cg = ctx.makeImage(), let m = Self.mask(cg) else { return nil }

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

        /// Our own picture of a symbol: aspect-fit in the layer's box, at 2x,
        /// in the replay's grey. Registered with `ReplayWireframe` so the
        /// outbound validator knows it is one of ours.
        private func render(_ name: String, _ size: CGSize) -> String? {
            let key = "\(name)@\(Int(size.width))x\(Int(size.height))"
            if let done = renders[key] { return done }
            guard
                let symbol = NSImage(systemSymbolName: name, accessibilityDescription: nil)?
                    .withSymbolConfiguration(
                        .init(pointSize: size.height, weight: .regular)
                            .applying(.init(paletteColors: [Self.tint])))
            else { return nil }
            let px = NSSize(width: size.width * 2, height: size.height * 2)
            let image = NSImage(size: px)
            image.lockFocus()
            let fit = min(px.width / symbol.size.width, px.height / symbol.size.height)
            let dw = symbol.size.width * fit
            let dh = symbol.size.height * fit
            symbol.draw(
                in: NSRect(x: (px.width - dw) / 2, y: (px.height - dh) / 2, width: dw, height: dh))
            image.unlockFocus()
            guard let tiff = image.tiffRepresentation,
                let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:])
            else { return nil }
            let url = "data:image/png;base64," + png.base64EncodedString()
            ReplayWireframe.registerImage(url)
            renders[key] = url
            return url
        }

        // MARK: - shape

        /// Alpha cropped to its ink, box-averaged onto the grid, lightly
        /// blurred; plus the ink's aspect ratio.
        nonisolated private static func mask(_ img: CGImage) -> (mask: [Float], aspect: Float)? {
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
            for y in 0..<h {
                for x in 0..<w where px[(y * w + x) * 4 + 3] > 40 {
                    minX = min(minX, x)
                    maxX = max(maxX, x)
                    minY = min(minY, y)
                    maxY = max(maxY, y)
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
            return (blurred, Float(cw) / Float(ch))
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
