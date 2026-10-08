// The byte check that stands in for a Content-Type the host got wrong.
//
// Both directions are pinned. A PNG served as `application/octet-stream` must
// be recognised, or the newsletter that prompted this stays a column of
// broken-image glyphs. And everything that is not a raster image must stay nil,
// because nil is the refusal: the fetcher drops the connection on it, and a
// false positive here is a stranger's html or svg answered as an image.

import Foundation

@main
struct ImageSniffTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static func main() {
        rasterFormatsAreRecognised()
        sharedContainersNeedTheirImageBrand()
        markupIsNeverAnImage()
        shortBodiesAreRefusedNotCrashed()
        thePrefixIsAllItReads()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func rasterFormatsAreRecognised() {
        let tail = [UInt8](repeating: 0, count: 32)
        equal(
            ImageSniff.mime(of: [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] + tail),
            "image/png", "png")
        equal(ImageSniff.mime(of: [0xFF, 0xD8, 0xFF, 0xE0] + tail), "image/jpeg", "jfif jpeg")
        equal(ImageSniff.mime(of: [0xFF, 0xD8, 0xFF, 0xE1] + tail), "image/jpeg", "exif jpeg")
        equal(ImageSniff.mime(of: ascii("GIF89a") + tail), "image/gif", "gif89a")
        equal(ImageSniff.mime(of: ascii("GIF87a") + tail), "image/gif", "gif87a")
        equal(
            ImageSniff.mime(of: ascii("RIFF") + [0x24, 0, 0, 0] + ascii("WEBPVP8 ") + tail),
            "image/webp", "webp")
        equal(
            ImageSniff.mime(of: [0, 0, 0, 0x1C] + ascii("ftypavif") + tail), "image/avif", "avif")
        equal(
            ImageSniff.mime(of: [0, 0, 0, 0x18] + ascii("ftypheic") + tail), "image/heic", "heic")
    }

    /// RIFF and the ISO `ftyp` box are containers video and audio share. The
    /// container alone must not pass.
    static func sharedContainersNeedTheirImageBrand() {
        let tail = [UInt8](repeating: 0, count: 32)
        equal(
            ImageSniff.mime(of: ascii("RIFF") + [0x24, 0, 0, 0] + ascii("WAVEfmt ") + tail),
            nil, "wav is not webp")
        equal(
            ImageSniff.mime(of: ascii("RIFF") + [0x24, 0, 0, 0] + ascii("AVI LIST") + tail),
            nil, "avi is not webp")
        equal(ImageSniff.mime(of: [0, 0, 0, 0x20] + ascii("ftypisom") + tail), nil, "mp4")
        equal(ImageSniff.mime(of: [0, 0, 0, 0x14] + ascii("ftypqt  ") + tail), nil, "mov")
    }

    static func markupIsNeverAnImage() {
        equal(ImageSniff.mime(of: ascii("<svg xmlns=\"http://www.w3.org/2000/svg\">")), nil, "svg")
        equal(ImageSniff.mime(of: ascii("<?xml version=\"1.0\"?><svg>")), nil, "xml-prologued svg")
        equal(ImageSniff.mime(of: ascii("<!doctype html><html>")), nil, "html")
        equal(ImageSniff.mime(of: ascii("{\"error\":\"AccessDenied\"}")), nil, "json error body")
        equal(ImageSniff.mime(of: ascii("%PDF-1.7\n")), nil, "pdf")
        // A magic number that is not at the START is just bytes in a document.
        equal(
            ImageSniff.mime(of: ascii("<p>") + [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
            nil, "png signature after markup")
    }

    static func shortBodiesAreRefusedNotCrashed() {
        equal(ImageSniff.mime(of: [UInt8]()), nil, "empty")
        equal(ImageSniff.mime(of: [0x89, 0x50, 0x4E]), nil, "truncated png signature")
        equal(ImageSniff.mime(of: ascii("RIFF") + [0x24, 0, 0, 0]), nil, "riff with no form type")
        equal(ImageSniff.mime(of: [0, 0, 0, 0x1C] + ascii("ftypav")), nil, "ftyp with half a brand")
        equal(ImageSniff.mime(of: [0xFF, 0xD8, 0xFF]), "image/jpeg", "jpeg needs only three")
    }

    /// The fetcher gives up once it holds `prefixLength` bytes and still gets
    /// nil, which is only sound if no answer depends on a byte past that.
    static func thePrefixIsAllItReads() {
        let samples: [[UInt8]] = [
            [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, 1, 2, 3],
            ascii("RIFF") + [0x24, 0, 0, 0] + ascii("WEBPVP8 "),
            [0, 0, 0, 0x1C] + ascii("ftypavif") + [0, 0, 0, 0],
            [0, 0, 0, 0x20] + ascii("ftypisom") + [0, 0, 2, 0],
            ascii("<!doctype html><html>"),
        ]
        for sample in samples {
            equal(
                ImageSniff.mime(of: sample.prefix(ImageSniff.prefixLength)),
                ImageSniff.mime(of: sample), "prefix decides \(sample.prefix(4))")
        }
    }

    static func ascii(_ text: String) -> [UInt8] { Array(text.utf8) }

    static func equal<T: Equatable>(
        _ got: T, _ want: T, _ label: String, line: Int = #line
    ) {
        checks += 1
        guard got != want else { return }
        failures += 1
        print("line \(line): \(label)\n  got:  \(got)\n  want: \(want)")
    }
}
