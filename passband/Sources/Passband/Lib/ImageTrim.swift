// Reading-card logo extraction: find the MARK inside a hero that is mostly its
// own flat background, so the card can draw the mark at the size of the square
// instead of the whole banner at the size of a hyphen. The case this exists for
// is the email masthead: 1200px of white with a 60px logo in the middle.
//
// Every failure path returns nil and the hero is drawn whole, as it was before
// this file existed. A photo is the common nil: it has no flat background.

import CoreGraphics

enum ImageTrim {
    struct Mark: Sendable {
        /// Where the mark sits, in the image's own pixels with the origin at
        /// the top left — the space `CGImage.cropping(to:)` takes.
        let rect: CGRect
        /// The flat colour the mark sits on, which is what the rest of the
        /// square should be painted. nil when that background is transparency:
        /// there is no colour to continue, and `ImageFill` picks one.
        let background: ImageFill.RGB?
    }

    /// Refuse anything bigger than this outright. The caller decodes to ~1024
    /// on the long side, so this only ever bites a caller that forgot to.
    private static let maxPixels = 1 << 21

    /// Below this alpha a pixel is background whatever its colour says.
    private static let minAlpha = 24

    /// TUNABLE per-channel distance from the background before a pixel counts
    /// as ink. Wide enough to swallow JPEG mush around a flat field, narrow
    /// enough that a pale grey wordmark on white still registers.
    private static let tolerance = 30

    /// TUNABLE: how much bigger the mark must get for a crop to be worth it,
    /// as image long side over mark long side, in tenths. Under this the art
    /// already fills its frame and the card's old fit rules do fine.
    private static let minGainTenths = 15

    /// A mark smaller than this (at analysis size) is a speck or would be
    /// drawn as a blur. Long side, short side.
    private static let minLong = 20
    private static let minShort = 8

    private struct Bin {
        var r = 0, g = 0, b = 0, n = 0
    }

    /// A stretch of inked columns, bridged across gaps too small to separate
    /// two things. `mass` is its ink, which is how the logo is told from the
    /// "view in browser" beside it.
    private struct Run {
        var start: Int
        var end: Int
        var mass: Int
    }

    /// The image split into field and ink: which colour the background is (nil
    /// for transparency) and which pixels are not it.
    private struct Field {
        let ground: (r: Int, g: Int, b: Int)?
        var ink: [Bool]
    }

    /// The cheap first look, meant for the small thumbnail the card decodes
    /// anyway: is this mostly ONE flat field? A photo says no in a few hundred
    /// pixels, and is spared the full-size decode `mark(in:)` wants.
    nonisolated static func hasFlatField(_ image: CGImage) -> Bool {
        let w = image.width
        let h = image.height
        guard w > 0, h > 0, w * h <= maxPixels, let px = pixels(image, w, h) else { return false }
        return field(px, w * h) != nil
    }

    /// The mark's box, or nil when the image is not a mark on a flat field or
    /// cropping to it would not make it meaningfully bigger.
    nonisolated static func mark(in image: CGImage) -> Mark? {
        let w = image.width
        let h = image.height
        guard w > 0, h > 0, w * h <= maxPixels, let px = pixels(image, w, h),
            let field = field(px, w * h)
        else { return nil }
        let ground = field.ground
        var ink = field.ink

        // A THIN line that runs edge to edge is a rule or a frame, never a
        // logo — and one hairline under a masthead would otherwise make every
        // column "inked" and the mark as wide as the banner. Erase them first.
        // Thin only: an edge-to-edge BLOCK is the image's content, and erasing
        // it would promote whatever caption sits beside it to "the mark".
        var (rows, cols) = profile(ink, w, h)
        let ruleRows = thin(rows.indices.filter { rows[$0] * 10 >= w * 9 }, limit: max(3, h * 2 / 25))
        let ruleCols = thin(cols.indices.filter { cols[$0] * 10 >= h * 9 }, limit: max(3, w * 2 / 25))
        if !ruleRows.isEmpty || !ruleCols.isEmpty {
            for y in ruleRows { for x in 0..<w { ink[y * w + x] = false } }
            for x in ruleCols { for y in 0..<h { ink[y * w + x] = false } }
            (rows, cols) = profile(ink, w, h)
        }

        guard let top = rows.firstIndex(where: { $0 > 0 }),
            let bottom = rows.lastIndex(where: { $0 > 0 })
        else { return nil }

        // Split the columns into separate things. The gap that separates them
        // scales with the ink's height, so a letter-spaced wordmark stays one
        // run while a nav link a logo-width away becomes its own.
        let gap = max(8, (bottom - top + 1) * 3 / 4)
        let found = runs(cols, gap: gap)
        guard let first = found.first, let last = found.last else { return nil }
        var x0 = first.start
        var x1 = last.end

        // Take the heaviest run alone only when it CLEARLY carries the image.
        // Two things of similar weight are a composition, not a logo and a
        // footnote, and guessing between them is worse than keeping both.
        let total = found.reduce(0) { $0 + $1.mass }
        if let heavy = found.max(by: { $0.mass < $1.mass }), heavy.mass * 10 >= total * 6 {
            x0 = heavy.start
            x1 = heavy.end
        }

        // Rows are measured INSIDE the chosen columns, so whatever was left
        // out sideways cannot make the box taller than the mark.
        var y0 = h
        var y1 = -1
        for y in top...bottom {
            let base = y * w
            if (x0...x1).contains(where: { ink[base + $0] }) {
                y0 = min(y0, y)
                y1 = y
            }
        }
        guard y1 >= y0 else { return nil }

        let bw = x1 - x0 + 1
        let bh = y1 - y0 + 1
        guard max(bw, bh) >= minLong, min(bw, bh) >= minShort,
            max(w, h) * 10 >= max(bw, bh) * minGainTenths
        else { return nil }

        // A little of the image's own background comes along: it keeps
        // antialiased edges whole, and on a JPEG it is where the ringing is.
        let pad = max(2, max(bw, bh) / 16)
        let rect = CGRect(x: x0 - pad, y: y0 - pad, width: bw + pad * 2, height: bh + pad * 2)
            .intersection(CGRect(x: 0, y: 0, width: w, height: h))

        return Mark(
            rect: rect,
            background: ground.map { ImageFill.RGB(sum: $0.r, $0.g, $0.b, over: 1) })
    }

