// The Reading card's logo extraction, against banners built pixel by pixel.
//
// It is asserted rather than eyeballed because every way it goes wrong is a
// CONFIDENT picture: a crop of the wrong thing still looks like a thumbnail.
// The two directions matter equally — a masthead that is not cut down stays an
// unreadable strip, and a photo that IS cut down loses half of itself — so the
// suite pins what must be found and, just as hard, what must be left alone.
//
// Every fixture is a raw RGBA buffer, top row first, so a coordinate here is
// the coordinate `CGImage.cropping(to:)` will be handed and nothing depends on
// which way up a drawing context thinks it is.

import CoreGraphics
import Foundation

@main
struct ImageTrimTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static func main() {
        logoInTheMiddleOfAMasthead()
        theFillIsTheBannersOwnBackground()
        aHairlineUnderTheMastheadIsNotInk()
        aFrameAroundTheBannerIsNotInk()
        theLogoBeatsTheNavLinkBesideIt()
        twoEqualThingsAreKeptTogether()
        aLetterSpacedWordmarkIsOneMark()
        transparentBackgroundHasNoColourToContinue()
        jpegMushIsStillBackground()
        aTightLogoIsLeftAlone()
        aPhotoIsLeftAlone()
        aPillarboxedPhotoIsLeftAlone()
        aFullWidthBlockIsNotARule()
        aSpeckIsNotAMark()
        theCropActuallyHoldsTheMark()
        theFirstLookSparesPhotos()
        inkOnTransparencyIsStagedOnItsOpposite()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: - what must be found

    /// The screenshot that started this: a white strip, a small logo dead centre.
    static func logoInTheMiddleOfAMasthead() {
        var c = Canvas(1200, 160, white)
        c.fill(560, 50, 80, 60, blue)
        let mark = ImageTrim.mark(in: c.image)
        holds(mark, x: 560, y: 50, w: 80, h: 60, "centred logo")
    }

    static func theFillIsTheBannersOwnBackground() {
        var onWhite = Canvas(1200, 160, white)
        onWhite.fill(560, 50, 80, 60, blue)
        near(ImageTrim.mark(in: onWhite.image)?.background, white, "white field")

        // Reversed out of a brand colour: the square continues the BRAND, and
        // the white logo is the ink.
        var onBlue = Canvas(1200, 160, blue)
        onBlue.fill(100, 40, 120, 80, white)
        let mark = ImageTrim.mark(in: onBlue.image)
        holds(mark, x: 100, y: 40, w: 120, h: 80, "left-aligned reversed logo")
        near(mark?.background, blue, "brand field")
    }

    /// One 2px rule under the logo used to make every column inked.
    static func aHairlineUnderTheMastheadIsNotInk() {
        var c = Canvas(1200, 160, white)
        c.fill(560, 40, 80, 60, blue)
        c.fill(0, 156, 1200, 2, grey)
        holds(ImageTrim.mark(in: c.image), x: 560, y: 40, w: 80, h: 60, "logo above a rule")
    }

    static func aFrameAroundTheBannerIsNotInk() {
        var c = Canvas(1000, 200, white)
        c.fill(0, 0, 1000, 2, grey)
        c.fill(0, 198, 1000, 2, grey)
        c.fill(0, 0, 2, 200, grey)
        c.fill(998, 0, 2, 200, grey)
        c.fill(450, 60, 100, 80, blue)
        holds(ImageTrim.mark(in: c.image), x: 450, y: 60, w: 100, h: 80, "logo in a frame")
    }

    /// Logo on the left, a thin "view in browser" far to the right, and the
    /// link sits LOWER than the logo — so a box that included it sideways, or
    /// measured rows across the whole width, would show here.
    static func theLogoBeatsTheNavLinkBesideIt() {
        var c = Canvas(1200, 160, white)
        c.fill(60, 30, 120, 70, blue)
        c.fill(1000, 120, 140, 10, grey)
        holds(ImageTrim.mark(in: c.image), x: 60, y: 30, w: 120, h: 70, "logo, not the link")
    }

    /// Neither carries the image, so neither is thrown away.
    static func twoEqualThingsAreKeptTogether() {
        var c = Canvas(1200, 160, white)
        c.fill(300, 50, 80, 60, blue)
        c.fill(700, 50, 80, 60, blue)
        holds(ImageTrim.mark(in: c.image), x: 300, y: 50, w: 480, h: 60, "both halves")
    }

    /// A lockup: a solid icon, then four thin tracked letters. The icon alone
    /// outweighs the letters, so if the air between glyphs split them into
    /// separate things, the icon would win and the name would be cut off.
    static func aLetterSpacedWordmarkIsOneMark() {
        var c = Canvas(1200, 160, white)
        c.fill(440, 50, 60, 60, blue)
        for i in 0..<4 { c.fill(520 + i * 30, 50, 10, 60, blue) }
        holds(ImageTrim.mark(in: c.image), x: 440, y: 50, w: 180, h: 60, "icon and its wordmark")
    }

