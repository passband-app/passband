// SwiftUI host for the live P7 squelch (PhosphorRenderer).
//
// An MTKView at 30 frames a second, one beam sweep a frame, which is the
// timebase the offline renders were cut at. It simulates at half the
// drawable's pixels: everything it draws is blurred and bloomed, so the
// other three quarters would buy nothing but fill rate.
//
// Paused means paused: the view stops asking for frames, so a backdrop that
// sits behind a mail app all day costs nothing while the window is not key.
// Reduce Motion settles the phosphor once and holds that frame.

import MetalKit
import SwiftUI

/// The renderer, built once per process OFF the main thread: compiling the
/// shader source on first use is tens of milliseconds, which is a visible
/// hitch at the exact moment the board goes quiet.
@MainActor @Observable
final class PhosphorScene {
    static let shared = PhosphorScene()

    private(set) var renderer: PhosphorRenderer?
    private(set) var isUnavailable = false
    @ObservationIgnored private var warming = false

    func warm() {
        guard !warming else { return }
        warming = true
        Task.detached(priority: .userInitiated) {
            let built = PhosphorRenderer()
            await MainActor.run {
                self.renderer = built
                self.isUnavailable = built == nil
            }
        }
    }
}

struct PhosphorBackdrop: View {
    var params = PhosphorParams()
    var paused = false

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    private var scene: PhosphorScene { .shared }

    /// finish_p7's background, shown until the renderer is ready so the
    /// scene fades up out of its own black rather than out of the page.
    static let backdrop = Color(red: 0.020, green: 0.028, blue: 0.040)

    var body: some View {
        ZStack {
            Self.backdrop
            if let renderer = scene.renderer {
                PhosphorRepresentable(
                    renderer: renderer, params: params, paused: paused, still: reduceMotion)
                    .transition(.opacity)
            }
        }
        .animation(.easeOut(duration: 0.6), value: scene.renderer != nil)
        .onAppear { scene.warm() }
    }
}

#if os(macOS)
    private struct PhosphorRepresentable: NSViewRepresentable {
        let renderer: PhosphorRenderer
        let params: PhosphorParams
        let paused: Bool
        let still: Bool

        func makeNSView(context: Context) -> PhosphorMTKView {
            PhosphorMTKView(renderer: renderer)
        }

        func updateNSView(_ view: PhosphorMTKView, context: Context) {
            view.configure(params: params, paused: paused, still: still)
        }

        static func dismantleNSView(_ view: PhosphorMTKView, coordinator: ()) {
            view.isPaused = true
        }
    }
#else
    private struct PhosphorRepresentable: UIViewRepresentable {
        let renderer: PhosphorRenderer
        let params: PhosphorParams
        let paused: Bool
        let still: Bool

        func makeUIView(context: Context) -> PhosphorMTKView {
            PhosphorMTKView(renderer: renderer)
        }

        func updateUIView(_ view: PhosphorMTKView, context: Context) {
            view.configure(params: params, paused: paused, still: still)
        }

        static func dismantleUIView(_ view: PhosphorMTKView, coordinator: ()) {
            view.isPaused = true
        }
    }
#endif

final class PhosphorMTKView: MTKView, MTKViewDelegate {
    private let renderer: PhosphorRenderer
    private var params = PhosphorParams()
    private var still = false
    /// Sweeps a Reduce Motion frame is settled over before it holds.
    private static let settleSweeps = 60

    init(renderer: PhosphorRenderer) {
        self.renderer = renderer
        super.init(frame: .zero, device: renderer.device)
        colorPixelFormat = renderer.colorFormat
        framebufferOnly = true
        preferredFramesPerSecond = 30
        #if os(macOS)
            layer?.isOpaque = true
        #endif
        delegate = self
    }

    required init(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func configure(params: PhosphorParams, paused: Bool, still: Bool) {
        self.params = params
        let wasStill = self.still
        self.still = still
        // Held (either reason): stop the clock and redraw only on demand, so
        // a resize still repaints instead of stretching the last frame.
        let hold = paused || still
        enableSetNeedsDisplay = hold
        isPaused = hold
        if still, !wasStill { settle() }
    }

    /// The working canvas follows the drawable. A no-op when it has not
    /// changed, so it is safe per frame, which also covers the first frame
    /// arriving before any size-change callback has.
    private func fitCanvas() {
        renderer.resize(width: Int(drawableSize.width / 2), height: Int(drawableSize.height / 2))
    }

    private func settle() {
        fitCanvas()
        guard let drawable = currentDrawable, let buffer = renderer.makeCommandBuffer() else { return }
        for _ in 0..<Self.settleSweeps - 1 {
            guard let warm = renderer.makeCommandBuffer() else { break }
            renderer.encode(params, into: warm, target: drawable.texture)
            warm.commit()
        }
        renderer.encode(params, into: buffer, target: drawable.texture)
        buffer.present(drawable)
        buffer.commit()
    }

    func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {
        if still { settle() }
    }

    func draw(in view: MTKView) {
        fitCanvas()
        guard let drawable = currentDrawable, let buffer = renderer.makeCommandBuffer() else { return }
        renderer.encode(params, into: buffer, target: drawable.texture)
        buffer.present(drawable)
        buffer.commit()
    }
}