    /// nil unless at least half the image is one flat field.
    private static func field(_ px: [UInt8], _ n: Int) -> Field? {
        // PASS 1 — what is the background? Transparency if half the image is
        // transparent; otherwise the most common colour, as the true mean of
        // the fullest 4-bit bucket rather than the bucket's quantized key.
        var clear = 0
        var bins = [Bin](repeating: Bin(), count: 4096)
        for i in 0..<n {
            let o = i * 4
            let a = Int(px[o + 3])
            if a < minAlpha {
                clear += 1
                continue
            }
            let r = ImageFill.straighten(px[o], a)
            let g = ImageFill.straighten(px[o + 1], a)
            let b = ImageFill.straighten(px[o + 2], a)
            let key = ((r >> 4) << 8) | ((g >> 4) << 4) | (b >> 4)
            bins[key].r += r
            bins[key].g += g
            bins[key].b += b
            bins[key].n += 1
        }

        var ground: (r: Int, g: Int, b: Int)?
        if clear * 2 < n {
            guard let top = bins.max(by: { $0.n < $1.n }), top.n > 0 else { return nil }
            ground = (top.r / top.n, top.g / top.n, top.b / top.n)
        }

        // PASS 2 — ink is whatever is not background. Transparent pixels are
        // background in BOTH modes: the fill shows through them either way.
        var ink = [Bool](repeating: false, count: n)
        var flat = clear
        for i in 0..<n {
            let o = i * 4
            let a = Int(px[o + 3])
            if a < minAlpha { continue }
            if let ground {
                let r = ImageFill.straighten(px[o], a)
                let g = ImageFill.straighten(px[o + 1], a)
                let b = ImageFill.straighten(px[o + 2], a)
                if abs(r - ground.r) <= tolerance, abs(g - ground.g) <= tolerance,
                    abs(b - ground.b) <= tolerance
                {
                    flat += 1
                    continue
                }
            }
            ink[i] = true
        }

        // A FLAT field is the premise. A photo's most common colour covers a
        // sliver of it, and everything else would read as one giant mark.
        guard flat * 2 >= n else { return nil }
        return Field(ground: ground, ink: ink)
    }

    /// Ink per row and per column.
    private static func profile(_ ink: [Bool], _ w: Int, _ h: Int) -> (rows: [Int], cols: [Int]) {
        var rows = [Int](repeating: 0, count: h)
        var cols = [Int](repeating: 0, count: w)
        for y in 0..<h {
            let base = y * w
            for x in 0..<w where ink[base + x] {
                rows[y] += 1
                cols[x] += 1
            }
        }
        return (rows, cols)
    }

    /// Of the ascending `lines`, those in a consecutive stretch no thicker than
    /// `limit` — the rules, without the blocks.
    private static func thin(_ lines: [Int], limit: Int) -> [Int] {
        var out: [Int] = []
        var stretch: [Int] = []
        for line in lines {
            if let last = stretch.last, line != last + 1 {
                if stretch.count <= limit { out += stretch }
                stretch = []
            }
            stretch.append(line)
        }
        if stretch.count <= limit { out += stretch }
        return out
    }

    /// Inked stretches of `counts`, joined across empty gaps shorter than `gap`.
    private static func runs(_ counts: [Int], gap: Int) -> [Run] {
        var out: [Run] = []
        for (i, count) in counts.enumerated() where count > 0 {
            if let last = out.indices.last, i - out[last].end <= gap {
                out[last].end = i
                out[last].mass += count
            } else {
                out.append(Run(start: i, end: i, mass: count))
            }
        }
        return out
    }

    /// The image at its own size as RGBA, top row first. sRGB, because the
    /// background read out of it is handed to the card as an sRGB colour.
    private static func pixels(_ cg: CGImage, _ w: Int, _ h: Int) -> [UInt8]? {
        guard let space = CGColorSpace(name: CGColorSpace.sRGB) else { return nil }
        var data = [UInt8](repeating: 0, count: w * h * 4)
        let drew = data.withUnsafeMutableBytes { raw -> Bool in
            guard let base = raw.baseAddress,
                let ctx = CGContext(
                    data: base, width: w, height: h, bitsPerComponent: 8,
                    bytesPerRow: w * 4, space: space,
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return false }
            ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        return drew ? data : nil
    }
}