    static func transparentBackgroundHasNoColourToContinue() {
        var c = Canvas(800, 200, clear)
        c.fill(360, 70, 80, 60, white)
        let mark = ImageTrim.mark(in: c.image)
        holds(mark, x: 360, y: 70, w: 80, h: 60, "white mark on transparency")
        check(mark != nil && mark?.background == nil, "transparent field reports no colour")
    }

    /// A flat field out of a JPEG is not one colour. Noise inside the
    /// tolerance must neither break the field nor count as ink.
    static func jpegMushIsStillBackground() {
        var c = Canvas(1200, 160, white)
        c.noise(amplitude: 12)
        c.fill(560, 50, 80, 60, blue)
        holds(ImageTrim.mark(in: c.image), x: 560, y: 50, w: 80, h: 60, "logo on a noisy field")
    }

    // MARK: - what must be left alone

    /// Already most of its image: the card's own fit rules draw it fine, and a
    /// crop would buy nothing but a different fill. Letters rather than a slab,
    /// so the field is still mostly background and only the GAIN says no.
    static func aTightLogoIsLeftAlone() {
        var c = Canvas(400, 120, white)
        for i in 0..<5 { c.fill(20 + i * 80, 20, 40, 80, blue) }
        check(ImageTrim.mark(in: c.image) == nil, "tight logo is not re-cropped")
    }

    /// No colour owns half of it, so there is no field for a mark to sit on.
    static func aPhotoIsLeftAlone() {
        var c = Canvas(600, 400, white)
        c.gradient()
        check(ImageTrim.mark(in: c.image) == nil, "photo has no flat field")
    }

    /// A photo with bars beside it is compact and would clear the gain — but
    /// most of the image is photo, so it is not a mark on a field.
    static func aPillarboxedPhotoIsLeftAlone() {
        var c = Canvas(600, 400, white)
        c.gradient(100, 0, 400, 400)
        check(ImageTrim.mark(in: c.image) == nil, "pillarboxed photo is not a mark")
    }

    /// A picture across the top, a caption under it. The picture's rows run
    /// edge to edge exactly like a rule's do; erasing them would crown the
    /// caption. Thick means content.
    static func aFullWidthBlockIsNotARule() {
        var c = Canvas(600, 600, white)
        c.fill(0, 0, 600, 240, blue)
        c.fill(200, 500, 200, 30, grey)
        check(ImageTrim.mark(in: c.image) == nil, "caption under a full-width block is not the mark")
    }

    static func aSpeckIsNotAMark() {
        var c = Canvas(1200, 160, white)
        c.fill(600, 80, 5, 5, blue)
        check(ImageTrim.mark(in: c.image) == nil, "a 5px speck is not blown up into art")
    }

    // MARK: - the crop itself

    /// The rect is only right if cropping the REAL image to it yields the
    /// mark: this is the one place an upside-down coordinate space would hide.
    /// The logo sits in the top left corner so a flipped axis lands on white.
    static func theCropActuallyHoldsTheMark() {
        var c = Canvas(1200, 400, white)
        c.fill(40, 30, 100, 60, blue)
        let image = c.image
        guard let mark = ImageTrim.mark(in: image), let cut = image.cropping(to: mark.rect) else {
            check(false, "corner logo found and cropped")
            return
        }
        check(cut.width < 140 && cut.height < 100, "crop is the mark's size, not the banner's")
        let centre = Canvas.pixel(of: cut, x: cut.width / 2, y: cut.height / 2)
        check(
            abs(centre.r - blue.r) < 8 && abs(centre.g - blue.g) < 8 && abs(centre.b - blue.b) < 8,
            "the middle of the crop is the logo's colour")
    }

    // MARK: - the first look, and the stage

    /// `hasFlatField` runs on the small thumbnail and decides whether the big
    /// decode happens at all. It must agree with `mark(in:)` about the premise.
    static func theFirstLookSparesPhotos() {
        var photo = Canvas(108, 72, white)
        photo.gradient()
        check(!ImageTrim.hasFlatField(photo.image), "a photo fails the first look")

        var banner = Canvas(108, 14, white)
        banner.fill(50, 4, 8, 6, blue)
        check(ImageTrim.hasFlatField(banner.image), "a masthead thumbnail passes it")
    }

    /// A grey wordmark on transparency once got a grey square, sampled from
    /// itself, and disappeared. The stage is whichever neutral the ink is NOT.
    static func inkOnTransparencyIsStagedOnItsOpposite() {
        let pale = Canvas.Colour(r: 150, g: 170, b: 180, a: 255)
        var light = Canvas(108, 24, clear)
        light.fill(10, 6, 88, 12, pale)
        var dark = Canvas(108, 24, clear)
        dark.fill(10, 6, 88, 12, blue)

        for (name, fill) in [
            ("stage", ImageFill.stageRGB(light.image)), ("dominant", ImageFill.dominantRGB(light.image)),
        ] {
            check(fill.map(luminance) ?? 255 < 80, "\(name): pale ink gets the dark stage")
        }
        for (name, fill) in [
            ("stage", ImageFill.stageRGB(dark.image)), ("dominant", ImageFill.dominantRGB(dark.image)),
        ] {
            check(fill.map(luminance) ?? 0 > 200, "\(name): dark ink gets the light stage")
        }

        // Opaque art is NOT restaged: its dominant colour is still its fill.
        var opaque = Canvas(108, 24, blue)
        opaque.fill(40, 8, 28, 8, white)
        near(ImageFill.dominantRGB(opaque.image), blue, "opaque brand banner")
    }

    static func luminance(_ c: ImageFill.RGB) -> Double {
        (0.299 * c.r + 0.587 * c.g + 0.114 * c.b) * 255
    }

    // MARK: - assertions

    static func check(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }

    /// The found box holds the whole mark and is not much bigger than it: the
    /// padding is allowed, the banner is not.
    static func holds(_ mark: ImageTrim.Mark?, x: Int, y: Int, w: Int, h: Int, _ what: String) {
        guard let rect = mark?.rect else {
            check(false, "\(what): found")
            return
        }
        let ink = CGRect(x: x, y: y, width: w, height: h)
        check(rect.contains(ink), "\(what): box \(rect) holds the ink \(ink)")
        let slack = CGFloat(max(w, h)) / 8 + 2
        check(
            rect.width <= ink.width + slack * 2 && rect.height <= ink.height + slack * 2,
            "\(what): box \(rect) is tight to the ink \(ink)")
    }

    static func near(_ rgb: ImageFill.RGB?, _ want: Canvas.Colour, _ what: String) {
        guard let rgb else {
            check(false, "\(what): has a background colour")
            return
        }
        let ok =
            abs(rgb.r * 255 - Double(want.r)) < 6 && abs(rgb.g * 255 - Double(want.g)) < 6
            && abs(rgb.b * 255 - Double(want.b)) < 6
        check(ok, "\(what): background \(rgb) is the field's colour")
    }
}

let white = Canvas.Colour(r: 255, g: 255, b: 255, a: 255)
let blue = Canvas.Colour(r: 20, g: 70, b: 160, a: 255)
let grey = Canvas.Colour(r: 120, g: 120, b: 120, a: 255)
let clear = Canvas.Colour(r: 0, g: 0, b: 0, a: 0)

/// A raw RGBA image, top row first.
struct Canvas {
    struct Colour {
        let r: Int, g: Int, b: Int, a: Int
    }

    let w: Int
    let h: Int
    var px: [UInt8]

    init(_ w: Int, _ h: Int, _ colour: Colour) {
        self.w = w
        self.h = h
        px = [UInt8](repeating: 0, count: w * h * 4)
        fill(0, 0, w, h, colour)
    }

    mutating func fill(_ x: Int, _ y: Int, _ fw: Int, _ fh: Int, _ c: Colour) {
        for row in y..<(y + fh) {
            for col in x..<(x + fw) {
                let o = (row * w + col) * 4
                px[o] = UInt8(c.r)
                px[o + 1] = UInt8(c.g)
                px[o + 2] = UInt8(c.b)
                px[o + 3] = UInt8(c.a)
            }
        }
    }

    /// Deterministic per-pixel darkening, up to `amplitude` per channel.
    mutating func noise(amplitude: Int) {
        var seed: UInt32 = 12345
        for i in 0..<(w * h) {
            seed = seed &* 1_664_525 &+ 1_013_904_223
            let d = Int(seed >> 16) % (amplitude + 1)
            for ch in 0..<3 { px[i * 4 + ch] = UInt8(max(0, Int(px[i * 4 + ch]) - d)) }
        }
    }

    /// A smooth two-axis ramp: every colour present, none of them dominant.
    mutating func gradient() { gradient(0, 0, w, h) }

    mutating func gradient(_ x: Int, _ y: Int, _ gw: Int, _ gh: Int) {
        for row in y..<(y + gh) {
            for col in x..<(x + gw) {
                let o = (row * w + col) * 4
                px[o] = UInt8(col * 255 / w)
                px[o + 1] = UInt8(row * 255 / h)
                px[o + 2] = UInt8((col + row) * 255 / (w + h))
                px[o + 3] = 255
            }
        }
    }

    var image: CGImage {
        let data = Data(px) as CFData
        return CGImage(
            width: w, height: h, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: w * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: CGDataProvider(data: data)!, decode: nil, shouldInterpolate: false,
            intent: .defaultIntent)!
    }

    /// One pixel of any image, read back through a 1x1 context.
    static func pixel(of image: CGImage, x: Int, y: Int) -> Colour {
        guard let one = image.cropping(to: CGRect(x: x, y: y, width: 1, height: 1)) else {
            return clear
        }
        var out = [UInt8](repeating: 0, count: 4)
        out.withUnsafeMutableBytes { raw in
            let ctx = CGContext(
                data: raw.baseAddress, width: 1, height: 1, bitsPerComponent: 8, bytesPerRow: 4,
                space: CGColorSpace(name: CGColorSpace.sRGB)!,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            ctx?.draw(one, in: CGRect(x: 0, y: 0, width: 1, height: 1))
        }
        return Colour(r: Int(out[0]), g: Int(out[1]), b: Int(out[2]), a: Int(out[3]))
    }
}
